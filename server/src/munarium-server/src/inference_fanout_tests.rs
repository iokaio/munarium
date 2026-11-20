// SPDX-License-Identifier: Apache-2.0
//! ClearView IR-3 on multi-collection reads (design 8.6 "Multi-collection
//! recommendation", I1, I2, SC2): post-merge group reranking and one shared
//! query embedding per compatible group, through session turns against an
//! in-process Lens-profile stub serving two reranking models.
//!
//! The stub's reranker is controlled per model: `Identity` returns the fused
//! order, `Reverse` the reversed order and `Down` a 503. Comparing a read
//! under `Identity` with the same read under `Reverse` shows exactly which
//! fused positions each group may reorder. Fixture text is fictional.
use super::*;

const RERANK_B: &str = "lens-reference/rerank-v2";
const UID: &str = "ir-fan-user";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rerank {
    Identity,
    Reverse,
    Down,
}

#[derive(Default)]
struct FanCalls {
    embeddings: AtomicUsize,
    rerank_a: AtomicUsize,
    rerank_b: AtomicUsize,
}

struct FanStub {
    calls: FanCalls,
    modes: StdMutex<std::collections::HashMap<String, Rerank>>,
}

#[derive(Clone)]
struct FanStubHandle {
    stub: Arc<FanStub>,
    base: String,
}

impl FanStubHandle {
    fn set(&self, model: &str, mode: Rerank) {
        self.stub
            .modes
            .lock()
            .unwrap()
            .insert(model.to_string(), mode);
    }
    fn embeddings(&self) -> usize {
        self.stub.calls.embeddings.load(AtomicOrdering::SeqCst)
    }
    fn reranks(&self) -> (usize, usize) {
        (
            self.stub.calls.rerank_a.load(AtomicOrdering::SeqCst),
            self.stub.calls.rerank_b.load(AtomicOrdering::SeqCst),
        )
    }
}

async fn fan_models(AxState(_): AxState<Arc<FanStub>>) -> axum::response::Response {
    let rerank = |id: &str| {
        json!({"id": id, "object": "model", "owned_by": "munarium-lens", "stage": "reranking",
               "backend": "reference", "retrieval_model": false})
    };
    axum::Json(json!({"object": "list", "data": [
        {"id": EMBED_MODEL, "object": "model", "owned_by": "munarium-lens", "stage": "embedding",
         "backend": "reference", "retrieval_model": false, "dimensions": DIMS},
        rerank(RERANK_MODEL), rerank(RERANK_B)
    ]}))
    .into_response()
}

async fn fan_embeddings(
    AxState(stub): AxState<Arc<FanStub>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    stub.calls.embeddings.fetch_add(1, AtomicOrdering::SeqCst);
    let map = match strict(&headers, &body, &["model", "input"], "embedding") {
        Ok(m) => m,
        Err(r) => return r,
    };
    let Some(input) = strings(map.get("input")) else {
        return refusal(StatusCode::BAD_REQUEST, "invalid_request", "embedding");
    };
    let used: usize = input.iter().map(|i| tokens(i).count()).sum();
    let data: Vec<Value> = input
        .iter()
        .enumerate()
        .map(|(i, text)| json!({"object": "embedding", "index": i, "embedding": reference_embedding(text)}))
        .collect();
    axum::Json(json!({"object": "list", "model": EMBED_MODEL, "data": data,
        "usage": {"prompt_tokens": used, "total_tokens": used}}))
    .into_response()
}

async fn fan_rerank(
    AxState(stub): AxState<Arc<FanStub>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let map = match strict(
        &headers,
        &body,
        &["model", "query", "documents"],
        "reranking",
    ) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let (Some(model), Some(documents)) = (
        map.get("model").and_then(Value::as_str),
        strings(map.get("documents")),
    ) else {
        return refusal(StatusCode::BAD_REQUEST, "invalid_request", "reranking");
    };
    match model {
        RERANK_MODEL => stub.calls.rerank_a.fetch_add(1, AtomicOrdering::SeqCst),
        RERANK_B => stub.calls.rerank_b.fetch_add(1, AtomicOrdering::SeqCst),
        _ => return refusal(StatusCode::NOT_FOUND, "model_not_found", "reranking"),
    };
    let mode = stub
        .modes
        .lock()
        .unwrap()
        .get(model)
        .copied()
        .unwrap_or(Rerank::Identity);
    let n = documents.len();
    let results: Vec<Value> = match mode {
        Rerank::Down => {
            return refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "inference_unavailable",
                "reranking",
            )
        }
        Rerank::Identity => (0..n)
            .map(|i| json!({"index": i, "relevance_score": (n - i) as f64}))
            .collect(),
        Rerank::Reverse => (0..n)
            .map(|i| json!({"index": i, "relevance_score": i as f64}))
            .collect(),
    };
    axum::Json(json!({"model": model, "results": results,
        "usage": {"prompt_tokens": n * 4, "total_tokens": n * 4}}))
    .into_response()
}

async fn start_fan_stub() -> FanStubHandle {
    let stub = Arc::new(FanStub {
        calls: FanCalls::default(),
        modes: StdMutex::new(Default::default()),
    });
    let app = axum::Router::new()
        .route("/v1/models", axum::routing::get(fan_models))
        .route("/v1/embeddings", axum::routing::post(fan_embeddings))
        .route("/v1/rerank", axum::routing::post(fan_rerank))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    FanStubHandle {
        stub,
        base: format!("http://{addr}/v1"),
    }
}

struct Fan {
    state: Arc<AppState>,
    router: Router,
    tenant: String,
    /// name -> collection id
    ids: std::collections::HashMap<String, String>,
    stub: FanStubHandle,
}

/// A tenant with collections alpha, beta, gamma and delta (three fictional
/// sources each, every one matching the query) and runbooks over them; the
/// endpoint configuration is applied and probed, nothing is bound.
async fn fan(url: String) -> Fan {
    let tenant = format!("irf-{}", uuid::Uuid::new_v4().simple());
    let state = crate::providers_api::usage_tests::test_state_with_auth(
        Some(url),
        crate::config::AuthMode::Static(vec![
            (MGMT.into(), tenant.clone(), "mgmt".into()),
            (READER.into(), tenant.clone(), "rw".into()),
        ]),
    )
    .await;
    state.inference.set_enabled(true);
    let retrieval = state.retrieval_for(&tenant).unwrap();
    let mut ids = std::collections::HashMap::new();
    for name in ["alpha", "beta", "gamma", "delta"] {
        let collection = retrieval
            .ensure_collection(name, "article@1", 0, &[], None)
            .await
            .unwrap();
        for (i, extra) in ["survey light", "maps colours", "catalogue numbers"]
            .iter()
            .enumerate()
        {
            let text = format!("The fictional orchid nebula {name} record {i}: {extra}.");
            let (source, _, _) = retrieval
                .put_source(
                    "",
                    "text/plain",
                    &format!("{name}-{i}.txt"),
                    Some("article@1"),
                    text.as_bytes(),
                )
                .await
                .unwrap();
            retrieval
                .bind_source(&collection.id, &source, None)
                .await
                .unwrap();
        }
        retrieval
            .build_collection_index(&collection.id, 400, 1, true)
            .await
            .unwrap();
        ids.insert(name.to_string(), collection.id);
    }
    for (runbook, collections) in [("abc", "alpha, beta, gamma"), ("abd", "alpha, beta, delta")] {
        let yaml = format!(
            "apiVersion: munarium.ioka.io/v1\nkind: Runbook\nmetadata: {{ name: {runbook}, version: 1 }}\nspec:\n  collections: [{}]\n  steps: [{{ buildIndex: {{}} }}]\n",
            collections
                .split(", ")
                .map(|c| format!("{{ name: {c}, shape: article@1 }}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        sqlx::query("INSERT INTO runbooks (tenant_id, runbook_ref, yaml) VALUES ($1, $2, $3)")
            .bind(&tenant)
            .bind(format!("{runbook}@1"))
            .bind(yaml)
            .execute(state.pg_pool().unwrap())
            .await
            .unwrap();
    }
    let stub = start_fan_stub().await;
    let router = router(state.clone());
    let f = Fan {
        state,
        router,
        tenant,
        ids,
        stub,
    };
    let yaml = format!(
        "apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{name: lens}}\nspec:\n  provider: openai-compatible\n  endpoint: {}\n  models:\n    embed: [{EMBED_MODEL}]\n    rerank: [{RERANK_MODEL}, {RERANK_B}]\n  inference: {{embedDimensions: {DIMS}, allowTestBackend: true}}\n  budgets: {{dailyTotalTokens: 1000000, shadowDailyTokens: 50000}}\n",
        f.stub.base
    );
    f.state
        .providers
        .apply(&f.state, &f.tenant, &yaml)
        .await
        .unwrap();
    let (status, probe) = call(
        &f.router,
        "POST",
        "/v1/providers/lens/inference-probe",
        MGMT,
        None,
    )
    .await;
    assert_eq!(status, 200, "{probe}");
    assert_eq!(probe["passed"], true, "{probe}");
    f
}

impl Fan {
    async fn bind(
        &self,
        name: &str,
        expected: u64,
        instruction: &str,
        rerank_model: &str,
        policy: &str,
    ) -> u64 {
        let (status, bound) = call(
            &self.router,
            "PUT",
            &format!("/v1/collections/{name}/inference"),
            MGMT,
            Some(json!({"expected_revision": expected, "provider_config": "lens",
                "embedding": {"model": EMBED_MODEL, "query_instruction": instruction, "policy": policy},
                "rerank": {"model": rerank_model, "max_candidates": 10, "policy": policy},
                "shadow": {}})),
        )
        .await;
        assert_eq!(status, 200, "{bound}");
        bound["revision"].as_u64().unwrap()
    }

    /// Bind, build the semantic index and enroll; returns the revision.
    async fn enroll(&self, name: &str, instruction: &str, rerank_model: &str, policy: &str) -> u64 {
        self.bind(name, 0, instruction, rerank_model, policy).await;
        let (status, job) = call(
            &self.router,
            "POST",
            "/v1/index-build-jobs",
            READER,
            Some(json!({"kind": "semantic", "collection_id": self.ids[name]})),
        )
        .await;
        assert_eq!(status, 200, "{job}");
        let jobs = munarium_store_pg::jobs::BuildJobs::new(self.state.pg_pool().unwrap().clone());
        let job = jobs
            .get(&self.tenant, job["job_id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        let built = crate::datastore_jobs::run_one_job(&self.state, &job, None, None)
            .await
            .unwrap();
        assert_eq!(built["complete"], true, "{built}");
        let (status, enrolled) = call(
            &self.router,
            "POST",
            &format!("/v1/collections/{name}/inference/enroll"),
            MGMT,
            Some(json!({"expected_revision": 2})),
        )
        .await;
        assert_eq!(status, 200, "{enrolled}");
        enrolled["revision"].as_u64().unwrap()
    }

    async fn session(&self, runbook: &str) -> String {
        let access = munarium_access::AccessCtx {
            uid: UID.into(),
            tenant_id: self.tenant.clone(),
            level: 0,
            compartments: Vec::new(),
            all_compartments: false,
            scopes: vec![munarium_access::SCOPE_QUERY.into()],
            runbooks: None,
            jti: String::new(),
        };
        crate::sessions_api::op_create_session(&self.state, &access, runbook)
            .await
            .unwrap()
            .session_id
    }

    /// One turn, retrieval only: (status, body, raw bytes).
    async fn turn(&self, session: &str, extra: Value) -> (u16, Value, Vec<u8>) {
        let mut body = json!({"query": "orchid nebula", "complete": false, "top_k": 12});
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        let response = self
            .router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/v1/sessions/{session}/turns"))
                    .header("authorization", format!("Bearer {READER}"))
                    .header("x-munarium-uid", UID)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 10_000_000)
            .await
            .unwrap()
            .to_vec();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            bytes,
        )
    }

    /// Serving attempts as (stage, model, sorted collection ids).
    async fn serving_attempts(&self) -> Vec<(String, String, Vec<String>)> {
        use sqlx::Row as _;
        let rows = sqlx::query(
            "SELECT stage, model, collection_ids FROM inference_attempts
              WHERE tenant_id = $1 AND purpose = 'serving' ORDER BY started_at, attempt_id",
        )
        .bind(&self.tenant)
        .fetch_all(self.state.pg_pool().unwrap())
        .await
        .unwrap();
        let mut out: Vec<_> = rows
            .iter()
            .map(|r| {
                let mut ids: Vec<String> = r.get("collection_ids");
                ids.sort();
                (r.get("stage"), r.get("model"), ids)
            })
            .collect();
        out.sort();
        out
    }

    async fn clear_attempts(&self) {
        sqlx::query("DELETE FROM inference_attempts WHERE tenant_id = $1")
            .bind(&self.tenant)
            .execute(self.state.pg_pool().unwrap())
            .await
            .unwrap();
    }

    fn sorted_ids(&self, names: &[&str]) -> Vec<String> {
        let mut ids: Vec<String> = names.iter().map(|n| self.ids[*n].clone()).collect();
        ids.sort();
        ids
    }
}

/// The served order as (collection, chunk_id).
fn order(body: &Value) -> Vec<(String, String)> {
    body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["collection"].as_str().unwrap().to_string(),
                h["chunk_id"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn positions(order: &[(String, String)], collection: &str) -> Vec<usize> {
    order
        .iter()
        .enumerate()
        .filter(|(_, (c, _))| c == collection)
        .map(|(i, _)| i)
        .collect()
}

fn chunks_at(order: &[(String, String)], at: &[usize]) -> Vec<String> {
    at.iter().map(|&i| order[i].1.clone()).collect()
}

fn receipt<'a>(body: &'a Value, collection: &str, stage: &str) -> Option<&'a Value> {
    body["execution"]["stages"]
        .as_array()?
        .iter()
        .find(|s| s["collection"] == collection && s["stage"] == stage)
}

/// Design 8.6 with B on a different reranking model and C unenrolled. Each
/// group reorders only within the fused positions it held; the unenrolled
/// member never moves; one rerank call per group, each charged to its own
/// collections, and the shared query embedding charged once to both.
#[tokio::test]
async fn group_rerank_keeps_each_group_in_its_fused_positions() {
    let Some(url) = db_url("fanout_groups") else {
        return;
    };
    let f = fan(url).await;
    f.enroll("alpha", "Query: ", RERANK_MODEL, "require").await;
    f.enroll("beta", "Query: ", RERANK_B, "require").await;
    let session = f.session("abc").await;

    // Both rerankers return the fused order: the baseline.
    f.clear_attempts().await;
    let (embeds, (ra, rb)) = (f.stub.embeddings(), f.stub.reranks());
    let (status, fused, _) = f.turn(&session, json!({})).await;
    assert_eq!(status, 200, "{fused}");
    let fused_order = order(&fused);
    let (a, b, c) = (
        positions(&fused_order, "alpha"),
        positions(&fused_order, "beta"),
        positions(&fused_order, "gamma"),
    );
    assert!(a.len() >= 2 && b.len() >= 2 && !c.is_empty(), "{fused}");
    assert_eq!(
        f.stub.embeddings(),
        embeds + 1,
        "one shared query embedding"
    );
    assert_eq!(
        f.stub.reranks(),
        (ra + 1, rb + 1),
        "one rerank call per group"
    );
    for name in ["alpha", "beta"] {
        for stage in ["embedding", "reranking"] {
            let r =
                receipt(&fused, name, stage).unwrap_or_else(|| panic!("{name} {stage}: {fused}"));
            assert_eq!(r["applied"], true, "{r}");
            assert_eq!(r["state"], "ready", "{r}");
        }
    }
    assert_eq!(
        receipt(&fused, "alpha", "reranking").unwrap()["identity"],
        RERANK_MODEL
    );
    assert_eq!(
        receipt(&fused, "beta", "reranking").unwrap()["identity"],
        RERANK_B
    );
    assert_eq!(
        receipt(&fused, "alpha", "reranking").unwrap()["scored"],
        a.len().min(10) as u64
    );
    assert!(receipt(&fused, "gamma", "embedding").is_none());
    assert!(receipt(&fused, "gamma", "reranking").is_none());

    // I2: the shared embedding is one attempt naming both collections; each
    // rerank attempt names its own group's collection and model.
    assert_eq!(f.serving_attempts().await, {
        let mut want = vec![
            (
                "embedding".to_string(),
                EMBED_MODEL.to_string(),
                f.sorted_ids(&["alpha", "beta"]),
            ),
            (
                "reranking".to_string(),
                RERANK_MODEL.to_string(),
                f.sorted_ids(&["alpha"]),
            ),
            (
                "reranking".to_string(),
                RERANK_B.to_string(),
                f.sorted_ids(&["beta"]),
            ),
        ];
        want.sort();
        want
    });

    // A's reranker reverses A's head: only A's positions change.
    f.stub.set(RERANK_MODEL, Rerank::Reverse);
    let (status, body, _) = f.turn(&session, json!({})).await;
    assert_eq!(status, 200, "{body}");
    let served = order(&body);
    assert_eq!(served.len(), fused_order.len());
    assert_eq!(positions(&served, "alpha"), a);
    assert_eq!(positions(&served, "beta"), b);
    assert_eq!(positions(&served, "gamma"), c);
    let mut reversed = chunks_at(&fused_order, &a);
    reversed.reverse();
    assert_eq!(chunks_at(&served, &a), reversed);
    assert_eq!(chunks_at(&served, &b), chunks_at(&fused_order, &b));
    assert_eq!(chunks_at(&served, &c), chunks_at(&fused_order, &c));

    // B's reranker reverses too: each within its own positions.
    f.stub.set(RERANK_B, Rerank::Reverse);
    let (status, body, _) = f.turn(&session, json!({})).await;
    assert_eq!(status, 200, "{body}");
    let served = order(&body);
    let mut b_reversed = chunks_at(&fused_order, &b);
    b_reversed.reverse();
    assert_eq!(chunks_at(&served, &a), reversed);
    assert_eq!(chunks_at(&served, &b), b_reversed);
    assert_eq!(chunks_at(&served, &c), chunks_at(&fused_order, &c));
}

/// I1 per group: B (`require`) down refuses the turn, or under an explicit
/// availability policy withdraws only B's evidence; A (`prefer`) down keeps
/// A's fused order with the disclosed fallback while B still reranks.
#[tokio::test]
async fn group_outage_follows_each_groups_own_policy() {
    let Some(url) = db_url("fanout_outage") else {
        return;
    };
    let f = fan(url).await;
    f.enroll("alpha", "Query: ", RERANK_MODEL, "prefer").await;
    f.enroll("beta", "Query: ", RERANK_B, "require").await;
    let session = f.session("abc").await;
    let (status, fused, _) = f.turn(&session, json!({})).await;
    assert_eq!(status, 200, "{fused}");
    let fused_order = order(&fused);
    let (a, b) = (
        positions(&fused_order, "alpha"),
        positions(&fused_order, "beta"),
    );

    // B's endpoint down, A healthy and reversing.
    f.stub.set(RERANK_B, Rerank::Down);
    f.stub.set(RERANK_MODEL, Rerank::Reverse);
    let (status, body, _) = f.turn(&session, json!({})).await;
    assert_eq!(status, 503, "{body}");
    assert!(body["type"]
        .as_str()
        .unwrap()
        .ends_with("/inference-stage-unavailable"));
    assert!(body["detail"].as_str().unwrap().contains("stage=reranking"));
    assert!(body.get("hits").is_none(), "no partial or fallback result");

    // Under `report`: B is unavailable and contributes nothing; A and the
    // unenrolled C are served, A reranked.
    let (status, body, _) = f
        .turn(&session, json!({"collection_availability": "report"}))
        .await;
    assert_eq!(status, 200, "{body}");
    let served = order(&body);
    assert!(positions(&served, "beta").is_empty(), "{body}");
    let beta = body["coverage"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["collection"] == "beta")
        .unwrap();
    assert_eq!(beta["outcome"], "unavailable");
    assert_eq!(beta["reason"], "inference_unavailable");
    let mut a_reversed = chunks_at(&fused_order, &a);
    a_reversed.reverse();
    let served_alpha: Vec<String> = served
        .iter()
        .filter(|(c, _)| c == "alpha")
        .map(|(_, id)| id.clone())
        .collect();
    assert_eq!(served_alpha, a_reversed);
    assert_eq!(
        receipt(&body, "beta", "reranking").unwrap()["applied"],
        false
    );
    assert_eq!(
        receipt(&body, "beta", "reranking").unwrap()["fallback"],
        Value::Null
    );

    // A's endpoint down under `prefer`, B healthy and reversing.
    f.stub.set(RERANK_MODEL, Rerank::Down);
    f.stub.set(RERANK_B, Rerank::Reverse);
    let (status, body, _) = f.turn(&session, json!({})).await;
    assert_eq!(status, 200, "{body}");
    let served = order(&body);
    assert_eq!(
        chunks_at(&served, &a),
        chunks_at(&fused_order, &a),
        "fused order kept"
    );
    let mut b_reversed = chunks_at(&fused_order, &b);
    b_reversed.reverse();
    assert_eq!(chunks_at(&served, &b), b_reversed);
    let ra = receipt(&body, "alpha", "reranking").unwrap();
    assert_eq!(ra["applied"], false);
    assert_eq!(ra["fallback"], "fused_order");
    assert_eq!(ra["policy"], "prefer");
    assert_eq!(ra["state"], "unavailable");
    assert_eq!(
        receipt(&body, "beta", "reranking").unwrap()["applied"],
        true
    );

    let events: Vec<(String, String)> = sqlx::query_as(
        "SELECT collection_id, outcome FROM inference_stage_events
          WHERE tenant_id = $1 AND stage = 'reranking' ORDER BY recorded_at",
    )
    .bind(&f.tenant)
    .fetch_all(f.state.pg_pool().unwrap())
    .await
    .unwrap();
    assert!(
        events.contains(&(f.ids["beta"].clone(), "refused".into())),
        "{events:?}"
    );
    assert!(
        events.contains(&(f.ids["alpha"].clone(), "fallback".into())),
        "{events:?}"
    );
}

/// One query embedding serves the compatible members (same configuration,
/// space and inputs) and is charged once; a member with a different query
/// instruction (a different semantic space) gets its own call. The three
/// members share one reranker binding, so they rerank as one group.
#[tokio::test]
async fn shared_embedding_is_one_call_per_compatible_group() {
    let Some(url) = db_url("fanout_embedding") else {
        return;
    };
    let f = fan(url).await;
    f.enroll("alpha", "Query: ", RERANK_MODEL, "require").await;
    f.enroll("beta", "Query: ", RERANK_MODEL, "require").await;
    f.enroll("delta", "Find: ", RERANK_MODEL, "require").await;
    let session = f.session("abd").await;
    f.clear_attempts().await;
    let (embeds, (ra, rb)) = (f.stub.embeddings(), f.stub.reranks());
    let (status, body, _) = f.turn(&session, json!({})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        f.stub.embeddings(),
        embeds + 2,
        "one call per compatible group"
    );
    assert_eq!(f.stub.reranks(), (ra + 1, rb), "one group, one rerank call");
    for name in ["alpha", "beta", "delta"] {
        assert_eq!(receipt(&body, name, "embedding").unwrap()["applied"], true);
        assert_eq!(receipt(&body, name, "reranking").unwrap()["applied"], true);
    }
    let mut want = vec![
        (
            "embedding".to_string(),
            EMBED_MODEL.to_string(),
            f.sorted_ids(&["alpha", "beta"]),
        ),
        (
            "embedding".to_string(),
            EMBED_MODEL.to_string(),
            f.sorted_ids(&["delta"]),
        ),
        (
            "reranking".to_string(),
            RERANK_MODEL.to_string(),
            f.sorted_ids(&["alpha", "beta", "delta"]),
        ),
    ];
    want.sort();
    assert_eq!(
        f.serving_attempts().await,
        want,
        "each physical call charged once"
    );
}

/// COMPAT: a turn over collections none of which is enrolled is the v1.4
/// turn, with inference enabled, a probed configuration and a bound (not
/// enrolled) member: same hits and envelopes, no `execution`, no call.
#[tokio::test]
async fn unenrolled_fan_out_is_unchanged() {
    let Some(url) = db_url("fanout_unenrolled") else {
        return;
    };
    let f = fan(url).await;
    let session = f.session("abc").await;
    let (status, before, _) = f.turn(&session, json!({})).await;
    assert_eq!(status, 200, "{before}");
    f.bind("alpha", 0, "Query: ", RERANK_MODEL, "require").await;
    let (calls, (ra, rb)) = (f.stub.embeddings(), f.stub.reranks());
    let (status, after, bytes) = f.turn(&session, json!({})).await;
    assert_eq!(status, 200, "{after}");
    assert!(after.get("execution").is_none(), "{after}");
    let keys: BTreeSet<&str> = after
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "session_id",
            "ordinal",
            "collections_searched",
            "skipped",
            "hits",
            "envelopes"
        ])
    );
    for member in ["collections_searched", "skipped", "hits", "envelopes"] {
        assert_eq!(
            serde_json::to_vec(&before[member]).unwrap(),
            serde_json::to_vec(&after[member]).unwrap(),
            "{member}"
        );
    }
    assert!(!String::from_utf8(bytes).unwrap().contains("stages"));
    assert_eq!(f.stub.embeddings(), calls);
    assert_eq!(f.stub.reranks(), (ra, rb));
}
