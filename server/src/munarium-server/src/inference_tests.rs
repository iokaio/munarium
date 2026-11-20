// SPDX-License-Identifier: Apache-2.0
//! ClearView IR-0 to IR-4, I1 and I2 at the Server boundary, against an
//! in-process stub of the Munarium Lens endpoint profile (LENS-1).
//!
//! The stub follows the LENS-1 profile: strict request bodies, `{code, stage}`
//! refusals, the OpenAI-style discovery list with `stage`, `backend`,
//! `retrieval_model` and `dimensions`, and the reference backend's algorithm
//! (FNV-1a hashed token counts, L2-normalized; rerank score = share of
//! distinct query tokens in the document). It counts every call, so a test
//! can prove that nothing was sent. No model weights, no network beyond
//! loopback, no paid provider.
use crate::config::AuthMode;
use crate::rest::router;
use crate::state::AppState;
use axum::body::{to_bytes, Body};
use axum::extract::State as AxState;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Router;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tower::ServiceExt;

const MGMT: &str = "ir-mgmt";
const READER: &str = "ir-reader";
const EMBED_MODEL: &str = "lens-reference/embedding-v1-1024d";
const RERANK_MODEL: &str = "lens-reference/rerank-v1";
const DIMS: usize = 1024;

// ---------------------------------------------------------------------------
// the Lens-profile stub
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Healthy,
    /// 503 `inference_unavailable` on every inference route.
    Down,
    /// A rerank reply with a duplicated index (contract violation).
    Malformed,
}

#[derive(Default)]
struct Calls {
    models: AtomicUsize,
    embeddings: AtomicUsize,
    rerank: AtomicUsize,
}

impl Calls {
    fn inference(&self) -> usize {
        self.embeddings.load(AtomicOrdering::SeqCst) + self.rerank.load(AtomicOrdering::SeqCst)
    }
    fn all(&self) -> usize {
        self.inference() + self.models.load(AtomicOrdering::SeqCst)
    }
}

struct Stub {
    calls: Calls,
    mode: StdMutex<Mode>,
}

#[derive(Clone)]
struct StubHandle {
    stub: Arc<Stub>,
    base: String,
    server: Arc<tokio::task::JoinHandle<()>>,
}

impl StubHandle {
    fn set(&self, mode: Mode) {
        *self.stub.mode.lock().unwrap() = mode;
    }
    fn calls(&self) -> &Calls {
        &self.stub.calls
    }
    /// Stop serving: the port refuses connections from now on.
    fn shut_down(&self) {
        self.server.abort();
    }
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(str::to_lowercase)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The LENS-1 reference embedding.
fn reference_embedding(input: &str) -> Vec<f32> {
    let mut counts = vec![0.0f64; DIMS];
    let mut any = false;
    for t in tokens(input) {
        counts[(fnv1a64(t.as_bytes()) % DIMS as u64) as usize] += 1.0;
        any = true;
    }
    if !any {
        counts[(fnv1a64(b"") % DIMS as u64) as usize] = 1.0;
    }
    let norm = counts.iter().map(|v| v * v).sum::<f64>().sqrt();
    counts.iter().map(|v| (v / norm) as f32).collect()
}

/// The LENS-1 reference rerank score.
fn reference_score(query: &str, document: &str) -> f32 {
    let q: BTreeSet<String> = tokens(query).collect();
    if q.is_empty() {
        return 0.0;
    }
    let d: BTreeSet<String> = tokens(document).collect();
    (q.intersection(&d).count() as f64 / q.len() as f64) as f32
}

fn refusal(status: StatusCode, code: &str, stage: &str) -> axum::response::Response {
    (status, axum::Json(json!({"code": code, "stage": stage}))).into_response()
}

/// Strict LENS-1 request parsing: JSON object, exactly the allowed fields.
fn strict(
    headers: &axum::http::HeaderMap,
    body: &[u8],
    allowed: &[&str],
    stage: &str,
) -> Result<serde_json::Map<String, Value>, axum::response::Response> {
    if headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| !v.starts_with("application/json"))
    {
        return Err(refusal(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            stage,
        ));
    }
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(body) else {
        return Err(refusal(StatusCode::BAD_REQUEST, "invalid_request", stage));
    };
    if map.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(refusal(StatusCode::BAD_REQUEST, "invalid_request", stage));
    }
    Ok(map)
}

fn strings(v: Option<&Value>) -> Option<Vec<String>> {
    let arr = v?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    arr.iter().map(|s| s.as_str().map(str::to_owned)).collect()
}

async fn stub_models(AxState(stub): AxState<Arc<Stub>>) -> axum::response::Response {
    stub.calls.models.fetch_add(1, AtomicOrdering::SeqCst);
    axum::Json(json!({"object": "list", "data": [
        {"id": EMBED_MODEL, "object": "model", "owned_by": "munarium-lens", "stage": "embedding",
         "backend": "reference", "retrieval_model": false, "dimensions": DIMS},
        {"id": RERANK_MODEL, "object": "model", "owned_by": "munarium-lens", "stage": "reranking",
         "backend": "reference", "retrieval_model": false}
    ]}))
    .into_response()
}

async fn stub_embeddings(
    AxState(stub): AxState<Arc<Stub>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    stub.calls.embeddings.fetch_add(1, AtomicOrdering::SeqCst);
    let map = match strict(&headers, &body, &["model", "input"], "embedding") {
        Ok(m) => m,
        Err(r) => return r,
    };
    let (Some(model), Some(input)) = (
        map.get("model").and_then(Value::as_str),
        strings(map.get("input")),
    ) else {
        return refusal(StatusCode::BAD_REQUEST, "invalid_request", "embedding");
    };
    if *stub.mode.lock().unwrap() == Mode::Down {
        return refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "inference_unavailable",
            "embedding",
        );
    }
    if model != EMBED_MODEL {
        return refusal(StatusCode::NOT_FOUND, "model_not_found", "embedding");
    }
    let tokens_used: usize = input.iter().map(|i| tokens(i).count()).sum();
    // Rows deliberately reversed: Server must restore order from `index`.
    let data: Vec<Value> = input
        .iter()
        .enumerate()
        .rev()
        .map(|(i, text)| json!({"object": "embedding", "index": i, "embedding": reference_embedding(text)}))
        .collect();
    axum::Json(json!({"object": "list", "model": model, "data": data,
        "usage": {"prompt_tokens": tokens_used, "total_tokens": tokens_used}}))
    .into_response()
}

async fn stub_rerank(
    AxState(stub): AxState<Arc<Stub>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    stub.calls.rerank.fetch_add(1, AtomicOrdering::SeqCst);
    let map = match strict(
        &headers,
        &body,
        &["model", "query", "documents"],
        "reranking",
    ) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let (Some(model), Some(query), Some(documents)) = (
        map.get("model").and_then(Value::as_str),
        map.get("query")
            .and_then(Value::as_str)
            .filter(|q| !q.is_empty()),
        strings(map.get("documents")),
    ) else {
        return refusal(StatusCode::BAD_REQUEST, "invalid_request", "reranking");
    };
    let mode = *stub.mode.lock().unwrap();
    if mode == Mode::Down {
        return refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "inference_unavailable",
            "reranking",
        );
    }
    if model != RERANK_MODEL {
        return refusal(StatusCode::NOT_FOUND, "model_not_found", "reranking");
    }
    let mut scored: Vec<(usize, f32)> = documents
        .iter()
        .enumerate()
        .map(|(i, d)| (i, reference_score(query, d)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut results: Vec<Value> = scored
        .iter()
        .map(|(i, s)| json!({"index": i, "relevance_score": s}))
        .collect();
    if mode == Mode::Malformed {
        results[1]["index"] = results[0]["index"].clone();
    }
    let used: usize = documents
        .iter()
        .map(|d| tokens(query).count() + tokens(d).count())
        .sum();
    axum::Json(json!({"model": model, "results": results,
        "usage": {"prompt_tokens": used, "total_tokens": used}}))
    .into_response()
}

async fn start_stub() -> StubHandle {
    let stub = Arc::new(Stub {
        calls: Calls::default(),
        mode: StdMutex::new(Mode::Healthy),
    });
    let app = axum::Router::new()
        .route("/v1/models", axum::routing::get(stub_models))
        .route("/v1/embeddings", axum::routing::post(stub_embeddings))
        .route("/v1/rerank", axum::routing::post(stub_rerank))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    StubHandle {
        stub,
        // The `/v1` form: Server must not append it twice.
        base: format!("http://{addr}/v1"),
        server: Arc::new(server),
    }
}

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

struct Harness {
    state: Arc<AppState>,
    router: Router,
    tenant: String,
    collection: String,
    hash_index: String,
}

async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("x-munarium-uid", "ir-operator");
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let response = router
        .clone()
        .oneshot(
            req.body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 10_000_000).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn search(h: &Harness, query: &str) -> (u16, Value) {
    let mut req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/search")
        .header("authorization", format!("Bearer {READER}"))
        .header("x-munarium-uid", "ir-reader-user")
        .header("content-type", "application/json");
    req = req.header("accept", "application/json");
    let response = h
        .router
        .clone()
        .oneshot(
            req.body(Body::from(
                json!({"query": query, "filter": {"collections": ["alpha"]}}).to_string(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 10_000_000).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn config_yaml(base: &str, daily: u64, shadow: Option<u64>) -> String {
    let shadow = shadow.map_or(String::new(), |s| format!(", shadowDailyTokens: {s}"));
    format!(
        "apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{name: lens}}\nspec:\n  provider: openai-compatible\n  endpoint: {base}\n  models:\n    embed: [{EMBED_MODEL}]\n    rerank: [{RERANK_MODEL}]\n  inference: {{embedDimensions: {DIMS}, allowTestBackend: true}}\n  budgets: {{dailyTotalTokens: {daily}{shadow}}}\n"
    )
}

/// A tenant with one collection ("alpha", three sources) and its active
/// hash index. Inference is enabled only when `enabled`.
async fn harness(url: String, enabled: bool) -> Harness {
    let tenant = format!("ir-{}", uuid::Uuid::new_v4().simple());
    let state = crate::providers_api::usage_tests::test_state_with_auth(
        Some(url),
        AuthMode::Static(vec![
            (MGMT.into(), tenant.clone(), "mgmt".into()),
            (READER.into(), tenant.clone(), "rw".into()),
        ]),
    )
    .await;
    state.inference.set_enabled(enabled);
    let retrieval = state.retrieval_for(&tenant).unwrap();
    let collection = retrieval
        .ensure_collection("alpha", "article@1", 0, &[], None)
        .await
        .unwrap();
    for (name, text) in [
        (
            "orchid.txt",
            &b"The orchid nebula survey recorded faint fictional light."[..],
        ),
        (
            "survey.txt",
            &b"A survey of fictional nebula maps and their orchid colours."[..],
        ),
        (
            "tables.txt",
            &b"Tables list fictional nebula catalogue numbers."[..],
        ),
    ] {
        let (source, _, _) = retrieval
            .put_source("", "text/plain", name, Some("article@1"), text)
            .await
            .unwrap();
        retrieval
            .bind_source(&collection.id, &source, None)
            .await
            .unwrap();
    }
    let hash_index = retrieval
        .build_collection_index(&collection.id, 400, 1, true)
        .await
        .unwrap()
        .id;
    Harness {
        router: router(state.clone()),
        state,
        tenant,
        collection: collection.id,
        hash_index,
    }
}

async fn apply_config(h: &Harness, yaml: &str) {
    h.state
        .providers
        .apply(&h.state, &h.tenant, yaml)
        .await
        .unwrap();
}

async fn bind(h: &Harness, expected: u64, policy: Option<&str>, shadow: Value) -> (u16, Value) {
    let mut embedding = json!({"model": EMBED_MODEL, "query_instruction": "Query: "});
    let mut rerank = json!({"model": RERANK_MODEL, "max_candidates": 10});
    if let Some(p) = policy {
        embedding["policy"] = json!(p);
        rerank["policy"] = json!(p);
    }
    call(
        &h.router,
        "PUT",
        "/v1/collections/alpha/inference",
        MGMT,
        Some(
            json!({"expected_revision": expected, "provider_config": "lens",
            "embedding": embedding, "rerank": rerank, "shadow": shadow}),
        ),
    )
    .await
}

async fn build_semantic(h: &Harness) -> Value {
    let (status, job) = call(
        &h.router,
        "POST",
        "/v1/index-build-jobs",
        READER,
        Some(json!({"kind": "semantic", "collection_id": h.collection})),
    )
    .await;
    assert_eq!(status, 200, "{job}");
    let jobs = munarium_store_pg::jobs::BuildJobs::new(h.state.pg_pool().unwrap().clone());
    let job = jobs
        .get(&h.tenant, job["job_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    crate::datastore_jobs::run_one_job(&h.state, &job, None, None)
        .await
        .unwrap()
}

async fn scalar_i64(h: &Harness, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(&h.tenant)
        .fetch_one(h.state.pg_pool().unwrap())
        .await
        .unwrap()
}

/// Probe, bind, build and enroll; returns the stub and the binding revision.
async fn enrolled(h: &Harness, policy: Option<&str>) -> (StubHandle, u64) {
    let stub = start_stub().await;
    apply_config(h, &config_yaml(&stub.base, 1_000_000, Some(50_000))).await;
    let (status, probe) = call(
        &h.router,
        "POST",
        "/v1/providers/lens/inference-probe",
        MGMT,
        None,
    )
    .await;
    assert_eq!(status, 200, "{probe}");
    assert_eq!(probe["passed"], true, "{probe}");
    let (status, bound) = bind(h, 0, policy, json!({})).await;
    assert_eq!(status, 200, "{bound}");
    let built = build_semantic(h).await;
    assert_eq!(built["complete"], true, "{built}");
    let (status, enrolled) = call(
        &h.router,
        "POST",
        "/v1/collections/alpha/inference/enroll",
        MGMT,
        Some(json!({"expected_revision": 2})),
    )
    .await;
    assert_eq!(status, 200, "{enrolled}");
    (stub, enrolled["revision"].as_u64().unwrap())
}

fn db_url(test: &str) -> Option<String> {
    let url = std::env::var("MUNARIUM_TEST_DATABASE_URL").ok();
    if url.is_none() {
        eprintln!("ClearView inference test {test} NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
    }
    url
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

/// COMPAT-T4: installation alone sends nothing. Default-off and even
/// enabled-but-unbound searches make zero outbound calls and return the
/// v1.4 response; the probe is refused while disabled; the hash index id is
/// the v1.4 identity.
#[tokio::test]
async fn default_off_makes_zero_outbound_calls() {
    let Some(url) = db_url("default_off") else {
        return;
    };
    let h = harness(url, false).await;
    let stub = start_stub().await;
    apply_config(&h, &config_yaml(&stub.base, 1_000_000, None)).await;

    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 200);
    assert!(body.get("execution").is_none(), "{body}");
    assert_eq!(body["envelope"]["index_version"], h.hash_index.as_str());
    assert_eq!(
        body["envelope"]["provider_fingerprint"],
        "local/local-hash@1/256"
    );

    let (status, refused) = call(
        &h.router,
        "POST",
        "/v1/providers/lens/inference-probe",
        MGMT,
        None,
    )
    .await;
    assert_eq!(status, 409, "{refused}");
    assert!(refused["type"]
        .as_str()
        .unwrap()
        .ends_with("/inference-not-qualified"));

    // A binding exists but the deployment switch is off: still nothing sent.
    let (status, _) = bind(&h, 0, None, json!({})).await;
    assert_eq!(status, 200);
    let (status, _) = search(&h, "orchid nebula").await;
    assert_eq!(status, 200);

    // Enabled, a binding, but nothing enrolled: still the v1.4 path.
    h.state.inference.set_enabled(true);
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 200);
    assert!(body.get("execution").is_none());
    assert_eq!(stub.calls().all(), 0, "no outbound call of any kind");
    assert_eq!(
        scalar_i64(
            &h,
            "SELECT count(*) FROM inference_attempts WHERE tenant_id = $1"
        )
        .await,
        0
    );
}

/// IR-1 to IR-4 end to end: probe, bind, queued semantic build, enroll,
/// serve both stages with receipts, then roll back with the endpoint down.
#[tokio::test]
async fn enrollment_serves_both_stages_and_rollback_needs_no_endpoint() {
    let Some(url) = db_url("lifecycle") else {
        return;
    };
    let h = harness(url, true).await;
    let (stub, revision) = enrolled(&h, None).await;

    let (_, described) = call(
        &h.router,
        "GET",
        "/v1/collections/alpha/inference",
        MGMT,
        None,
    )
    .await;
    let semantic = described["semantic_index_id"].as_str().unwrap().to_string();
    // Index identity separation: a new index id beside the hash index.
    assert_ne!(semantic, h.hash_index);
    assert_eq!(described["active_index_id"], semantic.as_str());
    assert_eq!(described["prior_index_id"], h.hash_index.as_str());
    assert_eq!(described["state"], "enrolled");
    assert_eq!(described["semantic_chunks"], described["semantic_vectors"]);

    let before = stub.calls().inference();
    let (status, body) = search(&h, "orchid nebula survey").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["envelope"]["index_version"], semantic.as_str());
    assert!(body["envelope"]["provider_fingerprint"]
        .as_str()
        .unwrap()
        .starts_with("endpoint/sem-"));
    let stages = body["execution"]["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2, "{body}");
    assert_eq!(stages[0]["stage"], "embedding");
    assert_eq!(stages[0]["applied"], true);
    assert_eq!(stages[0]["policy"], "require");
    assert_eq!(stages[1]["stage"], "reranking");
    assert_eq!(stages[1]["applied"], true);
    assert_eq!(
        stub.calls().inference(),
        before + 2,
        "one embed, one rerank"
    );
    // Reranked by the reference score: the hit holding every query token first.
    assert_eq!(body["hits"][0]["source_path"], "orchid.txt");
    // Citation identities survive the reorder.
    let chunk_ids: Vec<&Value> = body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| &h["chunk_id"])
        .collect();
    assert_eq!(
        body["envelope"]["chunk_ids"]
            .as_array()
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        chunk_ids
    );

    // I2 attribution: serving attempts name the reader; build attempts the
    // job's initiating caller, with the job id.
    let rows = sqlx::query(
        "SELECT purpose, caller, job_id, collection_ids, usage_source FROM inference_attempts
          WHERE tenant_id = $1 ORDER BY started_at",
    )
    .bind(&h.tenant)
    .fetch_all(h.state.pg_pool().unwrap())
    .await
    .unwrap();
    use sqlx::Row as _;
    let serving: Vec<_> = rows
        .iter()
        .filter(|r| r.get::<String, _>("purpose") == "serving")
        .collect();
    assert_eq!(serving.len(), 2);
    assert!(serving
        .iter()
        .all(|r| r.get::<String, _>("caller") == "ir-reader-user"));
    assert!(serving
        .iter()
        .all(|r| r.get::<Vec<String>, _>("collection_ids") == vec![h.collection.clone()]));
    let build: Vec<_> = rows
        .iter()
        .filter(|r| r.get::<String, _>("purpose") == "build")
        .collect();
    assert!(!build.is_empty());
    assert!(build
        .iter()
        .all(|r| r.get::<String, _>("caller") == "ir-operator"
            && r.get::<Option<String>, _>("job_id").is_some()
            && r.get::<Option<String>, _>("usage_source").as_deref() == Some("provider_reported")));

    // Rollback with the endpoint gone: no call, the prior index serves.
    stub.shut_down();
    let calls = stub.calls().all();
    let (status, rolled) = call(
        &h.router,
        "POST",
        "/v1/collections/alpha/inference/rollback",
        MGMT,
        Some(json!({"expected_revision": revision})),
    )
    .await;
    assert_eq!(status, 200, "{rolled}");
    assert_eq!(rolled["state"], "rolled_back");
    assert_eq!(rolled["active_index_id"], h.hash_index.as_str());
    let (status, body) = search(&h, "orchid nebula survey").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["envelope"]["index_version"], h.hash_index.as_str());
    assert!(
        body.get("execution").is_none(),
        "a rolled-back read is the v1.4 read"
    );
    assert_eq!(stub.calls().all(), calls);

    // A stale revision is a head conflict.
    let (status, _) = call(
        &h.router,
        "POST",
        "/v1/collections/alpha/inference/rollback",
        MGMT,
        Some(json!({"expected_revision": revision})),
    )
    .await;
    assert_eq!(status, 409);
}

/// I1-T1: with the policy omitted, an outage is a typed refusal and never a
/// fallback; an explicit `prefer` serves the disclosed fallbacks; an invalid
/// rerank reply fails the stage.
#[tokio::test]
async fn outage_refuses_by_default_and_prefer_discloses_the_fallback() {
    let Some(url) = db_url("outage") else { return };
    let h = harness(url, true).await;
    let (stub, revision) = enrolled(&h, None).await;

    stub.set(Mode::Down);
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 503, "{body}");
    assert!(body["type"]
        .as_str()
        .unwrap()
        .ends_with("/inference-stage-unavailable"));
    assert!(body["detail"].as_str().unwrap().contains("stage=embedding"));
    assert_eq!(body["retry_action"], "retry_later");
    assert!(body.get("hits").is_none(), "no partial or fallback result");

    stub.set(Mode::Malformed);
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 503, "{body}");
    assert!(body["detail"].as_str().unwrap().contains("stage=reranking"));
    assert!(body["detail"]
        .as_str()
        .unwrap()
        .contains("reason=duplicate_index"));

    let refused = scalar_i64(
        &h,
        "SELECT count(*) FROM inference_stage_events WHERE tenant_id = $1 AND outcome = 'refused'",
    )
    .await;
    assert_eq!(refused, 2);

    // Explicit operator selection of `prefer`.
    let (status, rebound) = bind(&h, revision, Some("prefer"), json!({})).await;
    assert_eq!(status, 200, "{rebound}");
    stub.set(Mode::Down);
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 200, "{body}");
    let stages = body["execution"]["stages"].as_array().unwrap();
    assert_eq!(stages[0]["applied"], false);
    assert_eq!(stages[0]["fallback"], "lexical_only");
    assert_eq!(stages[0]["state"], "unavailable");
    assert_eq!(stages[0]["policy"], "prefer");
    assert_eq!(stages[0]["policy_revision"], revision + 1);
    assert_eq!(stages[1]["applied"], false);
    assert_eq!(stages[1]["fallback"], "fused_order");
    let legs: Vec<&str> = body["execution"]["legs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["leg"].as_str().unwrap())
        .collect();
    assert_eq!(legs, vec!["lexical"], "the vector leg is absent, not faked");
    assert!(!body["hits"].as_array().unwrap().is_empty());
    let fallbacks = scalar_i64(
        &h,
        "SELECT count(*) FROM inference_stage_events WHERE tenant_id = $1 AND outcome = 'fallback'",
    )
    .await;
    assert_eq!(fallbacks, 2);
}

/// I2: a denied reservation dispatches nothing; probe drift expires the
/// stage; shadow work reserves only from its own allocation and is metered
/// separately.
#[tokio::test]
async fn budgets_gate_dispatch_and_shadow_is_metered_separately() {
    let Some(url) = db_url("budgets") else { return };
    let h = harness(url, true).await;
    let stub = start_stub().await;
    // Room for the probe and the build, then a cap the serving share cannot
    // meet: shadow 3,000 of a 12,000 cap.
    apply_config(&h, &config_yaml(&stub.base, 12_000, Some(3_000))).await;
    let (_, probe) = call(
        &h.router,
        "POST",
        "/v1/providers/lens/inference-probe",
        MGMT,
        None,
    )
    .await;
    assert_eq!(probe["passed"], true, "{probe}");

    // Bound, not enrolled, with shadow reranking: serving is the v1.4 path,
    // the shadow comparison is metered as shadow.
    let (status, bound) = bind(&h, 0, None, json!({"rerank": true})).await;
    assert_eq!(status, 200, "{bound}");
    let (status, body) = search(&h, "orchid nebula survey").await;
    assert_eq!(status, 200);
    assert!(
        body.get("execution").is_none(),
        "shadow never changes the response"
    );
    let mut compared = 0;
    for _ in 0..50 {
        compared = scalar_i64(
            &h,
            "SELECT count(*) FROM inference_shadow_comparisons WHERE tenant_id = $1 AND outcome = 'compared'",
        )
        .await;
        if compared > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(compared, 1);
    let shadow_reserved = scalar_i64(
        &h,
        "SELECT count(*) FROM token_budget_reservations WHERE tenant_id = $1 AND tier = 'shadow'",
    )
    .await;
    assert_eq!(
        shadow_reserved, 1,
        "shadow reserves from its own allocation"
    );
    assert_eq!(
        scalar_i64(
            &h,
            "SELECT count(*) FROM inference_attempts WHERE tenant_id = $1 AND purpose = 'shadow'"
        )
        .await,
        1
    );

    // Exhaust the serving share; the next serving call is denied before
    // dispatch: zero outbound calls, a typed budget refusal.
    let built = build_semantic(&h).await;
    assert_eq!(built["complete"], true, "{built}");
    let (status, enrolled) = call(
        &h.router,
        "POST",
        "/v1/collections/alpha/inference/enroll",
        MGMT,
        Some(json!({"expected_revision": 2})),
    )
    .await;
    assert_eq!(status, 200, "{enrolled}");
    sqlx::query(
        "INSERT INTO token_budget_reservations (id, tenant_id, config_name, tier, day, units, state)
         VALUES ($1 || '-fill', $1, 'lens', 'all', (now() AT TIME ZONE 'UTC')::date, 9000, 'settled')",
    )
    .bind(&h.tenant)
    .execute(h.state.pg_pool().unwrap())
    .await
    .unwrap();
    let before = stub.calls().all();
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 503, "{body}");
    assert!(body["detail"]
        .as_str()
        .unwrap()
        .contains("state=budget_exhausted"));
    assert_eq!(stub.calls().all(), before, "denied admission sends nothing");

    // Drift: a changed configuration text expires the probe.
    apply_config(&h, &config_yaml(&stub.base, 1_000_000, Some(3_000))).await;
    let (_, described) = call(
        &h.router,
        "GET",
        "/v1/collections/alpha/inference",
        MGMT,
        None,
    )
    .await;
    assert_eq!(
        described["stages"][0]["state"], "probe_failed",
        "{described}"
    );
    assert_eq!(described["stages"][0]["reason"], "no_current_probe");
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 503);
    assert!(body["detail"]
        .as_str()
        .unwrap()
        .contains("state=probe_failed"));
    assert_eq!(stub.calls().all(), before);
}
