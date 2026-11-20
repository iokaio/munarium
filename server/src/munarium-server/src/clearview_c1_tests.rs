// SPDX-License-Identifier: Apache-2.0
//! ClearView C1-T1/C1-T2: explicit partial multi-collection reads, on unary
//! REST, streamed REST (SSE) and typed gRPC, against isolated PostgreSQL and a
//! loopback provider. Fixture text is fictional.
use super::*;
use crate::config::AuthMode;
use axum::body::{to_bytes, Body};
use munarium_proto::mmp::v1 as pb;
use pb::session_service_server::SessionService;
use serde_json::{json, Value};
use std::sync::Mutex;
use tower::ServiceExt;

const TOKEN: &str = "c1-reader";
const UID: &str = "c1-user";

struct Fixture {
    state: Arc<AppState>,
    router: axum::Router,
    tenant: String,
    prompts: Arc<Mutex<Vec<String>>>,
    _provider: AbortOnDrop,
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn indexed(state: &AppState, tenant: &str, name: &str, level: i32, text: &[u8]) -> String {
    let retrieval = state.retrieval_for(tenant).unwrap();
    let collection = retrieval
        .ensure_collection(name, "article@1", level, &[], None)
        .await
        .unwrap();
    let (source, _, _) = retrieval
        .put_source(
            "",
            "text/plain",
            &format!("{name}.txt"),
            Some("article@1"),
            text,
        )
        .await
        .unwrap();
    retrieval
        .bind_source(&collection.id, &source, None)
        .await
        .unwrap();
    retrieval
        .build_collection_index(&collection.id, 400, 1, true)
        .await
        .unwrap();
    collection.id
}

/// A member whose search fails as storage: a damaged index row. Only this
/// collection's search reads it, so the other members are unaffected.
async fn damage(state: &AppState, tenant: &str, collection_id: &str) {
    sqlx::query(
        "UPDATE index_versions SET watermark_seq = -1 WHERE tenant_id = $1 AND collection_id = $2",
    )
    .bind(tenant)
    .bind(collection_id)
    .execute(state.pg_pool().unwrap())
    .await
    .unwrap();
}

async fn fixture(url: String) -> Fixture {
    let tenant = format!("c1-{}", uuid::Uuid::new_v4().simple());
    let state = crate::providers_api::usage_tests::test_state_with_auth(
        Some(url),
        AuthMode::Static(vec![(TOKEN.into(), tenant.clone(), "rw".into())]),
    )
    .await;
    let prompts = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = prompts.clone();
    let app = axum::Router::new().route(
        "/api/chat",
        axum::routing::post(move |Json(body): Json<Value>| {
            let sink = sink.clone();
            async move {
                sink.lock().unwrap().push(body.to_string());
                Json(json!({"done":true,"done_reason":"stop",
                    "message":{"role":"assistant","content":"The orchid nebula is described [alpha]."},
                    "prompt_eval_count":3,"eval_count":2}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let provider = AbortOnDrop(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap()
    }));
    state
        .providers
        .apply(&state, &tenant, &format!(
            "apiVersion: munarium.ioka.io/v1\nkind: ProviderConfig\nmetadata: {{ name: fixture }}\nspec:\n  provider: ollama\n  endpoint: {endpoint}\n  models: {{ fast: fixture, capable: fixture }}\n"
        ))
        .await
        .unwrap();
    // The good member, an optional member that will be damaged, a member the
    // session cannot see (also damaged: searching it would show), and an
    // authorized member whose index holds no chunks.
    indexed(
        &state,
        &tenant,
        "alpha",
        0,
        b"Fictional orchid nebula evidence from the alpha archive.",
    )
    .await;
    let beta = indexed(
        &state,
        &tenant,
        "beta",
        0,
        b"Fictional orchid nebula notes from the beta archive.",
    )
    .await;
    let hidden = indexed(
        &state,
        &tenant,
        "hidden",
        5,
        b"Fictional orchid nebula ledger from the restricted archive.",
    )
    .await;
    indexed(&state, &tenant, "vacant", 0, b"   \n  ").await;
    damage(&state, &tenant, &beta).await;
    damage(&state, &tenant, &hidden).await;
    for (name, collections) in [
        ("unavailable", "alpha, beta"),
        ("hidden", "alpha, hidden"),
        ("vacant", "alpha, vacant"),
        ("broken", "beta, hidden"),
    ] {
        let yaml = format!(
            "apiVersion: munarium.ioka.io/v1\nkind: Runbook\nmetadata: {{ name: {name}, version: 1 }}\nspec:\n  collections: [{}]\n  models:\n    tasks:\n      completion: {{ provider: fixture, tier: fast }}\n  completion: {{ promptTemplate: \"{{query}}\\n{{context}}\", maxTokens: 64 }}\n  steps: [{{ buildIndex: {{}} }}]\n",
            collections
                .split(", ")
                .map(|c| format!("{{ name: {c}, shape: article@1 }}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        sqlx::query("INSERT INTO runbooks (tenant_id, runbook_ref, yaml) VALUES ($1, $2, $3)")
            .bind(&tenant)
            .bind(format!("{name}@1"))
            .bind(yaml)
            .execute(state.pg_pool().unwrap())
            .await
            .unwrap();
    }
    let router = crate::rest::router(state.clone());
    Fixture {
        state,
        router,
        tenant,
        prompts,
        _provider: provider,
    }
}

impl Fixture {
    /// A session snapshotting clearance level 0: `hidden` (level 5) is not
    /// visible to it.
    async fn session(&self, runbook: &str) -> String {
        let access = AccessCtx {
            uid: UID.into(),
            tenant_id: self.tenant.clone(),
            level: 0,
            compartments: Vec::new(),
            all_compartments: false,
            scopes: vec![munarium_access::SCOPE_QUERY.into()],
            runbooks: None,
            jti: String::new(),
        };
        op_create_session(&self.state, &access, runbook)
            .await
            .unwrap()
            .session_id
    }

    async fn rest(&self, path: &str, body: Value) -> (u16, Vec<u8>) {
        let response = self
            .router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .header("x-munarium-uid", UID)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        (
            status,
            to_bytes(response.into_body(), 10_000_000)
                .await
                .unwrap()
                .to_vec(),
        )
    }

    async fn unary(&self, session: &str, body: Value) -> (u16, Value) {
        let (status, bytes) = self
            .rest(&format!("/v1/sessions/{session}/turns"), body)
            .await;
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// The SSE turn: every (event, data) pair in order.
    async fn stream(&self, session: &str, body: Value) -> Vec<(String, Value)> {
        let (status, bytes) = self
            .rest(&format!("/v1/sessions/{session}/turns/stream"), body)
            .await;
        assert_eq!(status, 200);
        let text = String::from_utf8(bytes).unwrap();
        text.split("\n\n")
            .filter_map(|frame| {
                let event = frame.lines().find_map(|l| l.strip_prefix("event: "))?;
                let data = frame.lines().find_map(|l| l.strip_prefix("data: "))?;
                Some((event.to_string(), serde_json::from_str(data).unwrap()))
            })
            .collect()
    }

    async fn grpc(
        &self,
        session: &str,
        policy: &str,
    ) -> std::result::Result<pb::TurnResponse, tonic::Status> {
        let mut request = tonic::Request::new(pb::TurnRequest {
            session_id: session.into(),
            query: "orchid nebula".into(),
            complete: true,
            collection_availability: policy.into(),
            ..Default::default()
        });
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        request
            .metadata_mut()
            .insert("munarium-uid", UID.parse().unwrap());
        crate::grpc_platform::SessionSvc {
            state: self.state.clone(),
        }
        .turn(request)
        .await
        .map(|r| r.into_inner())
    }

    fn take_prompts(&self) -> Vec<String> {
        std::mem::take(&mut *self.prompts.lock().unwrap())
    }
}

fn turn(policy: Option<&str>) -> Value {
    let mut body = json!({"query": "orchid nebula", "complete": true});
    if let Some(policy) = policy {
        body["collection_availability"] = json!(policy);
    }
    body
}

fn collections_of(response: &Value) -> Vec<String> {
    let mut names: Vec<String> = response["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["collection"].as_str().unwrap().to_string())
        .chain(
            response["envelopes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["collection"].as_str().unwrap().to_string()),
        )
        .collect();
    names.sort();
    names.dedup();
    names
}

fn done(events: &[(String, Value)]) -> &Value {
    let (event, data) = events.last().unwrap();
    assert_eq!(event, "done", "{data}");
    data
}

/// The fields of a turn that must agree across transports; ordinal and the
/// generated answer metadata legitimately differ between two turns.
fn semantics(response: &Value) -> Value {
    json!({
        "collections_searched": response["collections_searched"],
        "skipped": response["skipped"],
        "hits": collections_of(response),
        "answer": response["completion"]["text"],
        "coverage": response["coverage"],
    })
}

#[tokio::test]
async fn partial_reads_report_gaps_withhold_claims_and_refuse_missing_evidence() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("ClearView C1 PostgreSQL tests NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
        return;
    };
    let f = fixture(url).await;

    // --- 1. A good result with an unavailable optional member (`report`).
    let session = f.session("unavailable").await;
    let (status, unary) = f.unary(&session, turn(Some("report"))).await;
    assert_eq!(status, 200, "{unary}");
    assert_eq!(collections_of(&unary), vec!["alpha"], "{unary}");
    assert_eq!(unary["collections_searched"], json!(["alpha"]));
    assert_eq!(
        unary["skipped"],
        json!([]),
        "skipped keeps its v1.4 meaning: no active index"
    );
    assert_eq!(
        unary["coverage"],
        json!({
            "policy": "report",
            "complete": false,
            "members": [
                {"collection": "alpha", "stage": "search", "outcome": "complete", "hits": 1},
                {"collection": "beta", "stage": "search", "outcome": "unavailable", "reason": "search_failed"}
            ],
            "withheld_claims": ["absence", "exhaustive_coverage"]
        })
    );
    assert_eq!(
        unary["completion"]["text"],
        "The orchid nebula is described [alpha]."
    );
    let prompts = f.take_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].contains("[coverage notice]")
            && prompts[0].contains("beta")
            && prompts[0].contains("Do not state that anything is absent"),
        "the model is told what it cannot claim: {}",
        prompts[0]
    );
    let body = String::from_utf8(serde_json::to_vec(&unary).unwrap()).unwrap();
    assert!(
        !body.contains("watermark_seq") && !body.contains("negative"),
        "the reason is content-free"
    );

    let events = f.stream(&session, turn(Some("report"))).await;
    assert_eq!(semantics(done(&events)), semantics(&unary));
    // Emitted as each member completes, so in completion order.
    let mut retrieval: Vec<&Value> = events
        .iter()
        .filter(|(e, d)| e == "progress" && d["stage"] == "retrieval")
        .map(|(_, d)| d)
        .collect();
    retrieval.sort_by_key(|d| d["collection"].as_str().unwrap().to_string());
    assert_eq!(
        retrieval
            .iter()
            .map(|d| (
                d["collection"].as_str().unwrap(),
                d["outcome"].as_str().unwrap(),
                d["reason"].as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("alpha", "complete", None),
            ("beta", "unavailable", Some("search_failed"))
        ]
    );
    assert!(f.take_prompts()[0].contains("[coverage notice]"));

    let typed = f.grpc(&session, "report").await.unwrap();
    let coverage = typed.coverage.expect("coverage on typed gRPC");
    assert!(!coverage.complete);
    assert_eq!(coverage.policy, "report");
    assert_eq!(
        coverage
            .members
            .iter()
            .map(|m| (
                m.collection.as_str(),
                m.outcome.as_str(),
                m.reason.as_str(),
                m.hits
            ))
            .collect::<Vec<_>>(),
        vec![
            ("alpha", "complete", "", Some(1)),
            ("beta", "unavailable", "search_failed", None)
        ]
    );
    assert_eq!(
        coverage.withheld_claims,
        vec!["absence", "exhaustive_coverage"]
    );
    assert!(typed.hits.iter().all(|h| h.collection == "alpha"));
    assert!(f.take_prompts()[0].contains("[coverage notice]"));

    // The native bridge's streamed turn carries the same `done` semantics.
    {
        use pb::server_api_service_server::ServerApiService;
        use tokio_stream::StreamExt;
        let mut request = tonic::Request::new(pb::ServerApiRequest {
            path_parameters: [("id".to_string(), session.clone())].into_iter().collect(),
            body: serde_json::to_vec(&turn(Some("report"))).unwrap(),
            ..Default::default()
        });
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        request
            .metadata_mut()
            .insert("munarium-uid", UID.parse().unwrap());
        let mut frames = crate::grpc_api::ServerApiSvc::new(f.state.clone())
            .turn_stream(request)
            .await
            .unwrap()
            .into_inner();
        let mut text = Vec::new();
        while let Some(frame) = frames.next().await {
            text.extend(frame.unwrap().body);
        }
        let text = String::from_utf8(text).unwrap();
        let data = text
            .split(
                "

",
            )
            .filter(|frame| frame.contains("event: done"))
            .find_map(|frame| frame.lines().find_map(|l| l.strip_prefix("data: ")))
            .expect("a done event on the native stream");
        let bridged: Value = serde_json::from_str(data).unwrap();
        assert_eq!(semantics(&bridged), semantics(&unary));
        assert!(f.take_prompts()[0].contains("[coverage notice]"));
    }

    // v1.4 on the same read: the damaged member still fails the whole turn,
    // with no coverage invented for it, on every transport.
    let (status, legacy) = f.unary(&session, turn(None)).await;
    assert_eq!(status, 500, "{legacy}");
    assert!(legacy["type"].as_str().unwrap().ends_with("/storage-error"));
    let events = f.stream(&session, turn(None)).await;
    assert_eq!(events.last().unwrap().0, "error");
    assert!(events
        .iter()
        .filter(|(e, _)| e == "progress")
        .all(|(_, d)| d.get("outcome").is_none() && d.get("reason").is_none()));
    assert_eq!(
        f.grpc(&session, "").await.unwrap_err().code(),
        tonic::Code::Internal
    );
    assert!(f.take_prompts().is_empty(), "a failed read spends nothing");

    // --- 2. Missing required evidence: `require` refuses, before any spend.
    let (status, refused) = f.unary(&session, turn(Some("require"))).await;
    assert_eq!(status, 424, "{refused}");
    assert!(refused["type"]
        .as_str()
        .unwrap()
        .ends_with("/required-evidence-unavailable"));
    assert!(refused["detail"].as_str().unwrap().contains("'beta'"));
    let events = f.stream(&session, turn(Some("require"))).await;
    let (event, problem) = events.last().unwrap();
    assert_eq!(event, "error");
    assert_eq!(problem["status"], 424);
    let status = f.grpc(&session, "require").await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert!(
        f.take_prompts().is_empty(),
        "a refused answer is never generated"
    );
    // `report` with no member completing: the only possible answer rests on
    // missing evidence, so it is refused too, naming only a visible member.
    let broken = f.session("broken").await;
    let (status, refused) = f.unary(&broken, turn(Some("report"))).await;
    assert_eq!(status, 424, "{refused}");
    let detail = refused["detail"].as_str().unwrap();
    assert!(detail.contains("'beta'") && !detail.contains("hidden"));
    let events = f.stream(&broken, turn(Some("report"))).await;
    assert_eq!(events.last().unwrap().1["status"], 424);
    assert_eq!(
        f.grpc(&broken, "report").await.unwrap_err().code(),
        tonic::Code::FailedPrecondition
    );
    assert!(f.take_prompts().is_empty());

    // --- 3. A hidden member contributes no outcome, name, count or
    // diagnostic, even though searching it would fail.
    let hidden = f.session("hidden").await;
    let (status, unary) = f.unary(&hidden, turn(Some("report"))).await;
    assert_eq!(status, 200, "{unary}");
    assert_eq!(
        unary["coverage"],
        json!({
            "policy": "report",
            "complete": true,
            "members": [{"collection": "alpha", "stage": "search", "outcome": "complete", "hits": 1}],
            "withheld_claims": []
        })
    );
    let events = f.stream(&hidden, turn(Some("report"))).await;
    assert_eq!(semantics(done(&events)), semantics(&unary));
    let typed = f.grpc(&hidden, "report").await.unwrap();
    assert!(typed.coverage.as_ref().unwrap().complete);
    assert_eq!(typed.coverage.as_ref().unwrap().members.len(), 1);
    for (label, text) in [
        ("unary", serde_json::to_string(&unary).unwrap()),
        ("stream", serde_json::to_string(&events).unwrap()),
        ("grpc", format!("{typed:?}")),
    ] {
        assert!(!text.contains("hidden"), "{label} names a hidden member");
        assert!(
            !text.contains("restricted archive"),
            "{label} leaks content"
        );
    }
    for prompt in f.take_prompts() {
        assert!(!prompt.contains("[coverage notice]") && !prompt.contains("hidden"));
    }

    // --- 4. An authorized empty member is a successful empty outcome, not a
    // gap: nothing is withheld because of it.
    let vacant = f.session("vacant").await;
    let (status, unary) = f.unary(&vacant, turn(Some("report"))).await;
    assert_eq!(status, 200, "{unary}");
    assert_eq!(
        unary["coverage"],
        json!({
            "policy": "report",
            "complete": true,
            "members": [
                {"collection": "alpha", "stage": "search", "outcome": "complete", "hits": 1},
                {"collection": "vacant", "stage": "search", "outcome": "empty", "hits": 0}
            ],
            "withheld_claims": []
        })
    );
    let events = f.stream(&vacant, turn(Some("report"))).await;
    assert_eq!(semantics(done(&events)), semantics(&unary));
    assert!(events.iter().any(|(e, d)| e == "progress"
        && d["stage"] == "retrieval"
        && d["collection"] == "vacant"
        && d["outcome"] == "empty"));
    let typed = f.grpc(&vacant, "require").await.unwrap();
    assert_eq!(typed.coverage.unwrap().policy, "require");
    for prompt in f.take_prompts() {
        assert!(!prompt.contains("[coverage notice]"));
    }

    // v1.4 on a read with no gap: no new key anywhere, and the same answer.
    let (status, legacy) = f.unary(&vacant, turn(None)).await;
    assert_eq!(status, 200);
    let keys: std::collections::BTreeSet<&str> = legacy
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "collections_searched",
            "completion",
            "envelopes",
            "hits",
            "ordinal",
            "session_id",
            "skipped"
        ]
        .into_iter()
        .collect()
    );
    let events = f.stream(&vacant, turn(None)).await;
    assert!(done(&events).get("coverage").is_none());
    assert!(events
        .iter()
        .all(|(_, d)| d.get("outcome").is_none() && d.get("reason").is_none()));
    let typed = f.grpc(&vacant, "").await.unwrap();
    assert!(typed.coverage.is_none());
    for prompt in f.take_prompts() {
        assert!(!prompt.contains("[coverage notice]"));
    }

    // An unknown policy is refused on both transports, never read as v1.4.
    let (status, invalid) = f.unary(&vacant, turn(Some("best-effort"))).await;
    assert_eq!(status, 400, "{invalid}");
    assert_eq!(
        f.grpc(&vacant, "best-effort").await.unwrap_err().code(),
        tonic::Code::InvalidArgument
    );
    assert!(f.take_prompts().is_empty());
}

#[test]
fn an_unselected_policy_serializes_the_v14_turn_byte_for_byte() {
    let turn = dto::TurnResponse {
        session_id: "ses-fixture".into(),
        ordinal: 3,
        collections_searched: vec!["alpha".into()],
        skipped: vec!["beta".into()],
        hits: Vec::new(),
        envelopes: Vec::new(),
        completion: None,
        hierarchy: None,
    };
    let covered = dto::CoveredTurnResponse {
        turn: turn.clone(),
        coverage: None,
        execution: None,
    };
    assert_eq!(
        serde_json::to_vec(&covered).unwrap(),
        serde_json::to_vec(&turn).unwrap()
    );
    let request: dto::TurnRequest = serde_json::from_value(json!({"query": "q"})).unwrap();
    assert!(request.collection_availability.is_none());
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({"query": "q", "complete": null})
    );
    for (stage, event) in [
        (
            "probe",
            dto::TurnProgressEvent::Probe {
                collection: "alpha".into(),
                hits: 1,
                skipped: false,
                outcome: None,
                reason: None,
            },
        ),
        (
            "retrieval",
            dto::TurnProgressEvent::Retrieval {
                collection: "alpha".into(),
                hits: 1,
                skipped: false,
                outcome: None,
                reason: None,
            },
        ),
    ] {
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({"stage": stage, "collection": "alpha", "hits": 1, "skipped": false})
        );
    }
}

#[test]
fn withholding_follows_the_missing_member_and_only_it() {
    let complete = coverage(
        dto::CollectionAvailabilityDto::Report,
        vec![
            member_searched("alpha", "search", 2),
            member_searched("vacant", "probe", 0),
        ],
    );
    assert!(complete.complete);
    assert!(complete.withheld_claims.is_empty());
    assert_eq!(coverage_notice(&complete), None);

    let partial = coverage(
        dto::CollectionAvailabilityDto::Report,
        vec![
            member_searched("alpha", "search", 2),
            member_missing("beta", "probe", "engine_unavailable"),
        ],
    );
    assert!(!partial.complete);
    assert_eq!(
        partial.withheld_claims,
        vec!["absence", "exhaustive_coverage"]
    );
    let notice = coverage_notice(&partial).unwrap();
    assert!(notice.contains("beta") && !notice.contains("alpha"));

    // The availability classifier never turns an authorization or policy
    // decision into a reportable gap.
    for fatal in [
        KernelError::Forbidden("x".into()),
        KernelError::Unauthenticated("x".into()),
        KernelError::InvalidInput(format!(
            "{}x",
            munarium_store_pg::source_retention::DENIED_PREFIX
        )),
        KernelError::Provider("x".into()),
    ] {
        assert_eq!(member_unavailable_reason(&fatal), None, "{fatal:?}");
    }
    assert_eq!(
        event_outcome(None, &Err(KernelError::Storage("x".into()))),
        (None, None)
    );
}
