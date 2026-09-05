// SPDX-License-Identifier: Apache-2.0
//! Metric-view verification records.
//!
//! One table, one rule: a metric view may be executed only under a definition
//! that PASSED verification, and the fingerprint of that definition is what
//! this table remembers. The workers compare the live definition against the
//! latest passing record; a different fingerprint is `metric_view_changed`,
//! and no record at all is `not_covered` — an unverified view is not evidence.

use crate::{MatrixStore, Result};
use sqlx::Row;

/// What one verification writes.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricVerificationRecord<'a> {
    pub kind: &'a str,
    pub view_name: &'a str,
    pub view_version: u32,
    pub fingerprint: &'a str,
    pub passed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetricVerification {
    pub fingerprint: String,
    pub passed: i64,
    pub failed: i64,
    pub verified_at: String,
}

impl MatrixStore {
    /// Append a verification outcome. Never updates: the previous record
    /// stays as the history of when the definition was last known-good.
    pub async fn record_metric_verification(
        &self,
        tenant: &str,
        rec: &MetricVerificationRecord<'_>,
    ) -> Result<()> {
        let MetricVerificationRecord {
            kind,
            view_name,
            view_version,
            fingerprint,
            passed,
            failed,
        } = *rec;
        sqlx::query(
            "INSERT INTO matrix.metric_view_verifications
                 (tenant_id, view_name, view_version, fingerprint, passed, failed, kind)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(tenant)
        .bind(view_name)
        .bind(view_version as i32)
        .bind(fingerprint)
        .bind(passed as i32)
        .bind(failed as i32)
        .bind(kind)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The most recent verification of this view version, passing or not.
    /// The caller decides what a failing one means; the store only remembers.
    pub async fn latest_metric_verification(
        &self,
        tenant: &str,
        kind: &str,
        view_name: &str,
        view_version: u32,
    ) -> Result<Option<MetricVerification>> {
        let row = sqlx::query(
            "SELECT fingerprint, passed, failed, verified_at::text AS verified_at
               FROM matrix.metric_view_verifications
              WHERE tenant_id = $1 AND view_name = $2 AND view_version = $3 AND kind = $4
              ORDER BY verified_at DESC
              LIMIT 1",
        )
        .bind(tenant)
        .bind(view_name)
        .bind(view_version as i32)
        .bind(kind)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| MetricVerification {
            fingerprint: r.get("fingerprint"),
            passed: r.get::<i32, _>("passed") as i64,
            failed: r.get::<i32, _>("failed") as i64,
            verified_at: r.get("verified_at"),
        }))
    }
}
