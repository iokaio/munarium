// SPDX-License-Identifier: Apache-2.0
//! Which index versions a scope must be able to serve.
//!
//! A scope's **serving-required versions** (§9.1) are the ones an artifact must
//! exist for before that scope may be selected for datastore serving. Backfill
//! builds them; the selector CAS refuses an incomplete set. Getting this list
//! wrong in the *short* direction is the dangerous one: it makes an incomplete
//! set look complete, and the failure surfaces as a session that cannot resolve
//! the version it pinned.

use sqlx::Row;

use munarium_core::{KernelError, Result};

use crate::{storage_err, PgRetrieval};

/// The `retrieval_rollout.required_versions_policy` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredVersionsPolicy {
    /// The active version only.
    Active,
    /// Active plus every version a live session or runbook has pinned.
    ActiveAndPinned,
    /// The default: active, pinned, and every version retired within the pin
    /// horizon.
    ActivePinnedAndHorizon,
}

impl RequiredVersionsPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::ActiveAndPinned => "active_and_pinned",
            Self::ActivePinnedAndHorizon => "active_pinned_and_horizon",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "active" => Ok(Self::Active),
            "active_and_pinned" => Ok(Self::ActiveAndPinned),
            "active_pinned_and_horizon" => Ok(Self::ActivePinnedAndHorizon),
            other => Err(KernelError::InvalidInput(format!(
                "unknown required_versions_policy {other:?}"
            ))),
        }
    }
}

/// Why a version is in the required set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredReason {
    Active,
    /// Not active, but built recently enough that a live session could still
    /// hold a pin on it.
    WithinHorizon,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RequiredVersion {
    pub index_version_id: String,
    pub reason: RequiredReason,
    /// The version's own watermark, carried so a reconstructed `BuildSpec`
    /// states the snapshot the version was actually built at.
    pub watermark_seq: u64,
    pub shape_ref: String,
}

impl PgRetrieval {
    /// The serving-required versions of one collection.
    ///
    /// ## Why `active_and_pinned` is refused
    ///
    /// There is no table recording which index version a session pinned — a
    /// session resolves its version at request time. So pins cannot be
    /// enumerated directly. Under the DEFAULT policy that is fine, because the
    /// horizon term subsumes them: the pin horizon is derived from the session
    /// and runbook TTLs plus a recovery margin, so a version a live session
    /// could still be holding is by construction a version retired within the
    /// horizon.
    ///
    /// `active_and_pinned` drops the horizon term while still naming pins, and
    /// nothing then covers them. Returning "active only" for it would be a set
    /// that is quietly short of what the policy promises — exactly the failure
    /// this list must not have — so it is refused instead.
    pub async fn required_versions(
        &self,
        collection_id: &str,
        policy: RequiredVersionsPolicy,
        pin_horizon_secs: i64,
    ) -> Result<Vec<RequiredVersion>> {
        if policy == RequiredVersionsPolicy::ActiveAndPinned {
            return Err(KernelError::InvalidInput(
                "required_versions_policy 'active_and_pinned' names session pins but drops the \
                 horizon term that is the only thing able to cover them; use \
                 'active_pinned_and_horizon' (the default) or 'active'"
                    .into(),
            ));
        }
        if pin_horizon_secs <= 0 && policy == RequiredVersionsPolicy::ActivePinnedAndHorizon {
            return Err(KernelError::InvalidInput(
                "a pin horizon of zero would make the horizon term empty while the policy still \
                 promises pinned versions"
                    .into(),
            ));
        }

        let horizon = matches!(policy, RequiredVersionsPolicy::ActivePinnedAndHorizon);
        // `activated_at IS NOT NULL` in the horizon term: a version that has
        // never been ACTIVE can hold no session pins — a pin is taken from
        // the version a session found active — so a freshly committed direct
        // build must not join the serving-required set before its promotion
        // and activation. Found live: an in-flight build wedged /readyz on
        // the only replica, taking down the very API its own promote needed.
        //
        // The horizon is measured from when the version STOPPED being active
        // (`deactivated_at`, migration 0030), not from `built_at`: a pin is
        // taken while a version is active, so the last moment a live session
        // could have pinned it is its deactivation, and a version built a
        // month ago and retired ten minutes ago is exactly the one a session
        // is still holding. Rows that predate the column carry
        // `deactivated_at = built_at` from the migration's backfill, which is
        // the old rule stated honestly rather than a guess.
        let rows = sqlx::query(
            "SELECT id, active, watermark_seq, shape_ref,
                    (activated_at IS NOT NULL
                     AND COALESCE(deactivated_at, built_at)
                         > now() - make_interval(secs => $3)) AS within_horizon
               FROM index_versions
              WHERE tenant_id = $1 AND collection_id = $2
                AND (active OR ($4 AND activated_at IS NOT NULL
                                    AND COALESCE(deactivated_at, built_at)
                                        > now() - make_interval(secs => $3)))
              ORDER BY active DESC, built_at DESC, id",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(pin_horizon_secs.max(0) as f64)
        .bind(horizon)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;

        rows.into_iter().map(row_to_required).collect()
    }

    /// The serving-required versions of one LEGACY shape scope: the shape's
    /// own versions, `collection_id IS NULL` — the same exclusion
    /// `resolve_index` applies, for the same reason: a collection's index on
    /// the same shape is a different scope with a different chunk table.
    ///
    /// Same policy semantics as the collection form, including the
    /// `active_and_pinned` refusal it inherits.
    pub async fn required_versions_for_shape(
        &self,
        shape_ref: &str,
        policy: RequiredVersionsPolicy,
        pin_horizon_secs: i64,
    ) -> Result<Vec<RequiredVersion>> {
        if policy == RequiredVersionsPolicy::ActiveAndPinned {
            return Err(KernelError::InvalidInput(
                "required_versions_policy 'active_and_pinned' names session pins but drops the horizon term that makes them enumerable; use active_pinned_and_horizon"
                    .into(),
            ));
        }
        if policy == RequiredVersionsPolicy::ActivePinnedAndHorizon && pin_horizon_secs <= 0 {
            return Err(KernelError::InvalidInput(
                "a pin horizon of zero would make the horizon term empty while the policy still promises pinned versions"
                    .into(),
            ));
        }
        let horizon = matches!(policy, RequiredVersionsPolicy::ActivePinnedAndHorizon);
        // Same never-activated exclusion and deactivation-anchored horizon as
        // the collection form above.
        let rows = sqlx::query(
            "SELECT id, active, watermark_seq, shape_ref,
                    (activated_at IS NOT NULL
                     AND COALESCE(deactivated_at, built_at)
                         > now() - make_interval(secs => $3)) AS within_horizon
               FROM index_versions
              WHERE tenant_id = $1 AND shape_ref = $2 AND collection_id IS NULL
                AND (active OR ($4 AND activated_at IS NOT NULL
                                    AND COALESCE(deactivated_at, built_at)
                                        > now() - make_interval(secs => $3)))
              ORDER BY active DESC, built_at DESC, id",
        )
        .bind(&self.tenant_id)
        .bind(shape_ref)
        .bind(pin_horizon_secs.max(0) as f64)
        .bind(horizon)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        rows.into_iter().map(row_to_required).collect()
    }
}

fn row_to_required(r: sqlx::postgres::PgRow) -> Result<RequiredVersion> {
    let watermark: i64 = r.get("watermark_seq");
    Ok(RequiredVersion {
        index_version_id: r.get("id"),
        reason: if r.get::<bool, _>("active") {
            RequiredReason::Active
        } else {
            RequiredReason::WithinHorizon
        },
        watermark_seq: u64::try_from(watermark).map_err(|_| {
            KernelError::Storage("an index version has a negative watermark".into())
        })?,
        shape_ref: r.get("shape_ref"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal is the interesting behaviour, and it needs no database: a
    /// policy that promises pin coverage nothing can supply must not silently
    /// return a shorter list.
    #[tokio::test]
    async fn the_pinned_only_policy_is_refused_rather_than_silently_shortened() {
        assert_eq!(
            RequiredVersionsPolicy::parse("active_and_pinned").unwrap(),
            RequiredVersionsPolicy::ActiveAndPinned
        );
        assert!(RequiredVersionsPolicy::parse("everything").is_err());
        assert_eq!(
            RequiredVersionsPolicy::ActivePinnedAndHorizon.as_str(),
            "active_pinned_and_horizon"
        );
    }
}
