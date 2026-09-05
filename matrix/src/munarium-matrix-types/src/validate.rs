// SPDX-License-Identifier: Apache-2.0
//! Fail-closed asset validation.
//!
//! Every rule here has a matching file in `matrix/fixtures/assets/invalid/`,
//! and the fixture test asserts the exact code — so a rule cannot be added
//! without a case that proves it fires, and a rule cannot be quietly weakened
//! without a case turning green that should be red.

use crate::assets::*;
use munarium_matrix_core::checkpoint::validate_sync;
use munarium_matrix_core::result::{Column, ResultSchema, RowIdRule};
use munarium_matrix_core::value::ColumnType;
use serde::{Deserialize, Serialize};

/// A validation finding. `code` is the stable identity; the message is for a
/// human and may be reworded without breaking a test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Finding {
    pub code: String,
    pub path: String,
    pub message: String,
}

impl Finding {
    fn new(code: &str, path: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Shapes that look like a secret rather than a reference to one. A
/// `credentialRef` is a NAME; a value that looks like a key means someone
/// pasted the key itself into a file that lands in git.
fn looks_like_a_literal_secret(s: &str) -> bool {
    let t = s.trim();
    // The length rule catches an opaque token. It must NOT catch a hostname:
    // `psql-sample-a1b2c3d4.postgres.database.azure.com` is 48 characters and
    // `adb-1234567890123456.11.azuredatabricks.net` is 44, and
    // the first live mode-C run was refused at `spec.connection.host` for
    // exactly this — meaning no Azure Postgres or Databricks source could ever
    // have been registered on a deployed Matrix. Every offline fixture used a
    // short host, so only a live deployment could find it.
    (t.len() > 40 && !looks_like_a_hostname(t))
        || t.starts_with("dapi")            // Databricks PAT
        || t.starts_with("sk-")             // provider key shapes
        || t.starts_with("xoxb-")
        || t.starts_with("eyJ")             // a JWT, which is dotted like a host
        || carries_userinfo(t)             // a connection string WITH credentials
        || t.contains("password=")
        || t.contains("BEGIN PRIVATE KEY")
}

/// A URL whose authority carries credentials — `scheme://user:pass@host`.
///
/// `contains("://")` was the rule until 2026-08-30, and it refused every
/// plain URL: a Cube or dbt Semantic Layer source is REACHED at a base URL
/// (`http://cube:4000`), so no semantic provider could have been registered
/// at all — the same shape of false positive as the 40-character hostname
/// rule above, found the same way, by a source that could not be applied.
/// What actually matters is userinfo, which is a credential in a field that
/// is not `credentialRef`.
fn carries_userinfo(t: &str) -> bool {
    let Some((_scheme, rest)) = t.split_once("://") else {
        // No scheme: a bare `user:pass@host` is still a credential in a
        // connection value, and a bare `host:port` is not.
        return t
            .split_once('@')
            .is_some_and(|(userinfo, _)| userinfo.contains(':') && !userinfo.contains('/'));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority.contains('@')
}

/// The RFC shape of a DNS name: two or more labels of 1..=63 characters from
/// `[A-Za-z0-9-]`, joined by `.`, 253 characters or fewer. Deliberately
/// narrower than "contains a dot": a base64url token may contain `-` and `_`
/// and a JWT is three dotted segments, and `_` is not in the label alphabet
/// while a JWT segment is longer than a label.
fn looks_like_a_hostname(t: &str) -> bool {
    if t.len() > 253 || !t.contains('.') {
        return false;
    }
    t.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// A plain, unquoted Postgres identifier: what a replication slot or a
/// publication may be named without quoting rules entering the picture.
fn is_pg_identifier(v: &str) -> bool {
    let mut chars = v.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && v.len() <= 63
}

#[cfg(test)]
mod secret_shape_tests {
    use super::{carries_userinfo, looks_like_a_literal_secret};

    #[test]
    fn a_plain_url_is_a_location_and_a_url_with_userinfo_is_a_credential() {
        // A semantic provider is reached at a URL; refusing every `://` made
        // one unregisterable (2026-08-30).
        assert!(!looks_like_a_literal_secret("http://cube:4000"));
        assert!(!looks_like_a_literal_secret(
            "https://semantic-layer.cloud.getdbt.com"
        ));
        assert!(!carries_userinfo("http://cube:4000/cubejs-api/v1"));
        // What actually matters.
        assert!(looks_like_a_literal_secret(
            "postgres://matrix:hunter2@db.internal:5432/matrix"
        ));
        assert!(carries_userinfo("matrix:hunter2@db.internal"));
        // A path containing '@' after the authority is not userinfo.
        assert!(!carries_userinfo("https://host/path/@handle"));
    }
}

fn check_envelope(
    api_version: &str,
    kind: &str,
    expect_kind: &str,
    meta: &Metadata,
) -> Vec<Finding> {
    let mut f = Vec::new();
    if api_version != API_VERSION {
        f.push(Finding::new(
            "envelope.api-version",
            "apiVersion",
            format!("apiVersion must be {API_VERSION}, got '{api_version}'"),
        ));
    }
    if kind != expect_kind {
        f.push(Finding::new(
            "envelope.kind",
            "kind",
            format!("kind must be {expect_kind}, got '{kind}'"),
        ));
    }
    if meta.name.trim().is_empty() {
        f.push(Finding::new(
            "metadata.name-empty",
            "metadata.name",
            "name must be non-empty",
        ));
    }
    if meta.name.contains('@') {
        f.push(Finding::new(
            "metadata.name-at",
            "metadata.name",
            "name must not contain '@' — it separates name from version in a reference",
        ));
    }
    if meta.name != meta.name.to_lowercase() || meta.name.contains(' ') {
        f.push(Finding::new(
            "metadata.name-shape",
            "metadata.name",
            "name must be lowercase without spaces",
        ));
    }
    if meta.version == 0 {
        f.push(Finding::new(
            "metadata.version-zero",
            "metadata.version",
            "version starts at 1",
        ));
    }
    f
}

// ---------------------------------------------------------------------------
// DataSource
// ---------------------------------------------------------------------------

fn connection_str(spec: &DataSourceSpec, key: &str) -> Option<String> {
    spec.connection
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(str::to_string)
}

pub fn validate_data_source(doc: &DataSourceDoc) -> Vec<Finding> {
    let mut f = check_envelope(&doc.api_version, &doc.kind, "DataSource", &doc.metadata);
    let spec = &doc.spec;

    if let Some(cred) = &spec.credential_ref {
        if looks_like_a_literal_secret(cred) {
            f.push(Finding::new(
                "credential.literal",
                "spec.credentialRef",
                "credentialRef must NAME a secret, not contain one",
            ));
        }
    }
    // A connection map that carries a secret value is the same mistake one
    // level down, and is the more likely one.
    for (k, v) in &spec.connection {
        if let Some(s) = v.as_str() {
            if looks_like_a_literal_secret(s) {
                f.push(Finding::new(
                    "credential.literal",
                    &format!("spec.connection.{k}"),
                    "connection settings must not carry secret material; use credentialRef",
                ));
            }
        }
    }

    if spec.egress.allow_hosts.is_empty() {
        f.push(Finding::new(
            "egress.empty-allowlist",
            "spec.egress.allowHosts",
            "egress is default-deny: declare the hosts this source may reach",
        ));
    }

    match spec.authorization.strategy {
        AuthorizationStrategy::PerClassPrincipals => {
            if spec.authorization.classes.is_empty() {
                f.push(Finding::new(
                    "authorization.no-classes",
                    "spec.authorization.classes",
                    "strategy per_class_principals declares no classes",
                ));
            }
            for (i, c) in spec.authorization.classes.iter().enumerate() {
                if c.credential_ref.is_none() {
                    f.push(Finding::new(
                        "authorization.class-without-principal",
                        &format!("spec.authorization.classes[{i}]"),
                        format!(
                            "class '{}' has no credentialRef, so it has no principal",
                            c.name
                        ),
                    ));
                }
            }
        }
        AuthorizationStrategy::SourceNative => {
            if !spec.authorization.classes.is_empty() {
                f.push(Finding::new(
                    "authorization.classes-ignored",
                    "spec.authorization.classes",
                    "strategy source_native does not use declared classes",
                ));
            }
        }
        AuthorizationStrategy::Refuse => {}
    }
    if spec.authorization.classes.len() > spec.authorization.max_authorization_classes {
        f.push(Finding::new(
            "authorization.too-many-classes",
            "spec.authorization.classes",
            format!(
                "{} classes declared, over maxAuthorizationClasses {}",
                spec.authorization.classes.len(),
                spec.authorization.max_authorization_classes
            ),
        ));
    }

    if let Some(sync) = &spec.sync {
        if let Err(e) = validate_sync(sync.mode, sync.watermark.as_ref()) {
            f.push(Finding::new("sync.watermark", "spec.sync", e.to_string()));
        }
        if sync.entity.key.is_empty() {
            f.push(Finding::new(
                "sync.no-key",
                "spec.sync.entity.key",
                "a synced entity needs a stable key: it is the document's logical path",
            ));
        }
        // The key must be inside the projection, or the renderer cannot build
        // a path from data it never read.
        if !sync.projection.is_empty() {
            for k in &sync.entity.key {
                if !sync.projection.contains(k) {
                    f.push(Finding::new(
                        "sync.key-outside-projection",
                        "spec.sync.projection",
                        format!("key column '{k}' is not in the projection"),
                    ));
                }
            }
            if let Some(w) = &sync.watermark {
                if !sync.projection.contains(&w.column) {
                    f.push(Finding::new(
                        "sync.watermark-outside-projection",
                        "spec.sync.projection",
                        format!("watermark column '{}' is not in the projection", w.column),
                    ));
                }
            }
        }
        // A configured slot or publication name is a Postgres identifier,
        // and it is interpolated into the statement a refusal prints for the
        // operator to run — so anything but an identifier is refused here
        // rather than handed to `pg_create_logical_replication_slot`.
        if let Some(cdc) = &sync.cdc {
            for (field, value) in [("slot", &cdc.slot), ("publication", &cdc.publication)] {
                if let Some(v) = value {
                    if !is_pg_identifier(v) {
                        f.push(Finding::new(
                            "sync.cdc-name",
                            &format!("spec.sync.cdc.{field}"),
                            format!(
                                "'{v}' is not a plain Postgres identifier ([a-z_][a-z0-9_]*, at most 63 bytes)"
                            ),
                        ));
                    }
                }
            }
        }
    }
    // The three SQL engines beyond Postgres, each with the settings without which
    // it cannot be reached — checked here so an unusable source is refused at
    // apply rather than at 3am during a sync.
    //
    // The shared reason for every `*-no-host`-shaped rule below: the credential
    // for these adapters is a whole connection string or a bearer token, which
    // is a SECRET and therefore unreadable at validation time. `connection.host`
    // is the only place the egress allowlist has to check against, and a source
    // with none silently skips the check entirely.
    match doc.spec.adapter {
        AdapterKind::Sqlserver => {
            if connection_str(&doc.spec, "host").is_none() {
                f.push(Finding::new(
                    "datasource.sqlserver-no-host",
                    "spec.connection.host",
                    "a sqlserver source names its server host: the credential is an ADO.NET \
                     connection string and cannot be read at validation time, so this is the \
                     only value the egress allowlist can be checked against",
                ));
            }
        }
        AdapterKind::Snowflake => {
            if connection_str(&doc.spec, "account").is_none() {
                f.push(Finding::new(
                    "datasource.snowflake-no-account",
                    "spec.connection.account",
                    "a snowflake source names its account host \
                     (<account>.snowflakecomputing.com); it is what the adapter is scoped to \
                     and what egress is checked against",
                ));
            }
            if connection_str(&doc.spec, "warehouse").is_none() {
                f.push(Finding::new(
                    "datasource.snowflake-no-warehouse",
                    "spec.connection.warehouse",
                    "a snowflake statement runs on a warehouse; without one every execute \
                     fails at the API, and a source that cannot execute should not apply",
                ));
            }
        }
        AdapterKind::Bigquery => {
            if connection_str(&doc.spec, "project").is_none() {
                f.push(Finding::new(
                    "datasource.bigquery-no-project",
                    "spec.connection.project",
                    "a bigquery source names the project whose jobs it creates; it is part of \
                     the request URL and there is no default",
                ));
            }
            if connection_str(&doc.spec, "dataset").is_none() {
                f.push(Finding::new(
                    "datasource.bigquery-no-dataset",
                    "spec.connection.dataset",
                    "a bigquery source names its default dataset: a contract's unqualified \
                     table name has nothing to resolve against without one",
                ));
            }
        }
        _ => {}
    }

    // A declared planner surface is checked HERE, at apply time.
    //
    // The adapter checks it again when it is constructed, which is not
    // duplication so much as the difference between "this asset cannot be
    // applied" and "this process cannot start". An allowlist that turns out
    // to be empty at the first question is one somebody has already built a
    // workflow on.
    if let Some(raw) = doc.spec.connection.get("genie") {
        match serde_json::from_value::<munarium_matrix_core::planner::PlannerSpec>(raw.clone()) {
            Ok(g) => {
                if g.space_id.trim().is_empty() {
                    f.push(Finding::new(
                        "datasource.planner-no-space",
                        "spec.connection.genie.spaceId",
                        "a planner block names the space it may reach; without one there is                          nothing for the source to be scoped to",
                    ));
                }
                if g.trusted_assets.is_empty() && g.allowed_tables.is_empty() {
                    f.push(Finding::new(
                        "datasource.planner-no-allowlist",
                        "spec.connection.genie",
                        "a planner block declares `trustedAssets` or `allowedTables`:                          planner-assist with neither is 'run whatever the model wrote', which                          is the one thing this system exists not to do, and defaulting it to                          'everything' would make the safe posture the one nobody configures",
                    ));
                }
            }
            Err(e) => f.push(Finding::new(
                "datasource.planner-malformed",
                "spec.connection.genie",
                format!("the genie block does not parse: {e}"),
            )),
        }
    }

    // A semantic provider owns metric definitions, not tables. A
    // `sync` block on one would declare a materialization the adapter refuses
    // at run time; refusing it here means the asset cannot promise it.
    if matches!(doc.spec.adapter, AdapterKind::Cube | AdapterKind::Dbt) {
        if doc.spec.sync.is_some() {
            f.push(Finding::new(
                "datasource.semantic-provider-sync",
                "spec.sync",
                "a semantic provider has no tables to materialize; mode A is not available \
                 on this adapter, and declaring a sync would promise coverage it cannot give",
            ));
        }
        if connection_str(&doc.spec, "baseUrl").is_none() {
            f.push(Finding::new(
                "datasource.semantic-provider-no-url",
                "spec.connection.baseUrl",
                "a semantic provider is reached at a URL",
            ));
        }
        if doc.spec.adapter == AdapterKind::Dbt
            && connection_str(&doc.spec, "environmentId").is_none()
        {
            f.push(Finding::new(
                "datasource.dbt-no-environment",
                "spec.connection.environmentId",
                "a dbt Semantic Layer source names the environment whose metrics it exposes",
            ));
        }
    }
    f
}

// ---------------------------------------------------------------------------
// QueryContract
// ---------------------------------------------------------------------------

/// The declared result, as a core [`ResultSchema`] — the bridge between the
/// asset grammar and the kernel that hashes things.
/// The allowlist a contract's statement is compiled against.
///
/// One function, called by apply-time validation, execute and verify. The
/// defect this replaces (2026-08-29) was a divergent copy living inline in the
/// worker's `execute`: it built the scope from `result.columns` plus parameter
/// names, so every aliased aggregate and every filter on an unprojected column
/// was refused as undeclared, and the table name `"opportunities"` was
/// hard-coded. Mode B could not execute a realistic contract. The unit test
/// that should have caught it built its own scope by hand, and so tested the
/// compiler rather than the wiring.
pub fn compile_scope(
    spec: &crate::assets::QueryContractSpec,
) -> munarium_matrix_core::compile::CompileScope {
    // Result columns stay in scope: a pass-through projection names them in
    // both places, and every contract written before `reads` existed relies on
    // it.
    let mut columns: Vec<String> = contract_schema(spec)
        .columns
        .iter()
        .map(|c| c.name.clone())
        .collect();
    columns.extend(spec.reads.columns.iter().cloned());
    columns.extend(spec.parameters.keys().cloned());

    // The source name alone is not a table. It stays because a statement may
    // legitimately qualify with it, but the tables a statement may read are the
    // ones the contract declares.
    let mut tables: Vec<String> = vec![spec.source.clone()];
    for table in &spec.reads.tables {
        tables.push(table.clone());
        if !table.contains('.') {
            tables.push(format!("{}.{}", spec.source, table));
        }
    }

    munarium_matrix_core::compile::CompileScope::new(
        &tables,
        &columns,
        &spec.parameters.keys().cloned().collect::<Vec<_>>(),
    )
    // Applied last, and it wins: a `reads` declaration cannot grant a column
    // that policy denies.
    .deny(&spec.policy.denied_columns)
}

pub fn contract_schema(spec: &QueryContractSpec) -> ResultSchema {
    let order: Vec<String> = if spec.result.column_order.is_empty() {
        spec.result.columns.keys().cloned().collect()
    } else {
        spec.result.column_order.clone()
    };
    let columns: Vec<Column> = order
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let c = spec.result.columns.get(name)?;
            Some(Column {
                id: format!("c{i}"),
                name: name.clone(),
                ty: c.ty,
                nullable: c.nullable,
                scale: c.scale,
                unit: c.unit.clone(),
                additivity: c.additivity,
                key: c.key,
                element_type: c.element_type,
            })
        })
        .collect();
    let has_keys = columns.iter().any(|c| c.key);
    ResultSchema {
        columns,
        row_id_rule: if has_keys {
            RowIdRule::Keys
        } else {
            RowIdRule::Position
        },
        order_by: spec.result.order_by.clone(),
    }
}

pub fn validate_query_contract(doc: &QueryContractDoc) -> Vec<Finding> {
    let mut f = check_envelope(&doc.api_version, &doc.kind, "QueryContract", &doc.metadata);
    let spec = &doc.spec;

    if spec.source.trim().is_empty() {
        f.push(Finding::new(
            "contract.no-source",
            "spec.source",
            "a contract names its source",
        ));
    }
    if spec.statement_by_dialect.is_empty() {
        f.push(Finding::new(
            "contract.no-statement",
            "spec.statementByDialect",
            "declare a statement for at least one dialect",
        ));
    }
    // Compile every inline statement against the contract's own allowlist.
    //
    // Until 2026-08-29 nothing parsed the SQL before it ran: a contract applied
    // cleanly, passed `mxctl validate`, and refused at first execution — with a
    // message about an undeclared column that the author had no way to act on,
    // because there was nowhere to declare one. `reads` is that place, and this
    // is what makes forgetting it a validation error instead of a runtime
    // surprise.
    //
    // File-backed statements are skipped: the bytes are not here, and the hash
    // is what makes those "operator-reviewed".
    for (dialect, st) in &spec.statement_by_dialect {
        let Some(sql) = st.inline.as_deref() else {
            continue;
        };
        if let Err(e) = munarium_matrix_core::compile::compile(sql, dialect, &compile_scope(spec)) {
            f.push(Finding::new(
                "contract.statement-not-compilable",
                &format!("spec.statementByDialect.{dialect}"),
                format!(
                    "{e}. Columns the statement reads that are not result columns must be \
                     declared in `spec.reads.columns`, and tables in `spec.reads.tables`."
                ),
            ));
        }
    }

    for (dialect, st) in &spec.statement_by_dialect {
        match (&st.inline, &st.file) {
            (None, None) => f.push(Finding::new(
                "contract.statement-empty",
                &format!("spec.statementByDialect.{dialect}"),
                "declare either `inline` or `file` + `hash`",
            )),
            (Some(_), Some(_)) => f.push(Finding::new(
                "contract.statement-ambiguous",
                &format!("spec.statementByDialect.{dialect}"),
                "declare `inline` or `file`, not both",
            )),
            (None, Some(_)) if st.hash.is_none() => f.push(Finding::new(
                "contract.statement-unhashed",
                &format!("spec.statementByDialect.{dialect}"),
                "a file-backed statement must declare its sha256 — the hash is what makes \
                 'operator-reviewed' checkable",
            )),
            _ => {}
        }
    }

    // Result identity: the canon@1 rule, surfaced as an asset finding so it is
    // caught at apply rather than at seal.
    let schema = contract_schema(spec);
    if let Err(e) = schema.validate() {
        f.push(Finding::new(
            "result.not-identifiable",
            "spec.result",
            e.to_string(),
        ));
    }
    if !spec.result.column_order.is_empty() {
        for name in &spec.result.column_order {
            if !spec.result.columns.contains_key(name) {
                f.push(Finding::new(
                    "result.order-unknown-column",
                    "spec.result.columnOrder",
                    format!("columnOrder names '{name}', which is not a declared column"),
                ));
            }
        }
        for name in spec.result.columns.keys() {
            if !spec.result.column_order.contains(name) {
                f.push(Finding::new(
                    "result.order-missing-column",
                    "spec.result.columnOrder",
                    format!("column '{name}' is not in columnOrder"),
                ));
            }
        }
    }

    // A denied column may not be referenced anywhere: not as a result column,
    // not in an ordering, not inside a derivation. This is the rule that stops
    // a "denied" column from leaking through an aggregate.
    for denied in &spec.policy.denied_columns {
        if spec.result.columns.contains_key(denied) {
            f.push(Finding::new(
                "policy.denied-column-in-result",
                "spec.result.columns",
                format!("column '{denied}' is denied by policy but declared in the result"),
            ));
        }
        if spec.result.order_by.contains(denied) {
            f.push(Finding::new(
                "policy.denied-column-in-order",
                "spec.result.orderBy",
                format!("column '{denied}' is denied by policy but used in orderBy"),
            ));
        }
        for (name, d) in &spec.result.derivations {
            let refs = [&d.over, &d.numerator, &d.denominator];
            if refs.iter().any(|r| r.as_deref() == Some(denied.as_str())) {
                f.push(Finding::new(
                    "policy.denied-column-in-derivation",
                    &format!("spec.result.derivations.{name}"),
                    format!("derivation '{name}' references denied column '{denied}'"),
                ));
            }
        }
    }

    for (name, d) in &spec.result.derivations {
        if let Err(e) = d.to_derivation(name).validate(&schema) {
            f.push(Finding::new(
                "derivation.invalid",
                &format!("spec.result.derivations.{name}"),
                e.to_string(),
            ));
        }
    }

    for (name, p) in &spec.parameters {
        if p.ty == ColumnType::Decimal && p.scale.is_none() {
            f.push(Finding::new(
                "parameter.decimal-without-scale",
                &format!("spec.parameters.{name}"),
                "a decimal parameter must declare its scale",
            ));
        }
        if p.allowed_values.is_some() && p.allowed_values_from.is_some() {
            f.push(Finding::new(
                "parameter.ambiguous-allowed-values",
                &format!("spec.parameters.{name}"),
                "declare allowedValues or allowedValuesFrom, not both",
            ));
        }
    }

    if spec.limits.max_bytes > 1024 * 1024 {
        // Not an error: a bigger artifact simply cannot use the one-round-trip
        // inline seal. Saying so at apply time beats discovering it in a p95.
        f.push(Finding::new(
            "limits.above-inline-seal",
            "spec.limits.maxBytes",
            "maxBytes is above the 1 MiB inline-seal ceiling, so results will seal in three \
             round-trips instead of one",
        ));
    }

    for (i, q) in spec.verified_questions.iter().enumerate() {
        for pname in q.parameters.keys() {
            if !spec.parameters.contains_key(pname) {
                f.push(Finding::new(
                    "verified.unknown-parameter",
                    &format!("spec.verifiedQuestions[{i}].parameters"),
                    format!("'{pname}' is not a declared parameter"),
                ));
            }
        }
        for required in spec
            .parameters
            .iter()
            .filter(|(_, p)| p.required)
            .map(|(n, _)| n)
        {
            if !q.parameters.contains_key(required) {
                f.push(Finding::new(
                    "verified.missing-required-parameter",
                    &format!("spec.verifiedQuestions[{i}].parameters"),
                    format!("required parameter '{required}' is not bound"),
                ));
            }
        }
        if q.expect.rows.is_none()
            && q.expect.logical_result_hash.is_none()
            && q.expect.invariants.is_empty()
        {
            f.push(Finding::new(
                "verified.expects-nothing",
                &format!("spec.verifiedQuestions[{i}].expect"),
                "a verified question that expects nothing verifies nothing",
            ));
        }
    }
    f
}

// ---------------------------------------------------------------------------
// ClaimMapping
// ---------------------------------------------------------------------------

/// Placeholders in `subject.{col}` / `scope.{col}` templates.
pub fn template_placeholders(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        out.push(rest[open + 1..open + close].to_string());
        rest = &rest[open + close + 1..];
    }
    out
}

pub fn validate_claim_mapping(doc: &ClaimMappingDoc) -> Vec<Finding> {
    let mut f = check_envelope(&doc.api_version, &doc.kind, "ClaimMapping", &doc.metadata);
    let spec = &doc.spec;

    if spec.entity.key.is_empty() {
        f.push(Finding::new(
            "mapping.no-key",
            "spec.entity.key",
            "a mapping needs a stable key: it is the observation's row_key and its idempotency",
        ));
    }
    if spec.properties.is_empty() {
        f.push(Finding::new(
            "mapping.no-properties",
            "spec.properties",
            "a mapping with no properties observes nothing",
        ));
    }

    // Templates may only reference key columns: a subject built from a mutable
    // column changes identity when that column changes, which silently splits
    // an entity's history in two.
    for (label, template) in [
        ("subjectTemplate", Some(&spec.entity.subject_template)),
        ("scopeTemplate", spec.entity.scope_template.as_ref()),
    ] {
        let Some(t) = template else { continue };
        for p in template_placeholders(t) {
            if !spec.entity.key.contains(&p) {
                f.push(Finding::new(
                    "mapping.template-non-key",
                    &format!("spec.entity.{label}"),
                    format!(
                        "template references '{p}', which is not a key column; a subject built \
                         from a mutable column changes identity when the column changes"
                    ),
                ));
            }
        }
    }

    for (name, p) in &spec.properties {
        if p.ty == ColumnType::Decimal && p.scale.is_none() {
            f.push(Finding::new(
                "mapping.decimal-without-scale",
                &format!("spec.properties.{name}"),
                "a decimal property must declare its scale, or two spellings of one number \
                 will read as a discrepancy",
            ));
        }
    }

    for name in spec.changes.keys() {
        if !spec.properties.contains_key(name) {
            f.push(Finding::new(
                "mapping.change-unknown-property",
                &format!("spec.changes.{name}"),
                format!("change rule for '{name}', which is not a declared property"),
            ));
        }
    }

    if spec.identity_min_confidence_out_of_range() {
        f.push(Finding::new(
            "mapping.confidence-range",
            "spec.entity.identity.minConfidence",
            "minConfidence must be between 0 and 1",
        ));
    }

    // The alias table and the resolver that reads it must arrive together.
    // Either one alone is configuration that does nothing while reading as
    // though it does something — the failure mode the T0 mapping shipped with
    // and that made trap 9 unreachable for a whole phase.
    let identity = &spec.entity.identity;
    match (identity.resolver, &identity.aliases) {
        (Resolver::TerminologyAlias, None) => f.push(Finding::new(
            "mapping.alias-resolver-without-table",
            "spec.entity.identity.aliases",
            "resolver 'terminology_alias' is declared with no alias table, so it resolves              nothing; declare the table or use resolver 'entity_key'",
        )),
        (Resolver::EntityKey, Some(_)) => f.push(Finding::new(
            "mapping.alias-table-without-resolver",
            "spec.entity.identity.resolver",
            "an alias table is declared but resolver 'entity_key' never reads it; use              resolver 'terminology_alias' or remove the table",
        )),
        _ => {}
    }

    if let Some(aliases) = &identity.aliases {
        if !(0.0..=1.0).contains(&aliases.confidence) {
            f.push(Finding::new(
                "mapping.alias-confidence-range",
                "spec.entity.identity.aliases.confidence",
                "alias confidence must be between 0 and 1",
            ));
        }
        if aliases.entries.is_empty() {
            f.push(Finding::new(
                "mapping.alias-no-forms",
                "spec.entity.identity.aliases.entries",
                "an alias table with no entries resolves nothing",
            ));
        }
        for (i, e) in aliases.entries.iter().enumerate() {
            if e.forms.is_empty() {
                f.push(Finding::new(
                    "mapping.alias-no-forms",
                    &format!("spec.entity.identity.aliases.entries[{i}]"),
                    format!("alias entry for '{}' declares no surface forms", e.subject),
                ));
            }
        }
        // One form under two subjects is a declared contradiction: the author
        // has written down that a string means two different people. That is
        // unresolvable by anything downstream, so it is refused here rather
        // than turned into an ambiguity finding on every row that carries it.
        let mut seen: std::collections::BTreeMap<String, &str> = Default::default();
        for e in &aliases.entries {
            for form in &e.forms {
                let key = normalize_alias_form(form);
                match seen.get(&key) {
                    Some(other) if *other != e.subject.as_str() => f.push(Finding::new(
                        "mapping.alias-form-collision",
                        "spec.entity.identity.aliases.entries",
                        format!(
                            "form '{form}' is declared for both '{other}' and '{}'; one                              surface form cannot name two subjects",
                            e.subject
                        ),
                    )),
                    _ => {
                        seen.insert(key, e.subject.as_str());
                    }
                }
            }
        }
    }

    for (i, a) in spec.authority.iter().enumerate() {
        if !spec.properties.contains_key(&a.property) {
            f.push(Finding::new(
                "mapping.authority-unknown-property",
                &format!("spec.authority[{i}]"),
                format!(
                    "authority scope names '{}', which is not a declared property",
                    a.property
                ),
            ));
        }
    }
    // Authority in shadow mode is inert; saying so is better than letting an
    // operator believe it is armed.
    if spec.mode == MappingMode::Shadow && !spec.authority.is_empty() {
        f.push(Finding::new(
            "mapping.authority-inert",
            "spec.authority",
            "authority is declared but the mapping is in shadow mode, so it is inert",
        ));
    }
    // A ceiling of zero is not a limit, it is a mapping that can never run —
    // which is what an author who meant "none" should say by omitting it.
    if let Some(l) = &doc.spec.limits {
        for (name, v) in [
            ("maxFindingsPerRun", l.max_findings_per_run),
            ("maxProposalsPerRun", l.max_proposals_per_run),
        ] {
            if v == Some(0) {
                f.push(Finding::new(
                    "mapping.limit-zero",
                    &format!("spec.limits.{name}"),
                    "a per-run ceiling of 0 means the mapping can never run; omit the limit or set it above zero",
                ));
            }
        }
    }
    f
}

impl ClaimMappingSpec {
    fn identity_min_confidence_out_of_range(&self) -> bool {
        let c = self.entity.identity.min_confidence;
        !(0.0..=1.0).contains(&c)
    }
}

// ---------------------------------------------------------------------------
// MetricView
// ---------------------------------------------------------------------------

/// The closed lists the semantic compiler checks an intent against, built
/// from the asset.
pub fn semantic_scope(spec: &MetricViewSpec) -> munarium_matrix_core::semantic::SemanticScope {
    use munarium_matrix_core::semantic::{DimensionDef, MeasureDef, SemanticScope};
    SemanticScope::metric_view(
        spec.view.clone(),
        spec.measures
            .iter()
            .map(|(n, m)| {
                (
                    n.clone(),
                    MeasureDef {
                        ty: m.ty,
                        scale: m.scale,
                        unit: m.unit.clone(),
                        additivity: m.additivity,
                    },
                )
            })
            .collect(),
        spec.dimensions
            .iter()
            .map(|(n, d)| (n.clone(), DimensionDef { ty: d.ty }))
            .collect(),
        spec.filters.allowed_dimensions.clone(),
        spec.max_dimensions,
    )
}

/// The closed lists for a native data view. Quoting and placeholders
/// are the caller's to set from the source's dialect
/// (`SemanticScope::try_with_dialect`); the validator compiles under Postgres
/// conventions, which changes nothing the lists decide.
pub fn data_view_scope(spec: &DataViewSpec) -> munarium_matrix_core::semantic::SemanticScope {
    use munarium_matrix_core::semantic::{
        DimensionDef, MeasureDef, NativeMeasure, NativeOp, SemanticBackend, SemanticScope,
    };
    let op_of = |a: NativeAggregate| match a {
        NativeAggregate::Sum => NativeOp::Sum,
        NativeAggregate::Count => NativeOp::Count,
        NativeAggregate::Min => NativeOp::Min,
        NativeAggregate::Max => NativeOp::Max,
        NativeAggregate::Avg => NativeOp::Avg,
    };
    let mut scope = SemanticScope::metric_view(
        spec.table.clone(),
        spec.measures
            .iter()
            .map(|(n, m)| {
                (
                    n.clone(),
                    MeasureDef {
                        ty: m.ty,
                        scale: m.scale,
                        unit: m.unit.clone(),
                        additivity: m.additivity,
                    },
                )
            })
            .collect(),
        spec.dimensions
            .iter()
            .map(|(n, d)| (n.clone(), DimensionDef { ty: d.ty }))
            .collect(),
        spec.filters.allowed_dimensions.clone(),
        spec.max_dimensions,
    )
    // The validator compiles under Postgres conventions, which changes nothing
    // the closed lists decide. `expect` is safe here and nowhere else: this is
    // a literal the compiler can see, not a dialect a source reported.
    .try_with_dialect("postgres")
    .expect("postgres is a known dialect");
    scope.backend = SemanticBackend::Native {
        measures: spec
            .measures
            .iter()
            .map(|(n, m)| {
                (
                    n.clone(),
                    NativeMeasure {
                        op: op_of(m.op),
                        column: m.column.clone(),
                    },
                )
            })
            .collect(),
        dimensions: spec
            .dimensions
            .iter()
            .map(|(n, d)| (n.clone(), d.column.clone().unwrap_or_else(|| n.clone())))
            .collect(),
    };
    scope
}

pub fn validate_data_view(doc: &DataViewDoc) -> Vec<Finding> {
    let mut f = check_envelope(&doc.api_version, &doc.kind, "DataView", &doc.metadata);
    let spec = &doc.spec;
    if spec.source.trim().is_empty() {
        f.push(Finding::new(
            "dataview.no-source",
            "spec.source",
            "a data view names the DataSource its table lives in",
        ));
    }
    if spec.table.trim().is_empty() || spec.table.split('.').any(|p| p.trim().is_empty()) {
        f.push(Finding::new(
            "dataview.no-table",
            "spec.table",
            "name the fact table: `schema.table`, or a bare name under the source's schema",
        ));
    }
    if spec.measures.is_empty() {
        f.push(Finding::new(
            "dataview.no-measures",
            "spec.measures",
            "declare at least one measure a caller may ask for",
        ));
    }
    for (name, m) in &spec.measures {
        if spec.dimensions.contains_key(name) {
            f.push(Finding::new(
                "dataview.name-collision",
                &format!("spec.dimensions.{name}"),
                format!("'{name}' is declared as both a measure and a dimension; a result column has one meaning"),
            ));
        }
        if m.column.is_none() && m.op != NativeAggregate::Count {
            f.push(Finding::new(
                "dataview.measure-needs-column",
                &format!("spec.measures.{name}.column"),
                format!(
                    "'{name}' is {:?} over nothing; only `count` may omit its column",
                    m.op
                ),
            ));
        }
    }
    for d in &spec.filters.allowed_dimensions {
        if !spec.dimensions.contains_key(d) {
            f.push(Finding::new(
                "dataview.filter-unknown-dimension",
                "spec.filters.allowedDimensions",
                format!("'{d}' is not a declared dimension, so it cannot be opened to filtering"),
            ));
        }
    }
    let scope = data_view_scope(spec);
    for (i, q) in spec.verified_questions.iter().enumerate() {
        let filters: Vec<munarium_matrix_core::semantic::FilterRef<'_>> = q
            .intent
            .filters
            .iter()
            .map(|x| munarium_matrix_core::semantic::FilterRef {
                dimension: &x.dimension,
                op: &x.op,
            })
            .collect();
        let req = munarium_matrix_core::semantic::SemanticRequest {
            measures: &q.intent.measures,
            dimensions: &q.intent.dimensions,
            filters,
        };
        if let Err(e) = munarium_matrix_core::semantic::compile(&scope, &req) {
            f.push(Finding::new(
                "dataview.question-not-compilable",
                &format!("spec.verifiedQuestions[{i}].intent"),
                e.message.clone(),
            ));
        }
    }
    f
}

pub fn validate_metric_view(doc: &MetricViewDoc) -> Vec<Finding> {
    let mut f = check_envelope(&doc.api_version, &doc.kind, "MetricView", &doc.metadata);
    let spec = &doc.spec;

    if spec.source.trim().is_empty() {
        f.push(Finding::new(
            "metricview.no-source",
            "spec.source",
            "a metric view names the DataSource that serves it",
        ));
    }
    // `schema.name` at least: a bare name would resolve in whatever the
    // session's current schema happened to be, which is not an identity.
    let parts: Vec<&str> = spec.view.split('.').collect();
    if spec.view.trim().is_empty() || parts.len() < 2 || parts.iter().any(|p| p.trim().is_empty()) {
        f.push(Finding::new(
            "metricview.no-view",
            "spec.view",
            "name the view by catalog identity: `catalog.schema.name`, or `schema.name` under the source's catalog",
        ));
    }
    if spec.measures.is_empty() {
        f.push(Finding::new(
            "metricview.no-measures",
            "spec.measures",
            "declare at least one measure a caller may ask for",
        ));
    }
    for name in spec.measures.keys() {
        if spec.dimensions.contains_key(name) {
            f.push(Finding::new(
                "metricview.name-collision",
                &format!("spec.dimensions.{name}"),
                format!("'{name}' is declared as both a measure and a dimension; a result column has one meaning"),
            ));
        }
    }
    for d in &spec.filters.allowed_dimensions {
        if !spec.dimensions.contains_key(d) {
            f.push(Finding::new(
                "metricview.filter-unknown-dimension",
                "spec.filters.allowedDimensions",
                format!("'{d}' is not a declared dimension, so it cannot be opened to filtering"),
            ));
        }
    }
    // Every verified question must compile against the asset's own lists —
    // the same discipline as a contract's inline statement. A question that
    // names an undeclared measure is refused here, not at the first verify.
    let scope = semantic_scope(spec);
    for (i, q) in spec.verified_questions.iter().enumerate() {
        let filters: Vec<munarium_matrix_core::semantic::FilterRef<'_>> = q
            .intent
            .filters
            .iter()
            .map(|x| munarium_matrix_core::semantic::FilterRef {
                dimension: &x.dimension,
                op: &x.op,
            })
            .collect();
        let req = munarium_matrix_core::semantic::SemanticRequest {
            measures: &q.intent.measures,
            dimensions: &q.intent.dimensions,
            filters,
        };
        if let Err(e) = munarium_matrix_core::semantic::compile(&scope, &req) {
            f.push(Finding::new(
                "metricview.question-not-compilable",
                &format!("spec.verifiedQuestions[{i}].intent"),
                e.message.clone(),
            ));
        }
    }
    f
}

/// Findings of severity "error" — everything except the advisory codes.
pub fn is_error(f: &Finding) -> bool {
    !matches!(
        f.code.as_str(),
        "limits.above-inline-seal" | "mapping.authority-inert" | "authorization.classes-ignored"
    )
}

/// True when nothing blocking was found.
pub fn is_valid(findings: &[Finding]) -> bool {
    !findings.iter().any(is_error)
}

#[cfg(test)]
mod secret_heuristic_tests {
    use super::*;

    #[test]
    fn a_long_hostname_is_not_a_secret() {
        assert!(!looks_like_a_literal_secret(
            "psql-sample-a1b2c3d4.postgres.database.azure.com"
        ));
        assert!(!looks_like_a_literal_secret(
            "adb-1234567890123456.11.azuredatabricks.net"
        ));
    }

    #[test]
    fn a_long_opaque_token_still_is() {
        assert!(looks_like_a_literal_secret(
            "9f8c1c1b2e3d4a5b6c7d8e9f0a1b2c3d4e5f60718293a4b5"
        ));
        // Dotted like a host, but a JWT.
        assert!(looks_like_a_literal_secret(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"
        ));
        // A dotted base64url-ish token with `_` in a label is not a hostname.
        assert!(looks_like_a_literal_secret(
            "abcdefghijklmnopqrstu_vwxyz.abcdefghijklmnopqrstuvwxyz0123456789"
        ));
    }
}
