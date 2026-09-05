// SPDX-License-Identifier: Apache-2.0
//! PostgreSQL evidence plane (migration `0023_evidence`).
//!
//! Semantics contract: identical to `munarium-store-mem`'s — the conformance
//! suite runs the same scenarios against both, which is the drift net that
//! stands in for `query!` macros (this tree uses runtime-checked SQL by
//! design).
//!
//! Two operations are concurrency-sensitive and both push the race into the
//! database rather than resolving it in Rust:
//!
//! - **Registration** relies on `ON CONFLICT (tenant_id, domain_key)`. Two
//!   Matrix replicas sealing the same logical result at the same instant must
//!   yield ONE artifact; a read-then-insert would yield two under a race, and
//!   the second id would be a citation nobody can explain.
//! - **Spending a grant** is one conditional `UPDATE ... WHERE used_at IS
//!   NULL` returning the row. Checking then marking would make a leaked grant
//!   usable twice by two simultaneous callers.
//!
//! Timestamps cross this boundary as RFC 3339 strings, matching the manifest's
//! own encoding, and are cast in SQL. Keeping one textual form end to end is
//! what lets the memory store be a faithful double rather than an approximate
//! one.

use async_trait::async_trait;
use munarium_core::evidence::{
    EvidenceAccess, EvidenceArtifact, EvidenceGrant, EvidenceManifest, EvidenceState,
    EvidenceStore, SealOutcome,
};
use munarium_core::{KernelError, Result};
use sqlx::{PgPool, Row};

use crate::storage_err;

#[derive(Clone)]
pub struct PgEvidenceStore {
    pool: PgPool,
}

impl PgEvidenceStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn row_to_artifact(row: &sqlx::postgres::PgRow) -> Result<EvidenceArtifact> {
    let manifest_json: serde_json::Value = row.get("manifest");
    let mut manifest: EvidenceManifest = serde_json::from_value(manifest_json)
        .map_err(|e| KernelError::Storage(format!("stored manifest is unreadable: {e}")))?;

    // Retention STATE lives in the columns, not in the stored JSON, and is
    // overlaid here. The manifest as sealed is immutable evidence — it must
    // still read back exactly as it was sealed — but `legal_hold` and
    // `purged_at` are server-owned facts that change afterwards. Keeping them
    // in columns means one source of truth for the janitor's predicate, and
    // overlaying them means a reader is never told an artifact is unheld when
    // an operator has held it.
    {
        let expires_at: Option<chrono::DateTime<chrono::Utc>> = row.get("expires_at");
        let purged_at: Option<chrono::DateTime<chrono::Utc>> = row.get("purged_at");
        let legal_hold: bool = row.get("legal_hold");
        // Overlay whenever the manifest CARRIES a retention block, not only
        // when a column is set: a manifest sealed with `legal_hold: true` and
        // no expiry has all three columns "falsy" once the hold is lifted,
        // and skipping the overlay then handed readers the sealed JSON's
        // `legal_hold: true` — so `op_purge` refused `evidence-on-hold`
        // forever for an artifact no longer held. The columns always win.
        if manifest.retention.is_some() || expires_at.is_some() || purged_at.is_some() || legal_hold
        {
            let r = manifest
                .retention
                .get_or_insert_with(munarium_core::evidence::Retention::default);
            r.expires_at =
                expires_at.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            r.purged_at = purged_at.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            r.legal_hold = legal_hold;
        }
    }
    let state_text: String = row.get("state");
    let state = EvidenceState::parse(&state_text).ok_or_else(|| {
        KernelError::Storage(format!(
            "unknown evidence state '{state_text}' in the database"
        ))
    })?;
    let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");
    let committed_at: Option<chrono::DateTime<chrono::Utc>> = row.get("committed_at");
    Ok(EvidenceArtifact {
        evidence_id: row.get("evidence_id"),
        tenant: row.get("tenant_id"),
        state,
        manifest,
        blob_path: row.get("blob_path"),
        created_at: created_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        committed_at: committed_at.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    })
}

const ARTIFACT_COLS: &str = "tenant_id, evidence_id, state, manifest, blob_path, created_at, \n     committed_at, expires_at, purged_at, legal_hold";

#[async_trait]
impl EvidenceStore for PgEvidenceStore {
    async fn register(
        &self,
        artifact: &EvidenceArtifact,
        grant: Option<&EvidenceGrant>,
    ) -> Result<SealOutcome> {
        let m = &artifact.manifest;
        let manifest_json = serde_json::to_value(m)
            .map_err(|e| KernelError::InvalidInput(format!("manifest is not serializable: {e}")))?;
        let expires_at = m.retention.as_ref().and_then(|r| r.expires_at.clone());
        let legal_hold = m.retention.as_ref().map(|r| r.legal_hold).unwrap_or(false);

        let mut tx = self.pool.begin().await.map_err(storage_err)?;

        // The whole idempotency decision is this ON CONFLICT. `DO NOTHING`
        // plus `RETURNING` means the insert returns a row only when it won, so
        // an empty result is precisely "someone else already sealed this".
        let inserted: Option<(String,)> = sqlx::query_as(
            "INSERT INTO evidence_artifacts
                (tenant_id, evidence_id, state, manifest, domain_key, logical_result_hash,
                 artifact_hash, bytes_len, media_type, kind, access_level, compartments,
                 expires_at, legal_hold, blob_path)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                     $13::timestamptz, $14, $15)
             ON CONFLICT (tenant_id, domain_key) DO NOTHING
             RETURNING evidence_id",
        )
        .bind(&artifact.tenant)
        .bind(&artifact.evidence_id)
        .bind(artifact.state.as_str())
        .bind(&manifest_json)
        .bind(m.domain_key())
        .bind(&m.logical_result_hash)
        .bind(&m.artifact_hash)
        .bind(m.bytes_len)
        .bind(&m.media_type)
        .bind(m.kind.as_str())
        .bind(m.authorization_class.access_level)
        .bind(&m.authorization_class.compartments)
        .bind(expires_at)
        .bind(legal_hold)
        .bind(&artifact.blob_path)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;

        let Some((evidence_id,)) = inserted else {
            // Lost the race (or a plain replay). Hand back the winner's id.
            let existing: (String,) = sqlx::query_as(
                "SELECT evidence_id FROM evidence_artifacts
                 WHERE tenant_id = $1 AND domain_key = $2",
            )
            .bind(&artifact.tenant)
            .bind(m.domain_key())
            .fetch_one(&mut *tx)
            .await
            .map_err(storage_err)?;
            tx.commit().await.map_err(storage_err)?;
            return Ok(SealOutcome {
                evidence_id: existing.0,
                created: false,
                grant: None,
            });
        };

        if let Some(g) = grant {
            sqlx::query(
                "INSERT INTO evidence_grants (tenant_id, grant_id, evidence_id, expires_at)
                 VALUES ($1, $2, $3, $4::timestamptz)",
            )
            .bind(&g.tenant)
            .bind(&g.grant_id)
            .bind(&g.evidence_id)
            .bind(&g.expires_at)
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;
        }
        tx.commit().await.map_err(storage_err)?;
        Ok(SealOutcome {
            evidence_id,
            created: true,
            grant: grant.cloned(),
        })
    }

    async fn get(&self, tenant: &str, evidence_id: &str) -> Result<Option<EvidenceArtifact>> {
        let row = sqlx::query(&format!(
            "SELECT {ARTIFACT_COLS} FROM evidence_artifacts
             WHERE tenant_id = $1 AND evidence_id = $2"
        ))
        .bind(tenant)
        .bind(evidence_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        row.as_ref().map(row_to_artifact).transpose()
    }

    async fn find_by_domain_key(
        &self,
        tenant: &str,
        domain_key: &str,
    ) -> Result<Option<EvidenceArtifact>> {
        let row = sqlx::query(&format!(
            "SELECT {ARTIFACT_COLS} FROM evidence_artifacts
             WHERE tenant_id = $1 AND domain_key = $2"
        ))
        .bind(tenant)
        .bind(domain_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        row.as_ref().map(row_to_artifact).transpose()
    }

    async fn commit(&self, tenant: &str, evidence_id: &str, at: &str) -> Result<bool> {
        // Conditional on the CURRENT state, so a replayed commit reports false
        // rather than silently restamping `committed_at` — which would make
        // the retention clock restartable by anyone who could replay a commit.
        // `pending` specifically, not "anything but committed": a purged
        // artifact whose bytes happened to survive a raced blob delete must
        // not be revived by a late commit.
        let updated = sqlx::query(
            "UPDATE evidence_artifacts
                SET state = 'committed', committed_at = $3::timestamptz
              WHERE tenant_id = $1 AND evidence_id = $2 AND state = 'pending'",
        )
        .bind(tenant)
        .bind(evidence_id)
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        if updated.rows_affected() > 0 {
            return Ok(true);
        }
        // Nothing changed: either already committed, or no such row.
        let exists: Option<(String,)> = sqlx::query_as(
            "SELECT evidence_id FROM evidence_artifacts WHERE tenant_id = $1 AND evidence_id = $2",
        )
        .bind(tenant)
        .bind(evidence_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        if exists.is_none() {
            return Err(KernelError::NotFound {
                kind: "evidence",
                id: evidence_id.to_string(),
            });
        }
        Ok(false)
    }

    async fn consume_grant(
        &self,
        tenant: &str,
        evidence_id: &str,
        grant_id: &str,
        now: &str,
    ) -> Result<Option<EvidenceGrant>> {
        // Check and mark in ONE statement. Every condition — right artifact,
        // unused, unexpired — is in the WHERE clause, so two callers arriving
        // together cannot both win.
        let row = sqlx::query(
            "UPDATE evidence_grants
                SET used_at = $4::timestamptz
              WHERE tenant_id = $1 AND grant_id = $2 AND evidence_id = $3
                AND used_at IS NULL AND expires_at > $4::timestamptz
              RETURNING tenant_id, grant_id, evidence_id, expires_at, used_at",
        )
        .bind(tenant)
        .bind(grant_id)
        .bind(evidence_id)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        let Some(row) = row else { return Ok(None) };
        let expires_at: chrono::DateTime<chrono::Utc> = row.get("expires_at");
        let used_at: Option<chrono::DateTime<chrono::Utc>> = row.get("used_at");
        Ok(Some(EvidenceGrant {
            grant_id: row.get("grant_id"),
            evidence_id: row.get("evidence_id"),
            tenant: row.get("tenant_id"),
            expires_at: expires_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            used_at: used_at.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        }))
    }

    async fn record_access(&self, access: &EvidenceAccess) -> Result<()> {
        sqlx::query(
            "INSERT INTO evidence_access
                (tenant_id, evidence_id, uid, kind, row_from, row_limit, outcome, at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::timestamptz)",
        )
        .bind(&access.tenant)
        .bind(&access.evidence_id)
        .bind(&access.uid)
        .bind(&access.kind)
        .bind(access.row_from)
        .bind(access.row_limit)
        .bind(&access.outcome)
        .bind(&access.at)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(())
    }

    async fn accesses(
        &self,
        tenant: &str,
        evidence_id: &str,
        limit: usize,
    ) -> Result<Vec<EvidenceAccess>> {
        let rows = sqlx::query(
            "SELECT tenant_id, evidence_id, uid, kind, row_from, row_limit, outcome, at
               FROM evidence_access
              WHERE tenant_id = $1 AND evidence_id = $2
              ORDER BY at DESC
              LIMIT $3",
        )
        .bind(tenant)
        .bind(evidence_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows
            .iter()
            .map(|row| {
                let at: chrono::DateTime<chrono::Utc> = row.get("at");
                EvidenceAccess {
                    evidence_id: row.get("evidence_id"),
                    tenant: row.get("tenant_id"),
                    uid: row.get("uid"),
                    kind: row.get("kind"),
                    row_from: row.get("row_from"),
                    row_limit: row.get("row_limit"),
                    outcome: row.get("outcome"),
                    at: at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                }
            })
            .collect())
    }

    // -- retention ---------------------------------------------------------

    async fn purge_due(&self, now: &str, limit: usize) -> Result<Vec<EvidenceArtifact>> {
        // The predicate matches the partial index `evidence_artifacts_due`
        // exactly, so this scan stays cheap as the table grows. NULL
        // `expires_at` is excluded by the comparison itself: an artifact
        // nobody gave a lifetime to is kept, never guessed at.
        let rows = sqlx::query(&format!(
            "SELECT {ARTIFACT_COLS} FROM evidence_artifacts
              WHERE state = 'committed'
                AND purged_at IS NULL
                AND legal_hold = FALSE
                AND expires_at IS NOT NULL
                AND expires_at <= $1::timestamptz
              ORDER BY expires_at
              LIMIT $2"
        ))
        .bind(now)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        rows.iter().map(row_to_artifact).collect()
    }

    async fn mark_purged(&self, tenant: &str, evidence_id: &str, at: &str) -> Result<bool> {
        // Conditional on `purged_at IS NULL`, so two instances sweeping the
        // same artifact cannot both claim it. No advisory lock is needed: the
        // byte delete that precedes this is idempotent, so duplicated work is
        // wasted effort rather than a correctness problem.
        let updated = sqlx::query(
            "UPDATE evidence_artifacts
                SET state = 'purged', purged_at = $3::timestamptz
              WHERE tenant_id = $1 AND evidence_id = $2 AND purged_at IS NULL",
        )
        .bind(tenant)
        .bind(evidence_id)
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        if updated.rows_affected() > 0 {
            return Ok(true);
        }
        let exists: Option<(String,)> = sqlx::query_as(
            "SELECT evidence_id FROM evidence_artifacts WHERE tenant_id = $1 AND evidence_id = $2",
        )
        .bind(tenant)
        .bind(evidence_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        if exists.is_none() {
            return Err(KernelError::NotFound {
                kind: "evidence",
                id: evidence_id.to_string(),
            });
        }
        Ok(false)
    }

    async fn set_legal_hold(&self, tenant: &str, evidence_id: &str, hold: bool) -> Result<bool> {
        let updated = sqlx::query(
            "UPDATE evidence_artifacts SET legal_hold = $3
              WHERE tenant_id = $1 AND evidence_id = $2",
        )
        .bind(tenant)
        .bind(evidence_id)
        .bind(hold)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(updated.rows_affected() > 0)
    }
}
