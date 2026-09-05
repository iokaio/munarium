// SPDX-License-Identifier: Apache-2.0
//! `munarium-matrix-store` — Postgres persistence for Matrix.
//!
//! Schema `matrix.*`, owned by role `matrix_owner`, which holds no privileges
//! in `public`. Every statement is a runtime-checked string (no `query!`
//! macros), matching the server's posture: the build never needs a database,
//! and conformance against a real Postgres is the drift net.
//!
//! Table names are fully qualified rather than relying on `search_path`. A
//! search path is per-session state, and a pooled connection that lost it
//! would silently write to `public` — which is exactly the isolation the
//! deployment is trying to guarantee.

#![forbid(unsafe_code)]
#![allow(clippy::result_large_err)]

pub mod budget;
pub mod journal;
pub mod metric_views;
pub mod promotions;
pub mod queue;
pub mod registry;
pub mod reports;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub use budget::{BudgetOutcome, Reservation};
pub use journal::{JournalQuery, JournalRecord};
pub use metric_views::{MetricVerification, MetricVerificationRecord};
pub use promotions::{MappingRunStats, Promotion, ProposalRow};
pub use registry::{ApplyOutcome, StoredAsset};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("{0}")]
    Conflict(String),
    #[error("not found: {kind} '{id}'")]
    NotFound { kind: &'static str, id: String },
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone)]
pub struct MatrixStore {
    pool: PgPool,
}

impl std::fmt::Debug for MatrixStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatrixStore")
            .field("pool_size", &self.pool.size())
            .finish()
    }
}

impl MatrixStore {
    pub async fn connect(url: &str, max_conns: u32) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_conns)
            // Every connection lands in `matrix` FIRST.
            //
            // This is not a convenience. `sqlx::migrate!` creates its
            // `_sqlx_migrations` bookkeeping table in whatever the search path
            // resolves to, and `matrix_owner` is deliberately denied `public`
            // — that denial is a tested guarantee, not an oversight. Without
            // this the service cannot migrate against its own designed role
            // posture, which is exactly what happened the first time the binary
            // was pointed at a real database (2026-08-28).
            //
            // `public` stays on the path so extension functions resolve; the
            // role still cannot write there, and the isolation scenario still
            // proves it.
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    sqlx::query("SET search_path TO matrix, public")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// A store whose pool is never connected. For tests of code paths that do
    /// not touch the database (auth, routing, rendering) — it lets those tests
    /// run with no Postgres at all, and any accidental query fails loudly
    /// rather than silently succeeding against a real database.
    pub fn disconnected_for_tests() -> Self {
        Self {
            pool: PgPoolOptions::new()
                .max_connections(1)
                // 50 ms, not the 30-second default. Every query through this
                // pool is MEANT to fail — the point is to prove a handler
                // reports a store failure honestly — and waiting half a
                // minute per call made the admin console's router tests take
                // four minutes. A test that slow stops being run.
                .acquire_timeout(std::time::Duration::from_millis(50))
                .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
                .expect("a lazy pool never connects at construction"),
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Apply pending migrations. Additive only — enforced by a CI grep over
    /// the migrations directory, because the rule is easy to state and easy to
    /// forget at 2am.
    pub async fn migrate(&self) -> Result<()> {
        // The schema must exist before the MIGRATOR runs, not as part of it:
        // sqlx creates `_sqlx_migrations` on the search path before applying
        // migration 0001, and 0001 is what creates `matrix`. Without this the
        // first migration of a fresh database fails with "no schema has been
        // selected to create in", which reads like a configuration error and is
        // really an ordering one.
        //
        // Checked, then created — never `CREATE SCHEMA IF NOT EXISTS`.
        //
        // `IF NOT EXISTS` evaluates the CREATE privilege on the DATABASE before
        // it short-circuits, so a least-privilege `matrix_owner` that already
        // owns `matrix` is refused for a statement that would have done
        // nothing. The designed posture has the schema provisioned by compose
        // or terraform, so the common path must not need the privilege at all.
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'matrix')",
        )
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            // A bare database someone pointed at by hand. This needs CREATE on
            // the database, and if the role does not have it the error names
            // the real problem instead of surfacing later as a missing table.
            sqlx::query("CREATE SCHEMA matrix")
                .execute(&self.pool)
                .await?;
        }
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    /// A cheap liveness probe for `/readyz`.
    pub async fn ready(&self) -> bool {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            sqlx::query("SELECT 1").execute(&self.pool),
        )
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
    }
}

/// `prefix-<uuid7 simple>` — the id shape used across both trees. UUIDv7 so
/// ids sort by creation time without a separate column.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::now_v7().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_prefixed_and_sort_by_creation() {
        let a = new_id("jrn");
        let b = new_id("jrn");
        assert!(a.starts_with("jrn-"));
        assert_ne!(a, b);
        // uuid7 is time-ordered, so lexical order is creation order.
        assert!(a < b, "{a} !< {b}");
    }
}
