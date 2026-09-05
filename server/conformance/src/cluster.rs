// SPDX-License-Identifier: Apache-2.0
//! Cluster conformance (2026-08-17): two live instances, ONE shared
//! postgres — black-box proof of the N-replica hazards the clustering work
//! fixed. This is deliberately NOT the seven kernel scenarios through a
//! load balancer (postgres conformance already proves the storage
//! semantics); each scenario here targets one specific cross-instance
//! mechanism:
//!
//!   1. a shape applied on A is usable on B          (registry TTL reload)
//!   2. a provider config UPDATED on A converges on B (yaml-hash rebuild)
//!   3. an idempotent command replays across instances (table-backed keys)
//!   4. seq allocation interleaves A/B on one lineage  (FOR UPDATE mutex)
//!   5. concurrent approvals resolve to ONE executor   (run advisory lock)
//!
//! Environment contract (test.ps1 -Cluster is the harness): both instances
//! share one database AND one static rw token for a FRESH tenant; both run
//! MUNARIUM_REGISTRY_TTL_SECS=1 so scenario 1/2 waits are short; gRPC may be
//! disabled (the hazards are transport-independent). Zero provider keys —
//! scenario 2 asserts on budget/provider errors, never on completions.

use serde_json::{json, Value};

type R = Result<(), String>;

macro_rules! expect {
    ($cond:expr, $($msg:tt)*) => {
        if !$cond {
            return Err(format!($($msg)*));
        }
    };
}

const UID: &str = "cluster-conf";

pub struct ClusterEnv {
    pub a: String,
    pub b: String,
    pub token: String,
    http: reqwest::Client,
}

fn slug(body: &Value) -> String {
    body.get("type")
        .and_then(|t| t.as_str())
        .and_then(|t| t.rsplit('/').next())
        .unwrap_or("")
        .to_string()
}

impl ClusterEnv {
    pub fn new(a: &str, b: &str, token: &str) -> Self {
        Self {
            a: a.trim_end_matches('/').to_string(),
            b: b.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::new(),
        }
    }

    async fn send(
        &self,
        base: &str,
        method: reqwest::Method,
        path: &str,
        idem_key: Option<&str>,
        body: Option<(&str, String)>, // (content-type, body)
    ) -> Result<(u16, Value), String> {
        let mut rb = self
            .http
            .request(method, format!("{base}{path}"))
            .bearer_auth(&self.token)
            .header("x-munarium-uid", UID);
        if let Some(k) = idem_key {
            rb = rb.header("idempotency-key", k);
        }
        if let Some((ct, b)) = body {
            rb = rb.header("content-type", ct).body(b);
        }
        let resp = rb.send().await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let value: Value = resp.json().await.unwrap_or(Value::Null);
        Ok((status, value))
    }

    async fn post_json(
        &self,
        base: &str,
        path: &str,
        idem_key: Option<&str>,
        body: Value,
    ) -> Result<(u16, Value), String> {
        self.send(
            base,
            reqwest::Method::POST,
            path,
            idem_key,
            Some(("application/json", body.to_string())),
        )
        .await
    }

    async fn post_yaml(&self, base: &str, path: &str, yaml: &str) -> Result<(u16, Value), String> {
        self.send(
            base,
            reqwest::Method::POST,
            path,
            None,
            Some(("text/yaml", yaml.to_string())),
        )
        .await
    }

    async fn get(&self, base: &str, path: &str) -> Result<(u16, Value), String> {
        self.send(base, reqwest::Method::GET, path, None, None)
            .await
    }
}

/// Both instances' registries hold the tenant before the TTL clock matters.
async fn warm_both(env: &ClusterEnv) -> R {
    for base in [&env.a, &env.b] {
        let (status, _) = env.get(base, "/v1/runbooks").await?;
        expect!(status == 200, "warm {base}: expected 200, got {status}");
    }
    Ok(())
}

fn shape_yaml(name: &str) -> String {
    format!(
        "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: {{ name: {name}, version: 1 }}\nspec:\n  fact:\n    schema: {{ type: object }}\n"
    )
}

/// Scenario 1 — Shape applied on A, used on B. B touched the tenant FIRST (warm), so
/// without the TTL reload B's registry would never see the new shape — the
/// exact pre-fix staleness.
async fn shape_visible_across_instances(env: &ClusterEnv) -> R {
    warm_both(env).await?;
    let (status, body) = env
        .post_yaml(&env.a, "/v1/shapes", &shape_yaml("cl-shape"))
        .await?;
    expect!(status == 200, "apply on A: {status} {body}");
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let (status, body) = env
        .post_json(
            &env.b,
            "/v1/collections",
            None,
            json!({ "name": "cl-col-shape", "shape_ref": "cl-shape@1", "access_level": 0 }),
        )
        .await?;
    expect!(
        status == 200,
        "B must see the shape applied on A within the registry TTL: {status} {body}"
    );
    Ok(())
}

fn provider_yaml(rpm: u32) -> String {
    format!(
        "apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{ name: cl-prov }}\nspec:\n  provider: anthropic\n  credentialRef: {{ env: MUNARIUM_CLUSTER_CONF_UNSET_KEY }}\n  models: {{ complete: [claude-haiku-4-5] }}\n  budgets: {{ rpm: {rpm} }}\n"
    )
}

/// Scenario 2 — Provider config UPDATED on A converges on B: the update drops rpm to
/// 1, so B's SECOND completion attempt must be rate-limited — possible only
/// if B rebuilt its entry from the shared table (the yaml-hash reload).
/// Keyless by design: attempt #1 consumes the budget then fails
/// provider-error (the credential env var is deliberately unset); #2 hits
/// the budget check first.
async fn provider_update_converges(env: &ClusterEnv) -> R {
    let (status, body) = env
        .post_yaml(&env.a, "/v1/providers", &provider_yaml(1000))
        .await?;
    expect!(status == 200, "apply provider on A: {status} {body}");
    // B loads the generous config (first touch).
    let (status, _) = env
        .post_json(
            &env.b,
            "/v1/providers/cl-prov/complete",
            None,
            json!({ "prompt": "x", "max_tokens": 8 }),
        )
        .await?;
    expect!(
        status == 502,
        "B attempt under generous budget must fail on the missing credential (502), got {status}"
    );
    // A tightens the budget to rpm=1; B must converge within the TTL.
    let (status, body) = env
        .post_yaml(&env.a, "/v1/providers", &provider_yaml(1))
        .await?;
    expect!(status == 200, "update provider on A: {status} {body}");
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let (status, _) = env
        .post_json(
            &env.b,
            "/v1/providers/cl-prov/complete",
            None,
            json!({ "prompt": "x", "max_tokens": 8 }),
        )
        .await?;
    expect!(
        status == 502,
        "B first post-update attempt consumes the rebuilt rpm=1 budget (502), got {status}"
    );
    let (status, body) = env
        .post_json(
            &env.b,
            "/v1/providers/cl-prov/complete",
            None,
            json!({ "prompt": "x", "max_tokens": 8 }),
        )
        .await?;
    expect!(
        status == 429,
        "B second attempt must hit the UPDATED rpm=1 budget (429 rate-limited) — a stale \
         registry would 502 again: {status} {body}"
    );
    Ok(())
}

/// Scenario 3 — One Idempotency-Key across instances: the replay returns the recorded
/// response (same version id, no second version), and a mismatched body
/// under the same key is 422 idempotency-mismatch.
async fn idempotency_shared(env: &ClusterEnv) -> R {
    let key = "cluster-idem-1";
    // Vary a field the DTO actually carries (metadata): an unknown field
    // would be dropped at deserialization and both bodies would hash
    // identically (found live building this scenario).
    let body = json!({ "metadata": { "who": "cluster-idem" } });
    let (status, first) = env
        .post_json(&env.a, "/v1/versions", Some(key), body.clone())
        .await?;
    expect!(status == 200, "create via A: {status} {first}");
    let id_a = first["version_id"].as_str().unwrap_or("").to_string();
    expect!(!id_a.is_empty(), "no version_id from A: {first}");

    let (status, replay) = env
        .post_json(&env.b, "/v1/versions", Some(key), body)
        .await?;
    expect!(status == 200, "replay via B: {status} {replay}");
    let id_b = replay["version_id"].as_str().unwrap_or("");
    expect!(
        id_a == id_b,
        "replay via B must return A's recorded response ({id_a} vs {id_b})"
    );

    let (status, mism) = env
        .post_json(
            &env.b,
            "/v1/versions",
            Some(key),
            json!({ "metadata": { "who": "different-body" } }),
        )
        .await?;
    expect!(
        status == 422 && slug(&mism) == "idempotency-mismatch",
        "mismatched body under the same key must 422 idempotency-mismatch: {status} {mism}"
    );
    Ok(())
}

/// Scenario 4 — Alternating appends A/B/A/B on ONE lineage: seq must interleave
/// without gaps or duplicates (the lineage_heads FOR UPDATE mutex holds
/// across processes), and a stale expected_head from either instance 409s.
async fn seq_interleaves(env: &ClusterEnv) -> R {
    let (status, created) = env
        .post_json(
            &env.a,
            "/v1/versions",
            Some("cluster-seq-create"),
            json!({ "metadata": { "who": "cluster-seq" } }),
        )
        .await?;
    expect!(status == 200, "create: {status} {created}");
    let vid = created["version_id"].as_str().unwrap_or("").to_string();

    for (i, base) in [&env.a, &env.b, &env.a, &env.b].into_iter().enumerate() {
        let (status, resp) = env
            .post_json(
                base,
                &format!("/v1/versions/{vid}/claims"),
                Some(&format!("cluster-seq-{i}")),
                json!({ "claim_type": "fact", "subject": "cluster", "key": format!("k{i}"), "value": "v" }),
            )
            .await?;
        expect!(status == 200, "append {i}: {status} {resp}");
        let head = resp["head_seq"].as_i64().unwrap_or(-1);
        expect!(
            head == (i as i64) + 1,
            "append {i}: expected head_seq {} got {head}",
            i + 1
        );
    }
    let (status, conflict) = env
        .post_json(
            &env.b,
            &format!("/v1/versions/{vid}/claims"),
            Some("cluster-seq-stale"),
            json!({ "claim_type": "fact", "subject": "cluster", "key": "k9", "value": "v", "expected_head": 1 }),
        )
        .await?;
    expect!(
        status == 409 && slug(&conflict) == "head-conflict",
        "stale expected_head via B must 409 head-conflict: {status} {conflict}"
    );
    Ok(())
}

fn runbook_yaml() -> &'static str {
    r#"apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: cl-app, version: 1 }
spec:
  collections:
    - name: cl-docs
      shape: cl-shape@1
      accessLevel: 0
      sources: { filenamePrefix: "cl/" }
  retrieval: { topK: 5 }
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
"#
}

/// Scenario 5 — Concurrent approvals from A and B resolve to exactly ONE executor:
/// the winner drives the cutover, the loser draws `run-locked` (409) — or,
/// when the winner finishes before the loser's precondition check, the
/// step is no longer awaiting_approval and the loser draws invalid-input.
/// Either way: exactly one 200, and the run settles `done`.
async fn concurrent_approval_single_executor(env: &ClusterEnv) -> R {
    // Depends on scenario 1's shape (run_all order guarantees it).
    let (status, body) = env
        .post_yaml(&env.a, "/v1/runbooks", runbook_yaml())
        .await?;
    expect!(status == 200, "apply runbook: {status} {body}");
    let (status, body) = env
        .post_json(
            &env.a,
            "/v1/ingest",
            None,
            json!({
                "filename": "cl/one.txt",
                "media_type": "text/plain",
                "content_base64": "Y2x1c3RlciBjb25mb3JtYW5jZSBkb2N1bWVudA==",
                "runbook_ref": "cl-app@1"
            }),
        )
        .await?;
    expect!(status == 200, "ingest: {status} {body}");

    let (status, run) = env
        .post_json(&env.a, "/v1/runbooks/cl-app/runs", None, json!({}))
        .await?;
    expect!(status == 200, "run: {status} {run}");
    let run_id = run["run_id"].as_str().unwrap_or("").to_string();
    expect!(
        run["state"] == "awaiting_approval",
        "run must pause at the approval gate, got {run}"
    );
    // The run response is a summary; the step plan comes from the status
    // route (the same poll a human or mmctl --watch would do).
    let (status, run_status) = env.get(&env.a, &format!("/v1/runs/{run_id}")).await?;
    expect!(status == 200, "run status: {status} {run_status}");
    let ordinal = run_status["steps"]
        .as_array()
        .and_then(|steps| {
            steps
                .iter()
                .find(|s| s["state"] == "awaiting_approval")
                .and_then(|s| s["ordinal"].as_i64())
        })
        .ok_or_else(|| format!("no awaiting_approval step in {run_status}"))?;

    let path = format!("/v1/runs/{run_id}/steps/{ordinal}/approve");
    let (ra, rb) = tokio::join!(
        env.post_json(&env.a, &path, None, json!({})),
        env.post_json(&env.b, &path, None, json!({}))
    );
    let (sa, ba) = ra?;
    let (sb, bb) = rb?;
    let oks = [sa, sb].iter().filter(|s| **s == 200).count();
    expect!(
        oks == 1,
        "exactly one approval must execute; got A={sa} {ba} / B={sb} {bb}"
    );
    let loser = if sa == 200 { (sb, bb) } else { (sa, ba) };
    let loser_slug = slug(&loser.1);
    expect!(
        loser.0 == 409 && loser_slug == "run-locked"
            || loser.0 == 400 && loser_slug == "invalid-input",
        "loser must draw run-locked (409) or the post-settle invalid-input (400): {} {}",
        loser.0,
        loser.1
    );

    let (status, settled) = env.get(&env.a, &format!("/v1/runs/{run_id}")).await?;
    expect!(
        status == 200 && settled["state"] == "done",
        "run must settle done: {status} {settled}"
    );
    Ok(())
}

pub async fn run_all(env: &ClusterEnv) -> Vec<(&'static str, crate::ScenarioResult)> {
    vec![
        (
            "cluster.shape-visible-across-instances",
            shape_visible_across_instances(env).await,
        ),
        (
            "cluster.provider-update-converges",
            provider_update_converges(env).await,
        ),
        ("cluster.idempotency-shared", idempotency_shared(env).await),
        ("cluster.seq-interleaves", seq_interleaves(env).await),
        (
            "cluster.concurrent-approval-single-executor",
            concurrent_approval_single_executor(env).await,
        ),
    ]
}
