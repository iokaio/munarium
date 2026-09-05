// SPDX-License-Identifier: Apache-2.0
//! The journal: one row per meaningful thing Matrix did.
//!
//! Redaction is the default and the reveal is itself journaled. A journal that
//! quietly contains customer parameters and result cells is a second copy of
//! the data with none of the controls around it, so `payload_json` is written
//! only when the caller explicitly says the policy allows it.

use crate::{new_id, MatrixStore, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalRecord {
    pub kind: String,
    pub source_name: Option<String>,
    pub asset_ref: Option<String>,
    pub request_id: Option<String>,
    pub actor: Option<String>,
    pub via: Option<String>,
    pub outcome: String,
    pub refusal_class: Option<String>,
    pub refusal_code: Option<String>,
    pub rows_out: Option<i64>,
    pub bytes_out: Option<i64>,
    pub duration_ms: Option<i64>,
    pub evidence_id: Option<String>,
    /// Only ever `Some` when the caller has decided the policy permits it.
    pub payload: Option<serde_json::Value>,
    /// For an `execute` row (2026-08-30): the source's statement window and
    /// the seal call, so `duration_ms` minus both is Matrix's own share.
    pub source_ms: Option<i64>,
    pub seal_ms: Option<i64>,
}

impl JournalRecord {
    pub fn new(kind: &str, outcome: &str) -> Self {
        Self {
            kind: kind.to_string(),
            source_name: None,
            asset_ref: None,
            request_id: None,
            actor: None,
            via: None,
            outcome: outcome.to_string(),
            refusal_class: None,
            refusal_code: None,
            rows_out: None,
            bytes_out: None,
            duration_ms: None,
            evidence_id: None,
            payload: None,
            source_ms: None,
            seal_ms: None,
        }
    }

    pub fn source(mut self, s: impl Into<String>) -> Self {
        self.source_name = Some(s.into());
        self
    }
    pub fn asset(mut self, s: impl Into<String>) -> Self {
        self.asset_ref = Some(s.into());
        self
    }
    pub fn request(mut self, s: Option<String>) -> Self {
        self.request_id = s;
        self
    }
    pub fn actor(mut self, s: Option<String>) -> Self {
        self.actor = s;
        self
    }
    pub fn via(mut self, s: &str) -> Self {
        self.via = Some(s.to_string());
        self
    }
    pub fn duration(mut self, ms: u128) -> Self {
        self.duration_ms = Some(ms as i64);
        self
    }

    /// The two pieces of an execute's time that are not Matrix's (2026-08-30).
    pub fn timings(mut self, source_ms: u64, seal_ms: u64) -> Self {
        self.source_ms = Some(source_ms as i64);
        self.seal_ms = Some(seal_ms as i64);
        self
    }
    pub fn evidence(mut self, id: Option<String>) -> Self {
        self.evidence_id = id;
        self
    }
    pub fn rows(mut self, n: usize) -> Self {
        self.rows_out = Some(n as i64);
        self
    }
    pub fn bytes(mut self, n: usize) -> Self {
        self.bytes_out = Some(n as i64);
        self
    }

    /// Record a refusal. Takes the whole refusal so the class and code cannot
    /// drift apart at a call site.
    pub fn refused(mut self, r: &munarium_matrix_core::Refusal) -> Self {
        self.outcome = "refused".into();
        self.refusal_class = Some(r.class.as_str().to_string());
        self.refusal_code = Some(r.code.clone());
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct JournalQuery {
    pub kind: Option<String>,
    pub source_name: Option<String>,
    pub refusals_only: bool,
    pub before: Option<chrono::DateTime<chrono::Utc>>,
    pub limit: i64,
}

impl MatrixStore {
    pub async fn journal(&self, tenant: &str, rec: JournalRecord) -> Result<String> {
        let id = new_id("jrn");
        let redacted = rec.payload.is_none();
        sqlx::query(
            "INSERT INTO matrix.journal
               (id, tenant_id, kind, source_name, asset_ref, request_id, actor, via, outcome,
                refusal_class, refusal_code, rows_out, bytes_out, duration_ms, evidence_id,
                payload_json, redacted, source_ms, seal_ms)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)",
        )
        .bind(&id)
        .bind(tenant)
        .bind(&rec.kind)
        .bind(&rec.source_name)
        .bind(&rec.asset_ref)
        .bind(&rec.request_id)
        .bind(&rec.actor)
        .bind(&rec.via)
        .bind(&rec.outcome)
        .bind(&rec.refusal_class)
        .bind(&rec.refusal_code)
        .bind(rec.rows_out)
        .bind(rec.bytes_out)
        .bind(rec.duration_ms)
        .bind(&rec.evidence_id)
        .bind(&rec.payload)
        .bind(redacted)
        .bind(rec.source_ms)
        .bind(rec.seal_ms)
        .execute(self.pool())
        .await?;
        Ok(id)
    }

    #[allow(clippy::type_complexity)]
    pub async fn list_journal(
        &self,
        tenant: &str,
        q: &JournalQuery,
    ) -> Result<Vec<munarium_matrix_types::dto::JournalEntry>> {
        let limit = q.limit.clamp(1, 500);
        // A named row rather than a tuple: sqlx implements `FromRow` for
        // tuples up to sixteen elements, and the timing columns (2026-08-30)
        // took this one to seventeen.
        #[derive(sqlx::FromRow)]
        struct Row {
            id: String,
            kind: String,
            source_name: Option<String>,
            asset_ref: Option<String>,
            request_id: Option<String>,
            actor: Option<String>,
            via: Option<String>,
            outcome: String,
            refusal_code: Option<String>,
            rows_out: Option<i64>,
            bytes_out: Option<i64>,
            duration_ms: Option<i64>,
            evidence_id: Option<String>,
            redacted: bool,
            created_at: String,
            source_ms: Option<i64>,
            seal_ms: Option<i64>,
        }
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, kind, source_name, asset_ref, request_id, actor, via, outcome,
                    refusal_code, rows_out, bytes_out, duration_ms, evidence_id, redacted,
                    created_at::text AS created_at, source_ms, seal_ms
               FROM matrix.journal
              WHERE tenant_id = $1
                AND ($2::text IS NULL OR kind = $2)
                AND ($3::text IS NULL OR source_name = $3)
                AND (NOT $4 OR refusal_code IS NOT NULL)
                AND ($5::timestamptz IS NULL OR created_at < $5)
              ORDER BY created_at DESC LIMIT $6",
        )
        .bind(tenant)
        .bind(&q.kind)
        .bind(&q.source_name)
        .bind(q.refusals_only)
        .bind(q.before)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| munarium_matrix_types::dto::JournalEntry {
                id: r.id,
                kind: r.kind,
                tenant: tenant.to_string(),
                source: r.source_name,
                asset_ref: r.asset_ref,
                request_id: r.request_id,
                actor: r.actor,
                via: r.via,
                outcome: r.outcome,
                refusal_code: r.refusal_code,
                rows: r.rows_out.map(|v| v as u64),
                bytes: r.bytes_out.map(|v| v as u64),
                duration_ms: r.duration_ms.map(|v| v as u64),
                evidence_id: r.evidence_id,
                created_at: r.created_at,
                redacted: r.redacted,
                source_ms: r.source_ms.map(|v| v as u64),
                seal_ms: r.seal_ms.map(|v| v as u64),
            })
            .collect())
    }
}

/// An asset whose LATEST successful apply came through the operator console
///: the deployment holds bytes the repository does not, until the
/// exported bundle lands and is applied from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftedAsset {
    pub asset_ref: String,
    /// The decision id the operator gave the console, which the console
    /// records as the apply's request id.
    pub decision_id: Option<String>,
    pub applied_at: String,
}

impl MatrixStore {
    /// Every asset drifted from git, newest first. Derived, not stored: the
    /// journal already holds every apply with the plane it came in on, and a
    /// second table saying the same thing would be a second thing to keep
    /// true. The flag CLEARS when a later apply of the same `asset_ref` comes
    /// in by any other plane — `mxctl` from the landed bundle, CI, the API —
    /// which is the only observable form of "the bundle landed".
    pub async fn drifted_assets(&self, tenant: &str) -> Result<Vec<DriftedAsset>> {
        let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT a.asset_ref, a.request_id, a.created_at::text
               FROM matrix.journal a
              WHERE a.tenant_id = $1
                AND a.kind = 'apply' AND a.outcome = 'ok' AND a.via = 'admin-ui'
                AND a.asset_ref IS NOT NULL
                AND NOT EXISTS (
                    SELECT 1 FROM matrix.journal b
                     WHERE b.tenant_id = a.tenant_id
                       AND b.kind = 'apply' AND b.outcome = 'ok'
                       AND b.asset_ref = a.asset_ref
                       AND b.created_at > a.created_at)
              ORDER BY a.created_at DESC",
        )
        .bind(tenant)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(asset_ref, decision_id, applied_at)| DriftedAsset {
                asset_ref,
                decision_id,
                applied_at,
            })
            .collect())
    }
}
