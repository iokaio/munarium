// SPDX-License-Identifier: Apache-2.0
//! `PgSourceStore` — the Postgres `SourceStore` backend.
//!
//! Lives here rather than in `munarium-retrieval-pg` because it is a STORAGE
//! backend, not a retrieval one: nothing about it is a search concern, and
//! AppState reached into the retrieval crate purely to construct it. That was a
//! layering inversion the stage 1 extraction corrected.
//!
//! Document bytes in a `source_blobs` table. This is the fallback that keeps
//! `docker compose up` and `cargo test --workspace` working with no cloud
//! account; production runs an `munarium-store-objects` backend. Bytes live in
//! their own table rather than a column on `sources`, so the object-store
//! seam does not depend on the metadata row's write ordering.

use async_trait::async_trait;
use munarium_core::sources::{SourceKey, SourceStore};
use munarium_core::{KernelError, Result};
use sqlx::PgPool;

use crate::storage_err;

pub struct PgSourceStore {
    pool: PgPool,
}

impl PgSourceStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SourceStore for PgSourceStore {
    async fn put(&self, key: &SourceKey, _media_type: &str, bytes: &[u8]) -> Result<String> {
        let blob_name = key.blob_name();
        sqlx::query(
            "INSERT INTO source_blobs (tenant_id, blob_name, bytes)
             VALUES ($1, $2, $3)
             ON CONFLICT (tenant_id, blob_name) DO UPDATE SET bytes = EXCLUDED.bytes",
        )
        .bind(&key.tenant)
        .bind(&blob_name)
        .bind(bytes)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(format!("pg://source_blobs/{blob_name}"))
    }

    async fn get(&self, key: &SourceKey) -> Result<Vec<u8>> {
        let row: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT bytes FROM source_blobs WHERE tenant_id = $1 AND blob_name = $2",
        )
        .bind(&key.tenant)
        .bind(key.blob_name())
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        row.map(|(b,)| b).ok_or_else(|| KernelError::NotFound {
            kind: "source blob",
            id: key.blob_name(),
        })
    }

    async fn exists(&self, key: &SourceKey) -> Result<bool> {
        let row: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM source_blobs WHERE tenant_id = $1 AND blob_name = $2")
                .bind(&key.tenant)
                .bind(key.blob_name())
                .fetch_optional(&self.pool)
                .await
                .map_err(storage_err)?;
        Ok(row.is_some())
    }

    async fn delete(&self, key: &SourceKey) -> Result<()> {
        sqlx::query("DELETE FROM source_blobs WHERE tenant_id = $1 AND blob_name = $2")
            .bind(&key.tenant)
            .bind(key.blob_name())
            .execute(&self.pool)
            .await
            .map_err(storage_err)?;
        Ok(())
    }

    fn backend_id(&self) -> &'static str {
        "pg"
    }
}
