// SPDX-License-Identifier: Apache-2.0
//! The artifact catalog, bindings and build attempts.
//!
//! Every method here is tenant-scoped **by construction**: `ArtifactCatalog`
//! is built from a tenant and binds it into every predicate. There is no RLS in
//! this database — isolation is a handle bound to one tenant plus an explicit
//! `WHERE tenant_id = $1` bound from that handle, never from caller input —
//! so a helper that took the tenant as an argument would be one refactor away
//! from taking it from a request.
//!
//! `artifact_id` is a content hash and never an authority. The same corpus in
//! two tenants legitimately produces the same hash, so no method accepts a bare
//! artifact id as permission to read anything.

use sqlx::{PgPool, Row};

use munarium_core::{KernelError, Result};

use crate::storage_err;

/// Catalog state for a physical artifact.
///
/// Pre-seal state deliberately has no variant: an artifact that does not exist
/// yet is an *attempt*, and giving it a catalog row would mean the catalog
/// advertises things that may never be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactState {
    Sealed,
    Verified,
    Failed,
    Retired,
}

impl ArtifactState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sealed => "sealed",
            Self::Verified => "verified",
            Self::Failed => "failed",
            Self::Retired => "retired",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "sealed" => Self::Sealed,
            "verified" => Self::Verified,
            "failed" => Self::Failed,
            "retired" => Self::Retired,
            other => {
                return Err(KernelError::InvalidInput(format!(
                    "unknown artifact state {other:?}"
                )))
            }
        })
    }
}

/// Which role an artifact plays for a logical version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingSlot {
    /// A candidate being prewarmed. Never serves.
    Staged,
    /// Answers sampled comparison queries. Never reaches a user.
    Shadow,
    /// Answers user traffic for scopes the selector routes here.
    Serving,
}

impl BindingSlot {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Shadow => "shadow",
            Self::Serving => "serving",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "staged" => Self::Staged,
            "shadow" => Self::Shadow,
            "serving" => Self::Serving,
            other => return Err(KernelError::InvalidInput(format!("unknown slot {other:?}"))),
        })
    }
}

/// A catalog row, as callers read it.
#[derive(Debug, Clone)]
pub struct ArtifactRow {
    pub index_version_id: String,
    pub artifact_id: String,
    pub engine_id: String,
    pub state: ArtifactState,
    pub format_version: i32,
    pub artifact_uri: String,
    pub artifact_plan_sha256: String,
    pub bytes_len: i64,
    pub file_count: i32,
}

/// What a caller supplies to catalog a freshly sealed artifact.
#[derive(Debug, Clone)]
pub struct NewArtifact {
    pub index_version_id: String,
    pub artifact_id: String,
    pub engine_id: String,
    pub format_version: i32,
    pub artifact_uri: String,
    pub artifact_plan: serde_json::Value,
    pub artifact_plan_sha256: String,
    /// An AUDIT PROJECTION. Never a reader input: the open path fetches the
    /// canonical L2 manifest and checks its hash.
    pub artifact_manifest: serde_json::Value,
    pub bytes_len: i64,
    pub file_count: i32,
    pub built_by: Option<String>,
    pub attempt_id: Option<String>,
}

/// What `insert_sealed` did.
///
/// The distinction is the whole point of §7.1 step 7: a rebuild that finds an
/// identical artifact already cataloged has **converged**, not failed, and a
/// caller that could not tell the difference would either report a healthy
/// rebuild as an error or silently publish over someone else's artifact.
#[derive(Debug, Clone, PartialEq)]
pub enum InsertOutcome {
    /// This call created the row; continue publishing.
    Inserted,
    /// A `verified` row already existed. Discard local output and stop.
    Converged { existing_state: ArtifactState },
    /// A `sealed` row existed — a publication in flight or interrupted.
    /// Adopt it and continue publication.
    Adopted { existing_state: ArtifactState },
    /// A `failed` or `retired` row existed. Publication cannot proceed:
    /// `mark_verified` moves only `sealed`→`verified`, so adopting such a row
    /// would carry the build all the way to the last step and fail there —
    /// and since the id is content-addressed, every rebuild of the same
    /// content would wedge on it the same way. Reported as its own outcome
    /// so the caller can say what is actually blocking.
    Blocked { existing_state: ArtifactState },
}

/// The current occupant of one slot.
#[derive(Debug, Clone)]
pub struct BindingRow {
    pub index_version_id: String,
    pub slot: BindingSlot,
    pub artifact_id: String,
    pub generation: i64,
}

/// Tenant-scoped access to the artifact catalog.
#[derive(Debug, Clone)]
pub struct ArtifactCatalog {
    pool: PgPool,
    tenant_id: String,
}

impl ArtifactCatalog {
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            pool,
            tenant_id: tenant_id.into(),
        }
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// Catalog a sealed artifact, converging on a conflict (§7.1 step 7).
    ///
    /// The conflict path is not an error handler. Two builders producing
    /// byte-identical output is the *expected* result of a rebuild, and the
    /// only question is which of them publishes.
    pub async fn insert_sealed(&self, a: &NewArtifact) -> Result<InsertOutcome> {
        let inserted = sqlx::query(
            "INSERT INTO index_artifacts (
                 tenant_id, index_version_id, artifact_id, engine_id, state,
                 format_version, artifact_uri, artifact_plan, artifact_plan_sha256,
                 artifact_manifest, bytes_len, file_count, built_by, attempt_id, sealed_at)
             VALUES ($1,$2,$3,$4,'sealed',$5,$6,$7,$8,$9,$10,$11,$12,$13, now())
             ON CONFLICT (tenant_id, index_version_id, artifact_id) DO NOTHING
             RETURNING artifact_id",
        )
        .bind(&self.tenant_id)
        .bind(&a.index_version_id)
        .bind(&a.artifact_id)
        .bind(&a.engine_id)
        .bind(a.format_version)
        .bind(&a.artifact_uri)
        .bind(&a.artifact_plan)
        .bind(&a.artifact_plan_sha256)
        .bind(&a.artifact_manifest)
        .bind(a.bytes_len)
        .bind(a.file_count)
        .bind(&a.built_by)
        .bind(&a.attempt_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;

        if inserted.is_some() {
            return Ok(InsertOutcome::Inserted);
        }

        let existing = self
            .artifact(&a.index_version_id, &a.artifact_id)
            .await?
            .ok_or_else(|| {
                // The insert conflicted, so a row exists. If it does not, the
                // row was deleted between the two statements -- which nothing
                // in this design does, so it is a real anomaly rather than a
                // race to paper over.
                KernelError::Storage(
                    "artifact insert conflicted but no row is readable; the catalog is \
                     being modified by something that should not be modifying it"
                        .into(),
                )
            })?;

        Ok(match existing.state {
            ArtifactState::Verified => InsertOutcome::Converged {
                existing_state: existing.state,
            },
            ArtifactState::Sealed => InsertOutcome::Adopted {
                existing_state: existing.state,
            },
            ArtifactState::Failed | ArtifactState::Retired => InsertOutcome::Blocked {
                existing_state: existing.state,
            },
        })
    }

    pub async fn artifact(
        &self,
        index_version_id: &str,
        artifact_id: &str,
    ) -> Result<Option<ArtifactRow>> {
        let row = sqlx::query(
            "SELECT index_version_id, artifact_id, engine_id, state, format_version,
                    artifact_uri, artifact_plan_sha256, bytes_len, file_count
               FROM index_artifacts
              WHERE tenant_id = $1 AND index_version_id = $2 AND artifact_id = $3",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(artifact_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        row.map(Self::to_artifact).transpose()
    }

    /// The canonical manifest URI, and nothing else.
    ///
    /// Returns only what the open path needs, so a caller cannot accidentally
    /// read the JSONB projection and treat it as the manifest.
    pub async fn artifact_uri(
        &self,
        index_version_id: &str,
        artifact_id: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .artifact(index_version_id, artifact_id)
            .await?
            .map(|a| a.artifact_uri))
    }

    pub async fn artifacts_for_version(&self, index_version_id: &str) -> Result<Vec<ArtifactRow>> {
        let rows = sqlx::query(
            "SELECT index_version_id, artifact_id, engine_id, state, format_version,
                    artifact_uri, artifact_plan_sha256, bytes_len, file_count
               FROM index_artifacts
              WHERE tenant_id = $1 AND index_version_id = $2
              ORDER BY artifact_id",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        rows.into_iter().map(Self::to_artifact).collect()
    }

    /// Mark an artifact verified. Only a `sealed` row may become `verified`:
    /// a catalog row must never advertise a usable artifact before L2
    /// verification has actually succeeded.
    pub async fn mark_verified(
        &self,
        index_version_id: &str,
        artifact_id: &str,
        verified_by: &str,
    ) -> Result<()> {
        let done = sqlx::query(
            "UPDATE index_artifacts
                SET state = 'verified', verified_by = $4,
                    verified_at = now(), last_verified_at = now()
              WHERE tenant_id = $1 AND index_version_id = $2 AND artifact_id = $3
                AND state = 'sealed'",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(artifact_id)
        .bind(verified_by)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        if done.rows_affected() == 0 {
            return Err(KernelError::InvalidInput(format!(
                "artifact {artifact_id} is not in state 'sealed'; only a sealed artifact \
                 may be verified"
            )));
        }
        Ok(())
    }

    fn to_artifact(row: sqlx::postgres::PgRow) -> Result<ArtifactRow> {
        Ok(ArtifactRow {
            index_version_id: row.get("index_version_id"),
            artifact_id: row.get("artifact_id"),
            engine_id: row.get("engine_id"),
            state: ArtifactState::parse(row.get::<String, _>("state").as_str())?,
            format_version: row.get("format_version"),
            artifact_uri: row.get("artifact_uri"),
            artifact_plan_sha256: row.get("artifact_plan_sha256"),
            bytes_len: row.get("bytes_len"),
            file_count: row.get("file_count"),
        })
    }

    // -- bindings ------------------------------------------------------------

    pub async fn binding(
        &self,
        index_version_id: &str,
        slot: BindingSlot,
    ) -> Result<Option<BindingRow>> {
        let row = sqlx::query(
            "SELECT index_version_id, slot, artifact_id, generation
               FROM index_artifact_bindings
              WHERE tenant_id = $1 AND index_version_id = $2 AND slot = $3",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(slot.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        row.map(|r| {
            Ok(BindingRow {
                index_version_id: r.get("index_version_id"),
                slot: BindingSlot::parse(r.get::<String, _>("slot").as_str())?,
                artifact_id: r.get("artifact_id"),
                generation: r.get("generation"),
            })
        })
        .transpose()
    }

    /// Fill an empty slot.
    ///
    /// An INSERT, not a compare-and-swap: the first binding has no generation
    /// to compare against, and §7.3 says so explicitly. A slot that is already
    /// occupied is refused here — replacing one is a promotion, which is a
    /// different operation with a different safety argument.
    ///
    /// Refuses an artifact that is not `verified`: a binding is a statement
    /// that something is servable, and an unverified artifact is not.
    /// Refuse unless the artifact exists and is `verified`, holding a share
    /// lock on its row for the rest of `tx` so a concurrent state change
    /// serializes behind the binding write rather than racing it.
    async fn require_verified_locked(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        index_version_id: &str,
        artifact_id: &str,
    ) -> Result<()> {
        let state: Option<String> = sqlx::query_scalar(
            "SELECT state FROM index_artifacts
              WHERE tenant_id = $1 AND index_version_id = $2 AND artifact_id = $3
              FOR SHARE",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(artifact_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage_err)?;
        match state.as_deref() {
            None => Err(KernelError::NotFound {
                kind: "artifact",
                id: artifact_id.to_string(),
            }),
            Some(s) if s == ArtifactState::Verified.as_str() => Ok(()),
            Some(other) => Err(KernelError::InvalidInput(format!(
                "artifact {artifact_id} is {other}, not verified; a binding asserts that an \
                 artifact is servable"
            ))),
        }
    }

    pub async fn bind_new(
        &self,
        index_version_id: &str,
        slot: BindingSlot,
        artifact_id: &str,
        selected_by: &str,
        reason: Option<&str>,
    ) -> Result<BindingRow> {
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        // The verified check runs INSIDE the transaction, holding a share
        // lock on the artifact row, so a retirement or failure that lands
        // between the check and the binding write waits behind it instead of
        // leaving a binding to an artifact that is no longer verified.
        // `promote_staged` re-reads under its lock for the same reason.
        self.require_verified_locked(&mut tx, index_version_id, artifact_id)
            .await?;
        let inserted = sqlx::query(
            "INSERT INTO index_artifact_bindings
                 (tenant_id, index_version_id, slot, artifact_id, generation, selected_by, reason)
             VALUES ($1,$2,$3,$4,1,$5,$6)
             ON CONFLICT (tenant_id, index_version_id, slot) DO NOTHING
             RETURNING generation",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(slot.as_str())
        .bind(artifact_id)
        .bind(selected_by)
        .bind(reason)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;

        let Some(row) = inserted else {
            return Err(KernelError::InvalidInput(format!(
                "slot {} for {index_version_id} is already occupied; replacing a binding is a \
                 promotion, not an insert",
                slot.as_str()
            )));
        };
        let generation: i64 = row.get("generation");

        self.append_event(
            &mut tx,
            index_version_id,
            slot,
            None,
            Some(artifact_id),
            None,
            Some(generation),
            "bind",
            selected_by,
            reason,
        )
        .await?;
        tx.commit().await.map_err(storage_err)?;

        Ok(BindingRow {
            index_version_id: index_version_id.to_string(),
            slot,
            artifact_id: artifact_id.to_string(),
            generation,
        })
    }

    /// Replace a slot's occupant, compare-and-swap on its generation.
    ///
    /// For the `staged` and `shadow` slots only. The `serving` slot is refused
    /// STRUCTURALLY: replacing what answers user traffic is the §7.3 promotion
    /// — a transaction that re-reads deployment expectations and re-verifies
    /// per-node opens — and a bare rebind that touched `serving` would be a
    /// way around every one of those checks. Refusing here rather than only at
    /// the API layer means a future caller cannot acquire the bypass by
    /// linking against the catalog.
    ///
    /// Refuses an artifact that is not `verified`, exactly as `bind_new` does,
    /// and refuses a stale expected generation — the caller read the binding,
    /// decided, and someone else moved it first.
    pub async fn rebind(
        &self,
        index_version_id: &str,
        slot: BindingSlot,
        artifact_id: &str,
        expected_generation: i64,
        selected_by: &str,
        reason: Option<&str>,
    ) -> Result<BindingRow> {
        if slot == BindingSlot::Serving {
            return Err(KernelError::InvalidInput(
                "the serving slot is changed by the promotion operation, never by a rebind;                  promotion re-checks deployment expectations and per-node opens, and this path deliberately cannot"
                    .into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        // Same in-transaction, share-locked check as `bind_new`.
        self.require_verified_locked(&mut tx, index_version_id, artifact_id)
            .await?;
        let updated = sqlx::query(
            "UPDATE index_artifact_bindings
                SET artifact_id = $5, generation = generation + 1,
                    selected_by = $6, reason = $7
              WHERE tenant_id = $1 AND index_version_id = $2 AND slot = $3
                AND generation = $4
             RETURNING generation",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(slot.as_str())
        .bind(expected_generation)
        .bind(artifact_id)
        .bind(selected_by)
        .bind(reason)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;

        let Some(row) = updated else {
            return Err(KernelError::InvalidInput(format!(
                "slot {} for {index_version_id} is not at generation {expected_generation};                  re-read the binding and decide again",
                slot.as_str()
            )));
        };
        let generation: i64 = row.get("generation");

        self.append_event(
            &mut tx,
            index_version_id,
            slot,
            None,
            Some(artifact_id),
            Some(expected_generation),
            Some(generation),
            "rebind",
            selected_by,
            reason,
        )
        .await?;
        tx.commit().await.map_err(storage_err)?;

        Ok(BindingRow {
            index_version_id: index_version_id.to_string(),
            slot,
            artifact_id: artifact_id.to_string(),
            generation,
        })
    }

    /// Promote the `staged` binding into `serving` — §7.3 step 4, as one
    /// tenant transaction.
    ///
    /// The caller supplies BOTH generations it read: the staged slot's, and
    /// the serving slot's (0 = "there is no serving binding yet"). Inside the
    /// transaction the two rows are locked in a stable order, the generations
    /// are re-verified, the candidate artifact is re-checked `verified`, the
    /// serving slot is written at `generation + 1`, the event is appended,
    /// and the staged row is deleted only if its locked generation still
    /// matches. Any comparison failing rolls the whole thing back and leaves
    /// `staged` intact.
    ///
    /// The fleet-side cutover gate — every required plane's nodes holding the
    /// staged candidate open — is evaluated by the CALLER before this runs
    /// (it reads node snapshots, which are server-side state this crate does
    /// not own). This method is the atomic tail of the promotion, not the
    /// whole of it.
    pub async fn promote_staged(
        &self,
        index_version_id: &str,
        expected_staged_generation: i64,
        expected_serving_generation: i64,
        actor: &str,
        reason: Option<&str>,
    ) -> Result<BindingRow> {
        let mut tx = self.pool.begin().await.map_err(storage_err)?;

        // Stable lock order: both slots in one statement, ordered by slot
        // text, so two concurrent promotions serialize instead of deadlocking.
        let locked = sqlx::query(
            "SELECT slot, artifact_id, generation FROM index_artifact_bindings
              WHERE tenant_id = $1 AND index_version_id = $2 AND slot IN ('serving','staged')
              ORDER BY slot FOR UPDATE",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(storage_err)?;

        let find = |name: &str| {
            locked
                .iter()
                .find(|r| r.get::<String, _>("slot") == name)
                .map(|r| {
                    (
                        r.get::<String, _>("artifact_id"),
                        r.get::<i64, _>("generation"),
                    )
                })
        };
        let Some((staged_artifact, staged_generation)) = find("staged") else {
            return Err(KernelError::InvalidInput(format!(
                "no staged binding for {index_version_id}; there is nothing to promote"
            )));
        };
        if staged_generation != expected_staged_generation {
            return Err(KernelError::InvalidInput(format!(
                "staged is at generation {staged_generation}, not {expected_staged_generation};                  re-read the binding and decide again"
            )));
        }
        let serving = find("serving");
        let serving_generation = serving.as_ref().map(|(_, g)| *g).unwrap_or(0);
        if serving_generation != expected_serving_generation {
            return Err(KernelError::InvalidInput(format!(
                "serving is at generation {serving_generation}, not                  {expected_serving_generation}; re-read the binding and decide again"
            )));
        }

        // Re-verify under the lock: retirement or failure arriving after the
        // caller's read must not be promoted past.
        let state: Option<String> = sqlx::query_scalar(
            "SELECT state FROM index_artifacts
              WHERE tenant_id = $1 AND index_version_id = $2 AND artifact_id = $3",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(&staged_artifact)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;
        if state.as_deref() != Some("verified") {
            return Err(KernelError::InvalidInput(format!(
                "staged artifact {staged_artifact} is {}, not verified; a promotion asserts servability",
                state.as_deref().unwrap_or("uncatalogued")
            )));
        }

        let new_generation = serving_generation + 1;
        sqlx::query(
            "INSERT INTO index_artifact_bindings
                 (tenant_id, index_version_id, slot, artifact_id, generation, selected_by, reason)
             VALUES ($1,$2,'serving',$3,$4,$5,$6)
             ON CONFLICT (tenant_id, index_version_id, slot) DO UPDATE SET
                 artifact_id = EXCLUDED.artifact_id,
                 generation = EXCLUDED.generation,
                 selected_by = EXCLUDED.selected_by,
                 reason = EXCLUDED.reason",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(&staged_artifact)
        .bind(new_generation)
        .bind(actor)
        .bind(reason)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;

        self.append_event(
            &mut tx,
            index_version_id,
            BindingSlot::Serving,
            serving.as_ref().map(|(a, _)| a.as_str()),
            Some(&staged_artifact),
            Some(serving_generation),
            Some(new_generation),
            "promote",
            actor,
            reason,
        )
        .await?;

        // Delete staged only at its locked generation — a concurrent rebind
        // between our lock release and... cannot happen inside the
        // transaction, but the guard is stated in SQL so a future refactor
        // that drops the lock still cannot delete someone else's staging.
        sqlx::query(
            "DELETE FROM index_artifact_bindings
              WHERE tenant_id = $1 AND index_version_id = $2 AND slot = 'staged'
                AND generation = $3",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .bind(staged_generation)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;

        tx.commit().await.map_err(storage_err)?;
        Ok(BindingRow {
            index_version_id: index_version_id.to_string(),
            slot: BindingSlot::Serving,
            artifact_id: staged_artifact,
            generation: new_generation,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn append_event(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        index_version_id: &str,
        slot: BindingSlot,
        from_artifact: Option<&str>,
        to_artifact: Option<&str>,
        from_generation: Option<i64>,
        to_generation: Option<i64>,
        operation: &str,
        actor: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO index_artifact_binding_events
                 (tenant_id, event_id, index_version_id, slot, from_artifact_id,
                  to_artifact_id, from_generation, to_generation, operation, actor, reason)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(&self.tenant_id)
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(index_version_id)
        .bind(slot.as_str())
        .bind(from_artifact)
        .bind(to_artifact)
        .bind(from_generation)
        .bind(to_generation)
        .bind(operation)
        .bind(actor)
        .bind(reason)
        .execute(&mut **tx)
        .await
        .map_err(storage_err)?;
        Ok(())
    }

    /// The append-only history for one version, newest first.
    pub async fn binding_events(&self, index_version_id: &str) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query(
            "SELECT operation, COALESCE(to_artifact_id, from_artifact_id, '') AS artifact
               FROM index_artifact_binding_events
              WHERE tenant_id = $1 AND index_version_id = $2
              ORDER BY occurred_at DESC, event_id",
        )
        .bind(&self.tenant_id)
        .bind(index_version_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get("operation"), r.get("artifact")))
            .collect())
    }
}
