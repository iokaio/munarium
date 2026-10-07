// SPDX-License-Identifier: Apache-2.0
//! Adapt current operator-governed policy to decision verification.
//! Only the signed chain is request input. Clock, recipient, key and task selection are local.
use crate::error::Error;
use crate::principal::{Delegation, Trust, TrustedKey};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    public_key: String,
    issuer: String,
    decision: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Authority {
    digest: String,
    nbf: i64,
    exp: i64,
    scopes: Vec<String>,
    resources: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerPolicy {
    task: Authority,
    policy: Authority,
    maximum_depth: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    schema_version: u8,
    registration_id: String,
    deployment: String,
    tenant: String,
    origin: String,
    origin_kind: String,
    from_actor: String,
    to_actor: String,
    presenter_service: String,
    audience: String,
    task_digest: String,
    policy_digest: String,
    scopes: Vec<String>,
    resources: Vec<String>,
    max_depth: usize,
    nbf: i64,
    exp: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityPolicy {
    keys: BTreeMap<String, Key>,
    peers: BTreeMap<String, PeerPolicy>,
    registrations: Vec<Registration>,
}
fn denied() -> Error {
    Error::Identity
}

/// Select only current operator-admitted policy. This function does not fetch or authenticate it.
/// Callers supply deployment, tenant, recipient, actual peer and clock independently of request data.
pub fn current(
    value: &serde_json::Value,
    deployment: &str,
    tenant: &str,
    audience: &str,
    peer: &str,
    now: i64,
) -> Result<Trust, Error> {
    let policy: IdentityPolicy = serde_json::from_value(value.clone()).map_err(|_| denied())?;
    if policy.keys.len() > 32 || policy.registrations.len() > 128 {
        return Err(denied());
    }
    let context = policy.peers.get(peer).ok_or_else(denied)?;
    let mut keys = BTreeMap::new();
    for (kid, key) in policy.keys {
        let bytes = URL_SAFE_NO_PAD
            .decode(&key.public_key)
            .map_err(|_| denied())?;
        if URL_SAFE_NO_PAD.encode(&bytes) != key.public_key {
            return Err(denied());
        }
        keys.insert(
            kid,
            TrustedKey {
                public_key: bytes.try_into().map_err(|_| denied())?,
                issuer: key.issuer,
                decision: key.decision,
            },
        );
    }
    let mut registrations = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for r in policy.registrations {
        if r.schema_version != 1 || !ids.insert(r.registration_id.clone()) {
            return Err(denied());
        }
        registrations.push(Delegation {
            registration_id: r.registration_id,
            deployment: r.deployment,
            tenant: r.tenant,
            origin: r.origin,
            origin_kind: r.origin_kind,
            from: r.from_actor,
            to: r.to_actor,
            service: r.presenter_service,
            audience: r.audience,
            task_digest: r.task_digest,
            policy_digest: r.policy_digest,
            scopes: r.scopes,
            resources: r.resources,
            not_before: r.nbf,
            expires: r.exp,
            maximum_depth: r.max_depth,
        });
    }
    let trust = Trust {
        deployment: deployment.into(),
        tenant: tenant.into(),
        audience: audience.into(),
        peer_service: peer.into(),
        now,
        available: true,
        keys,
        delegations: registrations,
        task_digest: context.task.digest.clone(),
        policy_digest: context.policy.digest.clone(),
        not_before: context.task.nbf.max(context.policy.nbf),
        expires: context.task.exp.min(context.policy.exp),
        scopes: context
            .task
            .scopes
            .iter()
            .filter(|v| context.policy.scopes.contains(v))
            .cloned()
            .collect(),
        resources: context
            .task
            .resources
            .iter()
            .filter(|v| context.policy.resources.contains(v))
            .cloned()
            .collect(),
        maximum_depth: context.maximum_depth,
    };
    Ok(trust)
}
