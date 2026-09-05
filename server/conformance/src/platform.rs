// SPDX-License-Identifier: Apache-2.0
//! Platform-surface conformance (M7–M12): black-box REST scenarios for the
//! uid contract, capability tokens, runbook-v2 applications, compartmentalized
//! sessions, ingestion, the removal lifecycle, and reports.
//!
//! Runs against a live pg-backed server (collections need postgres). The
//! caller supplies rw + mgmt static tokens for a FRESH tenant and the server
//! must have MUNARIUM_TOKEN_SECRET configured. Zero provider keys required —
//! nothing here triggers a completion.

use serde_json::{json, Value};

pub struct PlatformEnv {
    pub base: String,
    pub rw_token: String,
    pub mgmt_token: String,
    http: reqwest::Client,
}

type R = Result<(), String>;

macro_rules! expect {
    ($cond:expr, $($msg:tt)*) => {
        if !$cond {
            return Err(format!($($msg)*));
        }
    };
}

impl PlatformEnv {
    pub fn new(base: &str, rw_token: &str, mgmt_token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            rw_token: rw_token.to_string(),
            mgmt_token: mgmt_token.to_string(),
            http: reqwest::Client::new(),
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        uid: Option<&str>,
        body: Option<Body<'_>>,
    ) -> Result<(u16, Value), String> {
        // Every request carries a fresh Idempotency-Key: the command routes
        // REQUIRE one, and the routes that do not use it ignore it. A scenario
        // that wants to replay a command sends its own header instead.
        let mut rb = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("idempotency-key", uuid::Uuid::new_v4().to_string());
        if let Some(t) = token {
            rb = rb.bearer_auth(t);
        }
        if let Some(u) = uid {
            rb = rb.header("x-munarium-uid", u);
        }
        rb = match body {
            Some(Body::Json(v)) => rb.json(v),
            Some(Body::Yaml(y)) => rb.header("content-type", "text/yaml").body(y.to_string()),
            None => rb,
        };
        let resp = rb.send().await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let value: Value = resp.json().await.unwrap_or(Value::Null);
        Ok((status, value))
    }

    async fn get(&self, path: &str, token: &str, uid: &str) -> Result<(u16, Value), String> {
        self.send(reqwest::Method::GET, path, Some(token), Some(uid), None)
            .await
    }

    async fn post_json(
        &self,
        path: &str,
        token: &str,
        uid: &str,
        body: Value,
    ) -> Result<(u16, Value), String> {
        self.send(
            reqwest::Method::POST,
            path,
            Some(token),
            Some(uid),
            Some(Body::Json(&body)),
        )
        .await
    }

    async fn post_yaml(
        &self,
        path: &str,
        token: &str,
        uid: &str,
        yaml: &str,
    ) -> Result<(u16, Value), String> {
        self.send(
            reqwest::Method::POST,
            path,
            Some(token),
            Some(uid),
            Some(Body::Yaml(yaml)),
        )
        .await
    }

    /// GET returning the raw body — for the /admin HTML pages, whose point
    /// is that they RENDER; parsing them as JSON would fail on success.
    async fn get_raw(&self, path: &str, token: &str, uid: &str) -> Result<(u16, String), String> {
        let resp = self
            .http
            .get(format!("{}{}", self.base, path))
            .bearer_auth(token)
            .header("X-Munarium-Uid", uid)
            .send()
            .await
            .map_err(|e| format!("GET {path}: {e}"))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| format!("GET {path} body: {e}"))?;
        Ok((status, body))
    }

    /// Mint a capability token via the mgmt plane; returns (token, jti).
    async fn mint(
        &self,
        uid: &str,
        level: i64,
        compartments: &[&str],
        scopes: &[&str],
    ) -> Result<(String, String), String> {
        let (status, body) = self
            .post_json(
                "/v1/access-tokens",
                &self.mgmt_token,
                "platform-conformance",
                json!({
                    "uid": uid,
                    "access_level": level,
                    "compartments": compartments,
                    "scopes": scopes,
                }),
            )
            .await?;
        if status != 200 {
            return Err(format!("mint for {uid} failed: {status} {body}"));
        }
        Ok((
            body["token"].as_str().unwrap_or_default().to_string(),
            body["jti"].as_str().unwrap_or_default().to_string(),
        ))
    }
}

enum Body<'a> {
    Json(&'a Value),
    Yaml(&'a str),
}

fn slug(body: &Value) -> &str {
    body["type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

const SHAPE_YAML: &str = "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: { name: entdocs, version: 1 }\nspec:\n  fact:\n    schema: { type: object }\n";

fn runbook_yaml(version: u32) -> String {
    format!(
        r#"apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: {{ name: ent-support, version: {version} }}
spec:
  collections:
    - name: ent-public
      shape: entdocs@1
      accessLevel: 0
      sources: {{ filenamePrefix: "public/" }}
    - name: ent-secret
      shape: entdocs@1
      accessLevel: 2
      compartments: [eng]
      sources: {{ filenamePrefix: "eng/" }}
  retrieval: {{ topK: 5 }}
  models:
    default: {{ provider: default, tier: fast }}
    allowOverrides: [default]
  completion:
    promptTemplate: "Answer from context only.\n{{context}}\n\nQ: {{query}}"
  steps:
    - resolveSources: {{}}
    - buildIndex: {{}}
    - verify: {{}}
    - cutover: {{ approval: required }}
    - retireOld: {{ keep_versions: 2 }}
"#
    )
}

// ---------------------------------------------------------------------------
// scenarios
// ---------------------------------------------------------------------------

/// 8. The uid contract: missing uid is 400 uid-required; a JWT presented
///    under a different uid is 403 uid-mismatch.
async fn uid_contract(env: &PlatformEnv) -> R {
    let (status, body) = env
        .send(
            reqwest::Method::GET,
            "/v1/runbooks",
            Some(&env.rw_token),
            None,
            None,
        )
        .await?;
    expect!(status == 400, "missing uid: expected 400, got {status}");
    expect!(
        slug(&body) == "uid-required",
        "missing uid: expected uid-required slug, got {}",
        slug(&body)
    );

    let (token, _) = env.mint("uid-alice", 0, &[], &["query"]).await?;
    let (status, body) = env.get("/v1/runbooks", &token, "mallory").await?;
    expect!(status == 403, "uid mismatch: expected 403, got {status}");
    expect!(
        slug(&body) == "uid-mismatch",
        "uid mismatch: expected uid-mismatch slug, got {}",
        slug(&body)
    );
    Ok(())
}

/// 9a. Role partition: rw cannot mint tokens; mgmt cannot write the ledger.
async fn role_partition(env: &PlatformEnv) -> R {
    let (status, body) = env
        .post_json(
            "/v1/access-tokens",
            &env.rw_token,
            "ops",
            json!({"uid": "x", "access_level": 0, "scopes": ["query"]}),
        )
        .await?;
    expect!(
        status == 403 && slug(&body) == "forbidden",
        "rw minting a token must be 403 forbidden, got {status} {}",
        slug(&body)
    );

    let (status, body) = env
        .post_json("/v1/versions", &env.mgmt_token, "mgr", json!({}))
        .await?;
    expect!(
        status == 403 && slug(&body) == "forbidden",
        "mgmt writing the ledger must be 403 forbidden, got {status} {}",
        slug(&body)
    );
    Ok(())
}

/// 9b–10. The full retrieval-application lifecycle, then compartmentalized
/// multiturn sessions: publish shape + v2 runbook, ingest via matchers, run
/// with per-collection approvals, then a lvl-0 and a lvl-2+eng session see
/// disjoint result sets for the same query.
async fn application_and_compartments(env: &PlatformEnv) -> R {
    let ops = &env.rw_token;
    let (status, body) = env.post_yaml("/v1/shapes", ops, "ops", SHAPE_YAML).await?;
    expect!(status == 200, "apply shape: {status} {body}");

    // Validation first: the committed runbook is clean; a broken one reports.
    let (status, body) = env
        .post_yaml("/v1/runbooks/validate", ops, "ops", &runbook_yaml(1))
        .await?;
    expect!(
        status == 200 && body["valid"] == json!(true),
        "validate clean runbook: {status} {body}"
    );
    let broken = runbook_yaml(1).replace("topK: 5", "topK: 0");
    let (_, body) = env
        .post_yaml("/v1/runbooks/validate", ops, "ops", &broken)
        .await?;
    expect!(
        body["valid"] == json!(false),
        "topK: 0 must invalidate, got {body}"
    );

    let (status, body) = env
        .post_yaml("/v1/runbooks", ops, "ops", &runbook_yaml(1))
        .await?;
    expect!(status == 200, "apply runbook: {status} {body}");

    // Ingest two files under the ingest scope; matcher auto-bind routes them.
    let (ingest_token, _) = env.mint("loader", 2, &["eng"], &["ingest"]).await?;
    use base64::Engine as _;
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
    let (status, body) = env
        .post_json(
            "/v1/ingest/batch",
            &ingest_token,
            "loader",
            json!({"files": [
                {"filename": "public/handbook.md", "media_type": "text/markdown",
                 "content_base64": b64("The public handbook grants twenty vacation days.")},
                {"filename": "eng/launch.md", "media_type": "text/markdown",
                 "content_base64": b64("Secret launch window: vacation blackout in Q4.")},
            ]}),
        )
        .await?;
    expect!(status == 200, "batch ingest: {status} {body}");
    let results = body["results"].as_array().cloned().unwrap_or_default();
    expect!(results.len() == 2, "expected 2 ingest results, got {body}");
    expect!(
        results[0]["bound_to"] == json!(["ent-public"])
            && results[1]["bound_to"] == json!(["ent-secret"]),
        "matcher auto-bind wrong: {body}"
    );

    // A level-0 ingest token must NOT be able to write into ent-secret.
    let (low_token, _) = env.mint("lowloader", 0, &[], &["ingest"]).await?;
    let (status, body) = env
        .post_json(
            "/v1/ingest",
            &low_token,
            "lowloader",
            json!({"filename": "sneak.md", "media_type": "text/markdown",
                   "content_base64": b64("nope"), "collections": ["ent-secret"]}),
        )
        .await?;
    expect!(
        status == 403,
        "low-clearance write into ent-secret must be 403, got {status} {body}"
    );

    // Run: v2 executes per collection; approve both cutovers.
    let (status, body) = env
        .post_json("/v1/runbooks/ent-support/runs", ops, "ops", json!({}))
        .await?;
    expect!(
        status == 200 && body["state"] == json!("awaiting_approval"),
        "run: {status} {body}"
    );
    let run_id = body["run_id"].as_str().unwrap_or_default().to_string();
    for _pass in 0..2 {
        let (_, run) = env.get(&format!("/v1/runs/{run_id}"), ops, "ops").await?;
        let awaiting = run["steps"]
            .as_array()
            .and_then(|steps| {
                steps
                    .iter()
                    .find(|s| s["state"] == json!("awaiting_approval"))
            })
            .and_then(|s| s["ordinal"].as_u64())
            .ok_or_else(|| format!("no step awaiting approval: {run}"))?;
        let (status, body) = env
            .post_json(
                &format!("/v1/runs/{run_id}/steps/{awaiting}/approve"),
                ops,
                "ops",
                json!({}),
            )
            .await?;
        expect!(status == 200, "approve {awaiting}: {status} {body}");
    }
    let (_, run) = env.get(&format!("/v1/runs/{run_id}"), ops, "ops").await?;
    expect!(run["state"] == json!("done"), "run must finish done: {run}");

    // List + info expose per-collection access requirements.
    let (_, list) = env.get("/v1/runbooks", ops, "ops").await?;
    let entry = list["runbooks"]
        .as_array()
        .and_then(|r| {
            r.iter()
                .find(|b| b["runbook_ref"] == json!("ent-support@1"))
        })
        .ok_or_else(|| format!("ent-support@1 missing from list: {list}"))?;
    let levels: Vec<i64> = entry["collections"]
        .as_array()
        .map(|cols| {
            cols.iter()
                .filter_map(|c| c["access_level"].as_i64())
                .collect()
        })
        .unwrap_or_default();
    expect!(
        levels.contains(&0) && levels.contains(&2),
        "list must show levels 0 and 2: {entry}"
    );

    // Two clearances, one runbook: disjoint result sets for the same query.
    let (alice, _) = env.mint("comp-alice", 0, &[], &["query"]).await?;
    let (bob, _) = env.mint("comp-bob", 2, &["eng"], &["query"]).await?;

    let (status, session_a) = env
        .post_json(
            "/v1/runbooks/ent-support/sessions",
            &alice,
            "comp-alice",
            json!({}),
        )
        .await?;
    expect!(status == 200, "alice session: {status} {session_a}");
    expect!(
        session_a["permitted_collections"] == json!(["ent-public"]),
        "alice must see only ent-public: {session_a}"
    );
    let (status, session_b) = env
        .post_json(
            "/v1/runbooks/ent-support/sessions",
            &bob,
            "comp-bob",
            json!({}),
        )
        .await?;
    expect!(status == 200, "bob session: {status} {session_b}");
    let bob_permitted = session_b["permitted_collections"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    expect!(
        bob_permitted.len() == 2,
        "bob must see both collections: {session_b}"
    );

    let sid_a = session_a["session_id"].as_str().unwrap_or_default();
    let sid_b = session_b["session_id"].as_str().unwrap_or_default();
    let (status, turn_a) = env
        .post_json(
            &format!("/v1/sessions/{sid_a}/turns"),
            &alice,
            "comp-alice",
            json!({"query": "vacation"}),
        )
        .await?;
    expect!(status == 200, "alice turn: {status} {turn_a}");
    expect!(
        turn_a["hits"]
            .as_array()
            .map(|h| !h.is_empty() && h.iter().all(|hit| hit["collection"] == json!("ent-public")))
            .unwrap_or(false),
        "alice hits must be ent-public only: {turn_a}"
    );

    let (status, turn_b) = env
        .post_json(
            &format!("/v1/sessions/{sid_b}/turns"),
            &bob,
            "comp-bob",
            json!({"query": "vacation"}),
        )
        .await?;
    expect!(status == 200, "bob turn: {status} {turn_b}");
    let bob_collections: std::collections::HashSet<String> = turn_b["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .filter_map(|h| h["collection"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    expect!(
        bob_collections.contains("ent-secret"),
        "bob's merged hits must include ent-secret: {turn_b}"
    );
    expect!(
        turn_b["envelopes"].as_array().map(|e| e.len()) == Some(2),
        "bob must get one envelope per collection: {turn_b}"
    );

    // Multiturn continuity on the same session id.
    let (status, turn2) = env
        .post_json(
            &format!("/v1/sessions/{sid_b}/turns"),
            &bob,
            "comp-bob",
            json!({"query": "blackout"}),
        )
        .await?;
    expect!(
        status == 200 && turn2["ordinal"] == json!(2),
        "follow-on turn must be ordinal 2: {status} {turn2}"
    );

    // Cross-uid session access is refused.
    let (status, body) = env
        .post_json(
            &format!("/v1/sessions/{sid_b}/turns"),
            &alice,
            "comp-alice",
            json!({"query": "x"}),
        )
        .await?;
    expect!(
        status == 403,
        "alice on bob's session must be 403, got {status} {body}"
    );

    // Model-override policy: a provider outside allowOverrides is refused
    // with the dedicated slug (checked BEFORE any provider spend).
    let (status, body) = env
        .post_json(
            &format!("/v1/sessions/{sid_b}/turns"),
            &bob,
            "comp-bob",
            json!({"query": "x", "complete": true,
                   "model_override": {"provider": "not-allowed-provider"}}),
        )
        .await?;
    expect!(
        status == 403 && slug(&body) == "override-not-allowed",
        "disallowed override: expected 403 override-not-allowed, got {status} {}",
        slug(&body)
    );

    // Scope enforcement: a query token cannot ingest.
    let (status, body) = env
        .post_json(
            "/v1/ingest",
            &bob,
            "comp-bob",
            json!({"filename": "x.md", "media_type": "text/markdown",
                   "content_base64": b64("x")}),
        )
        .await?;
    expect!(
        status == 403 && slug(&body) == "scope-missing",
        "query token on ingest: expected scope-missing, got {status} {}",
        slug(&body)
    );
    Ok(())
}

/// 11. Soft removal is double-pass and leaves data intact.
async fn removal_double_pass(env: &PlatformEnv) -> R {
    let ops = &env.rw_token;
    let (status, body) = env
        .post_yaml("/v1/runbooks", ops, "ops", &runbook_yaml(9))
        .await?;
    expect!(status == 200, "apply @9: {status} {body}");

    // Single-pass confirm is refused.
    let (status, body) = env
        .post_json(
            "/v1/runbooks/ent-support@9/remove-confirm",
            ops,
            "ops",
            json!({"removal_id": "rm-guess"}),
        )
        .await?;
    expect!(
        status == 409 && slug(&body) == "removal-not-confirmed",
        "confirm without request: expected 409 removal-not-confirmed, got {status} {}",
        slug(&body)
    );

    let (status, body) = env
        .post_json(
            "/v1/runbooks/ent-support@9/remove-request",
            ops,
            "ops",
            json!({}),
        )
        .await?;
    expect!(status == 200, "remove-request: {status} {body}");
    let removal_id = body["removal_id"].as_str().unwrap_or_default().to_string();

    let (status, _) = env
        .post_json(
            "/v1/runbooks/ent-support@9/remove-confirm",
            ops,
            "ops",
            json!({"removal_id": "rm-wrong"}),
        )
        .await?;
    expect!(status == 409, "wrong removal_id must be 409, got {status}");

    let (status, body) = env
        .post_json(
            "/v1/runbooks/ent-support@9/remove-confirm",
            ops,
            "ops",
            json!({"removal_id": removal_id}),
        )
        .await?;
    expect!(
        status == 200 && body["status"] == json!("removed"),
        "confirm: {status} {body}"
    );

    // Sessions on the removed exact ref answer 410; the bare name still
    // resolves to the live @1.
    let (token, _) = env.mint("rm-user", 0, &[], &["query"]).await?;
    let (status, body) = env
        .post_json(
            "/v1/runbooks/ent-support@9/sessions",
            &token,
            "rm-user",
            json!({}),
        )
        .await?;
    expect!(
        status == 410 && slug(&body) == "runbook-removed",
        "session on removed ref: expected 410 runbook-removed, got {status} {}",
        slug(&body)
    );
    let (status, body) = env
        .post_json(
            "/v1/runbooks/ent-support/sessions",
            &token,
            "rm-user",
            json!({}),
        )
        .await?;
    expect!(
        status == 200 && body["runbook_ref"] == json!("ent-support@1"),
        "bare name must resolve to live @1: {status} {body}"
    );

    // Hidden from the default list; visible with include_removed.
    let (_, list) = env.get("/v1/runbooks", ops, "ops").await?;
    let refs: Vec<&str> = list["runbooks"]
        .as_array()
        .map(|r| r.iter().filter_map(|b| b["runbook_ref"].as_str()).collect())
        .unwrap_or_default();
    expect!(
        !refs.contains(&"ent-support@9"),
        "removed ref must be hidden: {refs:?}"
    );
    let (_, list) = env
        .get("/v1/runbooks?include_removed=true", ops, "ops")
        .await?;
    let all: Vec<&str> = list["runbooks"]
        .as_array()
        .map(|r| r.iter().filter_map(|b| b["runbook_ref"].as_str()).collect())
        .unwrap_or_default();
    expect!(
        all.contains(&"ent-support@9"),
        "include_removed must show it: {all:?}"
    );
    Ok(())
}

/// 12. Reports are mgmt-gated and reflect the traffic this suite generated;
///     revocation lands in the issuance audit.
async fn reports_and_revoke(env: &PlatformEnv) -> R {
    let (status, body) = env
        .get("/v1/reports/usage?group_by=uid", &env.rw_token, "ops")
        .await?;
    expect!(
        status == 403,
        "rw on reports must be 403, got {status} {body}"
    );

    let (status, body) = env
        .get("/v1/reports/usage?group_by=uid", &env.mgmt_token, "mgr")
        .await?;
    expect!(status == 200, "usage: {status} {body}");
    let keys: Vec<&str> = body["rows"]
        .as_array()
        .map(|rows| rows.iter().filter_map(|r| r["key"].as_str()).collect())
        .unwrap_or_default();
    expect!(
        keys.contains(&"comp-alice") && keys.contains(&"comp-bob"),
        "usage rows must include the session uids: {keys:?}"
    );

    let (status, body) = env
        .get(
            "/v1/reports/audit?uid=comp-bob&limit=10",
            &env.mgmt_token,
            "mgr",
        )
        .await?;
    expect!(
        status == 200
            && body["entries"]
                .as_array()
                .map(|e| !e.is_empty())
                .unwrap_or(false),
        "audit for comp-bob must be non-empty: {status} {body}"
    );

    // Revoke: the deny-list row lands (enforcement depends on the server's
    // MUNARIUM_TOKEN_REVOCATION_CHECK mode, which the response reports).
    let (_, jti) = env.mint("revokee", 0, &[], &["query"]).await?;
    let (status, body) = env
        .post_json(
            &format!("/v1/access-tokens/{jti}/revoke"),
            &env.mgmt_token,
            "mgr",
            json!({}),
        )
        .await?;
    expect!(
        status == 200 && body["revoked"] == json!(true),
        "revoke: {status} {body}"
    );
    let (status, body) = env
        .get("/v1/access-tokens?uid=revokee", &env.mgmt_token, "mgr")
        .await?;
    expect!(status == 200, "token list: {status} {body}");
    let revoked = body["tokens"]
        .as_array()
        .and_then(|t| t.first())
        .map(|t| !t["revoked_at"].is_null())
        .unwrap_or(false);
    expect!(revoked, "issuance audit must show revoked_at: {body}");
    Ok(())
}

/// Guided authoring, end to end and keyless: catalog → draft → answers →
/// validate → assist (MUST degrade to a note, zero provider keys) → export
/// (hash-verified client-side) → apply → the runbook and its collections
/// exist. Plus the refusal contract: a blank draft cannot export.
async fn authoring_lifecycle(env: &PlatformEnv) -> R {
    let uid = "authoring-tester";
    let rw = env.rw_token.clone();

    // Catalog: the §19 patterns this build serves — all seven with the experiment
    // exemplars embedded, fewer in a product build that ships only some of
    // the exemplar runbooks — and every one it lists resolves to a detail
    // that carries its exemplar. The contract is "never list a pattern you
    // cannot back", not a count.
    let (status, body) = env.get("/v1/authoring/patterns", &rw, uid).await?;
    expect!(status == 200, "patterns list: {status} {body}");
    let listed: Vec<String> = body["patterns"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| p["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    expect!(!listed.is_empty(), "the catalog lists no patterns: {body}");
    for id in &listed {
        let (status, body) = env
            .get(&format!("/v1/authoring/patterns/{id}"), &rw, uid)
            .await?;
        expect!(status == 200, "pattern detail {id}: {status}");
        expect!(
            body["runbook_yaml"]
                .as_str()
                .is_some_and(|y| y.contains("kind: Runbook")),
            "pattern detail {id} carries the exemplar: {body}"
        );
    }
    // red-flag-review starts from due-diligence, the exemplar every build
    // embeds (the product tree ships it), so the lifecycle drafts from it.
    expect!(
        listed.iter().any(|id| id == "red-flag-review"),
        "red-flag-review must be served by every build: {listed:?}"
    );

    // Create a draft; the interview comes back §16-ordered.
    let (status, body) = env
        .post_json(
            "/v1/authoring/drafts",
            &rw,
            uid,
            json!({ "name": "vendor-security", "pattern_id": "red-flag-review" }),
        )
        .await?;
    expect!(status == 200, "create draft: {status} {body}");
    let draft_id = body["draft_id"].as_str().unwrap_or_default().to_string();
    expect!(!draft_id.is_empty(), "draft_id: {body}");
    let first_section = body["interview"][0]["id"].as_str().unwrap_or_default();
    expect!(
        first_section == "identity",
        "interview starts at identity: {body}"
    );

    // A blank draft refuses to export.
    let (status, body) = env
        .post_json(
            &format!("/v1/authoring/drafts/{draft_id}/export"),
            &rw,
            uid,
            json!({}),
        )
        .await?;
    expect!(
        status == 409,
        "blank export must 409 authoring-draft-invalid: {status} {body}"
    );

    // Canonical answers materialize a clean set.
    let answers = json!({
        "identity.description": "Vendor security reviews for procurement.",
        "prefix.root": "vendors/",
        "prefix.areas": [
            { "path": "public/", "description": "published attestations" },
            { "path": "contracts/", "description": "signed agreements" },
        ],
        "access.uniform_public": false,
        "access.area_levels": { "public": 0, "contracts": 2 },
        "access.area_compartments": { "contracts": ["legal"] },
    });
    let (status, body) = env
        .send(
            reqwest::Method::PUT,
            &format!("/v1/authoring/drafts/{draft_id}/answers"),
            Some(&rw),
            Some(uid),
            Some(Body::Json(
                &json!({ "answers": answers, "materialize": true }),
            )),
        )
        .await?;
    expect!(status == 200, "answers: {status} {body}");
    expect!(
        body["validation"]["valid"].as_bool() == Some(true),
        "canonical answers must validate clean: {body}"
    );
    expect!(
        body["documents"].as_array().map(|a| a.len()) == Some(2),
        "one shape + one runbook: {body}"
    );

    // Assist DEGRADES on a keyless deployment — 200 with assist_note, and
    // the documents survive untouched.
    let (status, body) = env
        .post_json(
            &format!("/v1/authoring/drafts/{draft_id}/assist"),
            &rw,
            uid,
            json!({}),
        )
        .await?;
    expect!(status == 200, "keyless assist must 200: {status} {body}");
    expect!(
        body["assist_note"].as_str().is_some(),
        "keyless assist must carry a degrade note: {body}"
    );
    expect!(
        body["documents"].as_array().map(|a| a.len()) == Some(2),
        "assist must not lose documents: {body}"
    );

    // Validate stands alone too.
    let (status, body) = env
        .post_json(
            &format!("/v1/authoring/drafts/{draft_id}/validate"),
            &rw,
            uid,
            json!({}),
        )
        .await?;
    expect!(
        status == 200 && body["valid"].as_bool() == Some(true),
        "validate: {status} {body}"
    );

    // Export: verify the manifest CLIENT-side, exactly as mmctl does.
    let (status, bundle) = env
        .post_json(
            &format!("/v1/authoring/drafts/{draft_id}/export"),
            &rw,
            uid,
            json!({}),
        )
        .await?;
    expect!(status == 200, "export: {status} {bundle}");
    expect!(
        bundle["kind"].as_str() == Some("MunariumAuthoringBundle"),
        "bundle kind: {bundle}"
    );
    let files = bundle["files"].as_object().cloned().unwrap_or_default();
    let mut hashes: std::collections::BTreeMap<String, String> = Default::default();
    for (path, yaml) in &files {
        use sha2::Digest as _;
        let actual = hex::encode(sha2::Sha256::digest(
            yaml.as_str().unwrap_or_default().as_bytes(),
        ));
        expect!(
            bundle["hashes"][path].as_str() == Some(actual.as_str()),
            "per-file hash mismatch for {path}"
        );
        hashes.insert(path.clone(), actual);
    }
    let mut buf = String::new();
    for (path, hash) in &hashes {
        buf.push_str(path);
        buf.push('\0');
        buf.push_str(hash);
        buf.push('\n');
    }
    use sha2::Digest as _;
    let manifest = hex::encode(sha2::Sha256::digest(buf.as_bytes()));
    expect!(
        bundle["manifest_hash"].as_str() == Some(manifest.as_str()),
        "manifest hash mismatch: {bundle}"
    );
    let order = bundle["apply_order"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    expect!(
        order
            .first()
            .and_then(|v| v.as_str())
            .is_some_and(|p| p.starts_with("shapes/")),
        "shapes apply first: {bundle}"
    );

    // Apply in place; the runbook and its collections then exist.
    let (status, body) = env
        .post_json(
            &format!("/v1/authoring/drafts/{draft_id}/apply"),
            &rw,
            uid,
            json!({}),
        )
        .await?;
    expect!(status == 200, "apply: {status} {body}");
    expect!(
        body["applied"].as_array().map(|a| a.len()) == Some(2),
        "apply covers the set: {body}"
    );
    let (status, body) = env.get("/v1/runbooks/vendor-security", &rw, uid).await?;
    expect!(
        status == 200,
        "applied runbook must be hosted: {status} {body}"
    );
    expect!(
        body["collections"].as_array().is_some_and(|c| c.len() == 2),
        "applied runbook reaches its two collections: {body}"
    );
    Ok(())
}

/// Bulk upload sessions: manifest diff, chunked upload with per-file sha
/// verification, wholesale chunk replay idempotency, finalize verification,
/// and the zero-byte re-run. Self-contained: applies its own shape+runbook.
async fn bulk_upload_lifecycle(env: &PlatformEnv) -> R {
    use base64::Engine as _;
    use sha2::Digest as _;
    let ops = &env.rw_token;
    let sha = |s: &str| hex::encode(sha2::Sha256::digest(s.as_bytes()));
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);

    let shape = "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: { name: bulkdocs, version: 1 }\nspec:\n  fact:\n    schema: { type: object }\n";
    let (status, body) = env.post_yaml("/v1/shapes", ops, "ops", shape).await?;
    expect!(status == 200, "apply bulk shape: {status} {body}");
    let runbook = r#"apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: bulk-archive, version: 1 }
spec:
  collections:
    - name: bulk-open-docs
      shape: bulkdocs@1
      accessLevel: 0
      sources: { filenamePrefix: "bulkdocs/" }
    - name: bulk-secret-docs
      shape: bulkdocs@1
      accessLevel: 2
      compartments: [eng]
      sources: { filenamePrefix: "bulksecret/" }
  retrieval: { topK: 5 }
  models:
    default: { provider: default, tier: fast }
    allowOverrides: [default]
  completion:
    promptTemplate: "Answer from context only.\n{context}\n\nQ: {query}"
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
"#;
    let (status, body) = env.post_yaml("/v1/runbooks", ops, "ops", runbook).await?;
    expect!(status == 200, "apply bulk runbook: {status} {body}");

    let (ingest_token, _) = env.mint("bulkloader", 0, &[], &["ingest"]).await?;
    let uid = "bulkloader";
    let (a, b, c) = (
        "Bulk document alpha: the treaty was signed.",
        "Bulk document beta: the harbor closed in March.",
        "Bulk document gamma: the assembly dissolved.",
    );

    // A query-scope token must not reach the bulk plane.
    let (query_token, _) = env.mint("bulkreader", 0, &[], &["query"]).await?;
    let (status, _) = env
        .post_json(
            "/v1/ingest/bulk",
            &query_token,
            "bulkreader",
            json!({"files": [{"filename": "bulkdocs/a.md", "sha256": sha(a), "bytes_len": a.len(), "media_type": "text/markdown"}]}),
        )
        .await?;
    expect!(
        status == 403,
        "query token on bulk open must 403, got {status}"
    );

    // Manifest validation: duplicates and traversal paths are rejected whole.
    let (status, _) = env
        .post_json(
            "/v1/ingest/bulk",
            &ingest_token,
            uid,
            json!({"files": [
                {"filename": "bulkdocs/a.md", "sha256": sha(a), "bytes_len": a.len(), "media_type": "text/markdown"},
                {"filename": "bulkdocs/a.md", "sha256": sha(a), "bytes_len": a.len(), "media_type": "text/markdown"},
            ]}),
        )
        .await?;
    expect!(
        status == 400,
        "duplicate manifest entry must 400, got {status}"
    );
    let (status, _) = env
        .post_json(
            "/v1/ingest/bulk",
            &ingest_token,
            uid,
            json!({"files": [{"filename": "../evil.md", "sha256": sha(a), "bytes_len": a.len(), "media_type": "text/markdown"}]}),
        )
        .await?;
    expect!(status == 400, "traversal path must 400, got {status}");

    // Open: fresh manifest, nothing present, all three needed.
    let manifest = json!({"label": "conformance", "files": [
        {"filename": "bulkdocs/a.md", "sha256": sha(a), "bytes_len": a.len(), "media_type": "text/markdown"},
        {"filename": "bulkdocs/b.md", "sha256": sha(b), "bytes_len": b.len(), "media_type": "text/markdown"},
        {"filename": "bulkdocs/c.md", "sha256": sha(c), "bytes_len": c.len(), "media_type": "text/markdown"},
    ]});
    let (status, body) = env
        .post_json("/v1/ingest/bulk", &ingest_token, uid, manifest.clone())
        .await?;
    expect!(status == 200, "bulk open: {status} {body}");
    let bulk_id = body["bulk_id"].as_str().unwrap_or_default().to_string();
    expect!(!bulk_id.is_empty(), "bulk_id missing: {body}");
    expect!(
        body["total"] == json!(3)
            && body["already_present"] == json!(0)
            && body["needed"].as_array().map(|n| n.len()) == Some(3),
        "fresh open must need all three: {body}"
    );

    // Chunk 1: a is good; b's bytes deliberately do NOT match the manifest.
    let (status, body) = env
        .post_json(
            &format!("/v1/ingest/bulk/{bulk_id}/chunk"),
            &ingest_token,
            uid,
            json!({"files": [
                {"filename": "bulkdocs/a.md", "media_type": "text/markdown", "content_base64": b64(a)},
                {"filename": "bulkdocs/b.md", "media_type": "text/markdown", "content_base64": b64("corrupted bytes")},
            ]}),
        )
        .await?;
    expect!(status == 200, "chunk 1: {status} {body}");
    let results = body["results"].as_array().cloned().unwrap_or_default();
    expect!(
        results[0]["error"].is_null() && results[0]["bound_to"] == json!(["bulk-open-docs"]),
        "a.md must store and auto-bind: {body}"
    );
    expect!(
        results[1]["error"]
            .as_str()
            .is_some_and(|e| e.contains("sha256 mismatch")),
        "corrupt b.md must fail per-file on sha mismatch: {body}"
    );
    expect!(
        body["stored"] == json!(1) && body["failed"] == json!(1) && body["pending"] == json!(1),
        "chunk 1 counts: {body}"
    );

    // Finalize now: incomplete, naming exactly what is owed.
    let (status, body) = env
        .post_json(
            &format!("/v1/ingest/bulk/{bulk_id}/complete"),
            &ingest_token,
            uid,
            json!({}),
        )
        .await?;
    expect!(
        status == 200 && body["status"] == json!("incomplete"),
        "early complete must report incomplete: {status} {body}"
    );
    expect!(
        body["missing_count"] == json!(2),
        "b and c are missing: {body}"
    );

    // Chunk 2 — the wholesale replay: a again (idempotent no-op), b fixed, c.
    let (status, body) = env
        .post_json(
            &format!("/v1/ingest/bulk/{bulk_id}/chunk"),
            &ingest_token,
            uid,
            json!({"files": [
                {"filename": "bulkdocs/a.md", "media_type": "text/markdown", "content_base64": b64(a)},
                {"filename": "bulkdocs/b.md", "media_type": "text/markdown", "content_base64": b64(b)},
                {"filename": "bulkdocs/c.md", "media_type": "text/markdown", "content_base64": b64(c)},
            ]}),
        )
        .await?;
    expect!(status == 200, "chunk 2: {status} {body}");
    let results = body["results"].as_array().cloned().unwrap_or_default();
    expect!(
        results[0]["existed"] == json!(true),
        "replayed a.md must be an idempotent no-op: {body}"
    );
    expect!(
        results.iter().all(|r| r["error"].is_null()),
        "chunk 2 must fully land: {body}"
    );
    expect!(
        body["pending"] == json!(0) && body["failed"] == json!(0),
        "nothing owed after chunk 2: {body}"
    );

    // A file outside the manifest is refused per-file, not stored.
    let (status, body) = env
        .post_json(
            &format!("/v1/ingest/bulk/{bulk_id}/chunk"),
            &ingest_token,
            uid,
            json!({"files": [
                {"filename": "bulkdocs/stray.md", "media_type": "text/markdown", "content_base64": b64("stray")},
            ]}),
        )
        .await?;
    expect!(status == 200, "stray chunk: {status} {body}");
    expect!(
        body["results"][0]["error"]
            .as_str()
            .is_some_and(|e| e.contains("manifest")),
        "stray file must be refused: {body}"
    );

    // Finalize: completed; status agrees and carries the needed list shape.
    let (status, body) = env
        .post_json(
            &format!("/v1/ingest/bulk/{bulk_id}/complete"),
            &ingest_token,
            uid,
            json!({}),
        )
        .await?;
    expect!(
        status == 200 && body["status"] == json!("completed"),
        "complete: {status} {body}"
    );
    let (status, body) = env
        .get(
            &format!("/v1/ingest/bulk/{bulk_id}?include_needed=true"),
            &ingest_token,
            uid,
        )
        .await?;
    expect!(
        status == 200
            && body["status"] == json!("completed")
            && body["needed"].as_array().map(|n| n.len()) == Some(0),
        "status after complete: {status} {body}"
    );

    // A replayed chunk must not DOWNGRADE what the session already stored:
    // `existed: true` on re-send is an idempotent no-op, not evidence that
    // nothing landed. (Chunk 2 above re-sent a.md, so `stored` must still
    // count all three.)
    let (_, body) = env
        .get(&format!("/v1/ingest/bulk/{bulk_id}"), &ingest_token, uid)
        .await?;
    expect!(
        body["stored"] == json!(3) && body["skipped_existing"] == json!(0),
        "a replayed chunk must not rewrite 'stored' to 'skipped_existing': {body}"
    );

    // The zero-byte re-run: a second session over the same manifest needs
    // nothing and completes without a single chunk.
    let (status, body) = env
        .post_json("/v1/ingest/bulk", &ingest_token, uid, manifest)
        .await?;
    expect!(
        status == 200
            && body["already_present"] == json!(3)
            && body["needed"].as_array().map(|n| n.len()) == Some(0),
        "re-run open must owe nothing: {status} {body}"
    );
    let rerun_id = body["bulk_id"].as_str().unwrap_or_default().to_string();
    let (status, body) = env
        .post_json(
            &format!("/v1/ingest/bulk/{rerun_id}/complete"),
            &ingest_token,
            uid,
            json!({}),
        )
        .await?;
    expect!(
        status == 200 && body["status"] == json!("completed"),
        "zero-byte re-run completes: {status} {body}"
    );

    // Unknown session: 404.
    let (status, _) = env
        .get("/v1/ingest/bulk/blk-doesnotexist", &ingest_token, uid)
        .await?;
    expect!(status == 404, "unknown session must 404, got {status}");

    // Stored bytes are not the same as a landed document. Single ingest
    // writes the bytes BEFORE checking clearance, so a bind rejected on
    // clearance still leaves a matching `sources` row. A later session must
    // NOT read that row as "already present" — otherwise it reports
    // `completed` over a collection that never received the document.
    let orphan = "Bulk orphan: stored once, bound never.";
    let (status, body) = env
        .post_json(
            "/v1/ingest",
            &ingest_token,
            uid,
            json!({"filename": "bulkdocs/orphan.md", "media_type": "text/markdown",
                   "content_base64": b64(orphan), "collections": ["bulk-secret-docs"]}),
        )
        .await?;
    expect!(
        status == 403,
        "level-0 token must not bind into bulk-secret-docs: {status} {body}"
    );
    let (status, body) = env
        .post_json(
            "/v1/ingest/bulk",
            &ingest_token,
            uid,
            json!({"files": [
                {"filename": "bulkdocs/orphan.md", "sha256": sha(orphan),
                 "bytes_len": orphan.len(), "media_type": "text/markdown"},
            ]}),
        )
        .await?;
    expect!(status == 200, "orphan open: {status} {body}");
    expect!(
        body["already_present"] == json!(0) && body["needed"] == json!(["bulkdocs/orphan.md"]),
        "a stored-but-unbound document must still be NEEDED: {body}"
    );
    let orphan_id = body["bulk_id"].as_str().unwrap_or_default().to_string();
    let (_, body) = env
        .post_json(
            &format!("/v1/ingest/bulk/{orphan_id}/chunk"),
            &ingest_token,
            uid,
            json!({"files": [
                {"filename": "bulkdocs/orphan.md", "media_type": "text/markdown",
                 "content_base64": b64(orphan)},
            ]}),
        )
        .await?;
    expect!(
        body["results"][0]["bound_to"] == json!(["bulk-open-docs"]),
        "re-sending the orphan must bind it: {body}"
    );
    Ok(())
}

/// `POST /v1/versions/{id}/findings`: a service files a warn-only
/// finding with both evidence sides; a re-run files nothing twice; a block is
/// refused; the finding is invisible below the seq it was stamped at; and
/// the `findings` scope is what authorizes it.
async fn discrepancy_findings(env: &PlatformEnv) -> R {
    let uid = "matrix-svc";
    // A capability token carrying ONLY the findings scope: proves the scope
    // is sufficient on its own, and that it does not need query/ingest.
    let (svc_token, _jti) = env.mint(uid, 0, &[], &["findings"]).await?;

    let (status, body) = env
        .post_json("/v1/versions", &env.rw_token, uid, json!({}))
        .await?;
    expect!(status == 200, "create version: {status} {body}");
    let vid = body["version_id"].as_str().unwrap_or_default().to_string();

    // One real claim so the head is above zero and a pin BELOW the stamp
    // seq is expressible.
    let (status, body) = env
        .post_json(
            &format!("/v1/versions/{vid}/claims"),
            &env.rw_token,
            uid,
            json!({"claim_type": "fact", "subject": "shareholder.43", "key": "shares", "value": "90000"}),
        )
        .await?;
    expect!(status == 200, "seed claim: {status} {body}");
    let claim_id = body["claim"]["id"].as_str().unwrap_or_default().to_string();
    let head = body["head_seq"].as_u64().unwrap_or_default();
    expect!(head >= 1, "seed claim must advance the head, got {head}");

    let finding = json!({
        "rule_id": "matrix.discrepancy-candidate",
        "severity": "warn",
        "message": "register says 90500, ledger says 90000",
        "scope_path": "company.7.captable",
        "detail": {"evidence_ref": "evidence/ev-batch-0001#r0002", "claim_id": claim_id,
                   "source_value": "90500", "ledger_value": "90000", "verdict": "differ"}
    });
    let (status, body) = env
        .post_json(
            &format!("/v1/versions/{vid}/findings"),
            &svc_token,
            uid,
            json!({"findings": [finding]}),
        )
        .await?;
    expect!(
        status == 200 && body["recorded"] == json!(1) && body["skipped_duplicates"] == json!(0),
        "first filing records one: {status} {body}"
    );
    let stamped = body["seq"].as_u64().unwrap_or_default();
    expect!(
        stamped == head,
        "stamped at the current head {head}, got {stamped}"
    );

    // Re-run: byte-identical content, nothing written.
    let (status, body) = env
        .post_json(
            &format!("/v1/versions/{vid}/findings"),
            &svc_token,
            uid,
            json!({"findings": [finding]}),
        )
        .await?;
    expect!(
        status == 200 && body["recorded"] == json!(0) && body["skipped_duplicates"] == json!(1),
        "a replayed reconciliation must file nothing twice: {status} {body}"
    );

    // Visible through the prefix filter, with both evidence sides intact.
    let (status, body) = env
        .get(
            &format!("/v1/versions/{vid}/findings?rule_prefix=matrix."),
            &env.rw_token,
            uid,
        )
        .await?;
    expect!(status == 200, "read findings: {status} {body}");
    let rows = body["findings"].as_array().cloned().unwrap_or_default();
    expect!(
        rows.len() == 1,
        "exactly one matrix finding, got {}: {body}",
        rows.len()
    );
    let d = &rows[0]["finding"]["detail"];
    expect!(
        d["evidence_ref"] == json!("evidence/ev-batch-0001#r0002")
            && d["claim_id"] == json!(claim_id),
        "both evidence sides must survive the round trip: {d}"
    );

    // Invisible below the seq it was stamped at: one pin bounds this store
    // like every other.
    let (status, body) = env
        .get(
            &format!(
                "/v1/versions/{vid}/findings?rule_prefix=matrix.&as_of_seq={}",
                stamped - 1
            ),
            &env.rw_token,
            uid,
        )
        .await?;
    expect!(
        status == 200 && body["findings"].as_array().is_some_and(|a| a.is_empty()),
        "a pin below the stamp seq must not see the finding: {status} {body}"
    );

    // A block is refused: this route is not a gate.
    let (status, body) = env
        .post_json(
            &format!("/v1/versions/{vid}/findings"),
            &svc_token,
            uid,
            json!({"findings": [{"rule_id": "matrix.discrepancy-candidate", "severity": "block",
                                 "message": "no"}]}),
        )
        .await?;
    expect!(
        status == 400,
        "a block severity must be refused with 400: {status} {body}"
    );

    // Authorization: mgmt is off the data path; a query-only token lacks the scope.
    let (status, _) = env
        .post_json(
            &format!("/v1/versions/{vid}/findings"),
            &env.mgmt_token,
            uid,
            json!({"findings": [finding]}),
        )
        .await?;
    expect!(status == 403, "mgmt must not file findings, got {status}");
    let (query_only, _) = env.mint("reader", 0, &[], &["query"]).await?;
    let (status, body) = env
        .post_json(
            &format!("/v1/versions/{vid}/findings"),
            &query_only,
            uid,
            json!({"findings": [finding]}),
        )
        .await?;
    expect!(
        status == 403,
        "a token without the findings scope must be refused: {status} {body}"
    );

    // And the ledger is untouched: shadow reconciliation files findings, not facts.
    let (status, body) = env
        .get(&format!("/v1/versions/{vid}/facts"), &env.rw_token, uid)
        .await?;
    expect!(status == 200, "facts: {status}");
    let facts = body["facts"].as_array().cloned().unwrap_or_default();
    expect!(
        facts.len() == 1 && facts[0]["value"] == json!("90000"),
        "filing a discrepancy must not change canon: {body}"
    );
    Ok(())
}

/// The evidence hierarchy (S-3.x), end to end and keyless.
///
/// Four claims, and the last two are the ones that would be expensive to get
/// wrong:
///
/// 1. A turn naming NO profile carries no `hierarchy` key at all. The
///    governing invariant of S-3.x, checked on the wire rather than in a unit
///    test, because "the JSON is unchanged" is a statement about the wire.
/// 2. A named-but-undeclared profile is refused, not silently downgraded to
///    the document path — answering a different question than the one asked,
///    invisibly, is the worse failure.
/// 3. A document layer reports `supports_completeness: false`. Retrieval
///    returns what it found, never a proof that nothing else exists.
/// 4. A REQUIRED layer that cannot be served refuses the turn, and the
///    refusal **must not name the layer's sources**. That is the
///    hidden-required-layer rule, and a live check is the only place it can
///    really be tested: it is a property of the bytes a caller receives.
async fn evidence_hierarchy(env: &PlatformEnv) -> R {
    let uid = "hier-user";
    let (token, _) = env.mint(uid, 0, &[], &["query"]).await?;

    // Two profiles on one runbook: one servable from documents alone, one
    // whose required layer points at a Matrix data view that this conformance
    // deployment has no Matrix for. The second is the interesting one.
    let yaml = r#"apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: hier-support, version: 1 }
spec:
  collections:
    - name: hier-public
      shape: entdocs@1
      accessLevel: 0
      sources: { filenamePrefix: "public/" }
  dataViews:
    - name: register
      contract: revenue_by_region@2
      accessLevel: 0
  retrieval:
    topK: 5
    researchProfiles:
      - name: docs-only
        layers:
          - { name: documents, sources: [hier-public], role: primary }
      - name: needs-register
        layers:
          - name: register
            sources: [matrix:register]
            role: controlling
            requirement: required
  completion:
    promptTemplate: "Answer from context only.\n{context}\n\nQ: {query}"
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - cutover: {}
"#;
    let (status, body) = env
        .post_yaml("/v1/runbooks", &env.rw_token, uid, yaml)
        .await?;
    expect!(status == 200, "apply hierarchy runbook: {status} {body}");

    let (status, session) = env
        .post_json("/v1/runbooks/hier-support/sessions", &token, uid, json!({}))
        .await?;
    expect!(status == 200, "session: {status} {session}");
    let sid = session["session_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // 1. No profile named, and the runbook declares no default: the response
    //    must not have grown a key.
    let (status, turn) = env
        .post_json(
            &format!("/v1/sessions/{sid}/turns"),
            &token,
            uid,
            json!({"query": "vacation"}),
        )
        .await?;
    expect!(status == 200, "legacy turn: {status} {turn}");
    expect!(
        turn.get("hierarchy").is_none(),
        "a turn naming no profile must not grow a hierarchy key: {turn}"
    );

    // 2. A profile the runbook does not declare.
    let (status, body) = env
        .post_json(
            &format!("/v1/sessions/{sid}/turns"),
            &token,
            uid,
            json!({"query": "vacation", "research_profile": "no-such-profile"}),
        )
        .await?;
    expect!(
        status == 400,
        "unknown profile must be 400: {status} {body}"
    );
    expect!(
        slug(&body) == "unknown-research-profile",
        "typed slug, not a generic invalid-input: {body}"
    );

    // 3. The document profile runs and reports honestly.
    let (status, turn) = env
        .post_json(
            &format!("/v1/sessions/{sid}/turns"),
            &token,
            uid,
            json!({"query": "vacation", "research_profile": "docs-only"}),
        )
        .await?;
    expect!(status == 200, "docs-only turn: {status} {turn}");
    let h = &turn["hierarchy"];
    expect!(h["profile"] == json!("docs-only"), "profile echoed: {turn}");
    expect!(
        h["intent_explicit"] == json!(true),
        "no intent model is pinned, so the intent is the caller's own query \
         and must be marked explicit — otherwise a keyless run reads as a \
         planner run: {turn}"
    );
    let layers = h["layers"].as_array().cloned().unwrap_or_default();
    expect!(layers.len() == 1, "one layer ran: {turn}");
    expect!(
        layers[0]["block"] == json!("document_hits"),
        "the document layer produced document hits: {turn}"
    );
    expect!(
        layers[0]["supports_completeness"] == json!(false),
        "document hits NEVER support a completeness claim — retrieval returns \
         what it found, not a proof that nothing else exists: {turn}"
    );
    expect!(
        h["completeness_available"] == json!(false),
        "and so the turn as a whole cannot support one: {turn}"
    );

    // 4. The required layer nobody can serve.
    let (status, body) = env
        .post_json(
            &format!("/v1/sessions/{sid}/turns"),
            &token,
            uid,
            json!({"query": "vacation", "research_profile": "needs-register"}),
        )
        .await?;
    expect!(
        status == 424,
        "a required layer that produced nothing must refuse the turn: {status} {body}"
    );
    expect!(
        slug(&body) == "required-evidence-unavailable",
        "typed slug: {body}"
    );
    let detail = body["detail"].as_str().unwrap_or_default();
    expect!(
        detail.contains("register"),
        "the refusal names the LAYER so the operator can act on it: {body}"
    );
    // The hidden-required-layer rule. A caller who cannot see a source must
    // not learn it exists from the shape of a refusal, and the layer's data
    // view and its contract are both sources.
    expect!(
        !detail.contains("matrix:") && !detail.contains("revenue_by_region"),
        "the refusal must NOT name the layer's sources: {body}"
    );

    Ok(())
}

/// 13. The /admin dashboards RENDER against a pg-backed server. A dashboard
///     handler that panics resets the connection instead of erroring, so the
///     only place this class of defect can surface is an HTTP tier over real
///     PostgreSQL — which is exactly where /admin/storage was first found
///     dead: `SUM(bigint)` is NUMERIC in PostgreSQL, the handler `get()` a
///     BIGINT, and every prior exercise of the page ran on the memory store.
async fn admin_dashboards_render(env: &PlatformEnv) -> R {
    // Content assertions, not status: an unauthorized /admin request answers
    // with a redirect to the login page, the client follows it, and the login
    // page is a 200 — the exact "a bogus token produces the very redirect it
    // asserts" trap the matrix tree's admin tier recorded. The discriminator
    // is the title chrome: every RENDERED dashboard titles
    // "{page} — munarium admin"; the login page titles "munarium admin
    // login" — only the em-dash suffix marks a real page.
    for (path, marker) in [
        ("/admin", "— munarium admin"),
        ("/admin/storage", "Tiered storage"),
    ] {
        let (status, body) = env.get_raw(path, &env.mgmt_token, "mgr").await?;
        expect!(
            status == 200 && body.contains(marker),
            "{path} must render {marker:?} for mgmt, got {status} ({} bytes)",
            body.len()
        );
    }
    // And stays mgmt-gated: rw must land anywhere but a rendered dashboard.
    let (status, body) = env.get_raw("/admin/storage", &env.rw_token, "ops").await?;
    expect!(
        !body.contains("— munarium admin"),
        "rw on /admin/storage must not see a rendered dashboard, got {status} ({} bytes)",
        body.len()
    );
    Ok(())
}

pub async fn run_all(env: &PlatformEnv) -> Vec<(&'static str, R)> {
    vec![
        (
            "platform.discrepancy-findings",
            discrepancy_findings(env).await,
        ),
        ("platform.uid-contract", uid_contract(env).await),
        ("platform.role-partition", role_partition(env).await),
        (
            "platform.application-and-compartments",
            application_and_compartments(env).await,
        ),
        // AFTER application-and-compartments: that scenario publishes the
        // entdocs@1 shape this runbook binds to.
        ("platform.evidence-hierarchy", evidence_hierarchy(env).await),
        (
            "platform.removal-double-pass",
            removal_double_pass(env).await,
        ),
        ("platform.reports-and-revoke", reports_and_revoke(env).await),
        (
            "platform.authoring-lifecycle",
            authoring_lifecycle(env).await,
        ),
        (
            "platform.bulk-upload-lifecycle",
            bulk_upload_lifecycle(env).await,
        ),
        (
            "platform.admin-dashboards-render",
            admin_dashboards_render(env).await,
        ),
    ]
}
