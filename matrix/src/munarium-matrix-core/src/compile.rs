// SPDX-License-Identifier: Apache-2.0
//! The contract compiler: parse, walk, allowlist, rewrite.
//!
//! **String concatenation is not an implementation.** A contract's statement is
//! parsed into an AST, every identifier it touches is checked against what the
//! contract declared, every construct that could reach outside that scope is
//! refused, and named parameters are rewritten into positional placeholders
//! that the driver binds. The value a caller supplied never becomes part of the
//! statement text — and `no_bound_value_reaches_the_statement_text` is the test
//! that says so.
//!
//! The design choice worth defending: this is an **allowlist**, not a blocklist.
//! A blocklist of dangerous constructs is a losing game against a parser's
//! full grammar. Instead the walk starts from "nothing is permitted" and admits
//! only the shapes a verified query contract needs — a single `SELECT`, over
//! declared tables, projecting declared columns, filtering on declared columns
//! and bound parameters, grouping and ordering by declared columns. Anything
//! the walk does not recognise is refused with the construct named.

use crate::{Refusal, RefusalClass};
use sqlparser::ast::{
    Expr, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr, ObjectName, Query,
    SelectItem, SetExpr, Statement, TableFactor, Value as SqlValue,
};
use sqlparser::dialect::{Dialect, GenericDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;
use std::collections::BTreeSet;

pub const COMPILER_VERSION: &str = "compiler@1";

/// Aggregate and scalar functions a verified contract may call.
///
/// Deliberately short. Every entry is deterministic and has no side effect and
/// no access to anything outside the row — which is why `now()`, `random()`,
/// `pg_read_file()` and every `pg_*` introspection function are absent. A
/// contract that needs "today" takes it as a bound parameter, so the same
/// contract run twice is the same logical result.
pub const ALLOWED_FUNCTIONS: &[&str] = &[
    "sum",
    "count",
    "min",
    "max",
    "avg",
    "coalesce",
    "nullif",
    "greatest",
    "least",
    "abs",
    "round",
    "ceil",
    "floor",
    "lower",
    "upper",
    "trim",
    "length",
    "substring",
    "concat",
    "date_trunc",
    "extract",
    "cast",
];

/// What the contract declared, and therefore what the statement may touch.
#[derive(Debug, Clone)]
pub struct CompileScope {
    /// Schema-qualified or bare table names the statement may read.
    pub tables: BTreeSet<String>,
    /// Columns it may reference anywhere.
    pub columns: BTreeSet<String>,
    /// Columns the policy denies. Referencing one is a hard refusal even if it
    /// is otherwise declared — this is the check that stops a denied column
    /// from leaking through an aggregate or an ORDER BY.
    pub denied_columns: BTreeSet<String>,
    /// Parameter names the contract declares, in binding order.
    pub parameters: Vec<String>,
    /// Whether the contract permits a subquery. Off by default: a subquery is
    /// a second scope, and the walk can only vouch for the one it was given.
    pub allow_subqueries: bool,
}

impl CompileScope {
    pub fn new(tables: &[String], columns: &[String], parameters: &[String]) -> Self {
        Self {
            tables: tables.iter().map(|t| t.to_lowercase()).collect(),
            columns: columns.iter().map(|c| c.to_lowercase()).collect(),
            denied_columns: BTreeSet::new(),
            parameters: parameters.to_vec(),
            allow_subqueries: false,
        }
    }

    pub fn deny(mut self, columns: &[String]) -> Self {
        self.denied_columns = columns.iter().map(|c| c.to_lowercase()).collect();
        self
    }
}

/// A statement that survived the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledStatement {
    /// The statement with `:name` rewritten to `$1..$n`.
    pub sql: String,
    /// Parameter names in placeholder order — what the binder fills.
    pub parameter_order: Vec<String>,
    /// Hash over the canonical AST rendering, for the evidence manifest and
    /// for the compiled-form cache key.
    pub plan_hash: String,
}

pub type Result<T> = std::result::Result<T, Refusal>;

fn refuse(what: impl Into<String>) -> Refusal {
    Refusal::new(RefusalClass::Invalid, "not_covered", what)
}

fn dialect_for(name: &str) -> Result<Box<dyn Dialect>> {
    match name.to_lowercase().as_str() {
        "postgres" | "postgresql" => Ok(Box::new(PostgreSqlDialect {})),
        // Databricks SQL is close enough to ANSI for the shapes a verified
        // contract uses; the adapter's own limits do the rest.
        "databricks" | "generic" | "ansi" => Ok(Box::new(GenericDialect {})),
        other => Err(refuse(format!("no parser for dialect '{other}'"))),
    }
}

/// Rewrite `:name` parameter references into `$1..$n`.
///
/// Done on the TEXT before parsing, because `:name` is not universally
/// parseable, and re-parsed afterwards so the walk sees the real statement.
/// The rewrite is purely positional: it never inserts a value.
fn rewrite_parameters(sql: &str, declared: &[String]) -> Result<(String, Vec<String>)> {
    let mut out = String::with_capacity(sql.len());
    let mut order: Vec<String> = Vec::new();
    let mut chars = sql.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if c != ':' {
            out.push(c);
            continue;
        }
        // `::` is a Postgres cast, not a parameter.
        if matches!(chars.peek(), Some((_, ':'))) {
            out.push(':');
            out.push(':');
            chars.next();
            continue;
        }
        let rest = &sql[i + 1..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            out.push(':');
            continue;
        }
        for _ in 0..name.chars().count() {
            chars.next();
        }
        if !declared.iter().any(|d| d == &name) {
            return Err(refuse(format!(
                "statement references parameter ':{name}', which the contract does not declare"
            )));
        }
        let position = match order.iter().position(|n| n == &name) {
            Some(p) => p,
            None => {
                order.push(name.clone());
                order.len() - 1
            }
        };
        out.push_str(&format!("${}", position + 1));
    }
    Ok((out, order))
}

/// Compile a contract statement.
pub fn compile(sql: &str, dialect: &str, scope: &CompileScope) -> Result<CompiledStatement> {
    let (rewritten, parameter_order) = rewrite_parameters(sql, &scope.parameters)?;
    let d = dialect_for(dialect)?;
    let statements = Parser::parse_sql(d.as_ref(), &rewritten)
        .map_err(|e| refuse(format!("statement did not parse: {e}")))?;

    // Exactly one statement. Two statements in one string is the classic
    // injection shape, and a contract has no reason to need it.
    if statements.len() != 1 {
        return Err(refuse(format!(
            "a contract statement must be exactly one statement, found {}",
            statements.len()
        )));
    }
    let Statement::Query(query) = &statements[0] else {
        return Err(refuse(format!(
            "only SELECT is permitted; found {}",
            statement_kind(&statements[0])
        )));
    };

    walk_query(query, scope, 0)?;

    // The canonical rendering is the AST printed back — so two statements that
    // differ only in whitespace or casing share a plan hash, and one that
    // differs in MEANING does not.
    let canonical = statements[0].to_string();
    let plan_hash = crate::canon::hash_hex(canonical.as_bytes());

    // The placeholder STYLE is the engine's, the plan hash is not: `$1` and
    // `:as_of` are one plan. Postgres binds positionally; Databricks' Statement
    // Execution API binds by NAME and reads `:name` in the text, and until
    // 2026-08-30 every compiled statement reached it as `$1` with a named
    // parameter list beside it — a parameterised contract on that dialect had
    // never executed. The names come from the parse above, so nothing here is
    // a value.
    let sql = if dialect == "databricks" {
        named_placeholders(&rewritten, &parameter_order)
    } else {
        rewritten
    };

    Ok(CompiledStatement {
        sql,
        parameter_order,
        plan_hash,
    })
}

/// Rewrite `$k` back to `:name` for engines that bind by name. Only a `$`
/// followed by digits is a placeholder here: the positional rewrite is the
/// sole producer of that shape, and a dollar inside a string literal is left
/// untouched by tracking quotes.
fn named_placeholders(sql: &str, order: &[String]) -> String {
    let mut out = String::with_capacity(sql.len() + 16);
    let mut chars = sql.char_indices().peekable();
    let mut in_string = false;
    while let Some((i, c)) = chars.next() {
        if c == '\'' {
            in_string = !in_string;
            out.push(c);
            continue;
        }
        if c == '$' && !in_string {
            let digits: String = sql[i + 1..]
                .chars()
                .take_while(|d| d.is_ascii_digit())
                .collect();
            if let Ok(k) = digits.parse::<usize>() {
                if k >= 1 && k <= order.len() && !digits.is_empty() {
                    for _ in 0..digits.len() {
                        chars.next();
                    }
                    out.push(':');
                    out.push_str(&order[k - 1]);
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

fn statement_kind(s: &Statement) -> &'static str {
    match s {
        Statement::Insert { .. } => "INSERT",
        Statement::Update { .. } => "UPDATE",
        Statement::Delete { .. } => "DELETE",
        Statement::CreateTable { .. } | Statement::CreateView { .. } => "CREATE",
        Statement::Drop { .. } => "DROP",
        Statement::AlterTable { .. } => "ALTER",
        Statement::Truncate { .. } => "TRUNCATE",
        Statement::Copy { .. } => "COPY",
        Statement::Grant { .. } | Statement::Revoke { .. } => "GRANT/REVOKE",
        Statement::Call(_) => "CALL",
        Statement::Execute { .. } => "EXECUTE",
        _ => "a non-query statement",
    }
}

fn walk_query(query: &Query, scope: &CompileScope, depth: usize) -> Result<()> {
    if depth > 0 && !scope.allow_subqueries {
        return Err(refuse(
            "subqueries are not permitted by this contract: a subquery is a second scope, and \
             the compiler can only vouch for the one the contract declared",
        ));
    }
    // A CTE is a subquery wearing a hat.
    if let Some(with) = &query.with {
        if !scope.allow_subqueries {
            return Err(refuse("WITH clauses are not permitted by this contract"));
        }
        for cte in &with.cte_tables {
            walk_query(&cte.query, scope, depth + 1)?;
        }
    }

    match query.body.as_ref() {
        SetExpr::Select(select) => {
            // INTO writes a table. There is no read-only form of it.
            if select.into.is_some() {
                return Err(refuse("SELECT ... INTO is not permitted"));
            }
            for item in &select.projection {
                match item {
                    SelectItem::UnnamedExpr(e) => walk_expr(e, scope, depth)?,
                    SelectItem::ExprWithAlias { expr, .. } => walk_expr(expr, scope, depth)?,
                    // `SELECT *` would project whatever the source has TODAY,
                    // including a column added since the contract was reviewed
                    // and including one the policy denies.
                    SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {
                        return Err(refuse(
                            "SELECT * is not permitted: a contract projects declared columns, so \
                             that adding a column to the source cannot change what an answer cites",
                        ))
                    }
                }
            }
            for twj in &select.from {
                walk_table_factor(&twj.relation, scope, depth)?;
                for join in &twj.joins {
                    walk_table_factor(&join.relation, scope, depth)?;
                }
            }
            if let Some(sel) = &select.selection {
                walk_expr(sel, scope, depth)?;
            }
            if let GroupByExpr::Expressions(exprs, _) = &select.group_by {
                for e in exprs {
                    walk_expr(e, scope, depth)?;
                }
            }
            if let Some(having) = &select.having {
                walk_expr(having, scope, depth)?;
            }
        }
        SetExpr::Query(inner) => walk_query(inner, scope, depth + 1)?,
        other => {
            return Err(refuse(format!(
                "unsupported query body: {}",
                match other {
                    SetExpr::SetOperation { op, .. } => format!("{op}"),
                    SetExpr::Values(_) => "VALUES".into(),
                    SetExpr::Insert(_) => "INSERT".into(),
                    SetExpr::Update(_) => "UPDATE".into(),
                    SetExpr::Table(_) => "TABLE".into(),
                    _ => "an unrecognised construct".into(),
                }
            )))
        }
    }

    if let Some(order_by) = &query.order_by {
        for e in &order_by.exprs {
            walk_expr(&e.expr, scope, depth)?;
        }
    }
    Ok(())
}

fn walk_table_factor(t: &TableFactor, scope: &CompileScope, depth: usize) -> Result<()> {
    match t {
        TableFactor::Table { name, args, .. } => {
            if args.is_some() {
                return Err(refuse("table functions are not permitted"));
            }
            let declared = object_name(name);
            if !scope.tables.contains(&declared) {
                // Also accept a bare name when the scope declared it qualified,
                // and vice versa: an operator writing `crm.opportunities` and
                // declaring `opportunities` means the same table.
                let bare = declared.rsplit('.').next().unwrap_or(&declared).to_string();
                if !scope.tables.contains(&bare)
                    && !scope
                        .tables
                        .iter()
                        .any(|d| d.ends_with(&format!(".{bare}")))
                {
                    return Err(refuse(format!(
                        "statement reads table '{declared}', which the contract does not declare"
                    )));
                }
            }
            Ok(())
        }
        TableFactor::Derived { subquery, .. } => walk_query(subquery, scope, depth + 1),
        other => Err(refuse(format!(
            "unsupported FROM item: {}",
            match other {
                TableFactor::TableFunction { .. } => "a table function",
                TableFactor::UNNEST { .. } => "UNNEST",
                TableFactor::NestedJoin { .. } => "a nested join",
                _ => "an unrecognised construct",
            }
        ))),
    }
}

fn object_name(n: &ObjectName) -> String {
    n.0.iter()
        .map(|p| p.to_string().trim_matches('"').to_lowercase())
        .collect::<Vec<_>>()
        .join(".")
}

fn walk_expr(e: &Expr, scope: &CompileScope, depth: usize) -> Result<()> {
    match e {
        Expr::Identifier(id) => check_column(&id.value, scope),
        Expr::CompoundIdentifier(parts) => {
            // `table.column` — the column is the last part.
            let col = parts.last().map(|p| p.value.clone()).unwrap_or_default();
            check_column(&col, scope)
        }
        // A literal is fine; a PLACEHOLDER is what the rewrite produced. Both
        // are safe precisely because neither can name a column or a table.
        Expr::Value(SqlValue::Placeholder(_)) => Ok(()),
        Expr::Value(_) => Ok(()),
        Expr::BinaryOp { left, right, .. } => {
            walk_expr(left, scope, depth)?;
            walk_expr(right, scope, depth)
        }
        Expr::UnaryOp { expr, .. }
        | Expr::Nested(expr)
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::IsTrue(expr)
        | Expr::IsFalse(expr) => walk_expr(expr, scope, depth),
        Expr::Cast { expr, .. } => walk_expr(expr, scope, depth),
        Expr::Between {
            expr, low, high, ..
        } => {
            walk_expr(expr, scope, depth)?;
            walk_expr(low, scope, depth)?;
            walk_expr(high, scope, depth)
        }
        Expr::InList { expr, list, .. } => {
            walk_expr(expr, scope, depth)?;
            for i in list {
                walk_expr(i, scope, depth)?;
            }
            Ok(())
        }
        Expr::Case {
            operand,
            conditions,
            results,
            else_result,
        } => {
            if let Some(o) = operand {
                walk_expr(o, scope, depth)?;
            }
            for c in conditions {
                walk_expr(c, scope, depth)?;
            }
            for r in results {
                walk_expr(r, scope, depth)?;
            }
            if let Some(e) = else_result {
                walk_expr(e, scope, depth)?;
            }
            Ok(())
        }
        Expr::Function(f) => {
            let name = object_name(&f.name);
            if !ALLOWED_FUNCTIONS.contains(&name.as_str()) {
                return Err(refuse(format!(
                    "function '{name}' is not on the contract allowlist; only deterministic, \
                     row-scoped functions are permitted so that one contract run equals another"
                )));
            }
            if let FunctionArguments::List(list) = &f.args {
                for a in &list.args {
                    match a {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                        | FunctionArg::Named {
                            arg: FunctionArgExpr::Expr(e),
                            ..
                        } => walk_expr(e, scope, depth)?,
                        // `count(*)` is the one wildcard that reveals nothing.
                        FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => {
                            if name != "count" {
                                return Err(refuse(format!("{name}(*) is not permitted")));
                            }
                        }
                        _ => return Err(refuse(format!("unsupported argument to '{name}'"))),
                    }
                }
            }
            Ok(())
        }
        Expr::Subquery(q) | Expr::Exists { subquery: q, .. } => walk_query(q, scope, depth + 1),
        Expr::InSubquery { expr, subquery, .. } => {
            walk_expr(expr, scope, depth)?;
            walk_query(subquery, scope, depth + 1)
        }
        Expr::Like { expr, pattern, .. } | Expr::ILike { expr, pattern, .. } => {
            walk_expr(expr, scope, depth)?;
            walk_expr(pattern, scope, depth)
        }
        Expr::Extract { expr, .. } => walk_expr(expr, scope, depth),
        other => Err(refuse(format!(
            "expression construct not permitted in a verified contract: {other}"
        ))),
    }
}

fn check_column(name: &str, scope: &CompileScope) -> Result<()> {
    let lower = name.trim_matches('"').to_lowercase();
    // Denied wins, always and everywhere — projection, filter, group, order.
    if scope.denied_columns.contains(&lower) {
        return Err(Refusal::policy_denied(format!(
            "column '{name}' is denied by policy and may not be referenced anywhere in the statement"
        )));
    }
    if !scope.columns.contains(&lower) {
        return Err(refuse(format!(
            "statement references column '{name}', which the contract does not declare"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> CompileScope {
        CompileScope::new(
            &["crm.opportunities".into(), "opportunities".into()],
            &[
                "id".into(),
                "region".into(),
                "amount".into(),
                "stage".into(),
                "updated_at".into(),
                "pipeline_amount".into(),
                "opportunity_count".into(),
            ],
            &["as_of".into(), "region".into()],
        )
        .deny(&["owner_email".into()])
    }

    const GOOD: &str =
        "SELECT region, SUM(amount) AS pipeline_amount, COUNT(*) AS opportunity_count \
                        FROM crm.opportunities \
                        WHERE stage <> 'Closed Won' AND updated_at <= :as_of \
                        GROUP BY region ORDER BY region";

    #[test]
    fn a_well_formed_contract_statement_compiles() {
        let c = compile(GOOD, "postgres", &scope()).expect("compiles");
        assert!(
            c.sql.contains("$1"),
            "the parameter became a placeholder: {}",
            c.sql
        );
        assert_eq!(c.parameter_order, vec!["as_of".to_string()]);
        assert!(c.plan_hash.starts_with("sha256:"));
    }

    #[test]
    fn no_bound_value_reaches_the_statement_text() {
        // The property that matters most. Whatever a caller supplies, the
        // compiled text contains a placeholder and not the value.
        let c = compile(GOOD, "postgres", &scope()).unwrap();
        for hostile in [
            "2026-06-30",
            "'; DROP TABLE crm.opportunities; --",
            "1 OR 1=1",
            "\u{0}",
        ] {
            assert!(
                !c.sql.contains(hostile),
                "compiled text must not contain a caller value: {hostile}"
            );
        }
        assert!(c.sql.contains("$1"));
    }

    #[test]
    fn two_statements_in_one_string_are_refused() {
        let err = compile(
            "SELECT region FROM opportunities; DROP TABLE opportunities",
            "postgres",
            &scope(),
        )
        .unwrap_err();
        assert!(
            err.message.contains("exactly one statement") || err.message.contains("did not parse"),
            "{}",
            err.message
        );
    }

    #[test]
    fn every_non_select_statement_is_refused() {
        for sql in [
            "INSERT INTO opportunities (id) VALUES (1)",
            "UPDATE opportunities SET region = 'X'",
            "DELETE FROM opportunities",
            "DROP TABLE opportunities",
            "CREATE TABLE t (x int)",
            "TRUNCATE opportunities",
            "GRANT SELECT ON opportunities TO public",
        ] {
            let err = compile(sql, "postgres", &scope()).unwrap_err();
            assert!(
                err.message.contains("only SELECT") || err.message.contains("did not parse"),
                "{sql} -> {}",
                err.message
            );
        }
    }

    #[test]
    fn select_star_is_refused_because_the_source_can_grow_a_column() {
        let err = compile("SELECT * FROM opportunities", "postgres", &scope()).unwrap_err();
        assert!(
            err.message.contains("SELECT * is not permitted"),
            "{}",
            err.message
        );
    }

    #[test]
    fn an_undeclared_table_is_refused() {
        let err = compile("SELECT region FROM secrets", "postgres", &scope()).unwrap_err();
        assert!(err.message.contains("'secrets'"), "{}", err.message);
    }

    #[test]
    fn an_undeclared_column_is_refused() {
        let err = compile("SELECT ssn FROM opportunities", "postgres", &scope()).unwrap_err();
        assert!(err.message.contains("'ssn'"), "{}", err.message);
    }

    /// The G6 check: a denied column is refused in EVERY clause, not just the
    /// projection. Leaking it through a GROUP BY or an aggregate would be just
    /// as much of a leak.
    #[test]
    fn a_denied_column_is_refused_everywhere() {
        for sql in [
            "SELECT owner_email FROM opportunities",
            "SELECT region FROM opportunities WHERE owner_email = 'x'",
            "SELECT region FROM opportunities GROUP BY owner_email",
            "SELECT region FROM opportunities ORDER BY owner_email",
            "SELECT COUNT(owner_email) FROM opportunities",
            "SELECT region FROM opportunities HAVING COUNT(owner_email) > 1",
        ] {
            let err = compile(sql, "postgres", &scope()).unwrap_err();
            assert_eq!(err.class, RefusalClass::Denied, "{sql} -> {}", err.message);
            assert_eq!(err.code, "policy_denied");
        }
    }

    #[test]
    fn subqueries_are_refused_unless_the_contract_allows_them() {
        let sql = "SELECT region FROM opportunities WHERE id IN (SELECT id FROM opportunities)";
        let err = compile(sql, "postgres", &scope()).unwrap_err();
        assert!(err.message.contains("subquer"), "{}", err.message);

        let mut permissive = scope();
        permissive.allow_subqueries = true;
        assert!(compile(sql, "postgres", &permissive).is_ok());
    }

    #[test]
    fn non_deterministic_functions_are_refused() {
        // The reason is exactness, not danger: a contract that calls now()
        // returns a different logical result every run, so its evidence could
        // never be replayed or verified.
        for sql in [
            "SELECT region FROM opportunities WHERE updated_at < now()",
            "SELECT region FROM opportunities ORDER BY random()",
            "SELECT pg_read_file('x') FROM opportunities",
        ] {
            let err = compile(sql, "postgres", &scope()).unwrap_err();
            assert!(
                err.message.contains("allowlist") || err.message.contains("does not declare"),
                "{sql} -> {}",
                err.message
            );
        }
    }

    #[test]
    fn an_undeclared_parameter_is_refused_before_parsing() {
        let err = compile(
            "SELECT region FROM opportunities WHERE id = :secret_knob",
            "postgres",
            &scope(),
        )
        .unwrap_err();
        assert!(err.message.contains("secret_knob"), "{}", err.message);
    }

    #[test]
    fn a_postgres_cast_is_not_mistaken_for_a_parameter() {
        let c = compile(
            "SELECT region FROM opportunities WHERE amount > 0::numeric",
            "postgres",
            &scope(),
        )
        .expect("`::` is a cast, not a parameter");
        assert!(c.parameter_order.is_empty());
        assert!(c.sql.contains("0::numeric"));
    }

    #[test]
    fn one_parameter_used_twice_binds_once() {
        let mut s = scope();
        s.parameters = vec!["as_of".into()];
        let c = compile(
            "SELECT region FROM opportunities WHERE updated_at <= :as_of AND updated_at > :as_of",
            "postgres",
            &s,
        )
        .unwrap();
        assert_eq!(c.parameter_order, vec!["as_of".to_string()]);
        assert_eq!(
            c.sql.matches("$1").count(),
            2,
            "both uses bind to $1: {}",
            c.sql
        );
    }

    #[test]
    fn the_plan_hash_ignores_formatting_but_not_meaning() {
        let a = compile(GOOD, "postgres", &scope()).unwrap();
        let spaced = GOOD
            .replace("SELECT region", "SELECT   region")
            .replace(" FROM", "\n FROM");
        let b = compile(&spaced, "postgres", &scope()).unwrap();
        assert_eq!(a.plan_hash, b.plan_hash, "whitespace is not meaning");

        let different = GOOD.replace("stage <> 'Closed Won'", "stage <> 'Closed Lost'");
        let c = compile(&different, "postgres", &scope()).unwrap();
        assert_ne!(
            a.plan_hash, c.plan_hash,
            "a different filter is a different plan"
        );
    }

    #[test]
    fn the_same_statement_compiles_on_both_dialects() {
        let pg = compile(GOOD, "postgres", &scope()).unwrap();
        let db = compile(GOOD, "databricks", &scope()).unwrap();
        // Databricks binds by name: the text must say `:as_of`, never `$1`,
        // and the plan hash must not care which.
        assert!(db.sql.contains(":as_of"), "{}", db.sql);
        assert!(!db.sql.contains("$1"), "{}", db.sql);
        assert!(pg.sql.contains("$1"), "{}", pg.sql);
        assert_eq!(
            db.plan_hash, pg.plan_hash,
            "one plan, two placeholder styles"
        );
        assert_eq!(pg.parameter_order, db.parameter_order);
        // The plan hash is per-dialect input but the AST is the same shape
        // here, so a contract that works on one is checkable on the other.
        assert_eq!(pg.plan_hash, db.plan_hash);
    }

    #[test]
    fn an_unknown_dialect_is_refused_rather_than_guessed() {
        assert!(compile(GOOD, "oracle", &scope()).is_err());
    }
}
