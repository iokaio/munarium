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
    /// The exact `input` of the latest embedding request and `query` of the
    /// latest rerank request, as received on the wire.
    last_input: StdMutex<Vec<String>>,
    last_query: StdMutex<Option<String>>,
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
    fn last_input(&self) -> Vec<String> {
        self.stub.last_input.lock().unwrap().clone()
    }
    fn last_query(&self) -> Option<String> {
        self.stub.last_query.lock().unwrap().clone()
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
    *stub.last_input.lock().unwrap() = input.clone();
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
    *stub.last_query.lock().unwrap() = Some(query.to_string());
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
        last_input: StdMutex::new(Vec::new()),
        last_query: StdMutex::new(None),
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
            &b"A study of fictional nebula maps and their orchid colours."[..],
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

// ---------------------------------------------------------------------------
// SC2: shared and dedicated placement
// ---------------------------------------------------------------------------

/// A configuration like [`config_yaml`] with a declared placement.
fn placed_yaml(base: &str, placement: &str) -> String {
    config_yaml(base, 1_000_000, Some(50_000)).replace(
        "allowTestBackend: true}",
        &format!("allowTestBackend: true, placement: {placement}}}"),
    )
}

async fn bind_placed(h: &Harness, expected: u64, placement: Option<Value>) -> (u16, Value) {
    let mut body = json!({"expected_revision": expected, "provider_config": "lens",
        "embedding": {"model": EMBED_MODEL, "query_instruction": "Query: "},
        "rerank": {"model": RERANK_MODEL, "max_candidates": 10}});
    if let Some(p) = placement {
        body["placement"] = p;
    }
    call(
        &h.router,
        "PUT",
        "/v1/collections/alpha/inference",
        MGMT,
        Some(body),
    )
    .await
}

async fn probe(h: &Harness) -> (u16, Value) {
    call(
        &h.router,
        "POST",
        "/v1/providers/lens/inference-probe",
        MGMT,
        None,
    )
    .await
}

async fn describe_stage_reasons(h: &Harness) -> Vec<Value> {
    let (_, described) = call(
        &h.router,
        "GET",
        "/v1/collections/alpha/inference",
        MGMT,
        None,
    )
    .await;
    described["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| json!([s["state"], s["reason"]]))
        .collect()
}

fn refused_for(body: &Value, reason: &str) -> bool {
    body["detail"].as_str().is_some_and(|d| d.contains(reason))
}

/// SC2 rule, unit level: coverage, undeclared, dedicated and processing.
#[test]
fn sc2_placement_check_rules() {
    use munarium_api_types::inference::InferencePlacementRequirement as Req;
    use munarium_providers::openai_compatible::{Placement, PlacementMode};
    let rules = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let shared = Placement {
        mode: PlacementMode::Shared,
        tenants: vec!["t-a".into(), "t-b".into()],
        processing: rules(&[("location", "eu"), ("retention", "none")]),
    };
    let dedicated = Placement {
        mode: PlacementMode::Dedicated,
        tenants: vec![],
        processing: rules(&[("location", "eu")]),
    };
    let req = |isolation: &str, pairs: &[(&str, &str)]| Req {
        isolation: isolation.into(),
        processing: rules(pairs),
    };
    let check = crate::inference::placement_check;
    // Phase 6: nothing declared, nothing required.
    assert_eq!(check("t-a", None, None), Ok(()));
    // Coverage applies with or without a requirement.
    assert_eq!(check("t-a", Some(&shared), None), Ok(()));
    assert_eq!(
        check("t-c", Some(&shared), None),
        Err("placement_tenant_not_covered")
    );
    assert_eq!(
        check("t-c", Some(&shared), Some(&req("shared", &[]))),
        Err("placement_tenant_not_covered")
    );
    assert_eq!(check("t-c", Some(&dedicated), None), Ok(()));
    assert_eq!(
        check("t-a", None, Some(&req("shared", &[]))),
        Err("placement_undeclared")
    );
    assert_eq!(
        check("t-a", Some(&shared), Some(&req("dedicated", &[]))),
        Err("placement_dedicated_required")
    );
    assert_eq!(
        check(
            "t-a",
            Some(&dedicated),
            Some(&req("dedicated", &[("location", "eu")]))
        ),
        Ok(())
    );
    assert_eq!(
        check("t-a", Some(&dedicated), Some(&req("shared", &[]))),
        Ok(()),
        "a dedicated pool satisfies a collection that permits sharing"
    );
    assert_eq!(
        check(
            "t-a",
            Some(&shared),
            Some(&req("shared", &[("location", "eu")]))
        ),
        Ok(())
    );
    for incompatible in [
        &[("location", "us")][..],
        &[("retention", "30d")][..],
        &[("isolation", "process")][..],
    ] {
        assert_eq!(
            check("t-a", Some(&shared), Some(&req("shared", incompatible))),
            Err("placement_processing_incompatible"),
            "{incompatible:?}"
        );
    }
}

/// SC2-T2 (local): a shared pool that does not explicitly list the tenant
/// receives nothing on probe, bind, serving or build paths; listing the
/// tenant admits it; withdrawing coverage stops it again before any send.
#[tokio::test]
async fn sc2_shared_placement_requires_explicit_tenant_coverage() {
    let Some(url) = db_url("sc2_coverage") else {
        return;
    };
    let h = harness(url, true).await;
    let stub = start_stub().await;
    let uncovered = "{mode: shared, tenants: [ir-other-tenant], processing: {location: eu}}";
    apply_config(&h, &placed_yaml(&stub.base, uncovered)).await;
    let (status, body) = probe(&h).await;
    assert_eq!(status, 409, "{body}");
    assert!(refused_for(&body, "placement_tenant_not_covered"), "{body}");
    let (status, body) = bind_placed(&h, 0, None).await;
    assert_eq!(status, 409, "{body}");
    assert!(refused_for(&body, "placement_tenant_not_covered"), "{body}");
    assert_eq!(stub.calls().all(), 0, "nothing reaches an uncovered pool");

    let covered = format!(
        "{{mode: shared, tenants: [ir-other-tenant, {}], processing: {{location: eu}}}}",
        h.tenant
    );
    apply_config(&h, &placed_yaml(&stub.base, &covered)).await;
    let (status, body) = probe(&h).await;
    assert_eq!((status, &body["passed"]), (200, &json!(true)), "{body}");
    let (status, bound) = bind_placed(&h, 0, None).await;
    assert_eq!(status, 200, "{bound}");
    assert_eq!(
        bound["placement"],
        json!({"isolation": "shared", "processing": {"location": "eu"}}),
        "a first binding records the declared placement"
    );
    assert_eq!(build_semantic(&h).await["complete"], true);
    let (status, body) = call(
        &h.router,
        "POST",
        "/v1/collections/alpha/inference/enroll",
        MGMT,
        Some(json!({"expected_revision": 2})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 200, "{body}");

    // Coverage withdrawn: every stage stops before any client is used.
    apply_config(&h, &placed_yaml(&stub.base, uncovered)).await;
    assert_eq!(
        describe_stage_reasons(&h).await,
        vec![
            json!(["unconfigured", "placement_tenant_not_covered"]),
            json!(["unconfigured", "placement_tenant_not_covered"])
        ]
    );
    let before = stub.calls().all();
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 503, "{body}");
    assert!(refused_for(&body, "placement_tenant_not_covered"), "{body}");
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
    assert!(
        crate::datastore_jobs::run_one_job(&h.state, &job, None, None)
            .await
            .is_err()
    );
    assert_eq!(stub.calls().all(), before, "zero sends after coverage ends");
}

/// SC2-T3 (local): the dedicated pool is lost and the configuration is
/// repointed at a shared pool that covers the tenant and passes its probe.
/// The binding recorded `dedicated`, so the stages refuse and the shared
/// pool receives no content, under `require` and under `prefer` alike; a
/// rebind that omits the requirement keeps it; only an explicit `shared`
/// requirement admits the shared pool.
#[tokio::test]
async fn sc2_lost_dedicated_capacity_never_falls_over_to_a_shared_pool() {
    let Some(url) = db_url("sc2_dedicated") else {
        return;
    };
    let h = harness(url, true).await;
    let dedicated = start_stub().await;
    let shared = start_stub().await;
    apply_config(&h, &placed_yaml(&dedicated.base, "{mode: dedicated}")).await;
    assert_eq!(probe(&h).await.0, 200);
    let (status, bound) = bind_placed(&h, 0, None).await;
    assert_eq!(status, 200, "{bound}");
    assert_eq!(bound["placement"], json!({"isolation": "dedicated"}));
    assert_eq!(build_semantic(&h).await["complete"], true);
    let (status, body) = call(
        &h.router,
        "POST",
        "/v1/collections/alpha/inference/enroll",
        MGMT,
        Some(json!({"expected_revision": 2})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let revision = body["revision"].as_u64().unwrap();
    assert_eq!(search(&h, "orchid nebula").await.0, 200);

    // Dedicated capacity is lost; the configuration now names a shared pool.
    dedicated.shut_down();
    let shared_placement = format!(
        "{{mode: shared, tenants: [{}], processing: {{location: eu}}}}",
        h.tenant
    );
    apply_config(&h, &placed_yaml(&shared.base, &shared_placement)).await;
    let (status, body) = probe(&h).await;
    assert_eq!(status, 200, "{body}");
    let after_probe = shared.calls().inference();
    assert_eq!(
        describe_stage_reasons(&h).await,
        vec![
            json!(["unconfigured", "placement_dedicated_required"]),
            json!(["unconfigured", "placement_dedicated_required"])
        ]
    );
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 503, "{body}");
    assert!(refused_for(&body, "placement_dedicated_required"), "{body}");

    // A rebind that omits the requirement keeps `dedicated` and is refused.
    let (status, body) = bind_placed(&h, revision, None).await;
    assert_eq!(status, 409, "{body}");
    assert!(refused_for(&body, "placement_dedicated_required"), "{body}");

    // Under `prefer` the collection's own fallback applies, still with no
    // dispatch to the shared pool. (Set directly: the bind itself is
    // refused while the requirement cannot be met.)
    sqlx::query(
        "UPDATE collection_inference_bindings
            SET embedding = embedding || '{\"policy\": \"prefer\"}'::jsonb,
                rerank = rerank || '{\"policy\": \"prefer\"}'::jsonb
          WHERE tenant_id = $1",
    )
    .bind(&h.tenant)
    .execute(h.state.pg_pool().unwrap())
    .await
    .unwrap();
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 200, "{body}");
    let stages = body["execution"]["stages"].as_array().unwrap();
    assert_eq!(stages[0]["fallback"], "lexical_only", "{body}");
    assert_eq!(stages[0]["reason"], "placement_dedicated_required");
    assert_eq!(stages[1]["fallback"], "fused_order");
    assert_eq!(
        shared.calls().inference(),
        after_probe,
        "no content reached the shared pool"
    );

    // Only an explicit operator decision admits the shared pool.
    let (status, body) = bind_placed(&h, revision, Some(json!({"isolation": "shared"}))).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["placement"], json!({"isolation": "shared"}));
    for stage in describe_stage_reasons(&h).await {
        assert!(
            !stage[1].as_str().unwrap_or("").starts_with("placement_"),
            "{stage}"
        );
    }
}

/// SC2: processing rules must match; a requirement needs a declaration; an
/// existing Phase 6 configuration and binding are unchanged.
#[tokio::test]
async fn sc2_processing_rules_must_be_compatible_and_phase6_is_unchanged() {
    let Some(url) = db_url("sc2_processing") else {
        return;
    };
    let h = harness(url, true).await;
    let stub = start_stub().await;
    apply_config(&h, &config_yaml(&stub.base, 1_000_000, Some(50_000))).await;
    let (status, bound) = bind_placed(&h, 0, None).await;
    assert_eq!(status, 200, "{bound}");
    assert!(bound.get("placement").is_none(), "Phase 6 shape: {bound}");
    let history: Value = sqlx::query_scalar(
        "SELECT binding FROM collection_inference_binding_history WHERE tenant_id = $1",
    )
    .bind(&h.tenant)
    .fetch_one(h.state.pg_pool().unwrap())
    .await
    .unwrap();
    assert!(history.get("placement").is_none(), "{history}");

    let (status, body) = bind_placed(&h, 1, Some(json!({"isolation": "shared"}))).await;
    assert_eq!(status, 409, "{body}");
    assert!(refused_for(&body, "placement_undeclared"), "{body}");
    let (status, body) = bind_placed(&h, 1, Some(json!({"isolation": "pooled"}))).await;
    assert_eq!(status, 400, "{body}");

    let declared = format!(
        "{{mode: shared, tenants: [{}], processing: {{location: eu, retention: 30d}}}}",
        h.tenant
    );
    apply_config(&h, &placed_yaml(&stub.base, &declared)).await;
    // The existing Phase 6 binding is not pinned by a later declaration.
    let (status, body) = bind_placed(&h, 1, None).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.get("placement").is_none(), "{body}");
    for (requirement, reason) in [
        (
            json!({"isolation": "shared", "processing": {"location": "eu", "retention": "none"}}),
            "placement_processing_incompatible",
        ),
        (
            json!({"isolation": "shared", "processing": {"jurisdiction": "eu"}}),
            "placement_processing_incompatible",
        ),
        (
            json!({"isolation": "dedicated"}),
            "placement_dedicated_required",
        ),
    ] {
        let (status, body) = bind_placed(&h, 2, Some(requirement.clone())).await;
        assert_eq!(status, 409, "{requirement}: {body}");
        assert!(refused_for(&body, reason), "{requirement}: {body}");
    }
    let (status, body) = bind_placed(
        &h,
        2,
        Some(json!({"isolation": "shared", "processing": {"location": "eu"}})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["placement"],
        json!({"isolation": "shared", "processing": {"location": "eu"}})
    );

    // A later incompatible declaration stops the stages before any send.
    let moved = format!(
        "{{mode: shared, tenants: [{}], processing: {{location: us, retention: 30d}}}}",
        h.tenant
    );
    apply_config(&h, &placed_yaml(&stub.base, &moved)).await;
    assert_eq!(
        describe_stage_reasons(&h).await,
        vec![
            json!(["unconfigured", "placement_processing_incompatible"]),
            json!(["unconfigured", "placement_processing_incompatible"])
        ]
    );
    assert_eq!(stub.calls().inference(), 0);
}

// ---------------------------------------------------------------------------
// Qwen3 instructions (hub ADR 0029)
// ---------------------------------------------------------------------------

/// The Qwen3 model cards' example retrieval task.
const QWEN3_TASK: &str =
    "Given a web search query, retrieve relevant passages that answer the query";

async fn bind_stages(h: &Harness, expected: u64, embedding: Value, rerank: Value) -> (u16, Value) {
    call(
        &h.router,
        "PUT",
        "/v1/collections/alpha/inference",
        MGMT,
        Some(
            json!({"expected_revision": expected, "provider_config": "lens",
            "embedding": embedding, "rerank": rerank}),
        ),
    )
    .await
}

fn rerank_receipt(body: &Value) -> &Value {
    let stages = body["execution"]["stages"].as_array().unwrap();
    assert_eq!(stages[1]["stage"], "reranking", "{body}");
    &stages[1]
}

/// The rerank instruction: unset sends the query and records the identity
/// exactly as before; set sends the Qwen3-Reranker card's query part and
/// records an identity that changes with the instruction.
#[tokio::test]
async fn rerank_instruction_is_the_qwen3_card_query_and_part_of_the_identity() {
    let Some(url) = db_url("rerank_instruction") else {
        return;
    };
    let h = harness(url, true).await;
    let (stub, revision) = enrolled(&h, None).await;
    let embedding = json!({"model": EMBED_MODEL, "query_instruction": "Query: "});

    // Unset: the wire query is the read's query, byte for byte, and the
    // identity is the model name, as in Phase 6.
    let (status, body) = search(&h, "orchid nebula survey").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(stub.last_query().as_deref(), Some("orchid nebula survey"));
    assert_eq!(rerank_receipt(&body)["identity"], RERANK_MODEL);
    assert_eq!(rerank_receipt(&body)["applied"], true);
    let (_, described) = call(
        &h.router,
        "GET",
        "/v1/collections/alpha/inference",
        MGMT,
        None,
    )
    .await;
    assert!(
        described["rerank"].get("instruction").is_none(),
        "{described}"
    );
    assert_eq!(described["stages"][1]["identity"], RERANK_MODEL);
    let semantic = described["semantic_index_id"].clone();
    let stored: Value =
        sqlx::query_scalar("SELECT rerank FROM collection_inference_bindings WHERE tenant_id = $1")
            .bind(&h.tenant)
            .fetch_one(h.state.pg_pool().unwrap())
            .await
            .unwrap();
    assert!(stored.get("instruction").is_none(), "{stored}");

    // Malformed instructions are refused before anything is stored.
    let long = "x".repeat(1_025);
    for bad in ["", "   ", "two\nlines", "tab\there", long.as_str()] {
        let (status, body) = bind_stages(
            &h,
            revision,
            embedding.clone(),
            json!({"model": RERANK_MODEL, "max_candidates": 10, "instruction": bad}),
        )
        .await;
        assert_eq!(status, 400, "{bad:?}: {body}");
    }

    // Set: a rebind; the wire query is exactly the card's query part.
    let (status, rebound) = bind_stages(
        &h,
        revision,
        embedding.clone(),
        json!({"model": RERANK_MODEL, "max_candidates": 10, "instruction": QWEN3_TASK}),
    )
    .await;
    assert_eq!(status, 200, "{rebound}");
    assert_eq!(rebound["rerank"]["instruction"], QWEN3_TASK);
    assert_eq!(
        rebound["semantic_index_id"], semantic,
        "the semantic space is unchanged"
    );
    let (status, body) = search(&h, "orchid nebula survey").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        stub.last_query().unwrap().as_bytes(),
        b"<Instruct>: Given a web search query, retrieve relevant passages that answer the query\n<Query>: orchid nebula survey"
    );
    let first = rerank_receipt(&body)["identity"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        first.starts_with(&format!("{RERANK_MODEL}#instruct-")),
        "{first}"
    );
    assert!(
        !first.contains("web search"),
        "the identity carries no instruction text"
    );
    assert_eq!(rerank_receipt(&body)["policy_revision"], revision + 1);
    assert_eq!(rebound["stages"][1]["identity"], first.as_str());
    assert_eq!(body["hits"][0]["source_path"], "orchid.txt");

    // A changed instruction is a changed identity.
    let (status, changed) = bind_stages(
        &h,
        revision + 1,
        embedding.clone(),
        json!({"model": RERANK_MODEL, "max_candidates": 10,
               "instruction": "Find the passage that answers the question"}),
    )
    .await;
    assert_eq!(status, 200, "{changed}");
    let (_, body) = search(&h, "orchid nebula survey").await;
    let second = rerank_receipt(&body)["identity"].as_str().unwrap();
    assert!(second.starts_with(&format!("{RERANK_MODEL}#instruct-")));
    assert_ne!(second, first);
    assert_eq!(
        stub.last_query().as_deref(),
        Some(
            "<Instruct>: Find the passage that answers the question\n<Query>: orchid nebula survey"
        )
    );

    // Removing it restores the unchanged query and the model identity.
    let (status, _) = bind_stages(
        &h,
        revision + 2,
        embedding,
        json!({"model": RERANK_MODEL, "max_candidates": 10}),
    )
    .await;
    assert_eq!(status, 200);
    let (_, body) = search(&h, "orchid nebula survey").await;
    assert_eq!(rerank_receipt(&body)["identity"], RERANK_MODEL);
    assert_eq!(stub.last_query().as_deref(), Some("orchid nebula survey"));
}

/// The embedding `query_instruction` is a full literal prefix. The Qwen3
/// recommendation, `Instruct: {task}\nQuery:` with no trailing space, puts
/// the card's exact query on the wire; passages carry no prefix.
#[tokio::test]
async fn qwen3_embedding_query_instruction_is_the_card_format() {
    let Some(url) = db_url("qwen3_embedding_prefix") else {
        return;
    };
    let h = harness(url, true).await;
    let stub = start_stub().await;
    apply_config(&h, &config_yaml(&stub.base, 1_000_000, None)).await;
    let (_, probe) = call(
        &h.router,
        "POST",
        "/v1/providers/lens/inference-probe",
        MGMT,
        None,
    )
    .await;
    assert_eq!(probe["passed"], true, "{probe}");
    let instruction = format!("Instruct: {QWEN3_TASK}\nQuery:");
    let (status, bound) = bind_stages(
        &h,
        0,
        json!({"model": EMBED_MODEL, "query_instruction": instruction}),
        json!({"model": RERANK_MODEL, "max_candidates": 10, "instruction": QWEN3_TASK}),
    )
    .await;
    assert_eq!(status, 200, "{bound}");
    let built = build_semantic(&h).await;
    assert_eq!(built["complete"], true, "{built}");
    assert!(
        stub.last_input()
            .iter()
            .all(|p| !p.starts_with("Instruct:")),
        "passages are sent without a prefix"
    );
    let (status, enrolled) = call(
        &h.router,
        "POST",
        "/v1/collections/alpha/inference/enroll",
        MGMT,
        Some(json!({"expected_revision": 2})),
    )
    .await;
    assert_eq!(status, 200, "{enrolled}");
    let (status, body) = search(&h, "orchid nebula survey").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        stub.last_input()[0].as_bytes(),
        b"Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery:orchid nebula survey"
    );
    assert_eq!(
        stub.last_query().unwrap().as_bytes(),
        b"<Instruct>: Given a web search query, retrieve relevant passages that answer the query\n<Query>: orchid nebula survey"
    );
}

// ---------------------------------------------------------------------------
// the governed path is the only path
// ---------------------------------------------------------------------------

/// `POST /v1/providers` with a YAML body, as the tenant's rw caller.
async fn apply_rest(h: &Harness, yaml: &str) -> (u16, Value) {
    let response = h
        .router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/providers")
                .header("authorization", format!("Bearer {READER}"))
                .header("x-munarium-uid", "ir-operator")
                .header("content-type", "text/yaml")
                .body(Body::from(yaml.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The generic provider routes (`embed`, `complete`, `health`) refuse an
/// `openai-compatible` configuration with a typed problem and send nothing,
/// whatever the deployment switch, placement, probe or budget would say:
/// tenant text never reaches an inference endpoint unmetered.
#[tokio::test]
async fn generic_provider_routes_never_reach_an_inference_endpoint() {
    let Some(url) = db_url("generic_routes") else {
        return;
    };
    let h = harness(url, false).await;
    let stub = start_stub().await;
    // A shared pool that does not list this tenant.
    let uncovered = "{mode: shared, tenants: [ir-other-tenant], processing: {location: eu}}";
    apply_config(&h, &placed_yaml(&stub.base, uncovered)).await;
    for enabled in [false, true] {
        h.state.inference.set_enabled(enabled);
        for (method, uri, token, body) in [
            (
                "POST",
                "/v1/providers/lens/embed",
                READER,
                Some(json!({"inputs": ["confidential tenant text"]})),
            ),
            (
                "POST",
                "/v1/providers/lens/embed",
                READER,
                Some(json!({"inputs": ["confidential tenant text"], "model": EMBED_MODEL})),
            ),
            (
                "POST",
                "/v1/providers/lens/complete",
                READER,
                Some(json!({"prompt": "confidential tenant text", "model": EMBED_MODEL})),
            ),
            ("GET", "/v1/providers/lens/health", READER, None),
            ("GET", "/v1/providers/lens/health", MGMT, None),
        ] {
            let (status, body) = call(&h.router, method, uri, token, body).await;
            assert_eq!(status, 409, "{enabled} {method} {uri}: {body}");
            assert!(
                body["type"]
                    .as_str()
                    .is_some_and(|t| t.ends_with("/inference-not-qualified")),
                "{uri}: {body}"
            );
            assert!(refused_for(&body, "governed inference routes"), "{body}");
        }
    }
    assert_eq!(stub.calls().all(), 0, "no outbound call of any kind");
    for table in ["token_budget_reservations", "inference_attempts"] {
        assert_eq!(
            scalar_i64(
                &h,
                &format!("SELECT count(*) FROM {table} WHERE tenant_id = $1")
            )
            .await,
            0,
            "{table}: nothing was metered because nothing was sent"
        );
    }
}

/// I2: the daily cap is required of an inference configuration at apply,
/// and a stored configuration without one (written before the rule) is
/// neither probed nor served: zero outbound calls.
#[tokio::test]
async fn an_inference_configuration_without_a_daily_cap_sends_nothing() {
    let Some(url) = db_url("daily_cap_required") else {
        return;
    };
    let h = harness(url, true).await;
    let (stub, _) = enrolled(&h, None).await;
    let capped = config_yaml(&stub.base, 1_000_000, Some(50_000));
    let uncapped = capped.replace(
        "  budgets: {dailyTotalTokens: 1000000, shadowDailyTokens: 50000}\n",
        "",
    );
    assert_ne!(capped, uncapped);
    // A stored uncapped text (as if applied before the rule) with a passing
    // probe recorded after it: still nothing is probed, built or served.
    let pool = h.state.pg_pool().unwrap();
    sqlx::query(
        "UPDATE provider_configs SET yaml = $3, text_changed_at = now()
          WHERE tenant_id = $1 AND name = $2",
    )
    .bind(&h.tenant)
    .bind("lens")
    .bind(&uncapped)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO inference_probes
             (tenant_id, probe_id, provider_config, config_hash, endpoint_fingerprint,
              passed, checks, probed_by, probed_at)
         VALUES ($1, 'forged-probe', 'lens', $2, 'fp', true, '[]'::jsonb, 'test',
                 now() + interval '1 second')",
    )
    .bind(&h.tenant)
    .bind(crate::state::request_hash(uncapped.as_bytes()))
    .execute(pool)
    .await
    .unwrap();
    let before = stub.calls().all();
    let (status, body) = probe(&h).await;
    assert_eq!(status, 409, "{body}");
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 503, "{body}");
    assert!(refused_for(&body, "state=unconfigured"), "{body}");
    assert_eq!(
        stub.calls().all(),
        before,
        "an uncapped configuration sends nothing"
    );
    // And such a text can no longer be applied.
    let (status, body) = apply_rest(&h, &uncapped).await;
    assert_eq!(status, 400, "{body}");
    assert!(refused_for(&body, "dailyTotalTokens"), "{body}");
}

/// IR-1: a probe is valid only after the configuration text's latest change.
/// Re-applying identical text keeps it; a change and a revert to the earlier
/// text expire it, and the stages refuse with zero calls until a new probe.
#[tokio::test]
async fn reverting_a_configuration_needs_a_fresh_probe() {
    let Some(url) = db_url("probe_revert") else {
        return;
    };
    let h = harness(url, true).await;
    let (stub, _) = enrolled(&h, None).await;
    let a = config_yaml(&stub.base, 1_000_000, Some(50_000));
    let b = config_yaml(&stub.base, 2_000_000, Some(50_000));
    apply_config(&h, &a).await;
    assert_eq!(
        describe_stage_reasons(&h).await,
        vec![json!(["ready", null]), json!(["ready", null])],
        "identical text is not drift"
    );
    apply_config(&h, &b).await;
    apply_config(&h, &a).await;
    assert_eq!(
        describe_stage_reasons(&h).await,
        vec![
            json!(["probe_failed", "no_current_probe"]),
            json!(["probe_failed", "no_current_probe"])
        ],
        "a revert does not revive the earlier probe"
    );
    let before = stub.calls().all();
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 503, "{body}");
    assert!(refused_for(&body, "state=probe_failed"), "{body}");
    assert_eq!(stub.calls().all(), before);
    let (status, body) = probe(&h).await;
    assert_eq!((status, &body["passed"]), (200, &json!(true)), "{body}");
    let (status, body) = search(&h, "orchid nebula").await;
    assert_eq!(status, 200, "{body}");
}

#[path = "inference_fanout_tests.rs"]
mod fanout;
