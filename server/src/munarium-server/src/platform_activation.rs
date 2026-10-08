// SPDX-License-Identifier: Apache-2.0
//! Non-agent coordinator admission for Server's Stage 2 activation participant.
use crate::{
    error::ApiError,
    platform_api::{AuthenticatedPeer, PlatformConfig},
    state::AppState,
};
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use munarium_core::{
    platform::canonical_record, platform_actions::*, platform_authority::AuthorityState,
    KernelError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::Arc, time::Duration};

fn denied() -> KernelError {
    KernelError::Forbidden("platform activation authority refused".into())
}
fn unavailable() -> KernelError {
    KernelError::Storage("platform activation dependency unavailable".into())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    scope: Scope,
    coordinator: String,
    readers: BTreeSet<String>,
    initial_epoch: u64,
    initial_artifact_set_digest: String,
    stream_id: String,
    council_endpoint: String,
    gate_endpoint: String,
    registry_endpoint: String,
}
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
enum Operation {
    Apply { transition: String },
    Lookup { transition_id: String },
    Head,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    tenant: String,
    action: Operation,
}

fn policy(state: &AuthorityState) -> Result<(Policy, ActionPolicy), KernelError> {
    let bindings = &state.artifact.as_ref().ok_or_else(denied)?.bindings;
    let p: Policy = serde_json::from_value(
        bindings
            .get(&format!("stage2:{}", state.config.audience))
            .ok_or_else(denied)?
            .clone(),
    )
    .map_err(|_| denied())?;
    let a: ActionPolicy = serde_json::from_value(
        bindings
            .get(&format!("action-records:{}", state.config.audience))
            .ok_or_else(denied)?
            .clone(),
    )
    .map_err(|_| denied())?;
    if p.scope != a.scope
        || p.scope.tenant != state.config.tenant
        || p.scope.deployment != state.config.deployment
    {
        return Err(denied());
    }
    a.validate()?;
    Ok((p, a))
}
fn client(config: &PlatformConfig) -> Result<reqwest::Client, KernelError> {
    let mut pem = std::fs::read(&config.certificate_file).map_err(|_| unavailable())?;
    pem.extend(std::fs::read(&config.private_key_file).map_err(|_| unavailable())?);
    let identity = reqwest::Identity::from_pem(&pem).map_err(|_| unavailable())?;
    let ca = reqwest::Certificate::from_pem(
        &std::fs::read(&config.client_ca_file).map_err(|_| unavailable())?,
    )
    .map_err(|_| unavailable())?;
    reqwest::Client::builder()
        // Current participant adapters serve HTTP/1.1; native Server gRPC remains HTTP/2.
        .http1_only()
        .identity(identity)
        .add_root_certificate(ca)
        .tls_built_in_root_certs(false)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| unavailable())
}
async fn post(
    client: &reqwest::Client,
    endpoint: &str,
    path: &str,
    body: Value,
) -> Result<Value, KernelError> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| denied())?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(denied());
    }
    let mut response = client
        .post(url.join(path).map_err(|_| denied())?)
        .json(&body)
        .send()
        .await
        .map_err(|_| {
            KernelError::Storage(format!(
                "platform activation dependency transport failed: {path}"
            ))
        })?;
    if !response.status().is_success() {
        return Err(KernelError::Storage(format!(
            "platform activation dependency refused: {path} ({})",
            response.status().as_u16()
        )));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > 262144 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

#[utoipa::path(post,path="/v1/platform/{tenant}/activation",params(("tenant"=String,Path)),request_body=Value,
    security(("platformMtls"=[])),responses((status=200,body=Value),(status=403,description="activation authority refused"),
    (status=422,description="immutable transition or expected head conflict")),tag="platform")]
pub async fn platform_activation(
    State(state): State<Arc<AppState>>,
    Path(tenant): Path<String>,
    peer: Option<Extension<AuthenticatedPeer>>,
    crate::rest::ProblemJson(request): crate::rest::ProblemJson<Request>,
) -> Result<Json<Value>, ApiError> {
    let peer = peer.ok_or_else(denied)?.0;
    if tenant != request.tenant {
        return Err(denied().into());
    }
    let runtime = state.platform.as_ref().ok_or_else(denied)?;
    let _permit = runtime
        .activation_permits
        .try_acquire()
        .map_err(|_| unavailable())?;
    // mTLS enrollment and a current signed coordinator binding are both required.
    let authority = runtime.tenant(&peer, &tenant, "read")?;
    let snapshot = authority.snapshot().await?;
    let (policy, actions) = policy(&snapshot)?;
    if peer.0.service != policy.coordinator && !policy.readers.contains(&peer.0.service) {
        return Err(denied().into());
    }
    let enrollment = ActivationEnrollment {
        initial_epoch: policy.initial_epoch,
        initial_artifact_set_digest: policy.initial_artifact_set_digest.clone(),
        service: snapshot.config.audience.clone(),
        coordinator: policy.coordinator.clone(),
        stream_id: policy.stream_id.clone(),
    };
    let records = runtime.records.get(&tenant).ok_or_else(denied)?;
    let ledger = ActionLedger::new(records.store.as_ref(), &records.version, &actions);
    let proof = if let Operation::Apply { transition } = &request.action {
        if peer.0.service != policy.coordinator {
            return Err(denied().into());
        }
        let t = canonical_record(transition.as_bytes())?;
        validate_action_record(&t)?;
        if t["type"] != "activation" || t["scope"] != policy.scope.value() {
            return Err(denied().into());
        }
        let id = t["transition"]["id"].as_str().ok_or_else(denied)?;
        let client = client(&runtime.config)?;
        let lookup = json!({"tenant":tenant,"action":{"operation":"lookup","transition_id":id}});
        // No root authority/checkpoint lock across callbacks to services that read Server.
        let ratified = post(
            &client,
            &policy.council_endpoint,
            "/v1/transitions",
            lookup.clone(),
        )
        .await?;
        let pause = post(
            &client,
            &policy.gate_endpoint,
            "/v1/actions",
            json!({"tenant":tenant,"action":{"operation":"pause-lookup","transition_id":id}}),
        )
        .await?;
        let gate_head = post(
            &client,
            &policy.gate_endpoint,
            "/v1/actions",
            json!({"tenant":tenant,"action":{"operation":"activation-head"}}),
        )
        .await?;
        let registry_receipt =
            post(&client, &policy.registry_endpoint, "/v1/activation", lookup).await?;
        let registry_head = post(
            &client,
            &policy.registry_endpoint,
            "/v1/activation",
            json!({"tenant":tenant,"action":{"operation":"head"}}),
        )
        .await?;
        Some((
            t,
            ActivationEvidence {
                ratified,
                pause,
                gate_head,
                registry_receipt,
                registry_head,
                authority_revision: snapshot.revision.clone(),
                now: 0,
            },
        ))
    } else {
        None
    };
    let _serial = authority.serial.lock().await;
    let (_fence, current) = authority.fenced_snapshot().await?;
    if current != snapshot {
        return Err(denied().into());
    }
    let result = match request.action {
        Operation::Head => ledger.activation_head(&enrollment).await?,
        Operation::Lookup { transition_id } => {
            ledger
                .activation_lookup(&enrollment, &transition_id)
                .await?
        }
        Operation::Apply { .. } => {
            let (t, mut proof) = proof.ok_or_else(denied)?;
            proof.now = chrono::Utc::now()
                .timestamp()
                .try_into()
                .map_err(|_| denied())?;
            ledger.apply_activation(&enrollment, &t, &proof).await?
        }
    };
    Ok(Json(result))
}
