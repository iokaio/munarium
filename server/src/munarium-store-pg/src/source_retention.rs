// SPDX-License-Identifier: Apache-2.0
//! Durable source denial and transactional cleanup of PostgreSQL originals.
use crate::storage_err;
use async_trait::async_trait;
use munarium_core::{
    sources::{SourceKey, SourceStore},
    KernelError, Result,
};
use sqlx::{PgPool, Row};
use std::sync::Arc;

pub const DENIED_PREFIX: &str = "source-denied: ";
pub fn denied() -> KernelError {
    KernelError::InvalidInput(format!(
        "{DENIED_PREFIX}source content is unavailable under the tenant retention policy"
    ))
}

pub async fn assert_sources(pool: &PgPool, tenant: &str, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let found: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM source_retention WHERE tenant_id=$1 AND source_id=ANY($2) AND denied)")
        .bind(tenant).bind(ids).fetch_one(pool).await.map_err(storage_err)?;
    if found {
        Err(denied())
    } else {
        Ok(())
    }
}

pub async fn assert_session(pool: &PgPool, tenant: &str, session: &str) -> Result<()> {
    let found: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM session_turns t JOIN source_retention r ON r.tenant_id=t.tenant_id WHERE t.tenant_id=$1 AND t.session_id=$2 AND r.denied AND EXISTS(SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(t.hits)='array' THEN t.hits ELSE '[]'::jsonb END) h WHERE h->>'source_id'=r.source_id))")
        .bind(tenant).bind(session).fetch_one(pool).await.map_err(storage_err)?;
    if found {
        Err(denied())
    } else {
        Ok(())
    }
}

/// A bound scope is conservatively unavailable, including old pins and derived
/// vocabularies. Immutable artifacts and retained audit history are not erased.
pub async fn assert_scope(pool: &PgPool, tenant: &str, kind: &str, id: &str) -> Result<()> {
    let found: bool = if kind == "collection" {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM source_retention r JOIN collection_sources c ON c.tenant_id=r.tenant_id AND c.source_id=r.source_id WHERE r.tenant_id=$1 AND r.denied AND c.collection_id=$2)")
            .bind(tenant).bind(id).fetch_one(pool).await.map_err(storage_err)?
    } else {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM source_retention r JOIN sources s ON s.tenant_id=r.tenant_id AND s.source_id=r.source_id WHERE r.tenant_id=$1 AND r.denied AND s.shape_ref=$2)")
            .bind(tenant).bind(id).fetch_one(pool).await.map_err(storage_err)?
    };
    if found {
        Err(denied())
    } else {
        Ok(())
    }
}

pub struct GuardedSources {
    pool: PgPool,
    inner: Arc<dyn SourceStore>,
}
impl GuardedSources {
    pub fn new(pool: PgPool, inner: Arc<dyn SourceStore>) -> Self {
        Self { pool, inner }
    }
    async fn check(&self, key: &SourceKey) -> Result<()> {
        if key
            .path
            .starts_with(munarium_core::evidence::EVIDENCE_PATH_PREFIX)
        {
            return Ok(());
        }
        assert_sources(&self.pool, &key.tenant, &[key.source_id()]).await
    }
}
#[async_trait]
impl SourceStore for GuardedSources {
    async fn put(&self, key: &SourceKey, media_type: &str, bytes: &[u8]) -> Result<String> {
        self.check(key).await?;
        let result = self.inner.put(key, media_type, bytes).await?;
        self.check(key).await?;
        Ok(result)
    }
    async fn get(&self, key: &SourceKey) -> Result<Vec<u8>> {
        self.check(key).await?;
        let bytes = self.inner.get(key).await?;
        self.check(key).await?;
        Ok(bytes)
    }
    async fn exists(&self, key: &SourceKey) -> Result<bool> {
        self.check(key).await?;
        self.inner.exists(key).await
    }
    async fn delete(&self, key: &SourceKey) -> Result<()> {
        self.check(key).await?;
        self.inner.delete(key).await
    }
    fn backend_id(&self) -> &'static str {
        self.inner.backend_id()
    }
}

/// Management transition. `deny` is permanent for this stable path. Holds do
/// not grant read access, and releasing a hold never releases a denial.
pub async fn change(pool: &PgPool, tenant: &str, path: &str, action: &str) -> Result<()> {
    munarium_core::sources::refuse_reserved_document_path(path)?;
    let key = SourceKey::new(tenant, path, "")?;
    if !["deny", "deny-and-erase-pg-original", "hold", "release-hold"].contains(&action) {
        return Err(KernelError::InvalidInput(
            "unknown source retention action".into(),
        ));
    }
    let mut tx = pool.begin().await.map_err(storage_err)?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('source-retention/' || $1 || '/' || $2, 0))",
    )
    .bind(tenant)
    .bind(path)
    .execute(&mut *tx)
    .await
    .map_err(storage_err)?;
    if action == "deny-and-erase-pg-original" {
        let backend: Option<String> = sqlx::query_scalar(
            "SELECT storage_backend FROM sources WHERE tenant_id=$1 AND source_id=$2",
        )
        .bind(tenant)
        .bind(key.source_id())
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;
        if backend.as_deref() != Some("pg") {
            return Err(KernelError::InvalidInput("original cleanup requires an existing PostgreSQL source; other backends support denial only".into()));
        }
    }
    sqlx::query("INSERT INTO source_retention (tenant_id,source_id,path) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(tenant).bind(key.source_id()).bind(path).execute(&mut *tx).await.map_err(storage_err)?;
    let row = sqlx::query(
        "SELECT cleanup_state FROM source_retention WHERE tenant_id=$1 AND source_id=$2 FOR UPDATE",
    )
    .bind(tenant)
    .bind(key.source_id())
    .fetch_one(&mut *tx)
    .await
    .map_err(storage_err)?;
    if action == "hold" && row.get::<String, _>("cleanup_state") == "completed" {
        return Err(KernelError::InvalidInput(
            "the original was already erased; a hold cannot restore it".into(),
        ));
    }
    sqlx::query("UPDATE source_retention SET denied=denied OR $3, hold=CASE WHEN $4='hold' THEN true WHEN $4='release-hold' THEN false ELSE hold END, cleanup_state=CASE WHEN $4='deny-and-erase-pg-original' AND cleanup_state<>'completed' THEN 'pending' ELSE cleanup_state END, updated_at=now() WHERE tenant_id=$1 AND source_id=$2")
        .bind(tenant).bind(key.source_id()).bind(action.starts_with("deny")).bind(action).execute(&mut *tx).await.map_err(storage_err)?;
    tx.commit().await.map_err(storage_err)
}

/// A failed transaction leaves both bytes and pending work intact. Shared
/// collection ownership or a hold keeps the job pending with an explicit reason.
pub async fn cleanup_one(pool: &PgPool, tenant: &str, id: &str) -> Result<bool> {
    let path: Option<String> = sqlx::query_scalar("SELECT path FROM source_retention WHERE tenant_id=$1 AND source_id=$2 AND cleanup_state='pending'")
        .bind(tenant).bind(id).fetch_optional(pool).await.map_err(storage_err)?;
    let Some(path) = path else {
        return Ok(false);
    };
    let mut tx = pool.begin().await.map_err(storage_err)?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('source-retention/' || $1 || '/' || $2, 0))",
    )
    .bind(tenant)
    .bind(&path)
    .execute(&mut *tx)
    .await
    .map_err(storage_err)?;
    let hold: Option<bool> = sqlx::query_scalar("SELECT hold FROM source_retention WHERE tenant_id=$1 AND source_id=$2 AND cleanup_state='pending' FOR UPDATE")
        .bind(tenant).bind(id).fetch_optional(&mut *tx).await.map_err(storage_err)?;
    let Some(hold) = hold else {
        return Ok(false);
    };
    let owners: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM collection_sources WHERE tenant_id=$1 AND source_id=$2",
    )
    .bind(tenant)
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(storage_err)?;
    if hold || owners > 1 {
        sqlx::query("UPDATE source_retention SET cleanup_blocked=$3, updated_at=now() WHERE tenant_id=$1 AND source_id=$2")
            .bind(tenant).bind(id).bind(if hold {"hold"} else {"shared-source"}).execute(&mut *tx).await.map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        return Ok(false);
    }
    sqlx::query("DELETE FROM source_blobs WHERE tenant_id=$1 AND blob_name=$2")
        .bind(tenant)
        .bind(format!("{tenant}/{path}"))
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
    sqlx::query("UPDATE source_retention SET cleanup_state='completed',cleanup_blocked=NULL,updated_at=now() WHERE tenant_id=$1 AND source_id=$2")
        .bind(tenant).bind(id).execute(&mut *tx).await.map_err(storage_err)?;
    tx.commit().await.map_err(storage_err)?;
    Ok(true)
}

pub async fn sweep(pool: &PgPool) -> Result<()> {
    let jobs: Vec<(String,String)> = sqlx::query_as("SELECT tenant_id,source_id FROM source_retention WHERE cleanup_state='pending' ORDER BY updated_at LIMIT 32")
        .fetch_all(pool).await.map_err(storage_err)?;
    let mut first_error = None;
    for (tenant, id) in jobs {
        if let Err(error) = cleanup_one(pool, &tenant, &id).await {
            sqlx::query("UPDATE source_retention SET cleanup_blocked='storage-error',updated_at=now() WHERE tenant_id=$1 AND source_id=$2 AND cleanup_state='pending'")
                .bind(&tenant).bind(&id).execute(pool).await.map_err(storage_err)?;
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}
