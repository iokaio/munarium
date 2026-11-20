// SPDX-License-Identifier: Apache-2.0
//! ClearView R1-T1 at the Server boundary: opt-in search-execution disclosure
//! on REST (collection and legacy shape paths) and typed gRPC, with the v1.4
//! response bytes unchanged on omission. The PostgreSQL/Datastore engine
//! comparison is `munarium-retrieval`'s `mirror_integration` test.
use super::*;
use crate::config::AuthMode;
use axum::body::{to_bytes, Body};
use munarium_proto::mmp::v1 as pb;
use pb::retrieval_service_server::RetrievalService;
use serde_json::{json, Value};
use tower::ServiceExt;

const TOKEN: &str = "r1-reader";

async fn post(router: &Router, body: Value) -> (u16, Vec<u8>) {
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("x-munarium-uid", "r1-user")
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

fn expected_pg(limit: u64) -> Value {
    json!({
        "version": 1,
        "engine": "postgresql",
        "lexical_domain": munarium_retrieval::merge::PG_LEXICAL_DOMAIN,
        "legs": [
            {"leg": "lexical", "candidate_limit": limit, "fetched": null, "exhausted": null},
            {"leg": "vector", "candidate_limit": limit, "fetched": null, "exhausted": null}
        ]
    })
}

#[tokio::test]
async fn search_execution_is_disclosed_only_on_request() {
    let Ok(url) = std::env::var("MUNARIUM_TEST_DATABASE_URL") else {
        eprintln!("ClearView R1 PostgreSQL tests NOT RUN: MUNARIUM_TEST_DATABASE_URL unset");
        return;
    };
    let tenant = format!("r1-{}", uuid::Uuid::new_v4().simple());
    let state = crate::providers_api::usage_tests::test_state_with_auth(
        Some(url),
        AuthMode::Static(vec![(TOKEN.into(), tenant.clone(), "rw".into())]),
    )
    .await;
    let retrieval = state.retrieval_for(&tenant).unwrap();
    let collection = retrieval
        .ensure_collection("alpha", "article@1", 0, &[], None)
        .await
        .unwrap();
    for (name, text) in [
        ("alpha.txt", &b"Fictional orchid nebula evidence."[..]),
        ("beta.txt", &b"A fictional survey of the nebula."[..]),
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
    retrieval
        .build_collection_index(&collection.id, 400, 1, true)
        .await
        .unwrap();
    retrieval
        .build_index("article@1", 400, 1, true)
        .await
        .unwrap();
    let router = router(state.clone());
    let default_pool = munarium_core::retrieval::SearchParams::default().candidate_n as u64;

    for base in [
        json!({"query": "orchid nebula", "filter": {"collections": ["alpha"]}}),
        json!({"query": "orchid nebula", "shape_ref": "article@1"}),
    ] {
        let (status, legacy) = post(&router, base.clone()).await;
        assert_eq!(status, 200);
        let legacy_value: Value = serde_json::from_slice(&legacy).unwrap();
        assert!(legacy_value.get("execution").is_none());
        assert!(!legacy_value["hits"].as_array().unwrap().is_empty());

        let mut off = base.clone();
        off["disclose_execution"] = json!(false);
        assert_eq!(
            post(&router, off).await.1,
            legacy,
            "false is the v1.4 response, byte for byte"
        );

        let mut on = base.clone();
        on["disclose_execution"] = json!(true);
        let (status, disclosed) = post(&router, on).await;
        assert_eq!(status, 200);
        let disclosed: dto::DisclosedSearchResponse = serde_json::from_slice(&disclosed).unwrap();
        // The hits and envelope are the same search; only `execution` is new.
        assert_eq!(serde_json::to_vec(&disclosed.result).unwrap(), legacy);
        assert_eq!(
            serde_json::to_value(disclosed.execution.unwrap()).unwrap(),
            expected_pg(if base.get("filter").is_some() {
                default_pool
            } else {
                munarium_retrieval_pg::LEGACY_CANDIDATE_N as u64
            }),
            "PostgreSQL does not measure fetch counts or exhaustion: unknown, not zero"
        );
    }

    // The native bridge carries the same JSON, byte for byte, both ways.
    {
        use pb::server_api_service_server::ServerApiService;
        let native = crate::grpc_api::ServerApiSvc::new(state.clone());
        for body in [
            json!({"query": "orchid nebula", "shape_ref": "article@1"}),
            json!({"query": "orchid nebula", "shape_ref": "article@1", "disclose_execution": true}),
        ] {
            let mut request = tonic::Request::new(pb::ServerApiRequest {
                body: serde_json::to_vec(&body).unwrap(),
                ..Default::default()
            });
            request
                .metadata_mut()
                .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
            let bridged = native.hybrid_search(request).await.unwrap().into_inner();
            assert_eq!(bridged.body, post(&router, body).await.1);
        }
    }

    // Unknown stays unknown on an empty result, too.
    let (_, empty) = post(
        &router,
        json!({"query": "orchid", "shape_ref": "article@1", "top_k": 1, "index_version": "missing", "disclose_execution": true}),
    )
    .await;
    let empty: Value = serde_json::from_slice(&empty).unwrap();
    assert!(empty["type"].as_str().unwrap().ends_with("/not-found"));

    // Typed gRPC: a fresh message tag, present only when requested.
    let svc = crate::grpc_data::RetrievalSvc {
        state: state.clone(),
    };
    let call = |disclose: bool| {
        let mut request = tonic::Request::new(pb::HybridSearchRequest {
            query: "orchid nebula".into(),
            shape_ref: "article@1".into(),
            disclose_execution: disclose,
            ..Default::default()
        });
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        request
    };
    let legacy = svc.hybrid_search(call(false)).await.unwrap().into_inner();
    assert!(legacy.execution.is_none());
    let disclosed = svc.hybrid_search(call(true)).await.unwrap().into_inner();
    assert_eq!(disclosed.hits, legacy.hits);
    let execution = disclosed.execution.expect("requested execution");
    assert_eq!(
        (
            execution.version,
            execution.engine.as_str(),
            execution.lexical_domain.as_str()
        ),
        (
            1,
            "postgresql",
            munarium_retrieval::merge::PG_LEXICAL_DOMAIN
        )
    );
    assert_eq!(
        execution
            .legs
            .iter()
            .map(|l| (l.leg.as_str(), l.candidate_limit, l.fetched, l.exhausted))
            .collect::<Vec<_>>(),
        vec![
            ("lexical", Some(50), None, None),
            ("vector", Some(50), None, None)
        ]
    );
    // An older typed client's request (no field 6) decodes as `false`.
    use prost::Message;
    let old = pb::HybridSearchRequest {
        query: "q".into(),
        shape_ref: "article@1".into(),
        ..Default::default()
    }
    .encode_to_vec();
    assert!(
        !pb::HybridSearchRequest::decode(&old[..])
            .unwrap()
            .disclose_execution
    );
}

#[test]
fn the_disclosure_wrapper_adds_no_bytes_when_absent() {
    let result = dto::SearchResponse {
        hits: Vec::new(),
        envelope: dto::ProvenanceEnvelopeDto {
            chunk_ids: Vec::new(),
            source_ids: Vec::new(),
            source_paths: Vec::new(),
            source_content_hashes: Vec::new(),
            index_version: "idx-fixture".into(),
            event_watermark: 7,
            provider_fingerprint: None,
        },
    };
    let wrapped = dto::DisclosedSearchResponse {
        result: result.clone(),
        execution: None,
    };
    assert_eq!(
        serde_json::to_vec(&wrapped).unwrap(),
        serde_json::to_vec(&result).unwrap()
    );
    let request: dto::SearchRequest = serde_json::from_value(json!({"query": "q"})).unwrap();
    assert!(request.disclose_execution.is_none());
    assert!(serde_json::to_value(&request)
        .unwrap()
        .get("disclose_execution")
        .is_none());
}
