// SPDX-License-Identifier: Apache-2.0
//! Experimental verification of the hub's Ed25519 principal envelope.

use crate::{
    encoding::{canonical, current, decode, digest, identifier},
    error::Error,
};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    kid: String,
    typ: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    schema_version: u8,
    deployment: String,
    tenant: String,
    issuer: String,
    audience: String,
    origin: String,
    actor: String,
    origin_kind: String,
    service: String,
    purpose: String,
    scopes: Vec<String>,
    resources: Vec<String>,
    iat: i64,
    nbf: i64,
    exp: i64,
    parent_digest: Option<String>,
    bootstrap: Option<serde_json::Value>,
}

/// An operator-provisioned verification key. No remote key discovery is performed.
#[derive(Clone)]
pub struct TrustedKey {
    /// Public key bytes, never signing material.
    pub public_key: [u8; 32],
    /// Exact issuer authorized to use this key.
    pub issuer: String,
    /// Whether current provisioning authorizes decision assertions under this key.
    pub decision: bool,
}

/// A current Registry registration for one delegation edge.
#[derive(Clone)]
pub struct Delegation {
    /// Unique registration identifier, never selected by an untrusted chain.
    pub registration_id: String,
    /// Deployment owning this registration.
    pub deployment: String,
    /// Tenant owning this registration.
    pub tenant: String,
    /// Original actor kind, agent or service.
    pub origin_kind: String,
    /// Intended receiving service.
    pub audience: String,
    /// Current registered task digest.
    pub task_digest: String,
    /// Current target policy digest.
    pub policy_digest: String,
    /// Origin actor.
    pub origin: String,
    /// Delegating actor.
    pub from: String,
    /// Receiving actor.
    pub to: String,
    /// Authenticated presenting service.
    pub service: String,
    /// Permitted scopes.
    pub scopes: Vec<String>,
    /// Permitted resources.
    pub resources: Vec<String>,
    /// Earliest validity time.
    pub not_before: i64,
    /// Exclusive expiry.
    pub expires: i64,
    /// Maximum total number of edges.
    pub maximum_depth: usize,
}

/// Trusted per-request context supplied by the authenticated service adapter.
///
/// Never deserialize this context from the principal's request. Keys and registrations
/// must come from a current authoritative snapshot for this tenant, task and policy.
#[derive(Clone)]
pub struct Trust {
    /// Expected deployment.
    pub deployment: String,
    /// Expected tenant.
    pub tenant: String,
    /// Actual recipient service.
    pub audience: String,
    /// Actual authenticated transport peer.
    pub peer_service: String,
    /// Trusted clock in Unix seconds, with at most two seconds uncertainty.
    pub now: i64,
    /// False if authority is unavailable, stale or quarantined.
    pub available: bool,
    /// Current, non-retired keys indexed by key identifier.
    pub keys: BTreeMap<String, TrustedKey>,
    /// Current registered edges, already selected for this task/policy and origin kind.
    pub delegations: Vec<Delegation>,
    /// Current task revision obtained from Registry.
    pub task_digest: String,
    /// Current target policy revision.
    pub policy_digest: String,
    /// Earliest validity of the intersection of task and policy authority.
    pub not_before: i64,
    /// Exclusive expiry of task/policy authority.
    pub expires: i64,
    /// Intersection of task and policy scopes.
    pub scopes: Vec<String>,
    /// Intersection of task and policy resources.
    pub resources: Vec<String>,
    /// Registry/task depth limit, at most four.
    pub maximum_depth: usize,
}

/// Verified non-human authority. Construction is restricted to signature verification.
/// Deliberately has neither deserialization nor diagnostic formatting implementations.
pub struct Principal {
    leaf: Payload,
    fingerprint: String,
    policy_digest: String,
    task_digest: String,
}

impl Principal {
    /// Current policy under which the principal was verified.
    pub fn policy_digest(&self) -> &str {
        &self.policy_digest
    }
    /// Current task under which the principal was verified.
    pub fn task_digest(&self) -> &str {
        &self.task_digest
    }
    /// Verified tenant.
    pub fn tenant(&self) -> &str {
        &self.leaf.tenant
    }
    /// Verified deployment.
    pub fn deployment(&self) -> &str {
        &self.leaf.deployment
    }
    /// Verified leaf actor.
    pub fn actor(&self) -> &str {
        &self.leaf.actor
    }
    /// Verified originating actor; never a fabricated human.
    pub fn origin(&self) -> &str {
        &self.leaf.origin
    }
    /// Verified originating kind: agent or service in this profile.
    pub fn origin_kind(&self) -> &str {
        &self.leaf.origin_kind
    }
    /// Exact recipient service.
    pub fn audience(&self) -> &str {
        &self.leaf.audience
    }
    /// Authenticated presenting service bound by the assertion.
    pub fn service(&self) -> &str {
        &self.leaf.service
    }
    /// Effective signed scopes.
    pub fn scopes(&self) -> &[String] {
        &self.leaf.scopes
    }
    /// Effective signed resources.
    pub fn resources(&self) -> &[String] {
        &self.leaf.resources
    }
    /// Digest of the complete signed leaf envelope.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    /// Exclusive credential expiry.
    pub fn expires(&self) -> i64 {
        self.leaf.exp
    }
    /// Whether the signed leaf permits this resource and scope.
    pub fn permits(&self, scope: &str, resource: &str) -> bool {
        self.leaf.scopes.iter().any(|s| s == scope)
            && self.leaf.resources.iter().any(|r| r == resource)
    }
}

fn subset(child: &[String], parent: &[String]) -> bool {
    !child.is_empty()
        && child.iter().all(|v| parent.contains(v))
        && child.iter().collect::<BTreeSet<_>>().len() == child.len()
}

/// Verify a complete signed chain, including current registered transitions.
///
/// Human/bootstrap authority and audience exchange are intentionally unsupported.
/// All failures discard raw input and cryptographic/provider diagnostics.
pub fn verify(chain: &[String], trust: &Trust) -> Result<Principal, Error> {
    if !trust.available {
        return Err(Error::Unavailable);
    }
    if !crate::encoding::valid_digest(&trust.task_digest)
        || !crate::encoding::valid_digest(&trust.policy_digest)
        || !current(trust.not_before, trust.expires, trust.now)
    {
        return Err(Error::Identity);
    }
    if chain.is_empty()
        || chain.len() > 5
        || chain.len() - 1 > trust.maximum_depth
        || trust.maximum_depth > 4
        || chain.iter().map(String::len).sum::<usize>() > 65536
    {
        return Err(Error::Identity);
    }
    let mut previous: Option<Payload> = None;
    let mut previous_digest = None;
    let mut actors = BTreeSet::new();
    let mut registration_ids = BTreeSet::new();
    for token in chain {
        let parts: Vec<_> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(Error::Identity);
        }
        let header: Header =
            serde_json::from_value(canonical(&decode(parts[0])?)?).map_err(|_| Error::Identity)?;
        if header.alg != "Ed25519" || header.typ != "munarium-principal+jws" {
            return Err(Error::Identity);
        }
        let key = trust.keys.get(&header.kid).ok_or(Error::Identity)?;
        if !key.decision {
            return Err(Error::Identity);
        }
        let signature = Signature::from_slice(&decode(parts[2])?).map_err(|_| Error::Identity)?;
        VerifyingKey::from_bytes(&key.public_key)
            .map_err(|_| Error::Identity)?
            .verify_strict(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .map_err(|_| Error::Identity)?;
        let value = canonical(&decode(parts[1])?)?;
        if value.as_object().is_none_or(|v| {
            v.len() != 17 || !v.contains_key("bootstrap") || !v.contains_key("parent_digest")
        }) {
            return Err(Error::Identity);
        }
        let p: Payload = serde_json::from_value(value).map_err(|_| Error::Identity)?;
        if p.schema_version != 1
            || p.purpose != "decision"
            || p.bootstrap.is_some()
            || !matches!(p.origin_kind.as_str(), "agent" | "service")
            || p.issuer != key.issuer
            || p.tenant != trust.tenant
            || p.deployment != trust.deployment
            || p.audience != trust.audience
            || p.iat > p.nbf
            || p.iat < 0
            || !current(p.nbf, p.exp, trust.now)
            || p.nbf < trust.not_before
            || p.exp > trust.expires
            || p.exp.checked_sub(p.iat).is_none_or(|l| l > 60)
            || ![
                &p.deployment,
                &p.tenant,
                &p.issuer,
                &p.audience,
                &p.origin,
                &p.actor,
                &p.service,
            ]
            .iter()
            .all(|v| identifier(v))
            || p.scopes.len() > 3
            || !p
                .scopes
                .iter()
                .all(|s| matches!(s.as_str(), "read" | "evaluate" | "propose"))
            || p.resources.len() > 32
            || !p.resources.iter().all(|r| identifier(r))
            || !subset(&p.scopes, &trust.scopes)
            || !subset(&p.resources, &trust.resources)
            || !actors.insert(p.actor.clone())
        {
            return Err(Error::Identity);
        }
        if let Some(parent) = previous.as_ref() {
            if p.origin != parent.origin
                || p.origin_kind != parent.origin_kind
                || p.parent_digest != previous_digest
                || p.nbf < parent.nbf
                || p.exp > parent.exp
                || !subset(&p.scopes, &parent.scopes)
                || !subset(&p.resources, &parent.resources)
            {
                return Err(Error::Identity);
            }
            let registrations: Vec<_> = trust
                .delegations
                .iter()
                .filter(|d| {
                    d.origin == p.origin
                        && d.deployment == trust.deployment
                        && d.tenant == trust.tenant
                        && d.origin_kind == p.origin_kind
                        && d.audience == trust.audience
                        && d.task_digest == trust.task_digest
                        && d.policy_digest == trust.policy_digest
                        && d.from == parent.actor
                        && d.to == p.actor
                        && d.service == p.service
                })
                .collect();
            if registrations.len() != 1 {
                return Err(Error::Identity);
            }
            let d = registrations[0];
            if !identifier(&d.registration_id)
                || !registration_ids.insert(&d.registration_id)
                || !current(d.not_before, d.expires, trust.now)
                || p.nbf < d.not_before
                || p.exp > d.expires
                || chain.len() - 1 > d.maximum_depth
                || d.maximum_depth > 4
                || !subset(&p.scopes, &d.scopes)
                || !subset(&p.resources, &d.resources)
            {
                return Err(Error::Identity);
            }
        } else if p.parent_digest.is_some() || p.actor != p.origin {
            return Err(Error::Identity);
        }
        previous_digest = Some(digest(token.as_bytes()));
        previous = Some(p);
    }
    let leaf = previous.ok_or(Error::Identity)?;
    if leaf.service != trust.peer_service {
        return Err(Error::Identity);
    }
    Ok(Principal {
        policy_digest: trust.policy_digest.clone(),
        task_digest: trust.task_digest.clone(),
        leaf,
        fingerprint: previous_digest.ok_or(Error::Identity)?,
    })
}
