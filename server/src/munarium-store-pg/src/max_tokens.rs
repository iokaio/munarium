// SPDX-License-Identifier: Apache-2.0
//! PostgreSQL persistence for the per-call output-token budgets
//! (`GET`/`POST /v1/max-tokens`; migration `0031_max_tokens_budgets`).
//!
//! One row per tenant holding the whole replacement as JSON: the API's
//! contract is "replace the object, never part of it", so the storage shape
//! is the object, and a partial write is unrepresentable rather than
//! forbidden. The server owns the built-in and environment defaults; a tenant
//! with no row is on those, and this store never invents a row on read.
//!
//! The memory store has no counterpart on purpose: memory deployments are
//! confined to one replica by config validation, so the server's registry IS
//! their whole state, exactly as it is for provider configs.

use munarium_core::Result;
use sqlx::{PgPool, Row};

use crate::storage_err;

#[derive(Clone)]
pub struct PgMaxTokensStore {
    pool: PgPool,
}

/// RFC 3339 with microseconds in UTC, rendered by Postgres so the instant a
/// caller reads back is the one the row carries, not a re-parse of it.
const UPDATED_AT_RFC3339: &str =
    "to_char(updated_at AT TIME ZONE 'utc', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')";

impl PgMaxTokensStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The tenant's replacement and when it was written, if one exists.
    pub async fn get(&self, tenant: &str) -> Result<Option<(serde_json::Value, String)>> {
        let sql = format!(
            "SELECT budgets, {UPDATED_AT_RFC3339} AS updated_at
               FROM max_tokens_budgets WHERE tenant_id = $1"
        );
        let row = sqlx::query(&sql)
            .bind(tenant)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_err)?;
        Ok(row.map(|r| {
            (
                r.get::<serde_json::Value, _>("budgets"),
                r.get::<String, _>("updated_at"),
            )
        }))
    }

    /// Replace the tenant's whole set (insert or overwrite) and return the
    /// write instant. There is no merge path: the caller sends every field.
    pub async fn replace(&self, tenant: &str, budgets: &serde_json::Value) -> Result<String> {
        let sql = format!(
            "INSERT INTO max_tokens_budgets (tenant_id, budgets, updated_at)
             VALUES ($1, $2, now())
             ON CONFLICT (tenant_id) DO UPDATE
                SET budgets = EXCLUDED.budgets, updated_at = now()
             RETURNING {UPDATED_AT_RFC3339}"
        );
        sqlx::query_scalar(&sql)
            .bind(tenant)
            .bind(budgets)
            .fetch_one(&self.pool)
            .await
            .map_err(storage_err)
    }
}
