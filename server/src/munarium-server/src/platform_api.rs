// SPDX-License-Identifier: Apache-2.0
//! Platform profile: operator enrollment, durable authority and independently retained
//! restore fencing. A transport-authenticated peer is mandatory on every operation.
use crate::{error::ApiError, state::AppState};
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use munarium_core::{platform::canonical_record, platform_authority::*, KernelError};
use munarium_store_mem::platform_authority::MemAuthorityStore;
use munarium_store_pg::{platform_authority::PgAuthorityStore, PgStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    path::PathBuf,
    sync::Arc,
};

type Result<T> = std::result::Result<T, KernelError>;
fn unavailable() -> KernelError {
    KernelError::Forbidden("platform authority unavailable or restore-fenced".into())
}
fn io_error(_: std::io::Error) -> KernelError {
    KernelError::Storage("platform checkpoint I/O failed".into())
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub service: String,
    pub tenants: BTreeSet<String>,
    pub scopes: BTreeSet<String>,
}
/// Constructed by the mTLS listener, never from request metadata.
#[derive(Clone, Debug)]
pub struct AuthenticatedPeer(pub Peer);

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enrollment {
    pub authority: AuthorityConfig,
    pub checkpoint_file: PathBuf,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformConfig {
    pub certificate_file: PathBuf,
    pub private_key_file: PathBuf,
    pub client_ca_file: PathBuf,
    /// SHA-256 of the leaf DER certificate -> explicitly enrolled service.
    pub peers: BTreeMap<String, Peer>,
    pub tenants: BTreeMap<String, Enrollment>,
}
impl PlatformConfig {
    pub fn from_env() -> std::result::Result<Option<Self>, String> {
        let file = std::env::var("MUNARIUM_PLATFORM_CONFIG_FILE").ok();
        match std::env::var("MUNARIUM_AUTHORITY_PROFILE").as_deref().unwrap_or("legacy") {
            "legacy" if file.is_none() => Ok(None),
            "platform-v1" => {
                let raw = std::fs::read(file.ok_or("platform-v1 requires MUNARIUM_PLATFORM_CONFIG_FILE")?)
                    .map_err(|_| "cannot read platform configuration")?;
                let config: Self = serde_json::from_slice(&raw).map_err(|_| "invalid platform configuration")?;
                config.validate().map_err(|_| "invalid platform enrollment")?;
                Ok(Some(config))
            }
            _ => Err("authority profile must be legacy without platform configuration, or explicit platform-v1".into()),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.tenants.is_empty() || self.peers.is_empty() {
            return Err(unavailable());
        }
        let mut files = BTreeSet::new();
        for (tenant, enrollment) in &self.tenants {
            enrollment.authority.validate()?;
            if tenant.starts_with("platform-records:")
                || tenant != &enrollment.authority.tenant
                || !enrollment.checkpoint_file.is_absolute()
                || !files.insert(&enrollment.checkpoint_file)
            {
                return Err(unavailable());
            }
        }
        for (pin, peer) in &self.peers {
            if pin.len() != 64
                || !pin
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || peer.service.is_empty()
                || peer.tenants.is_empty()
                || peer.scopes.is_empty()
                || peer.tenants.iter().any(|t| !self.tenants.contains_key(t))
                || peer
                    .scopes
                    .iter()
                    .any(|s| !matches!(s.as_str(), "read" | "record" | "govern"))
            {
                return Err(unavailable());
            }
        }
        Ok(())
    }
    pub fn peer(&self, certificate: &[u8]) -> Result<AuthenticatedPeer> {
        self.peers
            .get(&hex::encode(Sha256::digest(certificate)))
            .cloned()
            .map(AuthenticatedPeer)
            .ok_or_else(unavailable)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    attestation_digest: String,
    next_revision: String,
    next_fence: AuthorityFence,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    fence: AuthorityFence,
    revision: String,
    prepared: Option<Prepared>,
}
fn read_checkpoint(file: &mut File) -> Result<Checkpoint> {
    if file.metadata().map_err(io_error)?.len() > 65_536 {
        return Err(unavailable());
    }
    file.rewind().map_err(io_error)?;
    let mut raw = Vec::new();
    file.take(65_537).read_to_end(&mut raw).map_err(io_error)?;
    serde_json::from_slice(&raw).map_err(|_| unavailable())
}
fn write_checkpoint(file: &mut File, checkpoint: &Checkpoint) -> Result<()> {
    let raw = serde_json::to_vec(checkpoint).map_err(|_| unavailable())?;
    file.rewind().map_err(io_error)?;
    file.write_all(&raw).map_err(io_error)?;
    file.set_len(raw.len() as u64).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}
fn locked_checkpoint(path: &PathBuf) -> Result<File> {
    // Never create or repair a missing checkpoint during serving. Partial writes
    // after a crash fail parsing and require operator reconciliation.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(io_error)?;
    file.try_lock().map_err(|_| unavailable())?;
    Ok(file)
}
fn reconcile(file: &mut File, checkpoint: &mut Checkpoint, state: &AuthorityState) -> Result<()> {
    if let Some(prepared) = &checkpoint.prepared {
        if state.fence() == prepared.next_fence && state.revision == prepared.next_revision {
            checkpoint.fence = prepared.next_fence.clone();
            checkpoint.revision = prepared.next_revision.clone();
            checkpoint.prepared = None;
            write_checkpoint(file, checkpoint)?;
        }
    }
    if state.fence() != checkpoint.fence || state.revision != checkpoint.revision {
        return Err(unavailable());
    }
    Ok(())
}

pub struct TenantAuthority {
    store: Arc<dyn AuthorityStore>,
    checkpoint: PathBuf,
    pub(crate) serial: tokio::sync::Mutex<()>,
}
impl TenantAuthority {
    pub async fn snapshot(&self) -> Result<AuthorityState> {
        let _serial = self.serial.lock().await;
        Ok(self.fenced_snapshot().await?.1)
    }
    /// The caller holds serial admission and keeps the returned file locked until its effect commits.
    pub(crate) async fn fenced_snapshot(&self) -> Result<(File, AuthorityState)> {
        let mut file = locked_checkpoint(&self.checkpoint)?;
        let mut checkpoint = read_checkpoint(&mut file)?;
        let state = self.store.snapshot().await?;
        reconcile(&mut file, &mut checkpoint, &state)?;
        if checkpoint.prepared.is_some() {
            return Err(unavailable());
        }
        Ok((file, state))
    }
    pub async fn apply(
        &self,
        peer: &AuthenticatedPeer,
        signed: &str,
        artifact: &GovernanceArtifact,
        now: i64,
    ) -> Result<AuthorityReceipt> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let _serial = self.serial.lock().await;
        let mut file = locked_checkpoint(&self.checkpoint)?;
        let mut checkpoint = read_checkpoint(&mut file)?;
        let state = self.store.snapshot().await?;
        reconcile(&mut file, &mut checkpoint, &state)?;
        let admission = AuthorityAdmission {
            tenant: state.config.tenant.clone(),
            peer_service: peer.0.service.clone(),
            now,
            fence: checkpoint.fence.clone(),
        };
        let (_, payload) = signed.split_once('.').ok_or_else(unavailable)?;
        let (payload, _) = payload.split_once('.').ok_or_else(unavailable)?;
        if signed.len() > 16_384 {
            return Err(unavailable());
        }
        let payload = URL_SAFE_NO_PAD.decode(payload).map_err(|_| unavailable())?;
        let payload: AuthorityAttestation =
            serde_json::from_value(canonical_record(&payload)?).map_err(|_| unavailable())?;
        if payload.expected_head < state.head && checkpoint.prepared.is_none() {
            return self.store.apply(&admission, signed, artifact).await;
        }
        let (next, receipt) = transition(&state, &admission, signed, artifact, None)?;
        if let Some(prepared) = &checkpoint.prepared {
            if prepared.attestation_digest != receipt.attestation_digest
                || prepared.next_revision != next.revision
            {
                return Err(unavailable());
            }
        } else {
            checkpoint.prepared = Some(Prepared {
                attestation_digest: receipt.attestation_digest.clone(),
                next_revision: next.revision.clone(),
                next_fence: next.fence(),
            });
            // The external fence is durable BEFORE the database may activate anything.
            write_checkpoint(&mut file, &checkpoint)?;
        }
        let receipt = self.store.apply(&admission, signed, artifact).await?;
        checkpoint.fence = next.fence();
        checkpoint.revision = next.revision;
        checkpoint.prepared = None;
        write_checkpoint(&mut file, &checkpoint)?;
        Ok(receipt)
    }
}

pub struct PlatformRuntime {
    tenants: BTreeMap<String, TenantAuthority>,
    pub(crate) records: BTreeMap<String, crate::platform_records::Records>,
}
impl PlatformRuntime {
    pub async fn open(config: &PlatformConfig, postgres: Option<&PgStore>) -> Result<Self> {
        config.validate()?;
        let mut tenants = BTreeMap::new();
        let mut records = BTreeMap::new();
        for (tenant, enrollment) in &config.tenants {
            let store: Arc<dyn AuthorityStore> = match postgres {
                Some(pg) => Arc::new(PgAuthorityStore::new(pg.with_tenant(tenant).await?)),
                None => Arc::new(MemAuthorityStore::new(tenant)),
            };
            let state = store.enroll(enrollment.authority.clone()).await?;
            let mut file = locked_checkpoint(&enrollment.checkpoint_file)?;
            let mut checkpoint = read_checkpoint(&mut file)?;
            reconcile(&mut file, &mut checkpoint, &state)?;
            records.insert(
                tenant.clone(),
                crate::platform_records::Records::open(tenant, &state.config.deployment, postgres)
                    .await?,
            );
            tenants.insert(
                tenant.clone(),
                TenantAuthority {
                    store,
                    checkpoint: enrollment.checkpoint_file.clone(),
                    serial: tokio::sync::Mutex::new(()),
                },
            );
        }
        Ok(Self { tenants, records })
    }
    pub fn tenant(
        &self,
        peer: &AuthenticatedPeer,
        tenant: &str,
        scope: &str,
    ) -> Result<&TenantAuthority> {
        if !peer.0.tenants.contains(tenant) || !peer.0.scopes.contains(scope) {
            return Err(unavailable());
        }
        self.tenants.get(tenant).ok_or_else(unavailable)
    }
}

/// Local operator ceremony only. Refuses to recreate checkpoints for used authority.
pub async fn initialize_checkpoints(
    config: &PlatformConfig,
    postgres: Option<&PgStore>,
) -> Result<()> {
    config.validate()?;
    for (tenant, enrollment) in &config.tenants {
        let store: Arc<dyn AuthorityStore> = match postgres {
            Some(pg) => Arc::new(PgAuthorityStore::new(pg.with_tenant(tenant).await?)),
            None => Arc::new(MemAuthorityStore::new(tenant)),
        };
        let state = store.enroll(enrollment.authority.clone()).await?;
        if state.head != 0 || state.bootstrap_retired {
            return Err(unavailable());
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&enrollment.checkpoint_file)
            .map_err(io_error)?;
        file.try_lock().map_err(|_| unavailable())?;
        write_checkpoint(
            &mut file,
            &Checkpoint {
                fence: state.fence(),
                revision: state.revision,
                prepared: None,
            },
        )?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityRequest {
    pub attestation: String,
    pub artifact: GovernanceArtifact,
}
#[utoipa::path(get, path="/v1/platform/{tenant}/authority", params(("tenant"=String,Path)), responses((status=200,body=Value),(status=403,description="authority unavailable or peer refused")), security(("platformMtls"=[])), tag="platform")]
pub async fn get_platform_authority(
    State(state): State<Arc<AppState>>,
    Path(tenant): Path<String>,
    peer: Option<Extension<AuthenticatedPeer>>,
) -> std::result::Result<Json<Value>, ApiError> {
    let peer = peer.ok_or_else(unavailable)?.0;
    let authority = state
        .platform
        .as_ref()
        .ok_or_else(unavailable)?
        .tenant(&peer, &tenant, "read")?;
    Ok(Json(
        serde_json::to_value(authority.snapshot().await?).map_err(|_| unavailable())?,
    ))
}
#[utoipa::path(post, path="/v1/platform/{tenant}/authority", params(("tenant"=String,Path)), request_body=Value, responses((status=200,body=Value),(status=403,description="governance authority refused"),(status=409,description="nonce or prior-state conflict")), security(("platformMtls"=[])), tag="platform")]
pub async fn transition_platform_authority(
    State(state): State<Arc<AppState>>,
    Path(tenant): Path<String>,
    peer: Option<Extension<AuthenticatedPeer>>,
    crate::rest::ProblemJson(body): crate::rest::ProblemJson<AuthorityRequest>,
) -> std::result::Result<Json<Value>, ApiError> {
    let peer = peer.ok_or_else(unavailable)?.0;
    let authority = state
        .platform
        .as_ref()
        .ok_or_else(unavailable)?
        .tenant(&peer, &tenant, "govern")?;
    let receipt = authority
        .apply(
            &peer,
            &body.attestation,
            &body.artifact,
            chrono::Utc::now().timestamp(),
        )
        .await?;
    Ok(Json(
        serde_json::to_value(receipt).map_err(|_| unavailable())?,
    ))
}

#[cfg(test)]
#[path = "platform_api_tests.rs"]
mod tests;
