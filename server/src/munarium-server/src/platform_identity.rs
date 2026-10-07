// SPDX-License-Identifier: Apache-2.0
//! Bind the current fenced authority state and actual transport peer to Warden verification.
use crate::platform_api::AuthenticatedPeer;
use munarium_core::{platform_authority::AuthorityState, KernelError};
use munarium_platform_identity::{
    policy,
    principal::{self, Principal},
};
fn denied() -> KernelError {
    KernelError::Forbidden("platform decision identity refused".into())
}
pub fn verify(
    state: &AuthorityState,
    peer: &AuthenticatedPeer,
    chain: &[String],
    now: i64,
) -> Result<Principal, KernelError> {
    if !peer.0.tenants.contains(&state.config.tenant) {
        return Err(denied());
    }
    let binding = state
        .artifact
        .as_ref()
        .ok_or_else(denied)?
        .bindings
        .get(&format!("identity:{}", state.config.audience))
        .ok_or_else(denied)?;
    let trust = policy::current(
        binding,
        &state.config.deployment,
        &state.config.tenant,
        &state.config.audience,
        &peer.0.service,
        now,
    )
    .map_err(|_| denied())?;
    principal::verify(chain, &trust).map_err(|_| denied())
}
