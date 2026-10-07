// SPDX-License-Identifier: Apache-2.0
//! Tenant-serialized authority transitions with immutable, transactional receipts.
use crate::{storage_err, PgStore};
use async_trait::async_trait;
use munarium_core::{platform_authority::*, KernelError, Result};
use serde_json::Value;

#[derive(Clone)]
pub struct PgAuthorityStore {
    store: PgStore,
}
impl PgAuthorityStore {
    pub fn new(store: PgStore) -> Self {
        Self { store }
    }
}
fn encode<T: serde::Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|_| KernelError::Storage("invalid authority state".into()))
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|_| KernelError::Storage("invalid authority state".into()))
}
#[async_trait]
impl AuthorityStore for PgAuthorityStore {
    async fn enroll(&self, config: AuthorityConfig) -> Result<AuthorityState> {
        if config.tenant != self.store.tenant_id {
            return Err(KernelError::Forbidden("authority tenant mismatch".into()));
        }
        let initial = AuthorityState::enrolled(config)?;
        let mut tx = self.store.pool.begin().await.map_err(storage_err)?;
        sqlx::query("INSERT INTO platform_authority(tenant_id,state) VALUES($1,$2) ON CONFLICT(tenant_id) DO NOTHING")
            .bind(&self.store.tenant_id).bind(encode(&initial)?)
            .execute(&mut *tx).await.map_err(storage_err)?;
        let raw: Value = sqlx::query_scalar(
            "SELECT state FROM platform_authority WHERE tenant_id=$1 FOR UPDATE",
        )
        .bind(&self.store.tenant_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_err)?;
        let current: AuthorityState = decode(raw)?;
        if current.config != initial.config {
            return Err(KernelError::Forbidden(
                "authority enrollment cannot be replaced".into(),
            ));
        }
        tx.commit().await.map_err(storage_err)?;
        Ok(current)
    }
    async fn snapshot(&self) -> Result<AuthorityState> {
        let raw: Option<Value> =
            sqlx::query_scalar("SELECT state FROM platform_authority WHERE tenant_id=$1")
                .bind(&self.store.tenant_id)
                .fetch_optional(&self.store.pool)
                .await
                .map_err(storage_err)?;
        decode(
            raw.ok_or_else(|| KernelError::Forbidden("platform authority is not enrolled".into()))?,
        )
    }
    async fn apply(
        &self,
        admission: &AuthorityAdmission,
        signed: &str,
        artifact: &GovernanceArtifact,
    ) -> Result<AuthorityReceipt> {
        let nonce = untrusted_nonce(signed)?;
        let mut tx = self.store.pool.begin().await.map_err(storage_err)?;
        let raw: Option<Value> = sqlx::query_scalar(
            "SELECT state FROM platform_authority WHERE tenant_id=$1 FOR UPDATE",
        )
        .bind(&self.store.tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;
        let state: AuthorityState =
            decode(raw.ok_or_else(|| {
                KernelError::Forbidden("platform authority is not enrolled".into())
            })?)?;
        let prior: Option<Value> = sqlx::query_scalar(
            "SELECT receipt FROM platform_authority_receipts WHERE tenant_id=$1 AND nonce=$2",
        )
        .bind(&self.store.tenant_id)
        .bind(&nonce)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;
        let prior: Option<AuthorityReceipt> = prior.map(decode).transpose()?;
        let (next, receipt) = transition(&state, admission, signed, artifact, prior.as_ref())?;
        if prior.is_none() {
            sqlx::query("INSERT INTO platform_authority_receipts(tenant_id,nonce,head,receipt,artifact) VALUES($1,$2,$3,$4,$5)")
                .bind(&self.store.tenant_id).bind(&nonce).bind(crate::pg_bigint(receipt.head, "authority head")?)
                .bind(encode(&receipt)?).bind(encode(artifact)?)
                .execute(&mut *tx).await.map_err(storage_err)?;
            sqlx::query("UPDATE platform_authority SET state=$2 WHERE tenant_id=$1")
                .bind(&self.store.tenant_id)
                .bind(encode(&next)?)
                .execute(&mut *tx)
                .await
                .map_err(storage_err)?;
        }
        tx.commit().await.map_err(storage_err)?;
        Ok(receipt)
    }
}
