// SPDX-License-Identifier: Apache-2.0
//! Authority state and receipt change under one lock, separate from ordinary memory.
use async_trait::async_trait;
use munarium_core::{platform_authority::*, KernelError, Result};
use std::collections::BTreeMap;
use tokio::sync::Mutex;

#[derive(Default)]
struct Records {
    state: Option<AuthorityState>,
    receipts: BTreeMap<String, AuthorityReceipt>,
}
pub struct MemAuthorityStore {
    tenant: String,
    records: Mutex<Records>,
}
impl MemAuthorityStore {
    pub fn new(tenant: &str) -> Self {
        Self {
            tenant: tenant.into(),
            records: Mutex::new(Records::default()),
        }
    }
}
fn unavailable() -> KernelError {
    KernelError::Forbidden("platform authority is not enrolled".into())
}
#[async_trait]
impl AuthorityStore for MemAuthorityStore {
    async fn enroll(&self, config: AuthorityConfig) -> Result<AuthorityState> {
        if config.tenant != self.tenant {
            return Err(unavailable());
        }
        let initial = AuthorityState::enrolled(config)?;
        let mut records = self.records.lock().await;
        match &records.state {
            Some(current) if current.config == initial.config => Ok(current.clone()),
            Some(_) => Err(KernelError::Forbidden(
                "authority enrollment cannot be replaced".into(),
            )),
            None => {
                records.state = Some(initial.clone());
                Ok(initial)
            }
        }
    }
    async fn snapshot(&self) -> Result<AuthorityState> {
        self.records
            .lock()
            .await
            .state
            .clone()
            .ok_or_else(unavailable)
    }
    async fn apply(
        &self,
        admission: &AuthorityAdmission,
        signed: &str,
        artifact: &GovernanceArtifact,
    ) -> Result<AuthorityReceipt> {
        let nonce = untrusted_nonce(signed)?;
        let mut records = self.records.lock().await;
        let state = records.state.as_ref().ok_or_else(unavailable)?;
        let (next, receipt) = transition(
            state,
            admission,
            signed,
            artifact,
            records.receipts.get(&nonce),
        )?;
        records
            .receipts
            .entry(nonce)
            .or_insert_with(|| receipt.clone());
        records.state = Some(next);
        Ok(receipt)
    }
}
