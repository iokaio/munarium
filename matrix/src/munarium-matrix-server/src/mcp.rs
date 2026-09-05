// SPDX-License-Identifier: Apache-2.0
//! The MCP toolset.
//!
//! MCP is the transport an enterprise agent expects, and this module makes
//! Matrix speak it — **without giving an agent one capability it did not
//! already have**. That is the whole design, and it is worth stating plainly
//! because the obvious thing to build here is the wrong one.
//!
//! **What this is not.** There is no `run_sql` tool, no `query` tool that
//! takes a string, and no tool whose arguments become part of a statement.
//! The agent-facing pattern the market settled on — Google's MCP Toolbox for
//! Databases, and every warehouse vendor's server since — is *pre-declared
//! parameterized tools*, and Matrix already has the pre-declaration: a
//! `QueryContract`'s parameters, a `MetricView`'s or `DataView`'s closed
//! lists of measures and dimensions. This module turns those declarations
//! into MCP tool schemas. An agent cannot ask for a measure the asset does
//! not declare, because the schema does not have one.
//!
//! **A transport, not an authority.** `tools/call` builds the same
//! `QueryIntent` a REST or gRPC caller would send and hands it to the same
//! [`crate::execute::execute_intent`]: the same bearer token, the same
//! tenant, the same authorization class, the same budget spent, the same
//! evidence sealed, the same journal row — with `via: "mcp"` so an operator
//! can see which plane a query came from. There is no privileged in-process
//! path, and there is no second policy to keep in step.
//!
//! **The answer is evidence, not prose.** A tool result carries the block's
//! `evidence_id` and its rows, so an agent that quotes a number can cite
//! `[evidence/<id>#<row>]` and a reader can resolve it. A refusal comes back
//! as an MCP tool error with its typed code, not as an empty result: an agent
//! that cannot tell "no rows" from "not allowed" will report the wrong thing.
//!
//! Served on the REST port of a role that serves the query plane, beside
//! `/v1`; a control or sync container answers 404 exactly as it does for
//! `/v1/contracts/{name}/execute`.

use crate::rest::auth;
use crate::state::{AppState, Caller};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use munarium_matrix_types::assets::{DataViewDoc, MetricViewDoc, QueryContractSpec};
use munarium_matrix_types::Asset;
use serde_json::{json, Value};
use std::sync::Arc;

/// The MCP revision this server implements. Sent back from `initialize`; a
/// client that wants a newer one gets this and decides for itself.
pub const PROTOCOL_VERSION: &str = "2025-03-26";

/// JSON-RPC 2.0 error codes MCP uses.
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// A tool name is `<kind>.<asset name>`; the dot keeps the two apart without
/// inventing an escaping rule, since an asset name is already lowercase with
/// no dots (`metadata.name-shape` enforces it).
pub fn tool_name(kind: &str, asset: &str) -> String {
    format!("{kind}.{asset}")
}

/// A `QueryContract`'s declared parameters, as a JSON Schema an agent can
/// fill in. Types come from the asset; an `allowedValues` list becomes an
/// `enum`, so a bounded parameter is bounded in the schema too and a
/// well-behaved client will not even offer the wrong value.
pub fn contract_input_schema(spec: &QueryContractSpec) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, p) in &spec.parameters {
        let mut prop = json!({
            "type": json_type_of(p.ty),
            "description": format!(
                "{} parameter, bound by the contract — never interpolated into its statement",
                p.ty
            ),
        });
        if let Some(allowed) = &p.allowed_values {
            prop["enum"] = json!(allowed);
        }
        properties.insert(name.clone(), prop);
        if p.required {
            required.push(name.clone());
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        // The agent may not add arguments. A parameter the contract does not
        // declare is refused by the binder anyway; saying so in the schema
        // turns a refusal into something the client can avoid.
        "additionalProperties": false,
    })
}

/// A semantic view's closed lists, as a JSON Schema. `measures` and
/// `dimensions` are arrays whose items are `enum`s of exactly what the asset
/// declares — the bound is in the schema, not only in the validator.
pub fn semantic_input_schema(
    measures: Vec<String>,
    dimensions: Vec<String>,
    filterable: Vec<String>,
    max_dimensions: usize,
) -> Value {
    let filter_names = if filterable.is_empty() {
        dimensions.clone()
    } else {
        filterable
    };
    let mut dims = json!({
        "type": "array",
        "items": { "type": "string", "enum": dimensions },
        "description": "Group by these; the result is keyed by them.",
    });
    if max_dimensions > 0 {
        dims["maxItems"] = json!(max_dimensions);
    }
    json!({
        "type": "object",
        "properties": {
            "measures": {
                "type": "array",
                "minItems": 1,
                "items": { "type": "string", "enum": measures },
                "description": "The measures to compute. Only these exist.",
            },
            "dimensions": dims,
            "filters": {
                "type": "array",
                "description": "Equality filters. The dimension must be one the view opens to filtering.",
                "items": {
                    "type": "object",
                    "properties": {
                        "dimension": { "type": "string", "enum": filter_names },
                        "value": { "type": "string" },
                    },
                    "required": ["dimension", "value"],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["measures"],
        "additionalProperties": false,
    })
}

fn json_type_of(ty: munarium_matrix_core::value::ColumnType) -> &'static str {
    use munarium_matrix_core::value::ColumnType as C;
    match ty {
        C::Bool => "boolean",
        C::Int64 => "integer",
        C::Float64 => "number",
        // A decimal, a date and a timestamp cross as STRINGS, deliberately:
        // a decimal through a JSON number loses its scale, which is the one
        // thing this system will not allow.
        _ => "string",
    }
}

/// One MCP tool, built from an applied asset.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// Which execute route a call routes to.
    pub kind: ToolKind,
    pub asset: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Contract,
    MetricView,
    DataView,
}

impl Tool {
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
        })
    }
}

pub fn tool_for_contract(name: &str, doc: &munarium_matrix_types::QueryContractDoc) -> Tool {
    let questions: Vec<&str> = doc
        .spec
        .verified_questions
        .iter()
        .map(|q| q.question.as_str())
        .collect();
    let mut description = doc
        .spec
        .description
        .clone()
        .unwrap_or_else(|| format!("The verified query contract '{name}'."));
    if !questions.is_empty() {
        // The verified questions ARE the documentation: they are the ones a
        // human reviewed and that a regression suite keeps answering the
        // same way, so an agent choosing a tool should see them.
        description.push_str("\n\nVerified questions this contract answers: ");
        description.push_str(&questions.join("; "));
    }
    description.push_str(
        "\n\nThe statement is fixed and reviewed; only the declared parameters vary. \
         The result carries an evidence id — cite it as [evidence/<id>#<row>].",
    );
    Tool {
        name: tool_name("contract", name),
        description,
        input_schema: contract_input_schema(&doc.spec),
        kind: ToolKind::Contract,
        asset: name.to_string(),
    }
}

pub fn tool_for_metric_view(name: &str, doc: &MetricViewDoc) -> Tool {
    Tool {
        name: tool_name("view", name),
        description: format!(
            "{}\n\nMeasures and dimensions are the source's own, defined in its semantic \
             layer; this tool cannot compute anything outside the lists in its schema. \
             The result carries an evidence id — cite it as [evidence/<id>#<row>].",
            doc.spec
                .description
                .clone()
                .unwrap_or_else(|| format!("The metric view '{name}'."))
        ),
        input_schema: semantic_input_schema(
            doc.spec.measures.keys().cloned().collect(),
            doc.spec.dimensions.keys().cloned().collect(),
            doc.spec.filters.allowed_dimensions.clone(),
            doc.spec.max_dimensions,
        ),
        kind: ToolKind::MetricView,
        asset: name.to_string(),
    }
}

pub fn tool_for_data_view(name: &str, doc: &DataViewDoc) -> Tool {
    Tool {
        name: tool_name("view", name),
        description: format!(
            "{}\n\nAggregates over one fact table, declared in the view; this tool cannot \
             compute anything outside the lists in its schema. The result carries an \
             evidence id — cite it as [evidence/<id>#<row>].",
            doc.spec
                .description
                .clone()
                .unwrap_or_else(|| format!("The data view '{name}'."))
        ),
        input_schema: semantic_input_schema(
            doc.spec.measures.keys().cloned().collect(),
            doc.spec.dimensions.keys().cloned().collect(),
            doc.spec.filters.allowed_dimensions.clone(),
            doc.spec.max_dimensions,
        ),
        kind: ToolKind::DataView,
        asset: name.to_string(),
    }
}

/// Every tool this tenant's applied assets declare.
pub async fn tools_for(state: &Arc<AppState>, tenant: &str) -> Vec<Tool> {
    let mut out = Vec::new();
    for (kind, build) in [
        ("QueryContract", ToolKind::Contract),
        ("MetricView", ToolKind::MetricView),
        ("DataView", ToolKind::DataView),
    ] {
        let Ok(assets) = state.store.list_assets(tenant, Some(kind), true).await else {
            continue;
        };
        for stored in assets {
            let Ok(asset) = munarium_matrix_types::parse_asset(&stored.yaml) else {
                continue;
            };
            match (build, asset) {
                (ToolKind::Contract, Asset::QueryContract(d)) => {
                    out.push(tool_for_contract(&stored.name, &d))
                }
                (ToolKind::MetricView, Asset::MetricView(d)) => {
                    out.push(tool_for_metric_view(&stored.name, &d))
                }
                (ToolKind::DataView, Asset::DataView(d)) => {
                    out.push(tool_for_data_view(&stored.name, &d))
                }
                _ => {}
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Build the intent a tool call means. Nothing here is free-form: a contract
/// call fills declared parameters, a view call fills declared names.
pub fn intent_for(
    tool: &Tool,
    arguments: &Value,
    caller: &Caller,
    contract_types: &std::collections::BTreeMap<String, munarium_matrix_core::value::ColumnType>,
) -> Result<Value, String> {
    // The authorization snapshot names the caller's tenant; the ACCESS LEVEL
    // and compartments are the source's own class, resolved by the execute
    // path from the applied DataSource. An MCP caller cannot raise them by
    // asking, because it does not supply them.
    let authorization = json!({
        "tenant": caller.tenant,
        "access_level": 0,
        "compartments": [],
    });
    let limits = json!({ "max_rows": 500, "max_bytes": 1_048_576 });
    match tool.kind {
        ToolKind::Contract => {
            let mut parameters = serde_json::Map::new();
            if let Some(map) = arguments.as_object() {
                for (name, value) in map {
                    let ty = contract_types.get(name).ok_or_else(|| {
                        format!("'{name}' is not a parameter this contract declares")
                    })?;
                    parameters.insert(
                        name.clone(),
                        json!({ "type": ty.to_string(), "value": value }),
                    );
                }
            }
            Ok(json!({
                "contract_version": munarium_matrix_core::CONTRACT_VERSION,
                "kind": "structured_query",
                "contract": tool.asset,
                "parameters": parameters,
                "authorization": authorization,
                "limits": limits,
            }))
        }
        ToolKind::MetricView | ToolKind::DataView => {
            let measures = arguments["measures"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if measures.is_empty() {
                return Err("a semantic call names at least one measure".into());
            }
            let filters: Vec<Value> = arguments["filters"]
                .as_array()
                .map(|fs| {
                    fs.iter()
                        .filter_map(|f| {
                            Some(json!({
                                "dimension": f.get("dimension")?,
                                "op": "eq",
                                "value": { "type": "string", "value": f.get("value")? },
                            }))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(json!({
                "contract_version": munarium_matrix_core::CONTRACT_VERSION,
                "kind": "semantic",
                "semantic": {
                    "provider": tool.asset,
                    "measures": measures,
                    "dimensions": arguments["dimensions"].as_array().cloned().unwrap_or_default(),
                    "filters": filters,
                },
                "authorization": authorization,
                "limits": limits,
            }))
        }
    }
}

/// Render an evidence block for an agent: the citable id, the columns, and
/// the rows as text. Not a prose summary — the agent writes that, and it can
/// only do so honestly if it can see what it is quoting.
pub fn block_to_content(block: &munarium_matrix_types::contract::EvidenceBlock) -> Value {
    use munarium_matrix_types::contract::EvidenceBlock as B;
    let text = match block {
        B::CompleteTable {
            evidence_id,
            manifest,
            rows,
            truncated,
            derivations,
            ..
        } => {
            let mut out = format!(
                "evidence_id: {evidence_id}\ncompleteness: {}\nrows: {}\n",
                if *truncated { "TRUNCATED" } else { "COMPLETE" },
                rows.len()
            );
            out.push_str(&format!(
                "columns: {}\n",
                manifest
                    .schema
                    .columns
                    .iter()
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            for r in rows {
                // A NULL cell renders as an empty field rather than being omitted:
                // an agent counting columns must see the same shape every row.
                let cells: Vec<&str> = r.cells.iter().map(|c| c.as_deref().unwrap_or("")).collect();
                out.push_str(&format!(
                    "[{}] {}
",
                    r.row_id,
                    cells.join(" | ")
                ));
            }
            for d in derivations {
                out.push_str(&format!(
                    "derivation {} = {}\n",
                    d.reference,
                    d.value.clone().unwrap_or_default()
                ));
            }
            if *truncated {
                out.push_str(
                    "NOTE: this result is TRUNCATED. Do not say \"all\" or \"there are N\" \
                     on the strength of it.\n",
                );
            }
            out
        }
        other => format!("{other:?}"),
    };
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn rpc_error(id: Option<&Value>, code: i64, message: impl Into<String>) -> Json<Value> {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id.cloned().unwrap_or(Value::Null),
        "error": { "code": code, "message": message.into() },
    }))
}

fn rpc_result(id: Option<&Value>, result: Value) -> Json<Value> {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id.cloned().unwrap_or(Value::Null),
        "result": result,
    }))
}

/// `POST /mcp` — one JSON-RPC 2.0 request, one response.
///
/// Streamable HTTP without the streaming: every method here answers in one
/// round trip, and a transport that promised SSE it never uses would be a
/// larger surface for no gain.
pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Json<Value> {
    let id = req.get("id");
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

    // A notification (no id) gets no response body worth building; MCP's
    // `notifications/initialized` is the one that matters and it is a no-op.
    if method.starts_with("notifications/") {
        return Json(json!({ "jsonrpc": "2.0", "result": {} }));
    }

    let caller = match auth(&state, &headers) {
        Ok(c) => c,
        // Authentication failure is a JSON-RPC error rather than a bare 401:
        // an MCP client reads the envelope, and a status code with no
        // envelope reaches it as a transport fault it cannot explain.
        Err(_) => {
            return rpc_error(
                id,
                INVALID_REQUEST,
                "this MCP endpoint needs the same bearer token the REST plane takes",
            )
        }
    };

    match method {
        "initialize" => rpc_result(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "munarium-matrix",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": "Every tool here is a pre-declared, reviewed query. There is \
                                 no free-SQL tool and never will be. Each result carries an \
                                 evidence id; cite numbers as [evidence/<id>#<row>].",
            }),
        ),
        "ping" => rpc_result(id, json!({})),
        "tools/list" => {
            let tools = tools_for(&state, &caller.tenant).await;
            rpc_result(
                id,
                json!({ "tools": tools.iter().map(|t| t.to_json()).collect::<Vec<_>>() }),
            )
        }
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(json!({}));
            let Some(name) = params.get("name").and_then(|n| n.as_str()) else {
                return rpc_error(id, INVALID_PARAMS, "tools/call needs a tool name");
            };
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            let tools = tools_for(&state, &caller.tenant).await;
            let Some(tool) = tools.iter().find(|t| t.name == name) else {
                return rpc_error(
                    id,
                    INVALID_PARAMS,
                    format!("no tool named '{name}' is declared for this tenant"),
                );
            };

            // Parameter types come from the asset, so a value crosses as the
            // type the contract declared rather than as whatever JSON made of
            // it. This is the one place the tool's arguments are interpreted.
            let contract_types = match tool.kind {
                ToolKind::Contract => {
                    match crate::runtime::load_contract(&state, &caller.tenant, &tool.asset).await {
                        Ok(doc) => doc
                            .spec
                            .parameters
                            .iter()
                            .map(|(k, v)| (k.clone(), v.ty))
                            .collect(),
                        Err(r) => return rpc_error(id, INVALID_PARAMS, r.message),
                    }
                }
                _ => Default::default(),
            };
            let intent_json = match intent_for(tool, &arguments, &caller, &contract_types) {
                Ok(v) => v,
                Err(e) => return rpc_error(id, INVALID_PARAMS, e),
            };
            let intent: munarium_matrix_types::contract::QueryIntent =
                match serde_json::from_value(intent_json) {
                    Ok(i) => i,
                    Err(e) => return rpc_error(id, INVALID_PARAMS, e.to_string()),
                };

            match crate::execute::execute_intent(
                &state,
                &caller,
                &tool.asset,
                &intent,
                None,
                "mcp",
                |_| {},
            )
            .await
            {
                Ok(block) => rpc_result(id, block_to_content(&block)),
                // A refusal is a TOOL error, not a protocol error: the call
                // was well formed and the system declined it, and the agent
                // must be able to tell that from "no rows".
                Err(refusal) => rpc_result(
                    id,
                    json!({
                        "isError": true,
                        "content": [{
                            "type": "text",
                            "text": format!("{} [{}]: {}", refusal.class.as_str(), refusal.code, refusal.message),
                        }],
                    }),
                ),
            }
        }
        other => rpc_error(id, METHOD_NOT_FOUND, format!("unknown method '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_matrix_types::parse_asset;

    const CONTRACT: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: QueryContract
metadata: { name: open-pipeline-by-region, version: 3 }
spec:
  source: crm
  description: Open pipeline in USD by region as of a date.
  parameters:
    as_of: { type: date, required: true }
    region: { type: string, allowedValues: [EMEA, AMER] }
  statementByDialect:
    postgres: { inline: "SELECT region FROM opportunities WHERE updated_at <= :as_of" }
  reads: { tables: [opportunities], columns: [updated_at] }
  result:
    columns:
      region: { type: string, key: true }
    columnOrder: [region]
    orderBy: [region]
  verifiedQuestions:
    - question: "What is the open pipeline by region?"
      parameters: { as_of: "2026-06-30" }
      expect: { rows: 1 }
"#;

    fn contract() -> munarium_matrix_types::QueryContractDoc {
        match parse_asset(CONTRACT).unwrap() {
            Asset::QueryContract(d) => *d,
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_contracts_tool_schema_is_its_declared_parameters_and_nothing_else() {
        let t = tool_for_contract("open-pipeline-by-region", &contract());
        assert_eq!(t.name, "contract.open-pipeline-by-region");
        let schema = &t.input_schema;
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["as_of"]["type"], "string");
        assert_eq!(schema["required"][0], "as_of");
        // A bounded parameter is bounded in the schema, so a client will not
        // even offer a value the contract would refuse.
        assert_eq!(schema["properties"]["region"]["enum"][0], "EMEA");
        // The verified questions are the documentation an agent chooses by.
        assert!(
            t.description
                .contains("What is the open pipeline by region?"),
            "{}",
            t.description
        );
        assert!(t.description.contains("[evidence/<id>#<row>]"));
    }

    #[test]
    fn a_semantic_tools_lists_are_enums_so_an_undeclared_measure_is_unaskable() {
        let schema = semantic_input_schema(
            vec!["pipeline_amount".into(), "opportunity_count".into()],
            vec!["region".into(), "stage".into()],
            vec!["region".into()],
            2,
        );
        assert_eq!(
            schema["properties"]["measures"]["items"]["enum"][0],
            "pipeline_amount"
        );
        assert_eq!(schema["properties"]["dimensions"]["maxItems"], 2);
        // Only the filterable dimension may be filtered.
        let filter_enum =
            &schema["properties"]["filters"]["items"]["properties"]["dimension"]["enum"];
        assert_eq!(filter_enum.as_array().unwrap().len(), 1);
        assert_eq!(filter_enum[0], "region");
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn a_decimal_parameter_crosses_as_a_string_not_a_json_number() {
        use munarium_matrix_core::value::ColumnType;
        // The one thing this system will not allow is a scale lost in
        // transit, and a JSON number is exactly how that happens.
        assert_eq!(json_type_of(ColumnType::Decimal), "string");
        assert_eq!(json_type_of(ColumnType::Date), "string");
        assert_eq!(json_type_of(ColumnType::Int64), "integer");
    }

    #[test]
    fn a_tool_call_becomes_the_same_intent_a_rest_caller_would_send() {
        let caller = Caller {
            tenant: "demo".into(),
            role: "rw".into(),
            disabled_mode: false,
        };
        let t = tool_for_contract("open-pipeline-by-region", &contract());
        let types = contract()
            .spec
            .parameters
            .iter()
            .map(|(k, v)| (k.clone(), v.ty))
            .collect();
        let intent = intent_for(&t, &json!({ "as_of": "2026-06-30" }), &caller, &types).unwrap();
        assert_eq!(intent["kind"], "structured_query");
        assert_eq!(intent["contract"], "open-pipeline-by-region");
        assert_eq!(intent["parameters"]["as_of"]["type"], "date");
        assert_eq!(intent["parameters"]["as_of"]["value"], "2026-06-30");
        assert_eq!(intent["authorization"]["tenant"], "demo");
        assert_eq!(intent["authorization"]["access_level"], 0);
        // No SQL anywhere in what an agent can influence.
        assert!(!intent.to_string().to_lowercase().contains("select"));
    }

    #[test]
    fn an_undeclared_argument_is_refused_before_an_intent_exists() {
        let caller = Caller {
            tenant: "demo".into(),
            role: "rw".into(),
            disabled_mode: false,
        };
        let t = tool_for_contract("open-pipeline-by-region", &contract());
        let types = contract()
            .spec
            .parameters
            .iter()
            .map(|(k, v)| (k.clone(), v.ty))
            .collect();
        let err = intent_for(&t, &json!({ "sneaky": "1=1" }), &caller, &types).unwrap_err();
        assert!(err.contains("sneaky"), "{err}");
    }
}
