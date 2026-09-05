// SPDX-License-Identifier: Apache-2.0
//! Per-role work queues, sync runs, and checkpoints.
//!
//! Claiming uses `FOR UPDATE SKIP LOCKED`, so N workers of a role claim
//! disjoint jobs with no coordinator and no leader election. The queues are
//! separate tables per role on purpose: a hung sync must not be able to hold a
//! lock a query worker needs, and separate tables make that structural rather
//! than a matter of care.

use crate::{new_id, MatrixStore, Result};
use munarium_matrix_core::checkpoint::Checkpoint;

/// `(watermark, tie_break, event_position, schema_fingerprint)` — a checkpoint
/// row. Every field is optional because a fresh checkpoint has none of them.
type CheckpointRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedJob {
    pub id: String,
    pub tenant_id: String,
    /// Source name for a sync job; mapping name for a mapping job.
    pub target: String,
    pub entity: String,
    pub attempts: i32,
}

impl MatrixStore {
    // ---- sync ---------------------------------------------------------------

    pub async fn enqueue_sync(&self, tenant: &str, source: &str, entity: &str) -> Result<String> {
        let id = new_id("sjb");
        sqlx::query(
            "INSERT INTO matrix.sync_jobs (id, tenant_id, source_name, entity) VALUES ($1,$2,$3,$4)",
        )
        .bind(&id)
        .bind(tenant)
        .bind(source)
        .bind(entity)
        .execute(self.pool())
        .await?;
        Ok(id)
    }

    /// Claim the next due sync job for this worker, or `None`.
    /// Claim the next job — or RE-claim one whose worker went quiet.
    ///
    /// A claimed job is a lease, not a title: a container restarted mid-run
    /// (an ACA revision roll, a crash) leaves its job `running` forever with
    /// nobody to finish it, which is worse than a job nobody claims. A running
    /// job older than `lease_secs` with attempts to spare is offered again;
    /// the work behind it is idempotent by design (render keys, proposal keys,
    /// content-addressed findings), so the second worker completes what the
    /// first began rather than doing it twice.
    pub async fn claim_sync_job(
        &self,
        worker: &str,
        lease_secs: i64,
    ) -> Result<Option<ClaimedJob>> {
        let row: Option<(String, String, String, String, i32)> = sqlx::query_as(
            "UPDATE matrix.sync_jobs SET state = 'running', claimed_by = $1, claimed_at = now(),
                                        attempts = attempts + 1, updated_at = now()
              WHERE id = (
                    SELECT id FROM matrix.sync_jobs
                     WHERE (state = 'queued' AND scheduled_at <= now())
                        OR (state = 'running'
                            AND claimed_at < now() - make_interval(secs => $2)
                            AND attempts < 3)
                     ORDER BY scheduled_at
                     FOR UPDATE SKIP LOCKED
                     LIMIT 1)
             RETURNING id, tenant_id, source_name, entity, attempts",
        )
        .bind(worker)
        .bind(lease_secs as f64)
        .fetch_optional(self.pool())
        .await?;
        Ok(
            row.map(|(id, tenant_id, target, entity, attempts)| ClaimedJob {
                id,
                tenant_id,
                target,
                entity,
                attempts,
            }),
        )
    }

    pub async fn finish_sync_job(
        &self,
        job_id: &str,
        state: &str,
        run_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE matrix.sync_jobs SET state = $2, run_id = $3, updated_at = now() WHERE id = $1",
        )
        .bind(job_id)
        .bind(state)
        .bind(run_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn start_sync_run(
        &self,
        tenant: &str,
        source: &str,
        entity: &str,
        mode: &str,
    ) -> Result<String> {
        let id = new_id("srn");
        sqlx::query(
            "INSERT INTO matrix.sync_runs (id, tenant_id, source_name, entity, state, mode)
             VALUES ($1,$2,$3,$4,'running',$5)",
        )
        .bind(&id)
        .bind(tenant)
        .bind(source)
        .bind(entity)
        .bind(mode)
        .execute(self.pool())
        .await?;
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_sync_run(
        &self,
        run_id: &str,
        state: &str,
        read: u64,
        rendered: u64,
        excluded: u64,
        uploaded: u64,
        skipped: u64,
        count_evidence_id: Option<&str>,
        watermark: Option<&str>,
        refusal: Option<&munarium_matrix_core::Refusal>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE matrix.sync_runs
                SET state = $2, records_read = $3, records_rendered = $4, records_excluded = $5,
                    documents_uploaded = $6, documents_skipped = $7, count_evidence_id = $8,
                    watermark = $9, refusal_json = $10, ended_at = now()
              WHERE id = $1",
        )
        .bind(run_id)
        .bind(state)
        .bind(read as i64)
        .bind(rendered as i64)
        .bind(excluded as i64)
        .bind(uploaded as i64)
        .bind(skipped as i64)
        .bind(count_evidence_id)
        .bind(watermark)
        .bind(refusal.and_then(|r| serde_json::to_value(r).ok()))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The state of one sync job.
    ///
    /// Tenant-scoped, so a job id from another tenant reads as absent rather
    /// than leaking that it exists. The enqueue route hands back job ids, and
    /// an id with nothing to poll is not much of an answer.
    pub async fn sync_job_state(&self, tenant: &str, job_id: &str) -> Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT state FROM matrix.sync_jobs WHERE id = $1 AND tenant_id = $2")
                .bind(job_id)
                .bind(tenant)
                .fetch_optional(self.pool())
                .await?;
        Ok(row.map(|r| r.0))
    }

    // ---- checkpoints --------------------------------------------------------

    pub async fn load_checkpoint(
        &self,
        tenant: &str,
        source: &str,
        entity: &str,
        version: &str,
    ) -> Result<Option<Checkpoint>> {
        let row: Option<CheckpointRow> = sqlx::query_as(
            "SELECT watermark, tie_break, event_position, schema_fingerprint
                   FROM matrix.sync_checkpoints
                  WHERE tenant_id = $1 AND source_name = $2 AND entity = $3 AND version = $4",
        )
        .bind(tenant)
        .bind(source)
        .bind(entity)
        .bind(version)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(
            |(watermark, tie_break, event_position, schema_fingerprint)| Checkpoint {
                source_id: source.to_string(),
                entity: entity.to_string(),
                version: version.to_string(),
                watermark,
                tie_break,
                event_position,
                schema_fingerprint,
            },
        ))
    }

    pub async fn save_checkpoint(&self, tenant: &str, cp: &Checkpoint) -> Result<()> {
        sqlx::query(
            "INSERT INTO matrix.sync_checkpoints
               (tenant_id, source_name, entity, version, watermark, tie_break, event_position,
                schema_fingerprint)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
             ON CONFLICT (tenant_id, source_name, entity, version)
             DO UPDATE SET watermark = EXCLUDED.watermark,
                           tie_break = EXCLUDED.tie_break,
                           event_position = EXCLUDED.event_position,
                           schema_fingerprint = EXCLUDED.schema_fingerprint,
                           updated_at = now()",
        )
        .bind(tenant)
        .bind(&cp.source_id)
        .bind(&cp.entity)
        .bind(&cp.version)
        .bind(&cp.watermark)
        .bind(&cp.tie_break)
        .bind(&cp.event_position)
        .bind(&cp.schema_fingerprint)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Delete a checkpoint — the operator-initiated resnapshot path, and what
    /// the checkpoint-loss recovery test truncates.
    pub async fn drop_checkpoint(
        &self,
        tenant: &str,
        source: &str,
        entity: &str,
        version: &str,
    ) -> Result<()> {
        sqlx::query(
            "DELETE FROM matrix.sync_checkpoints
              WHERE tenant_id = $1 AND source_name = $2 AND entity = $3 AND version = $4",
        )
        .bind(tenant)
        .bind(source)
        .bind(entity)
        .bind(version)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    // ---- idempotency --------------------------------------------------------

    /// Remember that a document was uploaded. Returns false when this key was
    /// already recorded — the replay case.
    pub async fn record_uploaded(
        &self,
        tenant: &str,
        key: &str,
        source: &str,
        path: &str,
        content_hash: &str,
    ) -> Result<bool> {
        let r = sqlx::query(
            "INSERT INTO matrix.uploaded_documents
               (tenant_id, idempotency_key, source_name, path, content_hash)
             VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
        )
        .bind(tenant)
        .bind(key)
        .bind(source)
        .bind(path)
        .bind(content_hash)
        .execute(self.pool())
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Same, for mode-C events.
    pub async fn record_observed(
        &self,
        tenant: &str,
        key: &str,
        mapping: &str,
        run_id: &str,
    ) -> Result<bool> {
        let r = sqlx::query(
            "INSERT INTO matrix.observed_events (tenant_id, idempotency_key, mapping_name, run_id)
             VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING",
        )
        .bind(tenant)
        .bind(key)
        .bind(mapping)
        .bind(run_id)
        .execute(self.pool())
        .await?;
        Ok(r.rows_affected() == 1)
    }

    // ---- mapping ------------------------------------------------------------

    pub async fn enqueue_mapping(&self, tenant: &str, mapping: &str) -> Result<String> {
        let id = new_id("mjb");
        sqlx::query(
            "INSERT INTO matrix.mapping_jobs (id, tenant_id, mapping_name) VALUES ($1,$2,$3)",
        )
        .bind(&id)
        .bind(tenant)
        .bind(mapping)
        .execute(self.pool())
        .await?;
        Ok(id)
    }

    /// Claim the next job — or RE-claim one whose worker went quiet.
    ///
    /// A claimed job is a lease, not a title: a container restarted mid-run
    /// (an ACA revision roll, a crash) leaves its job `running` forever with
    /// nobody to finish it, which is worse than a job nobody claims. A running
    /// job older than `lease_secs` with attempts to spare is offered again;
    /// the work behind it is idempotent by design (render keys, proposal keys,
    /// content-addressed findings), so the second worker completes what the
    /// first began rather than doing it twice.
    pub async fn claim_mapping_job(
        &self,
        worker: &str,
        lease_secs: i64,
    ) -> Result<Option<ClaimedJob>> {
        let row: Option<(String, String, String, i32)> = sqlx::query_as(
            "UPDATE matrix.mapping_jobs SET state = 'running', claimed_by = $1, claimed_at = now(),
                                           attempts = attempts + 1, updated_at = now()
              WHERE id = (
                    SELECT id FROM matrix.mapping_jobs
                     WHERE (state = 'queued' AND scheduled_at <= now())
                        OR (state = 'running'
                            AND claimed_at < now() - make_interval(secs => $2)
                            AND attempts < 3)
                     ORDER BY scheduled_at
                     FOR UPDATE SKIP LOCKED
                     LIMIT 1)
             RETURNING id, tenant_id, mapping_name, attempts",
        )
        .bind(worker)
        .bind(lease_secs as f64)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|(id, tenant_id, target, attempts)| ClaimedJob {
            id,
            tenant_id,
            target,
            entity: String::new(),
            attempts,
        }))
    }

    pub async fn finish_mapping_job(
        &self,
        job_id: &str,
        state: &str,
        run_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE matrix.mapping_jobs SET state = $2, run_id = $3, updated_at = now() WHERE id = $1",
        )
        .bind(job_id)
        .bind(state)
        .bind(run_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn start_mapping_run(&self, tenant: &str, mapping: &str) -> Result<String> {
        let id = new_id("mrn");
        sqlx::query(
            "INSERT INTO matrix.mapping_runs (id, tenant_id, mapping_name, state)
             VALUES ($1,$2,$3,'running')",
        )
        .bind(&id)
        .bind(tenant)
        .bind(mapping)
        .execute(self.pool())
        .await?;
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_mapping_run(
        &self,
        run_id: &str,
        state: &str,
        observations: u64,
        discrepancies: u64,
        ambiguous: u64,
        findings: u64,
        batch_evidence_id: Option<&str>,
        proposals: u64,
        nonconforming: u64,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE matrix.mapping_runs
                SET state = $2, observations = $3, discrepancies = $4, ambiguous = $5,
                    findings_filed = $6, batch_evidence_id = $7, ended_at = now(),
                    proposals = $8, nonconforming = $9
              WHERE id = $1",
        )
        .bind(run_id)
        .bind(state)
        .bind(observations as i64)
        .bind(discrepancies as i64)
        .bind(ambiguous as i64)
        .bind(findings as i64)
        .bind(batch_evidence_id)
        .bind(proposals as i64)
        .bind(nonconforming as i64)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}
