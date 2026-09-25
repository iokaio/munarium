// SPDX-License-Identifier: Apache-2.0
// Reuse the evidence fixture and observe durable effects, not just error codes.
use super::*;
use axum::http::StatusCode;
use munarium_proto::mmp::v1 as pb;
use serde_json::{json, Value};

async fn seal_through_transport(
    state: Arc<AppState>,
    native: bool,
    bearer: &str,
    uid: &str,
    manifest: &Value,
) -> StatusCode {
    let body = json!({"manifest": manifest});
    if native {
        use pb::server_api_service_server::ServerApiService;
        let mut request = tonic::Request::new(pb::ServerApiRequest {
            body: serde_json::to_vec(&body).unwrap(),
            ..Default::default()
        });
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
        request
            .metadata_mut()
            .insert("munarium-uid", uid.parse().unwrap());
        match crate::grpc_api::ServerApiSvc::new(state)
            .seal(request)
            .await
        {
            Ok(response) => StatusCode::from_u16(response.get_ref().status as u16).unwrap(),
            Err(error) => match error.code() {
                tonic::Code::Unauthenticated => StatusCode::UNAUTHORIZED,
                tonic::Code::PermissionDenied => StatusCode::FORBIDDEN,
                tonic::Code::InvalidArgument => StatusCode::BAD_REQUEST,
                other => panic!("unexpected native response: {other:?}: {error}"),
            },
        }
    } else {
        crate::rest::router(state)
            .oneshot(
                axum::http::Request::post("/v1/evidence")
                    .header("authorization", format!("Bearer {bearer}"))
                    .header("x-munarium-uid", uid)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }
}

fn forged_and_expired(valid: &str) -> (String, String) {
    let mut claims = munarium_access::verify(SECRET, valid).unwrap();
    claims.lvl = 99;
    let mut pieces: Vec<String> = valid.split('.').map(str::to_owned).collect();
    pieces[1] = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&claims).unwrap());
    claims.exp = claims.iat - 120;
    (
        pieces.join("."),
        munarium_access::mint(SECRET, &claims).unwrap(),
    )
}

#[tokio::test]
async fn authority_rejections_create_no_evidence_or_grant_on_either_transport() {
    for native in [false, true] {
        let mut config = test_config();
        config.require_uid = true;
        if let AuthMode::Static(tokens) = &mut config.auth {
            tokens.push(("ro-token".into(), "tenant-default".into(), "ro".into()));
        }
        let state = AppState::new(config).await.unwrap();
        let raw = manifest(2, &["fin"]);
        let domain =
            serde_json::from_value::<munarium_core::evidence::EvidenceManifest>(raw.clone())
                .unwrap()
                .domain_key();
        let valid = evidence_token(2, &["fin"]);
        let (forged, expired) = forged_and_expired(&valid);
        let cross_tenant = munarium_access::issue(
            SECRET,
            "matrix",
            "other-tenant",
            2,
            vec!["fin".into()],
            vec!["evidence".into()],
            None,
            600,
            "other-tenant-token".into(),
        )
        .unwrap()
        .0;
        for (label, bearer, uid, status) in [
            ("forged", forged, "matrix", StatusCode::UNAUTHORIZED),
            ("expired", expired, "matrix", StatusCode::UNAUTHORIZED),
            (
                "wrong uid",
                valid.clone(),
                "someone-else",
                StatusCode::FORBIDDEN,
            ),
            (
                "missing scope",
                token(2, &["fin"], &["query"]),
                "matrix",
                StatusCode::FORBIDDEN,
            ),
            (
                "level",
                evidence_token(1, &["fin"]),
                "matrix",
                StatusCode::FORBIDDEN,
            ),
            (
                "compartment",
                evidence_token(2, &[]),
                "matrix",
                StatusCode::FORBIDDEN,
            ),
            ("tenant", cross_tenant, "matrix", StatusCode::BAD_REQUEST),
            (
                "read only",
                "ro-token".into(),
                "matrix",
                StatusCode::FORBIDDEN,
            ),
            (
                "management",
                "mgmt-token".into(),
                "matrix",
                StatusCode::FORBIDDEN,
            ),
        ] {
            assert_eq!(
                seal_through_transport(state.clone(), native, &bearer, uid, &raw).await,
                status,
                "{label}, native={native}"
            );
            // A grant seal registers its pending artifact atomically with its
            // upload grant. No domain record means no protected effect occurred.
            for tenant in ["tenant-default", "other-tenant"] {
                let mut scoped_manifest = raw.clone();
                scoped_manifest["tenant"] = json!(tenant);
                let scoped_domain = serde_json::from_value::<
                    munarium_core::evidence::EvidenceManifest,
                >(scoped_manifest)
                .unwrap()
                .domain_key();
                assert!(
                    state
                        .evidence()
                        .find_by_domain_key(tenant, &scoped_domain)
                        .await
                        .unwrap()
                        .is_none(),
                    "{label} created evidence, native={native}"
                );
            }
        }
        assert_eq!(
            seal_through_transport(state.clone(), native, &valid, "matrix", &raw).await,
            StatusCode::OK
        );
        assert!(state
            .evidence()
            .find_by_domain_key("tenant-default", &domain)
            .await
            .unwrap()
            .is_some());

        // Static rw and explicit development mode retain unrestricted clearance.
        for disabled in [false, true] {
            let mut config = test_config();
            if disabled {
                config.auth = AuthMode::Disabled;
            }
            let unrestricted = AppState::new(config).await.unwrap();
            assert_eq!(
                seal_through_transport(
                    unrestricted,
                    native,
                    "rw-token",
                    "operator",
                    &manifest(99, &["fin", "hr"])
                )
                .await,
                StatusCode::OK,
                "static/development compatibility: disabled={disabled}, native={native}"
            );
        }
    }
}

fn session_request(bearer: &str, uid: &str) -> tonic::Request<pb::CreateSessionRequest> {
    let mut request = tonic::Request::new(pb::CreateSessionRequest {
        runbook_name: "fictional-runbook".into(),
    });
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
    request
        .metadata_mut()
        .insert("munarium-uid", uid.parse().unwrap());
    request
}

#[tokio::test]
async fn typed_service_binds_uid_even_without_capture_middleware() {
    use pb::session_service_server::SessionService;
    let state = state().await;
    let service = crate::grpc_platform::SessionSvc {
        state: state.clone(),
    };
    let valid = token(2, &["fin"], &["query"]);
    let (forged, expired) = forged_and_expired(&valid);
    for (bearer, uid, code) in [
        (forged, "matrix", tonic::Code::Unauthenticated),
        (expired, "matrix", tonic::Code::Unauthenticated),
        (valid.clone(), "someone-else", tonic::Code::PermissionDenied),
        (
            evidence_token(2, &["fin"]),
            "matrix",
            tonic::Code::PermissionDenied,
        ),
        ("mgmt-token".into(), "matrix", tonic::Code::PermissionDenied),
    ] {
        let error = service
            .create_session(session_request(&bearer, uid))
            .await
            .unwrap_err();
        assert_eq!(error.code(), code, "{error}");
    }
    // An adjacent valid call reaches the authenticated context unchanged.
    let request = session_request(&valid, "matrix");
    let access = crate::grpc_platform::data_plane_access(&state, request.metadata(), "query")
        .await
        .unwrap();
    assert_eq!(access.uid, "matrix");
    assert_eq!(access.tenant_id, "tenant-default");
    assert!(!access.all_compartments);
    assert!(!access.has_scope("evidence"));
}

#[tokio::test]
async fn static_roles_and_disabled_mode_keep_their_existing_authority() {
    let mut config = test_config();
    if let AuthMode::Static(tokens) = &mut config.auth {
        tokens.push(("ro-token".into(), "tenant-default".into(), "ro".into()));
    }
    let state = AppState::new(config).await.unwrap();
    for bearer in ["rw-token", "ro-token"] {
        let request = session_request(bearer, "operator");
        let access = crate::grpc_platform::data_plane_access(&state, request.metadata(), "query")
            .await
            .unwrap();
        assert_eq!(access.uid, "operator");
        assert!(access.permits(i32::MAX, &["fin".into(), "hr".into()]));
        assert_eq!(access.has_scope("ingest"), bearer == "rw-token");
        let tenant = state.authenticate(Some(bearer)).unwrap();
        assert_eq!(tenant.require_rw().is_ok(), bearer == "rw-token");
        assert!(tenant.require_mgmt().is_err());
    }
    let management = state.authenticate(Some("mgmt-token")).unwrap();
    assert!(management.require_mgmt().is_ok());
    assert!(management.require_rw().is_err());

    let mut config = test_config();
    config.auth = AuthMode::Disabled;
    let development = AppState::new(config).await.unwrap();
    let principal = development.authenticate_principal(None).unwrap();
    let access = principal.access_ctx("anonymous").unwrap();
    assert!(access.permits(i32::MAX, &["fin".into()]));
    assert!(access.has_scope("ingest"));
    let tenant = development.authenticate(None).unwrap();
    assert!(tenant.require_rw().is_ok());
    assert!(tenant.require_mgmt().is_ok());
}
