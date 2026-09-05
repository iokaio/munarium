// SPDX-License-Identifier: Apache-2.0
//! `MemEvidenceStore` — the in-memory evidence plane.
//!
//! Same contract as the Postgres store, no I/O. It exists so the in-process
//! conformance tier covers the whole plane: an evidence scenario that only
//! ever ran against Postgres would be a scenario most contributors never run.
//!
//! The interesting part is that the atomicity the Postgres store gets from a
//! `UNIQUE` index and a conditional `UPDATE` has to be *written* here, under
//! one lock. Two places matter: registration must resolve the domain key and
//! insert without a gap (or a concurrent double-seal mints two artifacts), and
//! spending a grant must check-and-mark in one step (or a leaked grant is
//! usable twice). Both are done while holding the write lock.

use std::collections::HashMap;

use async_trait::async_trait;
use munarium_core::evidence::{
    EvidenceAccess, EvidenceArtifact, EvidenceGrant, EvidenceStore, SealOutcome,
};
use munarium_core::{KernelError, Result};
use tokio::sync::RwLock;

#[derive(Default)]
struct State {
    /// (tenant, evidence_id) -> artifact
    artifacts: HashMap<(String, String), EvidenceArtifact>,
    /// (tenant, domain_key) -> evidence_id
    by_domain_key: HashMap<(String, String), String>,
    /// (tenant, grant_id) -> grant
    grants: HashMap<(String, String), EvidenceGrant>,
    /// (tenant, evidence_id) -> accesses, append order
    accesses: HashMap<(String, String), Vec<EvidenceAccess>>,
}

#[derive(Default)]
pub struct MemEvidenceStore {
    state: RwLock<State>,
}

impl MemEvidenceStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl EvidenceStore for MemEvidenceStore {
    async fn register(
        &self,
        artifact: &EvidenceArtifact,
        grant: Option<&EvidenceGrant>,
    ) -> Result<SealOutcome> {
        let mut st = self.state.write().await;
        let domain_key = artifact.manifest.domain_key();
        let dk = (artifact.tenant.clone(), domain_key);

        // Resolve-and-insert under one lock. Splitting these is exactly how a
        // concurrent double-seal mints two ids for one logical result.
        if let Some(existing_id) = st.by_domain_key.get(&dk) {
            return Ok(SealOutcome {
                evidence_id: existing_id.clone(),
                created: false,
                grant: None,
            });
        }
        let key = (artifact.tenant.clone(), artifact.evidence_id.clone());
        if st.artifacts.contains_key(&key) {
            return Err(KernelError::InvalidInput(format!(
                "evidence id '{}' already exists",
                artifact.evidence_id
            )));
        }
        st.by_domain_key.insert(dk, artifact.evidence_id.clone());
        st.artifacts.insert(key, artifact.clone());
        if let Some(g) = grant {
            st.grants
                .insert((g.tenant.clone(), g.grant_id.clone()), g.clone());
        }
        Ok(SealOutcome {
            evidence_id: artifact.evidence_id.clone(),
            created: true,
            grant: grant.cloned(),
        })
    }

    async fn get(&self, tenant: &str, evidence_id: &str) -> Result<Option<EvidenceArtifact>> {
        Ok(self
            .state
            .read()
            .await
            .artifacts
            .get(&(tenant.to_string(), evidence_id.to_string()))
            .cloned())
    }

    async fn find_by_domain_key(
        &self,
        tenant: &str,
        domain_key: &str,
    ) -> Result<Option<EvidenceArtifact>> {
        let st = self.state.read().await;
        let Some(id) = st
            .by_domain_key
            .get(&(tenant.to_string(), domain_key.to_string()))
        else {
            return Ok(None);
        };
        Ok(st.artifacts.get(&(tenant.to_string(), id.clone())).cloned())
    }

    async fn commit(&self, tenant: &str, evidence_id: &str, at: &str) -> Result<bool> {
        let mut st = self.state.write().await;
        let key = (tenant.to_string(), evidence_id.to_string());
        let Some(a) = st.artifacts.get_mut(&key) else {
            return Err(KernelError::NotFound {
                kind: "evidence",
                id: evidence_id.to_string(),
            });
        };
        // Only `pending` commits — never `purged`, whose bytes may have
        // survived a raced blob delete. Same rule as the Postgres store.
        if a.state != munarium_core::evidence::EvidenceState::Pending {
            return Ok(false);
        }
        a.state = munarium_core::evidence::EvidenceState::Committed;
        a.committed_at = Some(at.to_string());
        Ok(true)
    }

    async fn consume_grant(
        &self,
        tenant: &str,
        evidence_id: &str,
        grant_id: &str,
        now: &str,
    ) -> Result<Option<EvidenceGrant>> {
        let mut st = self.state.write().await;
        let key = (tenant.to_string(), grant_id.to_string());
        let Some(g) = st.grants.get_mut(&key) else {
            return Ok(None);
        };
        // Every condition is checked before the mark, and the mark happens
        // under the same lock — a grant is single-use, and "single" has to
        // survive two callers arriving together.
        if g.evidence_id != evidence_id || g.used_at.is_some() || g.expires_at.as_str() <= now {
            return Ok(None);
        }
        g.used_at = Some(now.to_string());
        Ok(Some(g.clone()))
    }

    async fn record_access(&self, access: &EvidenceAccess) -> Result<()> {
        self.state
            .write()
            .await
            .accesses
            .entry((access.tenant.clone(), access.evidence_id.clone()))
            .or_default()
            .push(access.clone());
        Ok(())
    }

    async fn accesses(
        &self,
        tenant: &str,
        evidence_id: &str,
        limit: usize,
    ) -> Result<Vec<EvidenceAccess>> {
        let st = self.state.read().await;
        let mut rows = st
            .accesses
            .get(&(tenant.to_string(), evidence_id.to_string()))
            .cloned()
            .unwrap_or_default();
        rows.reverse(); // newest first
        rows.truncate(limit);
        Ok(rows)
    }

    // -- retention ---------------------------------------------------------

    async fn purge_due(&self, now: &str, limit: usize) -> Result<Vec<EvidenceArtifact>> {
        let st = self.state.read().await;
        let mut due: Vec<EvidenceArtifact> = st
            .artifacts
            .values()
            .filter(|a| {
                if a.state != munarium_core::evidence::EvidenceState::Committed {
                    return false;
                }
                let Some(r) = a.manifest.retention.as_ref() else {
                    // No retention block means no expiry. An artifact nobody
                    // gave a lifetime to is kept, not guessed at.
                    return false;
                };
                if r.legal_hold || r.purged_at.is_some() {
                    return false;
                }
                // RFC 3339 in UTC with a fixed shape sorts lexicographically,
                // which is why the whole plane keeps one textual time form.
                r.expires_at.as_deref().is_some_and(|e| e <= now)
            })
            .cloned()
            .collect();
        due.sort_by(|a, b| {
            let key = |x: &EvidenceArtifact| {
                x.manifest
                    .retention
                    .as_ref()
                    .and_then(|r| r.expires_at.clone())
                    .unwrap_or_default()
            };
            key(a).cmp(&key(b))
        });
        due.truncate(limit);
        Ok(due)
    }

    async fn mark_purged(&self, tenant: &str, evidence_id: &str, at: &str) -> Result<bool> {
        let mut st = self.state.write().await;
        let Some(a) = st
            .artifacts
            .get_mut(&(tenant.to_string(), evidence_id.to_string()))
        else {
            return Err(KernelError::NotFound {
                kind: "evidence",
                id: evidence_id.to_string(),
            });
        };
        if a.state == munarium_core::evidence::EvidenceState::Purged {
            return Ok(false);
        }
        a.state = munarium_core::evidence::EvidenceState::Purged;
        a.manifest
            .retention
            .get_or_insert_with(Default::default)
            .purged_at = Some(at.to_string());
        Ok(true)
    }

    async fn set_legal_hold(&self, tenant: &str, evidence_id: &str, hold: bool) -> Result<bool> {
        let mut st = self.state.write().await;
        let Some(a) = st
            .artifacts
            .get_mut(&(tenant.to_string(), evidence_id.to_string()))
        else {
            return Ok(false);
        };
        a.manifest
            .retention
            .get_or_insert_with(Default::default)
            .legal_hold = hold;
        Ok(true)
    }
}
