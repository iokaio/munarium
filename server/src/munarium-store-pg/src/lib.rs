// SPDX-License-Identifier: Apache-2.0
//! PostgreSQL StorageBackend.
//!
//! Semantics contract: identical to `munarium-store-mem` — the conformance suite
//! runs the same scenarios against both, and reads resolve through the SAME
//! `munarium-core` reference functions (`ledger::resolve_slice`,
//! `promises::status_as_of`), so the two backends cannot drift apart.
//!
//! Write path: `append_claim` opens a transaction, takes `SELECT ... FOR
//! UPDATE` on the lineage root's `lineage_heads` row (the Postgres equivalent
//! of SQLite's BEGIN IMMEDIATE — serializes writers per lineage), computes
//! the chain head inside the lock, enforces `expected_head`, then inserts the
//! ledger event AND the claims-projection row in the same transaction.
//!
//! Tenancy: a `PgStore` handle is scoped to ONE tenant at construction (the
//! `TenantScopedStore` pattern) — every query carries the tenant predicate by
//! construction. Demo posture: single database, `tenant_id` column;
//! production posture (db-per-tenant per cell) swaps the pool router above
//! this type, not the queries.
//!
//! Hardening TODOs: push slice resolution down into SQL
//! (lineage CTE + pinned anti-join) once `cargo sqlx prepare` offline data is
//! wired; today reads fetch the lineage's rows and resolve in Rust, which is
//! correct at demo scale and provably agrees with the reference semantics.

use async_trait::async_trait;
use munarium_core::ledger::{resolve_slice, FactQuery};
use munarium_core::promises::status_as_of;
use munarium_core::storage::{NewClaim, StorageBackend};
use munarium_core::types::*;
use munarium_core::{KernelError, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::BTreeMap;

pub const DEFAULT_TENANT: &str = "tenant-default";

pub mod artifacts;
pub mod attempts;
pub mod budget;
pub mod evidence;
pub mod jobs;
pub mod max_tokens;
pub mod partitions;
pub mod rollout;
pub mod sources;
pub use artifacts::{ArtifactCatalog, ArtifactState, BindingSlot, InsertOutcome};
pub use attempts::{AttemptMode, AttemptState, BuildAttempts, ClaimOutcome};
pub use budget::PgBudgetStore;
pub use evidence::PgEvidenceStore;
pub use max_tokens::PgMaxTokensStore;
pub use rollout::{PlaneExpectations, RolloutSelector};
pub use sources::PgSourceStore;

pub(crate) fn storage_err(e: sqlx::Error) -> KernelError {
    KernelError::Storage(e.to_string())
}

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
    tenant_id: String,
}

impl PgStore {
    /// Connects with the default pool size (10 — matches the documented
    /// MUNARIUM_DB_MAX_CONNS default; tests and tools use this form). The
    /// server passes its configured size via `connect_with_pool_size`.
    pub async fn connect(database_url: &str, tenant_id: &str) -> Result<Self> {
        Self::connect_with_pool_size(database_url, tenant_id, 10).await
    }

    /// Connects, runs pending migrations, and returns a handle scoped to
    /// `tenant_id` (creating the tenant row if new). Pool sizing note: the
    /// append path holds a connection for `locked_head`'s FOR UPDATE
    /// transaction, so `max_conns` below 2 deadlocks writers against any
    /// concurrent work (config.rs enforces the floor for the server).
    pub async fn connect_with_pool_size(
        database_url: &str,
        tenant_id: &str,
        max_conns: u32,
    ) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_conns)
            .connect(database_url)
            .await
            .map_err(storage_err)?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
        sqlx::query("INSERT INTO tenants (id, slug) VALUES ($1, $1) ON CONFLICT (id) DO NOTHING")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .map_err(storage_err)?;
        Ok(Self {
            pool,
            tenant_id: tenant_id.to_string(),
        })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// A handle for another tenant over the SAME pool (the server derives
    /// per-request handles from one connected base store). Ensures the
    /// tenant row exists.
    pub async fn with_tenant(&self, tenant_id: &str) -> Result<Self> {
        // `ON CONFLICT DO NOTHING` on ANY constraint, not only the id:
        // `tenants.slug` is UNIQUE too, and a tenant whose id equals another
        // tenant's slug (the seed row is `('tenant-default', 'default')`)
        // conflicted on the slug, raised 23505, and turned every request for
        // that tenant into a 500.
        sqlx::query("INSERT INTO tenants (id, slug) VALUES ($1, $1) ON CONFLICT DO NOTHING")
            .bind(tenant_id)
            .execute(&self.pool)
            .await
            .map_err(storage_err)?;
        Ok(Self {
            pool: self.pool.clone(),
            tenant_id: tenant_id.to_string(),
        })
    }

    /// Version chain root -> leaf for `version_id` (recursive parent walk).
    async fn lineage_chain(&self, version_id: &str) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query(
            r#"
            WITH RECURSIVE chain AS (
                SELECT id, parent_id, lineage_root_id, 0 AS depth
                  FROM memory_versions
                 WHERE tenant_id = $1 AND id = $2
                UNION ALL
                SELECT v.id, v.parent_id, v.lineage_root_id, chain.depth + 1
                  FROM memory_versions v
                  JOIN chain ON v.id = chain.parent_id AND v.tenant_id = $1
            )
            SELECT id, lineage_root_id FROM chain ORDER BY depth DESC
            "#,
        )
        .bind(&self.tenant_id)
        .bind(version_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        if rows.is_empty() {
            return Err(KernelError::NotFound {
                kind: "version",
                id: version_id.to_string(),
            });
        }
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.get::<String, _>("id"),
                    r.get::<String, _>("lineage_root_id"),
                )
            })
            .collect())
    }

    async fn chain_claims(&self, chain: &[String]) -> Result<Vec<Claim>> {
        let rows = sqlx::query(
            "SELECT * FROM claims WHERE tenant_id = $1 AND version_id = ANY($2) ORDER BY seq",
        )
        .bind(&self.tenant_id)
        .bind(chain)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        rows.iter().map(row_to_claim).collect()
    }

    /// Chain head = MAX(seq) across every seq-stamped store, computed inside
    /// the given transaction (so it is stable under the lineage_heads lock).
    async fn chain_head_tx(
        tx: &mut Transaction<'_, Postgres>,
        tenant_id: &str,
        chain: &[String],
    ) -> Result<Seq> {
        let row = sqlx::query(
            r#"
            SELECT GREATEST(
                COALESCE((SELECT MAX(seq) FROM claims
                           WHERE tenant_id = $1 AND version_id = ANY($2)), 0),
                COALESCE((SELECT MAX(seq) FROM anchors
                           WHERE tenant_id = $1 AND version_id = ANY($2)), 0),
                COALESCE((SELECT MAX(GREATEST(seq, COALESCE(fulfilled_seq, 0))) FROM promises
                           WHERE tenant_id = $1 AND version_id = ANY($2)), 0),
                COALESCE((SELECT MAX(seq) FROM mesh_counters
                           WHERE tenant_id = $1 AND version_id = ANY($2)), 0)
            ) AS head
            "#,
        )
        .bind(tenant_id)
        .bind(chain)
        .fetch_one(&mut **tx)
        .await
        .map_err(storage_err)?;
        Ok(row.get::<i64, _>("head") as Seq)
    }

    /// Opens a transaction holding the lineage_heads FOR UPDATE lock and
    /// returns (tx, chain ids, lineage root, head). The chain and root are
    /// resolved BEFORE the transaction begins — nothing may acquire a second
    /// pool connection while the transaction holds one, or the pool deadlocks
    /// at max_connections concurrent writers.
    async fn locked_head(
        &self,
        version_id: &str,
    ) -> Result<(Transaction<'static, Postgres>, Vec<String>, String, Seq)> {
        let chain_pairs = self.lineage_chain(version_id).await?;
        let root = chain_pairs[0].1.clone();
        let chain: Vec<String> = chain_pairs.into_iter().map(|(id, _)| id).collect();

        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        sqlx::query(
            "SELECT current_seq FROM lineage_heads
              WHERE tenant_id = $1 AND lineage_root_id = $2 FOR UPDATE",
        )
        .bind(&self.tenant_id)
        .bind(&root)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_err)?;
        let head = Self::chain_head_tx(&mut tx, &self.tenant_id, &chain).await?;
        Ok((tx, chain, root, head))
    }
}

fn row_to_claim(row: &sqlx::postgres::PgRow) -> Result<Claim> {
    // Vocabulary drift fails CLOSED, as `severity_from_text` and
    // `row_to_artifact` already do: a value this code does not know is a
    // newer writer or a corrupt row, and reading it as the most permissive
    // member of the enum (`Fact`, `Accepted`, `Witnessed`) would quietly
    // promote a disputed or corrected claim.
    let unknown = |column: &str, value: &str| {
        KernelError::Storage(format!("unknown {column} '{value}' in the database"))
    };
    let claim_type = match row.get::<String, _>("claim_type").as_str() {
        "fact" => ClaimType::Fact,
        "update" => ClaimType::Update,
        "correction" => ClaimType::Correction,
        other => return Err(unknown("claim_type", other)),
    };
    let status = match row.get::<String, _>("status").as_str() {
        "accepted" => ClaimStatus::Accepted,
        "disputed" => ClaimStatus::Disputed,
        other => return Err(unknown("status", other)),
    };
    let provenance = match row.get::<String, _>("provenance").as_str() {
        "witnessed" => Provenance::Witnessed,
        "backfilled" => Provenance::Backfilled,
        "repaired" => Provenance::Repaired,
        "emergent" => Provenance::Emergent,
        "coverage_repair" => Provenance::CoverageRepair,
        other => return Err(unknown("provenance", other)),
    };
    Ok(Claim {
        id: row.get("id"),
        version_id: row.get("version_id"),
        seq: row.get::<i64, _>("seq") as Seq,
        claim_type,
        subject: row.get("subject"),
        key: row.get("key"),
        value: row.get("value"),
        scope_path: row.get("scope_path"),
        status,
        provenance,
        supersedes_id: row.get("supersedes_id"),
        entity_id: row.get("entity_id"),
        evidence: row.get("evidence"),
        confidence: row.get("confidence"),
        shape_ref: row.get("shape_ref"),
        // A JSONB that does not decode is a corrupt row, not an absent
        // origin; surfacing it as None would silently lose provenance.
        origin: match row.get::<Option<serde_json::Value>, _>("origin") {
            None => None,
            Some(v) => Some(
                serde_json::from_value(v)
                    .map_err(|e| storage_err(sqlx::Error::Decode(Box::new(e))))?,
            ),
        },
    })
}

fn claim_type_str(t: ClaimType) -> &'static str {
    match t {
        ClaimType::Fact => "fact",
        ClaimType::Update => "update",
        ClaimType::Correction => "correction",
    }
}

fn status_str(s: ClaimStatus) -> &'static str {
    match s {
        ClaimStatus::Accepted => "accepted",
        ClaimStatus::Disputed => "disputed",
    }
}

fn provenance_str(p: Provenance) -> &'static str {
    match p {
        Provenance::Witnessed => "witnessed",
        Provenance::Backfilled => "backfilled",
        Provenance::Repaired => "repaired",
        Provenance::Emergent => "emergent",
        Provenance::CoverageRepair => "coverage_repair",
    }
}

#[async_trait]
impl StorageBackend for PgStore {
    async fn create_version(
        &self,
        parent_id: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<String> {
        let id = format!("memv-{}", uuid::Uuid::new_v4().simple());
        let root = match parent_id {
            Some(p) => {
                let chain = self.lineage_chain(p).await?; // errors if parent missing
                chain[0].1.clone()
            }
            None => id.clone(),
        };
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        sqlx::query(
            "INSERT INTO memory_versions (tenant_id, id, parent_id, lineage_root_id, metadata)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&self.tenant_id)
        .bind(&id)
        .bind(parent_id)
        .bind(&root)
        .bind(metadata)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        sqlx::query(
            "INSERT INTO lineage_heads (tenant_id, lineage_root_id) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(&self.tenant_id)
        .bind(&root)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        Ok(id)
    }

    async fn lineage(&self, version_id: &str) -> Result<Vec<String>> {
        Ok(self
            .lineage_chain(version_id)
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .collect())
    }

    async fn head(&self, version_id: &str) -> Result<Seq> {
        let chain = self.lineage(version_id).await?;
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        let head = Self::chain_head_tx(&mut tx, &self.tenant_id, &chain).await?;
        tx.commit().await.map_err(storage_err)?;
        Ok(head)
    }

    async fn append_claim(
        &self,
        version_id: &str,
        claim: NewClaim,
        expected_head: Option<Seq>,
    ) -> Result<Claim> {
        let mut stored = self
            .append_claims(version_id, vec![claim], expected_head)
            .await?;
        stored
            .pop()
            .ok_or_else(|| KernelError::Storage("append_claims returned no claim for one".into()))
    }

    async fn append_claims(
        &self,
        version_id: &str,
        claims: Vec<NewClaim>,
        expected_head: Option<Seq>,
    ) -> Result<Vec<Claim>> {
        if claims.is_empty() {
            return Ok(Vec::new());
        }
        // ONE transaction under the lineage lock spans the whole batch:
        // every claim lands or none does, seqs consecutive from head + 1.
        let (mut tx, chain, root, head) = self.locked_head(version_id).await?;
        if let Some(expected) = expected_head {
            if expected != head {
                return Err(KernelError::HeadConflict {
                    expected,
                    actual: head,
                });
            }
        }
        // Validate every supersedes_id up front — a mid-batch failure must
        // reject the batch before any row is written.
        for claim in &claims {
            if let Some(sup) = &claim.supersedes_id {
                let exists = sqlx::query(
                    "SELECT 1 AS one FROM claims
                      WHERE tenant_id = $1 AND id = $2 AND version_id = ANY($3)",
                )
                .bind(&self.tenant_id)
                .bind(sup)
                .bind(&chain)
                .fetch_optional(&mut *tx)
                .await
                .map_err(storage_err)?;
                if exists.is_none() {
                    return Err(KernelError::NotFound {
                        kind: "claim",
                        id: sup.clone(),
                    });
                }
            }
        }

        let mut out = Vec::with_capacity(claims.len());
        let mut seq = head as i64;
        for claim in claims {
            seq += 1;
            let id = format!("claim-{}", uuid::Uuid::new_v4().simple());
            let normalized =
                munarium_core::ledger::normalize_claim(&claim.subject, &claim.key, &claim.value);

            // event + projection in ONE transaction; the projection is regenerable
            let origin_json: Option<serde_json::Value> = claim
                .origin
                .as_ref()
                .map(|o| serde_json::to_value(o).expect("ClaimOrigin serializes"));
            sqlx::query(
                "INSERT INTO ledger_events (tenant_id, version_id, seq, event_type, body)
                 VALUES ($1, $2, $3, 'claim.appended', $4)",
            )
            .bind(&self.tenant_id)
            .bind(version_id)
            .bind(seq)
            .bind(serde_json::json!({
                "claim_id": id,
                "claim_type": claim_type_str(claim.claim_type),
                "normalized": normalized,
                "status": status_str(claim.status),
                "supersedes_id": claim.supersedes_id,
                "origin": origin_json,
            }))
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;

            sqlx::query(
                "INSERT INTO claims (tenant_id, id, version_id, seq, claim_type, subject, key, value,
                                     scope_path, status, provenance, supersedes_id, entity_id,
                                     evidence, confidence, shape_ref, origin)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
            )
            .bind(&self.tenant_id)
            .bind(&id)
            .bind(version_id)
            .bind(seq)
            .bind(claim_type_str(claim.claim_type))
            .bind(&claim.subject)
            .bind(&claim.key)
            .bind(&claim.value)
            .bind(&claim.scope_path)
            .bind(status_str(claim.status))
            .bind(provenance_str(claim.provenance))
            .bind(&claim.supersedes_id)
            .bind(&claim.entity_id)
            .bind(&claim.evidence)
            .bind(claim.confidence)
            .bind(&claim.shape_ref)
            .bind(origin_json.as_ref())
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;

            out.push(Claim {
                id,
                version_id: version_id.to_string(),
                seq: seq as Seq,
                claim_type: claim.claim_type,
                subject: claim.subject,
                key: claim.key,
                value: claim.value,
                scope_path: claim.scope_path,
                status: claim.status,
                provenance: claim.provenance,
                supersedes_id: claim.supersedes_id,
                entity_id: claim.entity_id,
                evidence: claim.evidence,
                confidence: claim.confidence,
                shape_ref: claim.shape_ref,
                origin: claim.origin,
            });
        }

        sqlx::query(
            "UPDATE lineage_heads SET current_seq = GREATEST(current_seq, $3)
              WHERE tenant_id = $1 AND lineage_root_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(&root)
        .bind(seq)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;

        Ok(out)
    }

    async fn slice_facts(&self, version_id: &str, q: &FactQuery) -> Result<Vec<Claim>> {
        let chain = self.lineage(version_id).await?;
        let claims = self.chain_claims(&chain).await?;
        Ok(resolve_slice(claims, q))
    }

    async fn get_claim(&self, claim_id: &str) -> Result<Option<Claim>> {
        let row = sqlx::query("SELECT * FROM claims WHERE tenant_id = $1 AND id = $2")
            .bind(&self.tenant_id)
            .bind(claim_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_err)?;
        row.as_ref().map(row_to_claim).transpose()
    }

    async fn superseded_by(&self, claim_id: &str) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT id FROM claims WHERE tenant_id = $1 AND supersedes_id = $2
              ORDER BY seq LIMIT 1",
        )
        .bind(&self.tenant_id)
        .bind(claim_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(row.map(|r| r.get("id")))
    }

    async fn lock_anchor(
        &self,
        version_id: &str,
        subject: &str,
        key: &str,
        value: &str,
        scope_path: Option<&str>,
        evidence: Option<serde_json::Value>,
    ) -> Result<Anchor> {
        let (mut tx, _chain, _root, head) = self.locked_head(version_id).await?;
        let seq = (head + 1) as i64;
        let id = format!("anchor-{}", uuid::Uuid::new_v4().simple());
        let detail_key = format!("{subject}.{key}");
        sqlx::query(
            "INSERT INTO anchors (tenant_id, id, version_id, detail_key, locked_value,
                                  locked_at_scope, status, seq, evidence)
             VALUES ($1, $2, $3, $4, $5, $6, 'locked', $7, $8)",
        )
        .bind(&self.tenant_id)
        .bind(&id)
        .bind(version_id)
        .bind(&detail_key)
        .bind(value)
        .bind(scope_path)
        .bind(seq)
        .bind(&evidence)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        Ok(Anchor {
            id,
            version_id: version_id.to_string(),
            detail_key,
            locked_value: value.to_string(),
            locked_at_scope: scope_path.map(String::from),
            status: AnchorStatus::Locked,
            seq: seq as Seq,
            evidence,
        })
    }

    async fn anchors(
        &self,
        version_id: &str,
        as_of_seq: Option<Seq>,
    ) -> Result<BTreeMap<String, Anchor>> {
        let chain = self.lineage(version_id).await?;
        let mut out = BTreeMap::new();
        // One query over the whole chain (`promises`, `digests` and
        // `counter_totals` already read this way), ordered root -> leaf by
        // the chain's own position and then by seq, so the later version
        // still wins by overwriting — the same order the per-version loop
        // produced, without one round trip per version in the lineage.
        let rows = sqlx::query(
            "SELECT * FROM anchors
              WHERE tenant_id = $1 AND version_id = ANY($2) AND status = 'locked'
                AND ($3::bigint IS NULL OR seq <= $3)
              ORDER BY array_position($2, version_id), seq",
        )
        .bind(&self.tenant_id)
        .bind(&chain)
        .bind(as_of_seq.map(|s| s as i64))
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        for r in rows {
            let a = Anchor {
                id: r.get("id"),
                version_id: r.get("version_id"),
                detail_key: r.get("detail_key"),
                locked_value: r.get("locked_value"),
                locked_at_scope: r.get("locked_at_scope"),
                status: AnchorStatus::Locked,
                seq: r.get::<i64, _>("seq") as Seq,
                evidence: r.get("evidence"),
            };
            out.insert(a.detail_key.clone(), a);
        }
        Ok(out)
    }

    async fn register_promise(
        &self,
        version_id: &str,
        key: &str,
        kind: &str,
        description: &str,
        origin_scope: Option<&str>,
        due_scope: Option<&str>,
    ) -> Result<Promise> {
        let (mut tx, _chain, _root, head) = self.locked_head(version_id).await?;
        // Registration advances the ledger clock like every other store
        // (head + 1) so consecutive registrations stay orderable under a pin.
        let seq = head + 1;
        let id = format!("prom-{}", uuid::Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO promises (tenant_id, id, version_id, key, kind, description,
                                   origin_scope, due_scope, status, seq)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'open', $9)",
        )
        .bind(&self.tenant_id)
        .bind(&id)
        .bind(version_id)
        .bind(key)
        .bind(kind)
        .bind(description)
        .bind(origin_scope)
        .bind(due_scope)
        .bind(seq as i64)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        Ok(Promise {
            id,
            version_id: version_id.to_string(),
            key: key.to_string(),
            kind: kind.to_string(),
            description: description.to_string(),
            origin_scope: origin_scope.map(String::from),
            due_scope: due_scope.map(String::from),
            status: PromiseStatus::Open,
            seq,
            fulfilled_seq: None,
        })
    }

    async fn promises(&self, version_id: &str, as_of_seq: Option<Seq>) -> Result<Vec<Promise>> {
        let chain = self.lineage(version_id).await?;
        let rows = sqlx::query(
            "SELECT * FROM promises WHERE tenant_id = $1 AND version_id = ANY($2)
              ORDER BY seq, key",
        )
        .bind(&self.tenant_id)
        .bind(&chain)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        let mut out = Vec::new();
        for r in rows {
            let seq = r.get::<i64, _>("seq") as Seq;
            if let Some(pin) = as_of_seq {
                if seq > pin {
                    continue; // post-pin registration hidden
                }
            }
            let status = match r.get::<String, _>("status").as_str() {
                "fulfilled" => PromiseStatus::Fulfilled,
                "expired" => PromiseStatus::Expired,
                "violated" => PromiseStatus::Violated,
                _ => PromiseStatus::Open,
            };
            let mut p = Promise {
                id: r.get("id"),
                version_id: r.get("version_id"),
                key: r.get("key"),
                kind: r.get("kind"),
                description: r.get("description"),
                origin_scope: r.get("origin_scope"),
                due_scope: r.get("due_scope"),
                status,
                seq,
                fulfilled_seq: r.get::<Option<i64>, _>("fulfilled_seq").map(|s| s as Seq),
            };
            p.status = status_as_of(&p, as_of_seq); // post-pin fulfillment reads open
            if p.status == PromiseStatus::Open {
                p.fulfilled_seq = None;
            }
            out.push(p);
        }
        Ok(out)
    }

    async fn fulfill_promise(&self, version_id: &str, key: &str) -> Result<bool> {
        let (mut tx, chain, _root, head) = self.locked_head(version_id).await?;
        let updated = sqlx::query(
            "UPDATE promises SET status = 'fulfilled', fulfilled_seq = $4
              WHERE tenant_id = $1 AND key = $2 AND status = 'open' AND version_id = ANY($3)
                AND id = (SELECT id FROM promises
                           WHERE tenant_id = $1 AND key = $2 AND status = 'open'
                             AND version_id = ANY($3)
                           ORDER BY seq LIMIT 1)",
        )
        .bind(&self.tenant_id)
        .bind(key)
        .bind(&chain)
        // Fulfillment is a ledger event at head + 1: a pin taken at the
        // current head must still read the promise as open.
        .bind((head + 1) as i64)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        Ok(updated.rows_affected() > 0)
    }

    async fn record_counts(
        &self,
        version_id: &str,
        key: &str,
        scope_path: &str,
        count: u64,
        budget: Option<u64>,
    ) -> Result<()> {
        let (mut tx, _chain, _root, head) = self.locked_head(version_id).await?;
        // The upsert re-stamps seq: an updated count is a new observation at
        // the current head. Keeping the original stamp leaked future values
        // into pinned reads (a pin predating the update saw the new count).
        sqlx::query(
            "INSERT INTO mesh_counters (tenant_id, version_id, key, scope_path, count, budget, seq)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (tenant_id, version_id, key, scope_path)
             DO UPDATE SET count = EXCLUDED.count,
                           budget = COALESCE(EXCLUDED.budget, mesh_counters.budget),
                           seq = EXCLUDED.seq",
        )
        .bind(&self.tenant_id)
        .bind(version_id)
        .bind(key)
        .bind(scope_path)
        .bind(count as i64)
        .bind(budget.map(|b| b as i64))
        .bind((head + 1) as i64)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;
        Ok(())
    }

    async fn counter_totals(
        &self,
        version_id: &str,
        as_of_seq: Option<Seq>,
    ) -> Result<Vec<CounterTotal>> {
        let chain = self.lineage(version_id).await?;
        let rows = sqlx::query(
            "SELECT key, SUM(count)::BIGINT AS total, MAX(budget) AS budget
               FROM mesh_counters
              WHERE tenant_id = $1 AND version_id = ANY($2)
                AND ($3::bigint IS NULL OR seq <= $3)
              GROUP BY key ORDER BY key",
        )
        .bind(&self.tenant_id)
        .bind(&chain)
        .bind(as_of_seq.map(|s| s as i64))
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows
            .into_iter()
            .map(|r| CounterTotal {
                key: r.get("key"),
                total: r.get::<i64, _>("total") as u64,
                budget: r.get::<Option<i64>, _>("budget").map(|b| b as u64),
            })
            .collect())
    }

    async fn upsert_digest(&self, digest: &Digest) -> Result<()> {
        sqlx::query(
            "INSERT INTO digests (tenant_id, version_id, tier, scope_path, content,
                                  content_hash, built_from_seq)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (tenant_id, version_id, tier, scope_path)
             DO UPDATE SET content = EXCLUDED.content,
                           content_hash = EXCLUDED.content_hash,
                           built_from_seq = EXCLUDED.built_from_seq",
        )
        .bind(&self.tenant_id)
        .bind(&digest.version_id)
        .bind(digest.tier as i16)
        .bind(&digest.scope_path)
        .bind(&digest.content)
        .bind(&digest.content_hash)
        .bind(digest.built_from_seq as i64)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(())
    }

    async fn digests(&self, version_id: &str) -> Result<Vec<Digest>> {
        let chain = self.lineage(version_id).await?;
        let rows = sqlx::query(
            "SELECT * FROM digests WHERE tenant_id = $1 AND version_id = ANY($2)
              ORDER BY version_id, tier, scope_path",
        )
        .bind(&self.tenant_id)
        .bind(&chain)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows
            .into_iter()
            .map(|r| Digest {
                version_id: r.get("version_id"),
                tier: r.get::<i16, _>("tier") as u8,
                scope_path: r.get("scope_path"),
                content: r.get("content"),
                content_hash: r.get("content_hash"),
                built_from_seq: r.get::<i64, _>("built_from_seq") as Seq,
            })
            .collect())
    }

    async fn version_metadata(&self, version_id: &str) -> Result<Option<serde_json::Value>> {
        let row: Option<(Option<serde_json::Value>,)> =
            sqlx::query_as("SELECT metadata FROM memory_versions WHERE tenant_id = $1 AND id = $2")
                .bind(&self.tenant_id)
                .bind(version_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage_err)?;
        match row {
            Some((meta,)) => Ok(meta),
            None => Err(KernelError::NotFound {
                kind: "version",
                id: version_id.to_string(),
            }),
        }
    }

    async fn record_findings(
        &self,
        version_id: &str,
        seq: Seq,
        findings: &[munarium_core::types::GateFinding],
    ) -> Result<()> {
        // One transaction, and the version checked first — the memory store
        // does both. Without the transaction a failure at finding k left
        // 1..k-1 persisted behind an error, and the caller's retry double-
        // filed them (`gate_findings` has no uniqueness). Without the check,
        // findings against an unknown version were accepted and then
        // unreachable, since `findings()` walks the lineage that does not
        // exist.
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        let exists: Option<(String,)> =
            sqlx::query_as("SELECT id FROM memory_versions WHERE tenant_id = $1 AND id = $2")
                .bind(&self.tenant_id)
                .bind(version_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(storage_err)?;
        if exists.is_none() {
            return Err(KernelError::NotFound {
                kind: "version",
                id: version_id.to_string(),
            });
        }
        for f in findings {
            sqlx::query(
                "INSERT INTO gate_findings
                   (tenant_id, version_id, seq, rule_id, severity, message, scope_path, detail)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(&self.tenant_id)
            .bind(version_id)
            .bind(seq as i64)
            .bind(&f.rule_id)
            .bind(severity_text(f.severity))
            .bind(&f.message)
            .bind(&f.scope_path)
            .bind(&f.detail)
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;
        }
        tx.commit().await.map_err(storage_err)
    }

    async fn findings(
        &self,
        version_id: &str,
        q: &munarium_core::storage::FindingsQuery,
    ) -> Result<Vec<munarium_core::storage::StoredFinding>> {
        let chain = self.lineage(version_id).await?;
        let rows = sqlx::query(
            "SELECT seq, rule_id, severity, message, scope_path, detail
               FROM gate_findings
              WHERE tenant_id = $1 AND version_id = ANY($2)
                AND ($3::bigint IS NULL OR seq <= $3)
                AND ($4::text IS NULL OR severity = $4)
                AND ($5::text IS NULL OR rule_id = $5)
                AND ($6::text IS NULL OR rule_id LIKE $6 || '%')
              ORDER BY seq, rule_id
              LIMIT $7",
        )
        .bind(&self.tenant_id)
        .bind(&chain)
        .bind(q.as_of_seq.map(|s| s as i64))
        .bind(q.severity.map(severity_text))
        .bind(&q.rule_id)
        .bind(q.rule_prefix.as_deref().map(escape_like))
        .bind(q.limit.map(|l| l as i64).unwrap_or(1000))
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(rows
            .into_iter()
            .map(|r| munarium_core::storage::StoredFinding {
                seq: r.get::<i64, _>("seq") as Seq,
                finding: munarium_core::types::GateFinding {
                    rule_id: r.get("rule_id"),
                    severity: severity_from_text(&r.get::<String, _>("severity")),
                    message: r.get("message"),
                    scope_path: r.get("scope_path"),
                    detail: r.get("detail"),
                },
            })
            .collect())
    }
}

/// A prefix for `LIKE` with its metacharacters escaped, so a rule id
/// containing `_` matches itself and not any single character.
fn escape_like(prefix: &str) -> String {
    prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn severity_text(s: munarium_core::types::Severity) -> &'static str {
    match s {
        munarium_core::types::Severity::Info => "info",
        munarium_core::types::Severity::Warn => "warn",
        munarium_core::types::Severity::Block => "block",
    }
}

/// Fail closed on vocabulary drift: an unknown stored severity reads as
/// `block` (the strictest), never silently as info.
fn severity_from_text(s: &str) -> munarium_core::types::Severity {
    match s {
        "info" => munarium_core::types::Severity::Info,
        "warn" => munarium_core::types::Severity::Warn,
        _ => munarium_core::types::Severity::Block,
    }
}

// The migration-embedding macro below this crate (sqlx::migrate! in lib.rs)
// tracks SOURCE files, not the migrations directory: adding a migration
// without touching a .rs file leaves a cached build serving the OLD migrator
// — locally (CLAUDE.md prescribes `cargo clean -p munarium-store-pg`) AND in
// the Docker image build, whose cargo cache mount replayed a pre-0025 crate
// on 2026-08-30 and shipped a server that could not see index_number_lexemes.
// This trailing comment is that incident: touch this file when adding a
// migration, so both builds recompile the migrator.
// 2026-09-02: 0029_index_version_deactivation (the code-review branch's
// number) was renamed to 0030 at merge because main already carried
// 0029_token_budgets; this line is that rename's touch.
// 2026-09-02, later the same day: 0031_max_tokens_budgets added (the per-call
// output-token budgets replaceable through /v1/max-tokens); this line is its
// touch.
