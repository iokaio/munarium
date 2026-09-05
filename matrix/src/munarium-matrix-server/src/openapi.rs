// SPDX-License-Identifier: Apache-2.0
//! The OpenAPI document.
//!
//! Hand-assembled from the DTO schemas rather than derived by a macro over the
//! router: the router is role-dependent, and a spec that changes shape with
//! `MUNARIUM_MATRIX_ROLE` would make the CI drift check meaningless. This
//! function describes the FULL surface, which is what a client codes against.

use serde_json::json;

/// The component schemas, derived from the DTOs' `ToSchema` implementations.
///
/// Until 2026-08-29 `components` held only `securitySchemes`: this document
/// described paths and status codes and nothing about the bodies, while the
/// `ToSchema` derives sitting on every DTO went unused. A client could code
/// against the URLs and had to guess the rest.
fn schemas() -> serde_json::Value {
    use utoipa::PartialSchema;
    let mut map = serde_json::Map::new();
    macro_rules! put {
        ($($ty:ty),* $(,)?) => {
            $( map.insert(
                <$ty as utoipa::ToSchema>::name().to_string(),
                serde_json::to_value(<$ty as PartialSchema>::schema()).unwrap_or_default(),
            ); )*
        };
    }
    use munarium_matrix_types::dto::*;
    use munarium_matrix_types::validate::Finding;
    put!(
        ProbeResponse,
        IntrospectResponse,
        RolePostureReport,
        PostureCheck,
        TableInfo,
        ColumnInfo,
        ApplyResponse,
        ValidateResponse,
        AssetSummary,
        AssetListResponse,
        JobAccepted,
        JournalEntry,
        JournalListResponse,
        HealthDataResponse,
        VerifyResponse,
        SyncRunResponse,
        MappingRunResponse,
        VersionResponse,
        PromotionGates,
        PromotionStatus,
        GateHistory,
        GateHistoryEntry,
        RollbackResponse,
        PlannerAskRequest,
        PlannerAskResponse,
        Finding,
    );
    serde_json::Value::Object(map)
}

pub fn document() -> serde_json::Value {
    json!({
      "openapi": "3.1.0",
      "info": {
        "title": "munarium-matrix",
        "version": env!("CARGO_PKG_VERSION"),
        "description":
          "The structured-evidence plane. Registers formal data sources, materializes governed \
           record collections, executes verified query contracts, and seals typed evidence into \
           munarium-server. The cross-tree contract with the server is matrix/contract/."
      },
      "servers": [{ "url": "http://localhost:8180" }],
      "components": {
        "securitySchemes": {
          "bearer": { "type": "http", "scheme": "bearer" }
        },
        "schemas": schemas()
      },
      "security": [{ "bearer": [] }],
      "paths": {
        "/healthz": { "get": { "summary": "Liveness", "security": [], "responses": { "200": { "description": "alive" } } } },
        "/readyz":  { "get": { "summary": "Readiness (store probe; 503 while draining)", "security": [], "responses": { "200": { "description": "ready" }, "503": { "description": "not ready or draining" } } } },
        "/version": { "get": { "summary": "Build, role and contract version", "security": [], "responses": { "200": { "description": "version" } } } },
        "/openapi.json": { "get": { "summary": "This document", "security": [], "responses": { "200": { "description": "spec" } } } },
        "/docs": { "get": { "summary": "Human landing page", "security": [], "responses": { "200": { "description": "html" } } } },
        "/healthdata": { "get": { "summary": "Per-source health (control role)", "responses": { "200": { "description": "sources" } } } },
        // One path, one verb, because JSON-RPC puts the method in the body.
        // Describing the individual MCP methods here would be inventing an
        // OpenAPI shape for a protocol that already has its own schema, and
        // the two would drift; `tools/list` is the authoritative description
        // of what this server offers, and it is generated from the assets.
        "/mcp": { "post": { "summary": "MCP (JSON-RPC 2.0): initialize, ping, tools/list, tools/call — pre-declared tools only, no free SQL", "responses": { "200": { "description": "a JSON-RPC response; a refusal rides it as a tool error" } } } },
        "/v1/datasources/{name}/planner/ask": { "post": { "summary": "Ask a conversational planner (Genie) a question; assist returns admitted SQL to run through a contract, evaluation records and admits nothing", "responses": { "200": { "description": "the proposal, its pin, and whether the plan is pinned" }, "422": { "description": "no planner surface, or nothing to admit" } } } },
        "/v1/assets": { "post": { "summary": "Apply any asset kind (YAML; kind sniffed by parsing)", "responses": { "200": { "description": "applied" }, "422": { "description": "validation findings" } } } },
        "/v1/assets/validate": { "post": { "summary": "Validate without applying — the same validators mxctl uses", "responses": { "200": { "description": "findings" } } } },
        "/v1/datasources": {
          "get": { "summary": "List data sources", "responses": { "200": { "description": "assets" } } },
          "post": { "summary": "Apply a DataSource", "responses": { "200": { "description": "applied" } } }
        },
        "/v1/datasources/{name}": { "get": { "summary": "The applied YAML, verbatim", "responses": { "200": { "description": "yaml" }, "404": { "description": "unknown" } } } },
        "/v1/contracts": {
          "get": { "summary": "List query contracts", "responses": { "200": { "description": "assets" } } },
          "post": { "summary": "Apply a QueryContract", "responses": { "200": { "description": "applied" } } }
        },
        "/v1/contracts/{name}": { "get": { "summary": "The applied YAML, verbatim", "responses": { "200": { "description": "yaml" } } } },
        "/v1/metricviews": {
          "get": { "summary": "List metric-view overlays", "responses": { "200": { "description": "assets" } } },
          "post": { "summary": "Apply a MetricView", "responses": { "200": { "description": "applied" } } }
        },
        "/v1/metricviews/{name}": { "get": { "summary": "The applied YAML, verbatim", "responses": { "200": { "description": "yaml" } } } },
        "/v1/dataviews": {
          "get": { "summary": "List native data views", "responses": { "200": { "description": "assets" } } },
          "post": { "summary": "Apply a DataView", "responses": { "200": { "description": "applied" } } }
        },
        "/v1/dataviews/{name}": { "get": { "summary": "The applied YAML, verbatim", "responses": { "200": { "description": "yaml" } } } },
        "/v1/mappings": {
          "get": { "summary": "List claim mappings", "responses": { "200": { "description": "assets" } } },
          "post": { "summary": "Apply a ClaimMapping", "responses": { "200": { "description": "applied" } } }
        },
        "/v1/mappings/{name}": { "get": { "summary": "The applied YAML, verbatim", "responses": { "200": { "description": "yaml" } } } },
        "/v1/journal": { "get": { "summary": "The journal (mgmt role; redacted by default)", "responses": { "200": { "description": "entries" } } } },
        "/v1/datasources/{name}/probe": { "post": { "summary": "Probe a source's reachability now (rw). A refusal is an ANSWER — reachable:false with a typed reason, not a 5xx.", "responses": { "200": { "description": "probe result", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProbeResponse" } } } }, "404": { "description": "no such source" } } } },
        "/v1/datasources/{name}/introspect": { "post": { "summary": "Prove the role posture and read the schema (rw). Refuses a superuser, owner, or DML-holding role.", "responses": { "200": { "description": "posture and schema", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/IntrospectResponse" } } } }, "403": { "description": "the role posture is not permitted" } } } },
        "/v1/datasources/{name}/sync": { "post": { "summary": "Enqueue a sync run, one job per authorization class (control role)", "responses": { "200": { "description": "queued job ids" }, "422": { "description": "the source declares no sync block" } } } },
        "/v1/mappings/{name}/run": { "post": { "summary": "Enqueue a reconcile pass (control role)", "responses": { "200": { "description": "queued job id" } } } },
        "/v1/contracts/{name}/execute": { "post": { "summary": "Execute a verified query contract against a QueryIntent (query role). Returns an EvidenceBlock, or a typed Refusal as problem+json.", "responses": { "200": { "description": "evidence block" }, "403": { "description": "the session dominates no authorization class" }, "422": { "description": "not covered: undeclared parameter, dialect or operation" }, "429": { "description": "budget exhausted" }, "503": { "description": "source unavailable" } } } },
        "/v1/mappings/{name}/promotion": { "get": { "summary": "Where a mapping stands against the promotion gates (control role)", "responses": { "200": { "description": "status" } } } },
        "/v1/mappings/{name}/gate-history": { "get": { "summary": "Gate values per run over time, against the CURRENT thresholds (control role).", "responses": { "200": { "description": "gate history", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/GateHistory" } } } } } } },
        "/v1/mappings/{name}/promote": { "post": { "summary": "Promote a mapping to authoritative under a recorded decision; every gate is checked here (control role, rw)", "responses": { "200": { "description": "promoted" }, "409": { "description": "already promoted" }, "422": { "description": "a gate did not clear; the refusal names it and the numbers" } } } },
        "/v1/mappings/{name}/demote": { "post": { "summary": "Stop a mapping writing canon, effective on the next poll (control role, rw)", "responses": { "200": { "description": "demoted" }, "422": { "description": "no active promotion" } } } },
        "/v1/mappings/{name}/rollback": { "post": { "summary": "Supersede every claim the mapping proposed with its prior value, under a decision; append-only (control role, rw)", "responses": { "200": { "description": "counts" } } } },
        "/v1/contracts/{name}/verify": { "post": { "summary": "Run the contract’s verified questions — its regression suite (query role)", "responses": { "200": { "description": "per-question outcomes; `failed` is non-zero when any moved" } } } },
        "/v1/metricviews/{name}/execute": { "post": { "summary": "Execute a `kind: semantic` QueryIntent against a metric view (query role): MEASURE() SQL compiled from the asset’s closed lists, gated on the fingerprint the last passing verify recorded. Returns an EvidenceBlock, or a typed Refusal as problem+json.", "responses": { "200": { "description": "evidence block" }, "403": { "description": "the session dominates no authorization class" }, "422": { "description": "metric_not_covered, or metric_view_changed / no passing verification on record" }, "429": { "description": "budget exhausted" }, "503": { "description": "source unavailable" } } } },
        "/v1/metricviews/{name}/verify": { "post": { "summary": "Run the metric view’s verified questions under the definition the source reports now, and record that definition’s fingerprint (rw)", "responses": { "200": { "description": "per-question outcomes plus `fingerprint`; `failed` is non-zero when any moved" } } } },
        "/v1/dataviews/{name}/execute": { "post": { "summary": "Execute a `kind: semantic` QueryIntent against a native data view (query role): the declared aggregates over one fact table, gated on the table definition’s verified fingerprint.", "responses": { "200": { "description": "evidence block" }, "422": { "description": "metric_not_covered, or metric_view_changed / no passing verification on record" }, "429": { "description": "budget exhausted" }, "503": { "description": "source unavailable" } } } },
        "/v1/dataviews/{name}/verify": { "post": { "summary": "Run the data view’s verified questions and record the table definition’s fingerprint (rw)", "responses": { "200": { "description": "per-question outcomes plus `fingerprint`" } } } }
      }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_document_is_valid_json_and_names_every_meta_route() {
        let d = super::document();
        for path in ["/healthz", "/readyz", "/version", "/openapi.json", "/docs"] {
            assert!(d["paths"][path].is_object(), "missing {path}");
        }
        // Meta routes are unauthenticated; the rest inherit the bearer scheme.
        assert_eq!(
            d["paths"]["/healthz"]["get"]["security"],
            serde_json::json!([])
        );
        assert!(d["paths"]["/v1/journal"]["get"]["security"].is_null());
    }

    #[test]
    fn the_document_describes_the_full_surface_not_one_roles_view() {
        // The spec is what a client codes against, so it must name every route
        // the product has — including the ones a given container 404s on.
        let d = super::document();
        for path in [
            "/v1/contracts/{name}/execute",
            "/v1/contracts/{name}/verify",
            "/v1/metricviews/{name}/execute",
            "/v1/metricviews/{name}/verify",
            "/v1/dataviews/{name}/execute",
            "/v1/dataviews/{name}/verify",
            "/v1/datasources/{name}/sync",
            "/v1/mappings/{name}/run",
        ] {
            assert!(
                d["paths"][path]["post"].is_object(),
                "the spec must describe {path} even though only one role serves it"
            );
        }
    }
}

#[cfg(test)]
mod drift {
    /// Every route the router serves is declared in the OpenAPI document, and
    /// every declared path is served.
    ///
    /// Matrix's CI checked only that the spec PARSED, so
    /// `/v1/mappings/{name}/gate-history` was routed, client-callable and
    /// undeclared for a whole phase, and `/docs` advertised four routes that
    /// answered 404. The server tree has had this check from the start; this is its
    /// twin.
    ///
    /// The router is read from the source rather than introspected, because
    /// axum's `Router` does not expose its paths and a role-dependent router
    /// cannot be walked without standing three of them up.
    #[test]
    fn every_served_route_is_declared_and_every_declared_route_is_served() {
        let src = include_str!("rest.rs");
        let mut served: Vec<String> = Vec::new();
        for line in src.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix(".route(\"") else {
                continue;
            };
            let Some(end) = rest.find('"') else { continue };
            served.push(rest[..end].to_string());
        }
        assert!(
            served.len() > 15,
            "the route scrape found only {} routes — the parser has drifted from rest.rs",
            served.len()
        );

        let doc = crate::openapi::document();
        let declared: Vec<String> = doc["paths"]
            .as_object()
            .expect("paths is an object")
            .keys()
            .cloned()
            .collect();

        // The ops plane (/metrics) lives on its own listener and is out of
        // this document by design.
        let undeclared: Vec<&String> = served.iter().filter(|r| !declared.contains(r)).collect();
        assert!(
            undeclared.is_empty(),
            "routes served but NOT in openapi.json: {undeclared:?}"
        );

        let unserved: Vec<&String> = declared
            .iter()
            .filter(|d| !served.contains(d) && !d.starts_with("/metrics"))
            .collect();
        assert!(
            unserved.is_empty(),
            "paths declared in openapi.json but NOT served: {unserved:?}"
        );
    }

    /// Every route `/docs` names is one the router actually serves.
    ///
    /// The page advertised `introspect`, `probe`, `/v1/syncs` and
    /// `/v1/reports/*` for months. Two of those now exist; the other two were
    /// never built, and a landing page that sends an operator to a 404 is
    /// worse than one that says less.
    #[test]
    fn the_docs_page_only_names_routes_that_answer() {
        let src = include_str!("rest.rs");
        let served: Vec<String> = src
            .lines()
            .filter_map(|l| l.trim().strip_prefix(".route(\"").map(str::to_string))
            .filter_map(|r| r.find('"').map(|e| r[..e].to_string()))
            .collect();

        // The page writes `/v1/datasources/{name}/introspect|probe|sync` as one
        // token; split the alternation back out before comparing.
        let page = src
            .split("<h1>munarium-matrix</h1>")
            .nth(1)
            .expect("the docs page is inline in this file");
        let page = &page[..page.find("</body>").expect("page end")];

        for raw in page.split("<code>").skip(1) {
            let token = &raw[..raw.find("</code>").expect("closing code tag")];
            if !token.starts_with("/v1/") {
                continue;
            }
            let (base, alts) = match token.rsplit_once('/') {
                Some((b, last)) if last.contains('|') => (b.to_string(), last.split('|').collect()),
                _ => (token.to_string(), vec![]),
            };
            let candidates: Vec<String> = if alts.is_empty() {
                vec![base]
            } else {
                alts.iter().map(|a| format!("{base}/{a}")).collect()
            };
            for c in candidates {
                assert!(
                    served.iter().any(|s| s == &c),
                    "/docs names `{c}`, which no route serves"
                );
            }
        }
    }
}
