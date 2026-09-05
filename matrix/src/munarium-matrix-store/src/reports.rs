// SPDX-License-Identifier: Apache-2.0
//! Read-only aggregates for the operator console.
//!
//! Every one of these is a SELECT. They live here rather than in the console
//! for one reason: a page that
//! writes its own SQL is a page that can quietly read another tenant's rows,
//! and one module is a reviewable surface where a hundred inline queries are
//! not. Every query is tenant-scoped in its WHERE clause, not by convention.
//!
//! Nothing here is on a hot path — these back a human refreshing a browser —
//! so they are written for clarity and bounded by an explicit LIMIT rather
//! than tuned.

use crate::{MatrixStore, Result};

/// One row of the queue depth summary.
#[derive(Debug, Clone)]
pub struct QueueDepth {
    pub queue: String,
    pub state: String,
    pub count: i64,
    /// Age of the OLDEST job in this state, in seconds. The number that says
    /// whether a queue is moving: a depth of 3 that is 4 seconds old is a busy
    /// system, and a depth of 3 that is 40 minutes old is a stuck worker.
    pub oldest_age_seconds: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct SyncRunRow {
    pub id: String,
    pub source_name: String,
    pub entity: String,
    pub state: String,
    pub mode: String,
    pub records_read: i64,
    pub records_rendered: i64,
    pub records_excluded: i64,
    pub documents_uploaded: i64,
    pub documents_skipped: i64,
    pub watermark: Option<String>,
    pub refusal: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CheckpointRow {
    pub source_name: String,
    pub entity: String,
    pub version: String,
    pub watermark: Option<String>,
    pub event_position: Option<String>,
    pub schema_fingerprint: Option<String>,
    pub updated_at: String,
}

/// Budget spent and held in the CURRENT hour window, per source.
#[derive(Debug, Clone)]
pub struct BudgetRow {
    pub source_name: String,
    pub held: i64,
    pub settled: i64,
    pub released: i64,
}

/// Refusals in a window, most frequent first — the "what is going wrong"
/// summary. Counted by CODE, not class: `budget_exceeded` and `schema_drift`
/// are both `invalid`-adjacent operational facts with completely different
/// remedies, and a page that grouped them would tell an operator nothing.
#[derive(Debug, Clone)]
pub struct RefusalCount {
    pub code: String,
    pub kind: String,
    pub count: i64,
}

impl MatrixStore {
    pub async fn queue_depth(&self, tenant: &str) -> Result<Vec<QueueDepth>> {
        let rows: Vec<(String, String, i64, Option<f64>)> = sqlx::query_as(
            "SELECT 'sync', state, COUNT(*),
                    EXTRACT(EPOCH FROM (now() - MIN(created_at)))
               FROM matrix.sync_jobs WHERE tenant_id = $1 GROUP BY state
             UNION ALL
             SELECT 'reconcile', state, COUNT(*),
                    EXTRACT(EPOCH FROM (now() - MIN(created_at)))
               FROM matrix.mapping_jobs WHERE tenant_id = $1 GROUP BY state
             ORDER BY 1, 2",
        )
        .bind(tenant)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(queue, state, count, age)| QueueDepth {
                queue,
                state,
                count,
                oldest_age_seconds: age.map(|a| a as i64),
            })
            .collect())
    }

    pub async fn list_sync_runs(
        &self,
        tenant: &str,
        source: Option<&str>,
        limit: i64,
    ) -> Result<Vec<SyncRunRow>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            String,
            String,
            String,
            String,
            i64,
            i64,
            i64,
            i64,
            i64,
            Option<String>,
            Option<serde_json::Value>,
            String,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, source_name, entity, state, mode, records_read, records_rendered,
                    records_excluded, documents_uploaded, documents_skipped, watermark,
                    refusal_json, started_at::text, ended_at::text
               FROM matrix.sync_runs
              WHERE tenant_id = $1 AND ($2::text IS NULL OR source_name = $2)
              ORDER BY started_at DESC LIMIT $3",
        )
        .bind(tenant)
        .bind(source)
        .bind(limit.clamp(1, 200))
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| SyncRunRow {
                id: r.0,
                source_name: r.1,
                entity: r.2,
                state: r.3,
                mode: r.4,
                records_read: r.5,
                records_rendered: r.6,
                records_excluded: r.7,
                documents_uploaded: r.8,
                documents_skipped: r.9,
                watermark: r.10,
                // The refusal's MESSAGE, not the whole object: the console
                // shows why a run stopped, and the typed object is in the
                // journal for anything more.
                refusal: r.11.and_then(|v| {
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                }),
                started_at: r.12,
                ended_at: r.13,
            })
            .collect())
    }

    pub async fn list_checkpoints(
        &self,
        tenant: &str,
        source: Option<&str>,
    ) -> Result<Vec<CheckpointRow>> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        )> = sqlx::query_as(
            "SELECT source_name, entity, version, watermark, event_position,
                    schema_fingerprint, updated_at::text
               FROM matrix.sync_checkpoints
              WHERE tenant_id = $1 AND ($2::text IS NULL OR source_name = $2)
              ORDER BY source_name, entity, version",
        )
        .bind(tenant)
        .bind(source)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| CheckpointRow {
                source_name: r.0,
                entity: r.1,
                version: r.2,
                watermark: r.3,
                event_position: r.4,
                schema_fingerprint: r.5,
                updated_at: r.6,
            })
            .collect())
    }

    /// The current hour's budget ledger. `date_trunc('hour', now())` is the
    /// same window expression `reserve_budget` uses, so the console reports
    /// the window the ceiling is actually enforced over rather than a
    /// rolling one that would disagree with it.
    pub async fn budget_ledger(&self, tenant: &str) -> Result<Vec<BudgetRow>> {
        let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
            "SELECT source_name,
                    COALESCE(SUM(units) FILTER (WHERE state = 'held'), 0),
                    COALESCE(SUM(units) FILTER (WHERE state = 'settled'), 0),
                    COALESCE(SUM(units) FILTER (WHERE state = 'released'), 0)
               FROM matrix.budget_reservations
              WHERE tenant_id = $1 AND window_start = date_trunc('hour', now())
              GROUP BY source_name ORDER BY source_name",
        )
        .bind(tenant)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(source_name, held, settled, released)| BudgetRow {
                source_name,
                held,
                settled,
                released,
            })
            .collect())
    }

    pub async fn refusal_counts(&self, tenant: &str, hours: i64) -> Result<Vec<RefusalCount>> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT refusal_code, kind, COUNT(*)
               FROM matrix.journal
              WHERE tenant_id = $1
                AND refusal_code IS NOT NULL
                AND created_at > now() - make_interval(hours => $2::int)
              GROUP BY refusal_code, kind
              ORDER BY 3 DESC LIMIT 25",
        )
        .bind(tenant)
        .bind(hours.clamp(1, 720) as i32)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(code, kind, count)| RefusalCount { code, kind, count })
            .collect())
    }

    /// Outcomes per journal kind in a window — the traffic summary. Deliberately
    /// two numbers per kind (total and refused) rather than a rate: a rate over
    /// three requests is noise wearing a percentage sign, and an operator can
    /// divide.
    pub async fn activity(&self, tenant: &str, hours: i64) -> Result<Vec<(String, i64, i64)>> {
        Ok(sqlx::query_as(
            "SELECT kind, COUNT(*), COUNT(*) FILTER (WHERE refusal_code IS NOT NULL)
               FROM matrix.journal
              WHERE tenant_id = $1 AND created_at > now() - make_interval(hours => $2::int)
              GROUP BY kind ORDER BY 2 DESC",
        )
        .bind(tenant)
        .bind(hours.clamp(1, 720) as i32)
        .fetch_all(self.pool())
        .await?)
    }

    /// The latest verification on record per contract or view, whichever kind.
    /// `DISTINCT ON` because the table is append-only: every verify writes a
    /// row, and the console wants the standing answer.
    #[allow(clippy::type_complexity)]
    pub async fn latest_verifications(
        &self,
        tenant: &str,
    ) -> Result<Vec<(String, String, String, i32, i32, String)>> {
        Ok(sqlx::query_as(
            "SELECT DISTINCT ON (kind, view_name)
                    kind, view_name, fingerprint, passed, failed, verified_at::text
               FROM matrix.metric_view_verifications
              WHERE tenant_id = $1
              ORDER BY kind, view_name, verified_at DESC",
        )
        .bind(tenant)
        .fetch_all(self.pool())
        .await?)
    }
}
