// SPDX-License-Identifier: Apache-2.0
//! The compiled MCP tool table and its dispatcher (MCP-RULE-3, §5–§7).
//!
//! **Closed table.** Eleven tools, each compiled to one fixed handler: a
//! native route (method + template, path values percent-encoded with the
//! native bridge's helper) or one of three dedicated bounded reads. The
//! caller supplies no route, method or query. A unit test enumerates every
//! form against a separately maintained allowlist.
//!
//! **Authority (A1).** Entry needs `mcp` (checked in `mcp::entry`). Each tool
//! also needs its operation scope — `query`, or the read-only evidence grant
//! `evidence_read` (A4) — on listing AND on a direct call; the native handler
//! then applies its own tenant, clearance, compartment, resource and runbook
//! checks with the caller's own bearer. The dedicated reads apply the same
//! rules themselves, filtering before ordering, counting or paging.
//!
//! **Durable admission (A6).** After the arguments are validated and before
//! anything reads application data, one transaction commits the admission
//! record (and, for the keyed tools, claims or finds the operation key);
//! nothing runs until its commit is acknowledged.
//!
//! **Recovery (§5.3).** The five keyed tools take `operation_key` with their
//! arguments (execution) or alone (recovery). Recovery reads a bounded receipt
//! and never starts the operation.

use crate::error::CustomError;
use crate::mcp::{Caller, McpService, Revision, RpcFailure, INVALID_PARAMS};
use crate::mcp_store::{self, Admitted, Claim, OperationKey, OperationRow, Outcome};
use crate::state::AppState;
use axum::body::Body;
use munarium_core::KernelError;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use tower::ServiceExt;

/// The `_meta` key distinguishing native results, projections, receipts and
/// problems in BOTH representations' envelope (§5.2).
pub const RESULT_KIND: &str = "io.munarium/result";
pub const PROBLEM_SLUG: &str = "io.munarium/problem";

/// How a tool executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handler {
    /// A native route, dispatched in process with the caller's bearer.
    Routed {
        method: &'static str,
        template: &'static str,
    },
    /// Dedicated filtered read: authorized active collections.
    ListCollections,
    /// Dedicated filtered read: runbooks the caller may open.
    ListRunbooks,
    /// Dedicated bounded projection of one session's turns.
    SessionProjection,
}

#[derive(Debug, Clone, Copy)]
pub struct ToolSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// The operation scope required beside `mcp`.
    pub scope: &'static str,
    /// Takes an operation key (§5.3).
    pub keyed: bool,
    /// Can make generative model calls (A2): its execution form is listed
    /// only with `MUNARIUM_MCP_PAID_TOOLS=enabled`.
    pub paid: bool,
    pub read_only: bool,
    pub destructive: bool,
    pub handler: Handler,
}

const Q: &str = munarium_access::SCOPE_QUERY;
const E: &str = munarium_access::SCOPE_EVIDENCE_READ;

/// The v1.5 catalog, in its fixed listing order.
pub const CATALOG: &[ToolSpec] = &[
    ToolSpec {
        name: "list_collections",
        title: "List collections",
        description: "List the active collections this credential may search, with their ids and active index. Paged; pass next_cursor back as cursor.",
        scope: Q,
        keyed: false,
        paid: false,
        read_only: true,
        destructive: false,
        handler: Handler::ListCollections,
    },
    ToolSpec {
        name: "list_runbooks",
        title: "List runbooks",
        description: "List the runbooks this credential may open a session on, with the collections it may see through each. Paged; pass next_cursor back as cursor.",
        scope: Q,
        keyed: false,
        paid: false,
        read_only: true,
        destructive: false,
        handler: Handler::ListRunbooks,
    },
    ToolSpec {
        name: "search_collection",
        title: "Search a collection",
        description: "Search one collection with its vocabulary and retrieval policy (POST /v1.2/search). Needs an operation_key; send the key alone to recover an interrupted call.",
        scope: Q,
        keyed: true,
        paid: false,
        read_only: true,
        destructive: false,
        handler: Handler::Routed {
            method: "POST",
            template: "/v1.2/search",
        },
    },
    ToolSpec {
        name: "ask_collections",
        title: "Ask collections",
        description: "Answer a question from the selected collections with checked citations (POST /v1.2/query). May call a model under the operator's token allowances. Needs an operation_key; send the key alone to recover an interrupted call.",
        scope: Q,
        keyed: true,
        paid: true,
        read_only: false,
        destructive: false,
        handler: Handler::Routed {
            method: "POST",
            template: "/v1.2/query",
        },
    },
    ToolSpec {
        name: "open_session",
        title: "Open a session",
        description: "Open a session on a runbook (POST /v1/runbooks/{name}/sessions). Needs an operation_key; send the key alone to recover an interrupted call.",
        scope: Q,
        keyed: true,
        paid: false,
        read_only: false,
        destructive: false,
        handler: Handler::Routed {
            method: "POST",
            template: "/v1/runbooks/{name}/sessions",
        },
    },
    ToolSpec {
        name: "ask_session",
        title: "Ask in a session",
        description: "Append a turn to an open session (POST /v1/sessions/{id}/turns) with the strict multi-collection availability policy. May call a model under the operator's token allowances. Needs an operation_key; send the key alone to recover an interrupted call.",
        scope: Q,
        keyed: true,
        paid: true,
        read_only: false,
        destructive: false,
        handler: Handler::Routed {
            method: "POST",
            template: "/v1/sessions/{id}/turns",
        },
    },
    ToolSpec {
        name: "get_session",
        title: "Read a session",
        description: "Read one turn (ordinal) or a short page of turns (after_ordinal, limit) of your session, with native field values.",
        scope: Q,
        keyed: false,
        paid: false,
        read_only: true,
        destructive: false,
        handler: Handler::SessionProjection,
    },
    ToolSpec {
        name: "close_session",
        title: "Close a session",
        description: "Close your session (POST /v1/sessions/{id}/close); closing a closed session reports its state. Needs an operation_key; send the key alone to recover an interrupted call.",
        scope: Q,
        keyed: true,
        paid: false,
        read_only: false,
        destructive: true,
        handler: Handler::Routed {
            method: "POST",
            template: "/v1/sessions/{id}/close",
        },
    },
    ToolSpec {
        name: "authorize_publication",
        title: "Authorize a publication",
        description: "Resolve the authorized original reference of a governed publication (GET /v1.2/collections/{id}/publications/{publication_id}). Grants no download.",
        scope: Q,
        keyed: false,
        paid: false,
        read_only: true,
        destructive: false,
        handler: Handler::Routed {
            method: "GET",
            template: "/v1.2/collections/{id}/publications/{publication_id}",
        },
    },
    ToolSpec {
        name: "get_evidence",
        title: "Read an evidence manifest",
        description: "Resolve a sealed evidence manifest you are cleared for (GET /v1/evidence/{evidence_id}). The read is audited.",
        scope: E,
        keyed: false,
        paid: false,
        read_only: true,
        destructive: false,
        handler: Handler::Routed {
            method: "GET",
            template: "/v1/evidence/{evidence_id}",
        },
    },
    ToolSpec {
        name: "read_evidence_rows",
        title: "Read evidence rows",
        description: "Read a bounded window of a sealed artifact's canonical CSV rows (GET /v1/evidence/{evidence_id}/rows). The read is audited.",
        scope: E,
        keyed: false,
        paid: false,
        read_only: true,
        destructive: false,
        handler: Handler::Routed {
            method: "GET",
            template: "/v1/evidence/{evidence_id}/rows",
        },
    },
];

/// The native routes the table may dispatch to, maintained SEPARATELY from
/// `CATALOG` so that adding a routed tool needs two deliberate edits (§6).
pub const ROUTE_ALLOWLIST: &[(&str, &str)] = &[
    ("POST", "/v1.2/search"),
    ("POST", "/v1.2/query"),
    ("POST", "/v1/runbooks/{name}/sessions"),
    ("POST", "/v1/sessions/{id}/turns"),
    ("POST", "/v1/sessions/{id}/close"),
    (
        "GET",
        "/v1.2/collections/{id}/publications/{publication_id}",
    ),
    ("GET", "/v1/evidence/{evidence_id}"),
    ("GET", "/v1/evidence/{evidence_id}/rows"),
];

pub fn find(name: &str) -> Option<&'static ToolSpec> {
    CATALOG.iter().find(|t| t.name == name)
}

// ---------------------------------------------------------------------------
// forms and schemas
// ---------------------------------------------------------------------------

/// Which argument forms of a tool this caller is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// Read tools, and keyed tools with execution enabled: every form.
    Full,
    /// Keyed tools whose execution is disabled here: the key alone.
    RecoveryOnly,
}

fn offer(state: &AppState, spec: &ToolSpec) -> Offer {
    if !spec.keyed {
        return Offer::Full;
    }
    let s = &state.mcp.settings;
    if s.execution == crate::mcp::Execution::RecoveryOnly || (spec.paid && !s.paid_tools) {
        Offer::RecoveryOnly
    } else {
        Offer::Full
    }
}

fn key_schema() -> Value {
    json!({
        "type": "object",
        "description": "Chosen and retained by the client BEFORE the call. Reuse it, alone, to recover an interrupted call; never extend it or mint a new one automatically.",
        "properties": {
            "id": {"type": "string", "minLength": OperationKey::MIN_ID, "maxLength": OperationKey::MAX_ID, "pattern": "^[A-Za-z0-9_-]+$"},
            "expires_at": {"type": "string", "format": "date-time", "description": "Immutable RFC 3339 UTC expiry ending in Z, within the server's validity window."}
        },
        "required": ["id", "expires_at"],
        "additionalProperties": false
    })
}

fn string(max: usize) -> Value {
    json!({"type": "string", "minLength": 1, "maxLength": max})
}

fn integer(min: u64, max: u64) -> Value {
    json!({"type": "integer", "minimum": min, "maximum": max})
}

/// (property, schema, required) for a tool's execution arguments.
fn execution_fields(name: &str) -> Vec<(&'static str, Value, bool)> {
    match name {
        "list_collections" | "list_runbooks" => vec![
            ("cursor", string(512), false),
            ("limit", integer(1, MAX_PAGE as u64), false),
        ],
        "search_collection" => vec![
            ("query", string(8000), true),
            ("collection", string(256), true),
            ("top_k", integer(1, 50), false),
            ("index_version", string(256), false),
        ],
        "ask_collections" => vec![
            ("question", string(8000), true),
            (
                "collections",
                json!({"type": "array", "minItems": 1, "maxItems": MAX_COLLECTIONS, "items": string(256)}),
                true,
            ),
            (
                "effective_on",
                json!({"type": "string", "format": "date"}),
                false,
            ),
        ],
        "open_session" => vec![("runbook", string(256), true)],
        "ask_session" => vec![
            ("session_id", string(256), true),
            ("query", string(8000), true),
            ("top_k", integer(1, 50), false),
            ("complete", json!({"type": "boolean"}), false),
            ("research_profile", string(256), false),
        ],
        "get_session" => vec![
            ("session_id", string(256), true),
            ("ordinal", integer(0, u32::MAX as u64), false),
            ("after_ordinal", integer(0, u32::MAX as u64), false),
            ("limit", integer(1, MAX_TURNS as u64), false),
        ],
        "close_session" => vec![("session_id", string(256), true)],
        "authorize_publication" => vec![
            ("collection", string(256), true),
            ("publication_id", string(256), true),
            (
                "effective_on",
                json!({"type": "string", "format": "date"}),
                false,
            ),
        ],
        "get_evidence" => vec![("evidence_id", string(256), true)],
        "read_evidence_rows" => vec![
            ("evidence_id", string(256), true),
            ("from", integer(0, u32::MAX as u64), false),
            ("limit", integer(1, MAX_ROWS as u64), false),
        ],
        _ => Vec::new(),
    }
}

pub const MAX_PAGE: usize = 100;
pub const DEFAULT_PAGE: usize = 25;
pub const MAX_TURNS: usize = 10;
pub const MAX_ROWS: usize = 200;
pub const MAX_COLLECTIONS: usize = 16;

fn input_schema(spec: &ToolSpec, offer: Offer) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    if spec.keyed {
        properties.insert("operation_key".into(), key_schema());
        required.push(json!("operation_key"));
    }
    if offer == Offer::Full {
        for (name, schema, req) in execution_fields(spec.name) {
            properties.insert(name.into(), schema);
            if req && !spec.keyed {
                required.push(json!(name));
            }
        }
    }
    let mut schema = json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    });
    if spec.keyed && offer == Offer::Full {
        let exec: Vec<&str> = execution_fields(spec.name)
            .into_iter()
            .filter(|(_, _, r)| *r)
            .map(|(n, _, _)| n)
            .collect();
        schema["description"] = json!(format!(
            "Execution: operation_key with {}. Recovery: operation_key alone.",
            exec.join(", ")
        ));
    } else if spec.keyed {
        schema["description"] = json!(
            "Recovery only on this deployment: operation_key alone reads the call's receipt."
        );
    }
    schema
}

/// `tools/list`: every tool the caller's scopes open, in catalog order, with
/// the forms offered here. Independent of tenant data (MCP-RULE-7).
pub fn list(state: &AppState, caller: &Caller) -> Value {
    let tools: Vec<Value> = CATALOG
        .iter()
        .filter(|t| caller.access.has_scope(t.scope))
        .map(|t| {
            json!({
                "name": t.name,
                "title": t.title,
                "description": t.description,
                "inputSchema": input_schema(t, offer(state, t)),
                "annotations": {
                    "title": t.title,
                    "readOnlyHint": t.read_only,
                    "destructiveHint": t.destructive,
                    "idempotentHint": t.keyed || t.read_only,
                    "openWorldHint": false,
                },
            })
        })
        .collect();
    json!({"tools": tools})
}

// ---------------------------------------------------------------------------
// argument validation
// ---------------------------------------------------------------------------

type Args = Map<String, Value>;

fn bad(msg: impl Into<String>) -> CustomError {
    CustomError {
        slug: "invalid-input",
        status: axum::http::StatusCode::BAD_REQUEST,
        code: tonic::Code::InvalidArgument,
        title: "invalid input",
        detail: msg.into(),
    }
}

fn get_str(a: &Args, k: &str, max: usize, required: bool) -> Result<Option<String>, CustomError> {
    match a.get(k) {
        None if required => Err(bad(format!("{k} is required"))),
        None => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() && s.len() <= max => Ok(Some(s.clone())),
        Some(_) => Err(bad(format!(
            "{k} must be a non-empty string of at most {max} bytes"
        ))),
    }
}

fn get_u64(a: &Args, k: &str, min: u64, max: u64) -> Result<Option<u64>, CustomError> {
    match a.get(k) {
        None => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if n >= min && n <= max => Ok(Some(n)),
            _ => Err(bad(format!("{k} must be an integer from {min} to {max}"))),
        },
    }
}

fn get_date(a: &Args, k: &str) -> Result<Option<String>, CustomError> {
    match get_str(a, k, 10, false)? {
        None => Ok(None),
        Some(d) => chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d")
            .map(|_| Some(d))
            .map_err(|_| bad(format!("{k} must be a YYYY-MM-DD date"))),
    }
}

/// A validated call: what to dispatch and what it touches.
#[derive(Debug, Clone)]
pub struct Plan {
    path: Vec<(&'static str, String)>,
    query: Vec<(&'static str, String)>,
    body: Option<Value>,
    /// What the receipt's authority recheck needs (§5.3); never a body.
    pub resources: Value,
    /// Canonical execution arguments (without the key), for the digest.
    canonical: Value,
}

fn plan(spec: &ToolSpec, a: &Args) -> Result<Plan, CustomError> {
    let allowed: Vec<&str> = execution_fields(spec.name)
        .into_iter()
        .map(|(n, _, _)| n)
        .chain(spec.keyed.then_some("operation_key"))
        .collect();
    if let Some(k) = a.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(bad(format!("{} does not take '{k}'", spec.name)));
    }
    let mut canonical = a.clone();
    canonical.remove("operation_key");
    let mut p = Plan {
        path: Vec::new(),
        query: Vec::new(),
        body: None,
        resources: json!({}),
        canonical: canonicalize(&Value::Object(canonical)),
    };
    match spec.name {
        "list_collections" | "list_runbooks" => {
            get_str(a, "cursor", 512, false)?;
            get_u64(a, "limit", 1, MAX_PAGE as u64)?;
        }
        "search_collection" => {
            let query = get_str(a, "query", 8000, true)?;
            let collection = get_str(a, "collection", 256, true)?;
            let mut body = json!({"query": query, "collection": collection});
            if let Some(k) = get_u64(a, "top_k", 1, 50)? {
                body["top_k"] = json!(k);
            }
            if let Some(iv) = get_str(a, "index_version", 256, false)? {
                body["index_version"] = json!(iv);
            }
            p.resources = json!({"collections": [collection]});
            p.body = Some(body);
        }
        "ask_collections" => {
            let question = get_str(a, "question", 8000, true)?;
            let list = a
                .get("collections")
                .and_then(Value::as_array)
                .filter(|l| !l.is_empty() && l.len() <= MAX_COLLECTIONS)
                .ok_or_else(|| {
                    bad(format!(
                        "collections must list 1 to {MAX_COLLECTIONS} collections"
                    ))
                })?;
            let mut names = Vec::new();
            for c in list {
                match c.as_str() {
                    Some(s) if !s.trim().is_empty() && s.len() <= 256 => names.push(s.to_string()),
                    _ => return Err(bad("each collection is a non-empty string")),
                }
            }
            let mut body = json!({"question": question, "collections": names});
            if let Some(d) = get_date(a, "effective_on")? {
                body["effective_on"] = json!(d);
            }
            p.resources = json!({"collections": names});
            p.body = Some(body);
        }
        "open_session" => {
            let runbook = get_str(a, "runbook", 256, true)?.unwrap_or_default();
            p.resources = json!({"runbook": runbook.split('@').next().unwrap_or_default()});
            p.path.push(("name", runbook));
        }
        "ask_session" => {
            let id = get_str(a, "session_id", 256, true)?.unwrap_or_default();
            let query = get_str(a, "query", 8000, true)?;
            let mut body = json!({"query": query, "collection_availability": "require"});
            if let Some(k) = get_u64(a, "top_k", 1, 50)? {
                body["top_k"] = json!(k);
            }
            match a.get("complete") {
                None => {}
                Some(Value::Bool(b)) => body["complete"] = json!(b),
                Some(_) => return Err(bad("complete must be a boolean")),
            }
            if let Some(rp) = get_str(a, "research_profile", 256, false)? {
                body["research_profile"] = json!(rp);
            }
            p.resources = json!({"session_id": id});
            p.path.push(("id", id));
            p.body = Some(body);
        }
        "get_session" => {
            get_str(a, "session_id", 256, true)?;
            let ordinal = get_u64(a, "ordinal", 0, u32::MAX as u64)?;
            let after = get_u64(a, "after_ordinal", 0, u32::MAX as u64)?;
            let limit = get_u64(a, "limit", 1, MAX_TURNS as u64)?;
            if ordinal.is_some() && (after.is_some() || limit.is_some()) {
                return Err(bad("pass ordinal alone, or after_ordinal and limit"));
            }
        }
        "close_session" => {
            let id = get_str(a, "session_id", 256, true)?.unwrap_or_default();
            p.resources = json!({"session_id": id});
            p.path.push(("id", id));
        }
        "authorize_publication" => {
            let collection = get_str(a, "collection", 256, true)?.unwrap_or_default();
            let publication = get_str(a, "publication_id", 256, true)?.unwrap_or_default();
            if let Some(d) = get_date(a, "effective_on")? {
                p.query.push(("effective_on", d));
            }
            p.path.push(("id", collection));
            p.path.push(("publication_id", publication));
        }
        "get_evidence" => {
            let id = get_str(a, "evidence_id", 256, true)?.unwrap_or_default();
            p.path.push(("evidence_id", id));
        }
        "read_evidence_rows" => {
            let id = get_str(a, "evidence_id", 256, true)?.unwrap_or_default();
            p.path.push(("evidence_id", id));
            if let Some(f) = get_u64(a, "from", 0, u32::MAX as u64)? {
                p.query.push(("from", f.to_string()));
            }
            // The MCP window is bounded below the native 1000-row cap so a
            // page fits the response bound more often.
            let limit = get_u64(a, "limit", 1, MAX_ROWS as u64)?.unwrap_or(100);
            p.query.push(("limit", limit.to_string()));
        }
        _ => return Err(bad("unknown tool")),
    }
    Ok(p)
}

/// Keys sorted at every level, independent of serde_json's map feature.
fn canonicalize(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                out.insert(k.clone(), canonicalize(&m[k]));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

fn digest(tool: &str, canonical: &Value) -> String {
    use sha2::Digest as _;
    // serde_json writes an object's keys in map order; `canonicalize` sorted
    // them, and the sorted order is preserved under either map feature.
    let mut text = String::from(tool);
    text.push('\n');
    write_canonical(canonical, &mut text);
    hex::encode(sha2::Sha256::digest(text.as_bytes()))
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(&m[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Which form a call uses.
#[derive(Debug)]
pub enum Form {
    Read,
    Execute(OperationKey),
    Recover(OperationKey),
}

fn classify(spec: &ToolSpec, a: &Args) -> Result<Form, CustomError> {
    if !spec.keyed {
        if a.contains_key("operation_key") {
            return Err(bad(format!("{} does not take an operation_key", spec.name)));
        }
        return Ok(Form::Read);
    }
    let key = a.get("operation_key").ok_or_else(|| {
        bad(format!(
            "{} needs an operation_key chosen before the call (see the tool's input schema)",
            spec.name
        ))
    })?;
    let key = OperationKey::parse(key).map_err(bad)?;
    if a.len() == 1 {
        Ok(Form::Recover(key))
    } else {
        Ok(Form::Execute(key))
    }
}

// ---------------------------------------------------------------------------
// results
// ---------------------------------------------------------------------------

fn text_block(text: String) -> Value {
    json!([{"type": "text", "text": text}])
}

/// A native result: the native JSON unchanged in `structuredContent`, and the
/// native body's own bytes as the text block (exact parity, §5.2).
fn native_result(raw: &[u8], parsed: Value) -> Value {
    json!({
        "content": text_block(String::from_utf8_lossy(raw).into_owned()),
        "structuredContent": parsed,
        "isError": false,
        "_meta": {RESULT_KIND: "native"},
    })
}

fn structured_result(kind: &str, v: Value) -> Value {
    json!({
        "content": text_block(v.to_string()),
        "structuredContent": v,
        "isError": false,
        "_meta": {RESULT_KIND: kind},
    })
}

/// A tool error carrying a native public problem (type URI, status, R11
/// guidance) plus its short slug.
fn problem_result(problem: Value) -> Value {
    let slug = problem
        .get("type")
        .and_then(Value::as_str)
        .and_then(|t| t.rsplit('/').next())
        .unwrap_or("error")
        .to_string();
    json!({
        "content": text_block(problem.to_string()),
        "structuredContent": problem,
        "isError": true,
        "_meta": {RESULT_KIND: "problem", PROBLEM_SLUG: slug},
    })
}

fn custom_problem(e: CustomError) -> Value {
    let (_, p) = e.to_problem();
    serde_json::to_value(p).unwrap_or_else(|_| json!({"type": "error"}))
}

fn kernel_problem(e: &KernelError) -> Value {
    let (_, p) = crate::error::to_problem(e);
    serde_json::to_value(p).unwrap_or_else(|_| json!({"type": "error"}))
}

fn api_problem(e: crate::error::ApiError) -> Value {
    match e {
        crate::error::ApiError::Mesh(k) => kernel_problem(&k),
        crate::error::ApiError::Custom(c) => custom_problem(c),
    }
}

fn status_of(problem: &Value) -> Option<i32> {
    problem
        .get("status")
        .and_then(Value::as_i64)
        .map(|s| s as i32)
}

// ---------------------------------------------------------------------------
// the call
// ---------------------------------------------------------------------------

/// Everything recorded about one call, for the interaction row and metrics.
struct Record<'a> {
    state: &'a AppState,
    caller: &'a Caller,
    tool: &'static str,
    request_id: &'a str,
    started: std::time::Instant,
}

impl Record<'_> {
    /// One interaction row per ADMITTED call, plane `mcp`. Content-free on
    /// purpose: the tool name and outcome, never arguments or results.
    fn interaction(&self, invocation_id: &str, status: i32, session_id: Option<String>) {
        crate::interactions::record(
            self.state,
            crate::interactions::InteractionRecord {
                tenant_id: self.caller.access.tenant_id.clone(),
                uid: self.caller.access.uid.clone(),
                session_id,
                request_id: self.request_id.to_string(),
                plane: "mcp",
                method: format!("tools/call {}", self.tool),
                runbook_ref: None,
                collection_ids: None,
                token_jti: Some(self.caller.access.jti.clone()).filter(|j| !j.is_empty()),
                request: Some(json!({"tool": self.tool, "invocation_id": invocation_id})),
                response: Some(json!({"status": status})),
                status: Some(status),
                latency_ms: self.started.elapsed().as_millis().min(i32::MAX as u128) as i32,
            },
        );
    }

    fn metric(&self, outcome: &str) {
        self.state.metrics.inc(
            "munarium_mcp_tool_calls_total",
            crate::metrics::labels(&[("tool", self.tool), ("outcome", outcome)]),
        );
    }
}

fn ok(v: Value) -> Result<Value, RpcFailure> {
    Ok(v)
}

/// `tools/call`. Protocol errors (unknown tool) are `Err`; everything that
/// reaches a recognized tool is a tool result, `isError` when refused.
pub async fn call(
    service: &Arc<McpService>,
    caller: &Caller,
    revision: Revision,
    request_id: &str,
    params: &Value,
) -> Result<Value, RpcFailure> {
    let state = &service.state;
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err(RpcFailure {
            code: INVALID_PARAMS,
            message: "params.name is required".into(),
        });
    };
    let Some(spec) = find(name) else {
        return Err(RpcFailure {
            code: INVALID_PARAMS,
            message: format!(
                "Unknown tool: {}",
                name.chars().take(64).collect::<String>()
            ),
        });
    };
    let rec = Record {
        state,
        caller,
        tool: spec.name,
        request_id,
        started: std::time::Instant::now(),
    };
    let args: Args = match params.get("arguments") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => {
            rec.metric("invalid");
            return ok(problem_result(custom_problem(bad(
                "arguments must be an object",
            ))));
        }
    };
    // A1: the operation scope, on a direct call exactly as on the listing.
    if !caller.access.has_scope(spec.scope) {
        rec.metric("scope-missing");
        return ok(problem_result(custom_problem(CustomError::scope_missing(
            spec.scope,
        ))));
    }
    // Bounded arguments and the form, before any admission or work.
    let form = match classify(spec, &args) {
        Ok(f) => f,
        Err(e) => {
            rec.metric("invalid");
            return ok(problem_result(custom_problem(e)));
        }
    };
    let plan = match &form {
        Form::Recover(_) => None,
        _ => match plan(spec, &args) {
            Ok(p) => Some(p),
            Err(e) => {
                rec.metric("invalid");
                return ok(problem_result(custom_problem(e)));
            }
        },
    };
    if matches!(form, Form::Execute(_)) && offer(state, spec) == Offer::RecoveryOnly {
        rec.metric("form-unavailable");
        let why = if spec.paid && !state.mcp.settings.paid_tools {
            "this tool can call a model and MCP generation is disabled on this deployment \
             (MUNARIUM_MCP_PAID_TOOLS); only its recovery form (operation_key alone) is offered"
        } else {
            "new work is disabled on this deployment (MUNARIUM_MCP_EXECUTION=recovery-only); \
             only the recovery form (operation_key alone) is offered"
        };
        return ok(problem_result(custom_problem(
            CustomError::mcp_tool_unavailable(why),
        )));
    }
    // R7 heavy bound, classified by the native route exactly as native
    // traffic is; taken before admission so a shed admits nothing.
    let _heavy = match (&form, spec.handler) {
        (Form::Execute(_) | Form::Read, Handler::Routed { method, template }) => {
            match crate::admission::rest_class(method, template) {
                crate::admission::WorkClass::Heavy => match state.admission.rest.try_heavy() {
                    Some(h) => h,
                    None => {
                        rec.metric("shed");
                        state.metrics.inc(
                            "munarium_admission_shed_total",
                            crate::metrics::labels(&[("plane", "rest"), ("class", "heavy")]),
                        );
                        return ok(problem_result(custom_problem(CustomError::overloaded())));
                    }
                },
                _ => None,
            }
        }
        _ => None,
    };

    // A6: the durable admission, confirmed before anything else happens.
    let invocation_id = format!("mci-{}", uuid::Uuid::now_v7().simple());
    let admission = mcp_store::Admission {
        invocation_id: invocation_id.clone(),
        tenant_id: caller.access.tenant_id.clone(),
        uid: caller.access.uid.clone(),
        token_jti: caller.access.jti.clone(),
        request_id: request_id.to_string(),
        tool: spec.name,
        protocol_version: revision.as_str(),
        form: match form {
            Form::Read => "read",
            Form::Execute(_) => "execute",
            Form::Recover(_) => "recover",
        },
        instance_id: state.config.instance_id.clone(),
    };
    let s = &state.mcp.settings;
    let exec_digest = plan.as_ref().map(|p| digest(spec.name, &p.canonical));
    let keyed = match (&form, &plan) {
        (Form::Execute(k), Some(p)) => Some(mcp_store::KeyedRequest {
            key: k,
            execute: Some((exec_digest.as_deref().unwrap_or_default(), &p.resources)),
            max_validity_secs: s.key_max_validity_secs,
            deadline_secs: s.call_deadline_secs,
        }),
        (Form::Recover(k), _) => Some(mcp_store::KeyedRequest {
            key: k,
            execute: None,
            max_validity_secs: s.key_max_validity_secs,
            deadline_secs: s.call_deadline_secs,
        }),
        _ => None,
    };
    let admitted = match mcp_store::admit(state, &admission, keyed).await {
        Ok(a) => a,
        Err(failure) => {
            let detail = match &failure {
                mcp_store::AdmissionFailure::Store(d) => d.as_str(),
                _ => "",
            };
            tracing::warn!(
                tool = spec.name,
                reason = failure.label(),
                detail,
                "MCP admission not confirmed; the tool did not run"
            );
            state.metrics.inc(
                "munarium_mcp_audit_refusals_total",
                crate::metrics::labels(&[("reason", failure.label())]),
            );
            rec.metric("audit-refused");
            if matches!(
                failure,
                mcp_store::AdmissionFailure::Deadline | mcp_store::AdmissionFailure::LostAck
            ) && matches!(form, Form::Execute(_))
            {
                // The claim may have committed. Say on the receipt, if the
                // store comes back, that this invocation never ran.
                let state = service.state.clone();
                let id = invocation_id.clone();
                tokio::spawn(async move { mcp_store::mark_withheld(&state, &id).await });
            }
            let mut problem = custom_problem(CustomError::mcp_audit_unavailable());
            problem["executed"] = json!(false);
            return ok(problem_result(problem));
        }
    };
    state.metrics.inc(
        "munarium_mcp_admissions_total",
        crate::metrics::labels(&[("tool", spec.name), ("form", admission.form)]),
    );

    let execution_key = match &form {
        Form::Execute(k) => Some(k),
        _ => None,
    };
    let result = match admitted {
        Admitted::Read => {
            let plan = plan.unwrap_or_else(empty_plan);
            execute(
                service,
                caller,
                spec,
                &invocation_id,
                None,
                &plan,
                &args,
                &rec,
            )
            .await
        }
        Admitted::Execute(Claim::Claimed) => {
            let plan = plan.unwrap_or_else(empty_plan);
            execute(
                service,
                caller,
                spec,
                &invocation_id,
                execution_key,
                &plan,
                &args,
                &rec,
            )
            .await
        }
        Admitted::Execute(Claim::Invalid(why)) => {
            mcp_store::record_outcome(
                state,
                &invocation_id,
                false,
                &Outcome {
                    outcome: "refused",
                    http_status: Some(400),
                    ..Default::default()
                },
            )
            .await;
            rec.metric("key-invalid");
            rec.interaction(&invocation_id, 400, None);
            problem_result(custom_problem(CustomError::mcp_operation_key_invalid(why)))
        }
        Admitted::Execute(Claim::Existing(row)) => {
            // A duplicate: never a second execution. Recheck authority before
            // revealing existence, a conflict or an outcome.
            let digest_now = exec_digest.clone().unwrap_or_default();
            let visible = recheck(state, caller, &row).await;
            let (outcome, value) = if !visible {
                ("receipt", receipt_absent(spec, &row.key_id))
            } else if row.args_digest != digest_now {
                (
                    "conflict",
                    problem_result(custom_problem(CustomError::mcp_operation_conflict())),
                )
            } else {
                ("receipt", receipt(state, caller, &row).await)
            };
            finish_lookup(state, &invocation_id, outcome, &rec).await;
            value
        }
        Admitted::Recover(row) => {
            let key_id = match &form {
                Form::Recover(k) => k.id.clone(),
                _ => String::new(),
            };
            let value = match row {
                Some(row) if recheck(state, caller, &row).await => {
                    receipt(state, caller, &row).await
                }
                _ => receipt_absent(spec, &key_id),
            };
            finish_lookup(state, &invocation_id, "receipt", &rec).await;
            value
        }
    };
    Ok(bound_result(state, spec, result))
}

fn empty_plan() -> Plan {
    Plan {
        path: Vec::new(),
        query: Vec::new(),
        body: None,
        resources: json!({}),
        canonical: json!({}),
    }
}

async fn finish_lookup(
    state: &AppState,
    invocation_id: &str,
    outcome: &'static str,
    rec: &Record<'_>,
) {
    mcp_store::record_outcome(
        state,
        invocation_id,
        false,
        &Outcome {
            outcome,
            http_status: Some(200),
            ..Default::default()
        },
    )
    .await;
    rec.metric(outcome);
    rec.interaction(invocation_id, 200, None);
}

/// Headroom for the JSON-RPC wrapper and the revision's `_meta`.
const ENVELOPE: usize = 1024;

fn serialized_len(v: &Value) -> usize {
    serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX)
}

/// Enforce the response bound on a finished tool result (the whole
/// serialized envelope, with headroom for the JSON-RPC wrapper and `_meta`).
fn bound_result(state: &AppState, spec: &ToolSpec, result: Value) -> Value {
    let size = serialized_len(&result);
    if size.saturating_add(ENVELOPE) <= state.mcp.settings.result_max_bytes {
        return result;
    }
    too_large(state, spec, size)
}

/// The typed refusal for a result over the bound: it names a smaller limit or
/// cursor for a read, and the recovery read for an operation that ran.
fn too_large(state: &AppState, spec: &ToolSpec, size: usize) -> Value {
    let max = state.mcp.settings.result_max_bytes;
    let hint = match spec.name {
        "list_collections" | "list_runbooks" => "request a smaller limit and follow next_cursor",
        "get_session" => "read one turn by ordinal, or a smaller limit",
        "read_evidence_rows" => "request a smaller limit and advance from",
        _ if spec.keyed => {
            "the operation ran; recover its receipt with the same operation_key (and read a \
             session turn with get_session by ordinal)"
        }
        _ => "the result cannot be returned over MCP",
    };
    let size = if size == usize::MAX {
        "over the bound".to_string()
    } else {
        format!("{size} bytes")
    };
    problem_result(custom_problem(CustomError::mcp_result_too_large(format!(
        "the {} result is {size}, over the {max}-byte MCP response bound \
         (MUNARIUM_MCP_RESULT_MAX_BYTES); {hint}",
        spec.name
    ))))
}

/// Run an admitted read or a claimed execution.
#[allow(clippy::too_many_arguments)]
async fn execute(
    service: &Arc<McpService>,
    caller: &Caller,
    spec: &'static ToolSpec,
    invocation_id: &str,
    key: Option<&OperationKey>,
    plan: &Plan,
    args: &Args,
    rec: &Record<'_>,
) -> Value {
    let state = &service.state;
    let keyed = key.is_some();
    // Session authority (§5.1 Sessions): current tenant, uid, runbook and the
    // live token covering the session's saved clearance — read only now,
    // after the admission committed.
    if let Some(session_id) = match spec.name {
        "ask_session" | "close_session" | "get_session" => {
            args.get("session_id").and_then(Value::as_str)
        }
        _ => None,
    } {
        if let Err(problem) = session_authority(state, &caller.access, session_id).await {
            let status = status_of(&problem);
            mcp_store::record_outcome(
                state,
                invocation_id,
                keyed,
                &Outcome {
                    outcome: "refused",
                    http_status: status,
                    problem_type: problem
                        .get("type")
                        .and_then(Value::as_str)
                        .map(String::from),
                    ..Default::default()
                },
            )
            .await;
            rec.metric("refused");
            rec.interaction(invocation_id, status.unwrap_or(403), None);
            return problem_result(problem);
        }
    }
    match mcp_store::begin_dispatch(state, invocation_id, key).await {
        Ok(true) => {}
        Ok(false) => {
            mcp_store::record_outcome(
                state,
                invocation_id,
                keyed,
                &Outcome {
                    outcome: "refused",
                    http_status: Some(400),
                    ..Default::default()
                },
            )
            .await;
            rec.metric("key-invalid");
            rec.interaction(invocation_id, 400, None);
            return problem_result(custom_problem(CustomError::mcp_operation_key_invalid(
                "the operation key expired before dispatch; nothing ran",
            )));
        }
        Err(e) => {
            rec.metric("audit-refused");
            tracing::warn!(error = %e, "MCP dispatch stamp failed; the tool did not run");
            let mut problem = custom_problem(CustomError::mcp_audit_unavailable());
            problem["executed"] = json!(false);
            mcp_store::record_outcome(
                state,
                invocation_id,
                keyed,
                &Outcome {
                    outcome: "withheld",
                    http_status: Some(503),
                    ..Default::default()
                },
            )
            .await;
            return problem_result(problem);
        }
    }
    state.metrics.inc(
        "munarium_mcp_dispatches_total",
        crate::metrics::labels(&[("tool", spec.name)]),
    );
    let deadline = std::time::Duration::from_secs(state.mcp.settings.call_deadline_secs);
    let outcome = match spec.handler {
        Handler::Routed { method, template } => {
            let ctx = Arc::new(crate::mcp_generation::CallContext {
                tenant: caller.access.tenant_id.clone(),
                invocation_id: invocation_id.to_string(),
                paid_enabled: spec.paid && state.mcp.settings.paid_tools,
                policy: if spec.paid && state.mcp.settings.paid_tools {
                    contributors(state, &caller.access, spec, args)
                        .await
                        .and_then(|c| {
                            crate::mcp_generation::resolve(
                                &state.mcp.settings.generation,
                                &caller.access.tenant_id,
                                &c,
                            )
                        })
                } else {
                    Err("this tool does not generate".into())
                },
                admitted: Default::default(),
            });
            let req = match build_request(caller, invocation_id, method, template, plan) {
                Ok(r) => r,
                Err(e) => {
                    let problem = custom_problem(bad(e));
                    mcp_store::record_outcome(
                        state,
                        invocation_id,
                        keyed,
                        &Outcome {
                            outcome: "refused",
                            http_status: Some(400),
                            ..Default::default()
                        },
                    )
                    .await;
                    return problem_result(problem);
                }
            };
            let dispatch = crate::mcp_generation::scope(ctx, service.inner.clone().oneshot(req));
            match tokio::time::timeout(deadline, dispatch).await {
                Err(_) => Dispatched::Unknown,
                Ok(Err(_)) => Dispatched::Unknown,
                Ok(Ok(response)) => {
                    let status = response.status().as_u16() as i32;
                    let limit = state.mcp.settings.result_max_bytes;
                    match axum::body::to_bytes(response.into_body(), limit).await {
                        Ok(bytes) => Dispatched::Http(status, bytes.to_vec()),
                        Err(_) => Dispatched::TooLarge(status),
                    }
                }
            }
        }
        Handler::ListCollections => dedicated(list_collections(state, &caller.access, args).await),
        Handler::ListRunbooks => dedicated(list_runbooks(state, &caller.access, args).await),
        Handler::SessionProjection => {
            dedicated(session_projection(state, &caller.access, args).await)
        }
    };
    finish_execution(state, spec, invocation_id, keyed, args, outcome, rec).await
}

enum Dispatched {
    Http(i32, Vec<u8>),
    Projection(Value),
    Problem(Value),
    TooLarge(i32),
    Unknown,
}

fn dedicated(r: Result<Value, crate::error::ApiError>) -> Dispatched {
    match r {
        Ok(v) => Dispatched::Projection(v),
        Err(e) => Dispatched::Problem(api_problem(e)),
    }
}

async fn finish_execution(
    state: &AppState,
    spec: &'static ToolSpec,
    invocation_id: &str,
    keyed: bool,
    args: &Args,
    outcome: Dispatched,
    rec: &Record<'_>,
) -> Value {
    let arg_session = args
        .get("session_id")
        .and_then(Value::as_str)
        .map(String::from);
    let (record, value) = match outcome {
        Dispatched::Unknown => {
            let mut problem = kernel_problem(&KernelError::Storage(
                "the tool did not finish within its deadline".into(),
            ));
            problem["detail"] = json!(
                "the tool did not report an outcome within MUNARIUM_MCP_CALL_DEADLINE_SECS; \
                 its effects are unknown. Recover the receipt with the same operation_key; \
                 never repeat the call with a new key automatically"
            );
            (
                Outcome {
                    outcome: "unknown",
                    session_id: arg_session,
                    ..Default::default()
                },
                problem_result(problem),
            )
        }
        Dispatched::TooLarge(status) => (
            Outcome {
                outcome: if (200..300).contains(&status) {
                    "completed"
                } else {
                    "refused"
                },
                http_status: Some(status),
                session_id: arg_session,
                result_too_large: true,
                ..Default::default()
            },
            Value::Null,
        ),
        Dispatched::Projection(v) => (
            Outcome {
                outcome: "completed",
                http_status: Some(200),
                session_id: arg_session,
                ..Default::default()
            },
            structured_result("projection", v),
        ),
        Dispatched::Problem(p) => (
            Outcome {
                outcome: "refused",
                http_status: status_of(&p),
                problem_type: p.get("type").and_then(Value::as_str).map(String::from),
                session_id: arg_session,
                ..Default::default()
            },
            problem_result(p),
        ),
        Dispatched::Http(status, bytes) => {
            let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            if (200..300).contains(&status) {
                let session_id = parsed
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .or(arg_session);
                let turn_ordinal = if spec.name == "ask_session" {
                    parsed
                        .get("ordinal")
                        .and_then(Value::as_i64)
                        .map(|o| o as i32)
                } else {
                    None
                };
                (
                    Outcome {
                        outcome: "completed",
                        http_status: Some(status),
                        session_id,
                        turn_ordinal,
                        result_bytes: Some(bytes.len() as i64),
                        ..Default::default()
                    },
                    native_result(&bytes, parsed),
                )
            } else {
                let problem = if parsed.is_object() {
                    parsed
                } else {
                    kernel_problem(&KernelError::Storage(format!(
                        "the native handler answered {status} without a problem body"
                    )))
                };
                (
                    Outcome {
                        outcome: "refused",
                        http_status: Some(status),
                        problem_type: problem
                            .get("type")
                            .and_then(Value::as_str)
                            .map(String::from),
                        session_id: arg_session,
                        ..Default::default()
                    },
                    problem_result(problem),
                )
            }
        }
    };
    // Bound check BEFORE recording, so a too-large completion is recorded as
    // such and its receipt can say so.
    let size = if value.is_null() {
        usize::MAX
    } else {
        serialized_len(&value)
    };
    let oversized = size.saturating_add(ENVELOPE) > state.mcp.settings.result_max_bytes;
    let mut record = record;
    if oversized {
        record.result_too_large = true;
    }
    mcp_store::record_outcome(state, invocation_id, keyed, &record).await;
    rec.metric(record.outcome);
    rec.interaction(
        invocation_id,
        record
            .http_status
            .unwrap_or(if record.outcome == "unknown" {
                504
            } else {
                200
            }),
        record.session_id.clone(),
    );
    if oversized {
        return too_large(state, spec, size);
    }
    value
}

fn build_request(
    caller: &Caller,
    invocation_id: &str,
    method: &'static str,
    template: &'static str,
    plan: &Plan,
) -> Result<axum::http::Request<Body>, String> {
    // Defense in depth for the closed table (§6): the compiled handler must
    // be on the separately maintained allowlist.
    if !ROUTE_ALLOWLIST.contains(&(method, template)) {
        return Err("internal: route is not on the MCP allowlist".into());
    }
    let mut uri = template.to_string();
    for (name, value) in &plan.path {
        let placeholder = format!("{{{name}}}");
        if !uri.contains(&placeholder) {
            return Err("internal: path parameter does not belong to this route".into());
        }
        uri = uri.replace(&placeholder, &crate::grpc_api::component(value));
    }
    if uri.contains('{') {
        return Err("internal: unfilled path parameter".into());
    }
    if !plan.query.is_empty() {
        let q: Vec<String> = plan
            .query
            .iter()
            .map(|(k, v)| format!("{k}={}", crate::grpc_api::component(v)))
            .collect();
        uri.push('?');
        uri.push_str(&q.join("&"));
    }
    let body = match &plan.body {
        Some(b) => Body::from(b.to_string()),
        None => Body::empty(),
    };
    axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {}", caller.bearer))
        .header("x-munarium-uid", &caller.access.uid)
        .header("content-type", "application/json")
        .extension(crate::mcp::McpInner {
            invocation_id: invocation_id.to_string(),
        })
        .body(body)
        .map_err(|_| "internal: request build failed".to_string())
}

// ---------------------------------------------------------------------------
// authority helpers
// ---------------------------------------------------------------------------

struct SessionRow {
    uid: String,
    runbook_ref: String,
    access_level: i32,
    compartments: Vec<String>,
}

async fn load_session(state: &AppState, tenant: &str, id: &str) -> Option<SessionRow> {
    let pool = state.pg_pool()?;
    let row: Option<(String, String, i32, Vec<String>)> = sqlx::query_as(
        "SELECT uid, runbook_ref, access_level, compartments FROM sessions
          WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant)
    .bind(id)
    .fetch_optional(pool)
    .await
    .ok()?;
    row.map(
        |(uid, runbook_ref, access_level, compartments)| SessionRow {
            uid,
            runbook_ref,
            access_level,
            compartments,
        },
    )
}

/// The live token must still own and cover the session (MCP is stricter
/// than native here by design: a narrower same-uid token cannot reach a
/// session opened under wider authority). Refusals never say which check
/// failed or whether the session exists.
async fn session_authority(
    state: &AppState,
    access: &munarium_access::AccessCtx,
    session_id: &str,
) -> Result<(), Value> {
    let hidden = || {
        custom_problem(CustomError {
            slug: "forbidden",
            status: axum::http::StatusCode::FORBIDDEN,
            code: tonic::Code::PermissionDenied,
            title: "forbidden",
            detail: "the session is not available to this credential".into(),
        })
    };
    let Some(s) = load_session(state, &access.tenant_id, session_id).await else {
        return Err(hidden());
    };
    let runbook = s.runbook_ref.split('@').next().unwrap_or_default();
    if s.uid != access.uid
        || !access.permits_runbook(runbook)
        || !access.permits(s.access_level, &s.compartments)
    {
        return Err(hidden());
    }
    if let Some(pool) = state.pg_pool() {
        if let Err(e) =
            munarium_store_pg::source_retention::assert_session(pool, &access.tenant_id, session_id)
                .await
        {
            return Err(kernel_problem(&e));
        }
    }
    Ok(())
}

/// Resolve a collection reference (id first, then name) the caller may use.
async fn permitted_collection(
    retrieval: &munarium_retrieval::Retrieval,
    access: &munarium_access::AccessCtx,
    reference: &str,
) -> Option<munarium_core::retrieval::CollectionInfo> {
    let info = match retrieval.collection_by_id(reference).await {
        Ok(c) => c,
        Err(_) => retrieval.collection_by_name(reference).await.ok()?,
    };
    (info.status == "active" && access.permits(info.access_level, &info.compartments))
        .then_some(info)
}

/// The contributors of a generating call (A9), identified before dispatch:
/// the authorized collections named by `ask_collections`, or the authorized
/// collections of the session's pinned runbook for `ask_session`, each with
/// its active index.
async fn contributors(
    state: &AppState,
    access: &munarium_access::AccessCtx,
    spec: &ToolSpec,
    args: &Args,
) -> Result<Vec<crate::mcp_generation::Contributor>, String> {
    let retrieval = state
        .retrieval_for(&access.tenant_id)
        .map_err(|_| "retrieval is unavailable".to_string())?;
    let mut infos = Vec::new();
    match spec.name {
        "ask_collections" => {
            for c in args
                .get("collections")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if let Some(info) = permitted_collection(&retrieval, access, c).await {
                    infos.push(info);
                }
            }
        }
        "ask_session" => {
            let id = args
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let s = load_session(state, &access.tenant_id, id)
                .await
                .ok_or("the session is not available")?;
            let (doc, _) = crate::runbooks_api::load_runbook_with_status(
                state,
                &access.tenant_id,
                &s.runbook_ref,
                true,
            )
            .await
            .map_err(|_| "the session's runbook is not available".to_string())?;
            let session_view = munarium_access::AccessCtx {
                level: s.access_level,
                compartments: s.compartments.clone(),
                ..access.clone()
            };
            for spec in doc.spec.effective_collections() {
                if let Some(info) =
                    permitted_collection(&retrieval, &session_view, &spec.name).await
                {
                    infos.push(info);
                }
            }
        }
        _ => {}
    }
    let mut out = Vec::new();
    for info in infos {
        let index_version = retrieval
            .active_collection_index(&info.id)
            .await
            .ok()
            .flatten();
        out.push(crate::mcp_generation::Contributor {
            collection_id: info.id,
            collection_name: info.name,
            index_version,
        });
    }
    Ok(out)
}

/// Before revealing a receipt's existence, a conflict or an outcome: the
/// current credential must still hold the original resource authority, and
/// retention must still allow it (§5.3).
async fn recheck(state: &AppState, caller: &Caller, row: &OperationRow) -> bool {
    let access = &caller.access;
    let res = &row.resources;
    if let Some(session) = res.get("session_id").and_then(Value::as_str) {
        if session_authority(state, access, session).await.is_err() {
            return false;
        }
    }
    if let Some(runbook) = res.get("runbook").and_then(Value::as_str) {
        if !access.permits_runbook(runbook) {
            return false;
        }
    }
    if let Some(list) = res.get("collections").and_then(Value::as_array) {
        let Ok(retrieval) = state.retrieval_for(&access.tenant_id) else {
            return false;
        };
        for c in list.iter().filter_map(Value::as_str) {
            if permitted_collection(&retrieval, access, c).await.is_none() {
                return false;
            }
        }
    }
    if let Some(session) = &row.session_id {
        if res.get("session_id").is_none()
            && session_authority(state, access, session).await.is_err()
        {
            return false;
        }
    }
    true
}

/// The bounded receipt (§5.3). No request body, transcript, answer or
/// evidence: identifiers, status and spend only.
async fn receipt(state: &AppState, caller: &Caller, row: &OperationRow) -> Value {
    let status = match row.state.as_str() {
        "pending" if row.past_deadline => "unknown",
        "pending" => "pending",
        "completed" => "completed",
        "refused" => "refused",
        "withheld" => "not_executed",
        _ => "unknown",
    };
    let mut r = json!({
        "kind": "operation_receipt",
        "tool": row.tool,
        "operation_key": row.key_id,
        "expires_at": row.expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "status": status,
        "invocation_id": row.invocation_id,
        "outcome": {
            "http_status": row.http_status,
            "problem_type": row.problem_type,
            "session_id": row.session_id,
            "turn_ordinal": row.turn_ordinal,
            "result_bytes": row.result_bytes,
            "result_too_large": row.result_too_large,
            "content": "not_retained",
        },
    });
    match row.tool.as_str() {
        "ask_session" | "ask_collections" => {
            r["spend"] = mcp_store::spend(state, &caller.access.tenant_id, &row.invocation_id)
                .await
                .unwrap_or_else(|_| json!({"status": "unavailable"}));
        }
        "search_collection" => {
            r["inference"] = json!({
                "generation": mcp_store::spend(state, &caller.access.tenant_id, &row.invocation_id)
                    .await
                    .unwrap_or_else(|_| json!({"status": "unavailable"})),
                "enrolled_inference": "not_correlated",
            });
        }
        _ => {}
    }
    structured_result("receipt", r)
}

/// Absent and inaccessible receipts look the same (§5.3).
fn receipt_absent(spec: &ToolSpec, key_id: &str) -> Value {
    structured_result(
        "receipt",
        json!({
            "kind": "operation_receipt",
            "tool": spec.name,
            "operation_key": key_id,
            "status": "not_found",
        }),
    )
}

// ---------------------------------------------------------------------------
// dedicated reads
// ---------------------------------------------------------------------------

/// A cursor bound to the caller's authority and the listing's ordering. A
/// forged or foreign cursor only positions a list that is already filtered
/// by the live token, so it can disclose nothing; the binding makes misuse
/// an explicit error rather than a silently different page.
fn fingerprint(access: &munarium_access::AccessCtx, listing: &str) -> String {
    use sha2::Digest as _;
    let mut cmp = access.compartments.clone();
    cmp.sort();
    let text = format!(
        "{listing}\n{}\n{}\n{}\n{}\n{:?}",
        access.tenant_id,
        access.uid,
        access.level,
        cmp.join(","),
        access.runbooks
    );
    hex::encode(&sha2::Sha256::digest(text.as_bytes())[..8])
}

fn encode_cursor(fp: &str, after: &(String, String)) -> String {
    use base64::Engine as _;
    let v = json!([fp, after.0, after.1]).to_string();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v)
}

fn decode_cursor(
    fp: &str,
    cursor: Option<&str>,
) -> Result<Option<(String, String)>, crate::error::ApiError> {
    use base64::Engine as _;
    let Some(c) = cursor else { return Ok(None) };
    let invalid = || {
        crate::error::ApiError::Custom(bad(
            "the cursor is not valid for this listing and credential",
        ))
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c)
        .map_err(|_| invalid())?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    match v.as_array().map(|a| a.as_slice()) {
        Some([Value::String(f), Value::String(a), Value::String(b)]) if f == fp => {
            Ok(Some((a.clone(), b.clone())))
        }
        _ => Err(invalid()),
    }
}

fn page<T>(
    mut items: Vec<((String, String), T)>,
    after: Option<(String, String)>,
    limit: usize,
) -> (Vec<T>, Option<(String, String)>, usize) {
    items.sort_by(|a, b| a.0.cmp(&b.0));
    let total = items.len();
    let start = match &after {
        Some(a) => items.iter().position(|(k, _)| k > a).unwrap_or(items.len()),
        None => 0,
    };
    let rest: Vec<((String, String), T)> = items.into_iter().skip(start).collect();
    let more = rest.len() > limit;
    let mut out = Vec::new();
    let mut last = None;
    for (k, v) in rest.into_iter().take(limit) {
        last = Some(k);
        out.push(v);
    }
    (out, if more { last } else { None }, total)
}

/// `list_collections`: active collections the caller's clearance and
/// compartments permit, authorized BEFORE ordering, counting and paging.
async fn list_collections(
    state: &AppState,
    access: &munarium_access::AccessCtx,
    args: &Args,
) -> Result<Value, crate::error::ApiError> {
    let fp = fingerprint(access, "collections");
    let after = decode_cursor(&fp, args.get("cursor").and_then(Value::as_str))?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|l| l as usize)
        .unwrap_or(DEFAULT_PAGE);
    let retrieval = state.retrieval_for(&access.tenant_id)?;
    let visible: Vec<((String, String), munarium_core::retrieval::CollectionInfo)> = retrieval
        .list_collections()
        .await?
        .into_iter()
        .filter(|c| c.status == "active" && access.permits(c.access_level, &c.compartments))
        .map(|c| ((c.name.clone(), c.id.clone()), c))
        .collect();
    let (items, next, total) = page(visible, after, limit);
    let mut out = Vec::new();
    for c in items {
        let active_index = retrieval.active_collection_index(&c.id).await?;
        out.push(json!({
            "id": c.id,
            "name": c.name,
            "description": c.description,
            "shape_ref": c.shape_ref,
            "active_index": active_index,
        }));
    }
    Ok(json!({
        "collections": out,
        "total": total,
        "next_cursor": next.map(|n| encode_cursor(&fp, &n)),
    }))
}

/// `list_runbooks`: the latest active version of each runbook the token's
/// allowlist names, listed only when at least one of its collections is
/// visible to the caller, with ONLY those collection names. No YAML, no
/// description of a hidden resource.
async fn list_runbooks(
    state: &AppState,
    access: &munarium_access::AccessCtx,
    args: &Args,
) -> Result<Value, crate::error::ApiError> {
    let fp = fingerprint(access, "runbooks");
    let after = decode_cursor(&fp, args.get("cursor").and_then(Value::as_str))?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|l| l as usize)
        .unwrap_or(DEFAULT_PAGE);
    let pool = crate::runbooks_api::pool(state)?;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT runbook_ref, yaml FROM runbooks WHERE tenant_id = $1 AND status != 'removed'",
    )
    .bind(&access.tenant_id)
    .fetch_all(pool)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let mut latest: std::collections::BTreeMap<
        String,
        (u32, String, munarium_runbooks::RunbookDoc),
    > = Default::default();
    for (runbook_ref, yaml) in rows {
        let Ok(doc) = munarium_runbooks::parse_runbook(&yaml) else {
            continue;
        };
        let name = doc.metadata.name.clone();
        if !access.permits_runbook(&name) {
            continue;
        }
        let version = doc.metadata.version;
        if latest
            .get(&name)
            .map(|(v, _, _)| *v < version)
            .unwrap_or(true)
        {
            latest.insert(name, (version, runbook_ref, doc));
        }
    }
    let retrieval = state.retrieval_for(&access.tenant_id)?;
    let mut visible = Vec::new();
    for (name, (version, runbook_ref, doc)) in latest {
        let mut names = Vec::new();
        for spec in doc.spec.effective_collections() {
            if let Ok(info) = retrieval.collection_by_name(&spec.name).await {
                if info.status == "active" && access.permits(info.access_level, &info.compartments)
                {
                    names.push(info.name);
                }
            }
        }
        if names.is_empty() {
            continue;
        }
        visible.push((
            (name.clone(), runbook_ref.clone()),
            json!({"name": name, "runbook_ref": runbook_ref, "version": version, "collections": names}),
        ));
    }
    let (items, next, total) = page(visible, after, limit);
    Ok(json!({
        "runbooks": items,
        "total": total,
        "next_cursor": next.map(|n| encode_cursor(&fp, &n)),
    }))
}

/// `get_session`: the session header and one turn or a short page of turns,
/// with the native field values of `GET /v1/sessions/{id}`, read with a
/// bounded store query. Authority was checked before dispatch.
async fn session_projection(
    state: &AppState,
    access: &munarium_access::AccessCtx,
    args: &Args,
) -> Result<Value, crate::error::ApiError> {
    let id = args
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let pool = crate::runbooks_api::pool(state)?;
    let header: Option<(String, String, i32, Vec<String>, String, String)> = sqlx::query_as(
        "SELECT uid, runbook_ref, access_level, compartments, state, created_at::text
           FROM sessions WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&access.tenant_id)
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let (uid, runbook_ref, access_level, compartments, session_state, created_at) = header
        .ok_or_else(|| KernelError::NotFound {
            kind: "session",
            id: id.to_string(),
        })?;
    let (low, limit): (i64, i64) = match (
        args.get("ordinal").and_then(Value::as_u64),
        args.get("after_ordinal").and_then(Value::as_u64),
    ) {
        (Some(o), _) => (o as i64, 1),
        (None, after) => (
            after.map(|a| a as i64 + 1).unwrap_or(0),
            args.get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(MAX_TURNS as u64 / 2) as i64,
        ),
    };
    let exact = args.get("ordinal").is_some();
    #[allow(clippy::type_complexity)]
    let turns: Vec<(
        i32,
        String,
        Vec<String>,
        sqlx::types::Json<munarium_api_types::json::LiteralValue>,
        sqlx::types::Json<munarium_api_types::json::LiteralValue>,
        Option<sqlx::types::Json<munarium_api_types::json::LiteralValue>>,
        String,
    )> = sqlx::query_as(
        "SELECT ordinal, query, collections_searched, hits, envelope, completion, created_at::text
           FROM session_turns
          WHERE tenant_id = $1 AND session_id = $2 AND ordinal >= $3
            AND ($5 = false OR ordinal = $3)
          ORDER BY ordinal LIMIT $4",
    )
    .bind(&access.tenant_id)
    .bind(id)
    .bind(low.min(i32::MAX as i64) as i32)
    .bind(limit + 1)
    .bind(exact)
    .fetch_all(pool)
    .await
    .map_err(|e| KernelError::Storage(e.to_string()))?;
    let more = !exact && turns.len() as i64 > limit;
    let selected: Vec<munarium_api_types::SessionTurnDto> = turns
        .into_iter()
        .take(limit as usize)
        .map(
            |(ordinal, query, searched, hits, envelope, completion, created_at)| {
                munarium_api_types::SessionTurnDto {
                    ordinal: ordinal as u32,
                    query,
                    collections_searched: searched,
                    hits: hits.0 .0,
                    envelope: envelope.0 .0,
                    completion: completion.map(|v| v.0 .0),
                    created_at,
                }
            },
        )
        .collect();
    if exact && selected.is_empty() {
        return Err(KernelError::NotFound {
            kind: "turn",
            id: format!("{id}#{low}"),
        }
        .into());
    }
    let next = if more {
        selected.last().map(|t| t.ordinal)
    } else {
        None
    };
    let turns = serde_json::to_value(&selected).map_err(|e| KernelError::Storage(e.to_string()))?;
    Ok(json!({
        "session_id": id,
        "uid": uid,
        "runbook_ref": runbook_ref,
        "access_level": access_level,
        "compartments": compartments,
        "state": session_state,
        "created_at": created_at,
        "turns": turns,
        "page": {"next_after_ordinal": next},
    }))
}
