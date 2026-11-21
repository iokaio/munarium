// SPDX-License-Identifier: Apache-2.0
//! MCP generation admission (ClearView A2, A8, A9; proposed ADR 0026).
//!
//! **Opt-in.** An MCP tool call can reach a model only when the operator has
//! enabled paid tools (`MUNARIUM_MCP_PAID_TOOLS=enabled`) AND the trusted
//! policy file (`MUNARIUM_MCP_GENERATION_POLICY`) names, for every
//! contributing collection, a budget owner plus both token allowances. Tool
//! arguments cannot select the policy or the payer.
//!
//! **Where it binds.** Every physical completion the native handlers make
//! passes through `providers_api::complete_guarded`. An MCP dispatch runs its
//! inner request inside [`scope`], a task-local carrying this invocation's
//! resolved policy; `complete_guarded` calls [`admit_attempt`] before EVERY
//! physical submission (expansion, verification, answer, re-ask and each
//! retry) and [`settle`] after it. Native traffic carries no task-local and
//! is untouched. The generation stages all run on the request's own task;
//! retrieval fan-out tasks never generate.
//!
//! **Accounting.** Each attempt is reserved in ONE PostgreSQL transaction
//! against (a) the invocation's request allowance — the largest effective
//! per-request allowance among the contributors (A9), shared across stages
//! and retries — and (b) every applicable daily allowance: each contributing
//! collection's, plus each contributing index's own when it overrides. The
//! UTC day is the database's. The reservation estimates input tokens from
//! serialized bytes and the model's bytes-per-token ratio, then adds fixed
//! framing and the permitted output. Refusal leaves no partial
//! reservation. Settlement uses provider-reported input and output counts
//! once; uncertain usage keeps the full reservation. Recorded usage above
//! the estimate is charged in full and reduces the remaining allowances.

use munarium_core::{KernelError, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::sync::Arc;

/// Sentinel prefixes promoted to problem types at the transport boundary
/// (error.rs), like `DAILY_CAP_PREFIX`.
pub const NOT_AUTHORIZED_PREFIX: &str = "mcp-generation: ";
pub const ALLOWANCE_PREFIX: &str = "mcp-allowance: ";
pub const ACCOUNTING_PREFIX: &str = "mcp-accounting: ";

/// Framing slack added to the byte-length bound (role markers, templates).
pub const FRAMING_TOKENS: u64 = 64;

pub(crate) fn not_authorized(why: &str) -> KernelError {
    KernelError::Forbidden(format!("{NOT_AUTHORIZED_PREFIX}{why}"))
}

fn exhausted(why: &str) -> KernelError {
    KernelError::RateLimited(format!("{ALLOWANCE_PREFIX}{why}"))
}

pub(crate) fn accounting(_e: impl std::fmt::Display) -> KernelError {
    // Driver text is never shown; the slug says what the caller needs.
    KernelError::Storage(format!(
        "{ACCOUNTING_PREFIX}generation accounting is unavailable; no model call was made"
    ))
}

// ---------------------------------------------------------------------------
// policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    version: u32,
    #[serde(default)]
    collections: Vec<CollectionEntry>,
    #[serde(default)]
    model_bytes_per_token: BTreeMap<String, NonZeroU64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionEntry {
    tenant: String,
    /// Collection id or name.
    collection: String,
    #[serde(default)]
    budget_owner: Option<String>,
    #[serde(default)]
    request_tokens: Option<u64>,
    #[serde(default)]
    daily_tokens: Option<u64>,
    #[serde(default)]
    indexes: Vec<IndexEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexEntry {
    index_version: String,
    #[serde(default)]
    request_tokens: Option<u64>,
    #[serde(default)]
    daily_tokens: Option<u64>,
}

/// One collection's policy as loaded. Incomplete entries load (and refuse
/// generation, A2); an incomplete index override is a validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionPolicy {
    pub budget_owner: Option<String>,
    pub request_tokens: Option<u64>,
    pub daily_tokens: Option<u64>,
    /// index_version -> (request, daily).
    pub indexes: BTreeMap<String, (u64, u64)>,
}

/// The trusted generation policy: (tenant, collection id-or-name) -> policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GenerationPolicy {
    entries: BTreeMap<(String, String), CollectionPolicy>,
    model_bytes_per_token: BTreeMap<String, NonZeroU64>,
    digest: Option<String>,
}

impl GenerationPolicy {
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let file: PolicyFile =
            serde_json::from_str(text).map_err(|e| format!("not a valid policy document: {e}"))?;
        if file.version != 1 {
            return Err(format!("unsupported policy version {}", file.version));
        }
        if file
            .model_bytes_per_token
            .keys()
            .any(|model| model.trim().is_empty())
        {
            return Err("each bytes-per-token override needs a model name".into());
        }
        let mut entries = BTreeMap::new();
        for c in file.collections {
            if c.tenant.trim().is_empty() || c.collection.trim().is_empty() {
                return Err("every entry names a tenant and a collection".into());
            }
            let mut indexes = BTreeMap::new();
            for i in c.indexes {
                match (i.request_tokens, i.daily_tokens) {
                    (Some(r), Some(d)) => {
                        if indexes.insert(i.index_version.clone(), (r, d)).is_some() {
                            return Err(format!(
                                "collection '{}' overrides index '{}' twice",
                                c.collection, i.index_version
                            ));
                        }
                    }
                    _ => {
                        return Err(format!(
                            "index '{}' of collection '{}' overrides only one allowance; an index \
                             override supplies both request_tokens and daily_tokens, or neither \
                             (it then inherits the collection's pair)",
                            i.index_version, c.collection
                        ))
                    }
                }
            }
            let key = (c.tenant.clone(), c.collection.clone());
            let policy = CollectionPolicy {
                budget_owner: c.budget_owner.filter(|o| !o.trim().is_empty()),
                request_tokens: c.request_tokens,
                daily_tokens: c.daily_tokens,
                indexes,
            };
            if entries.insert(key, policy).is_some() {
                return Err(format!(
                    "tenant '{}' collection '{}' is listed twice",
                    c.tenant, c.collection
                ));
            }
        }
        Ok(Self {
            entries,
            model_bytes_per_token: file.model_bytes_per_token,
            digest: Some(format!("{:x}", Sha256::digest(text.as_bytes()))),
        })
    }

    pub fn digest(&self) -> Option<&str> {
        self.digest.as_deref()
    }

    pub fn bytes_per_token(&self, model: &str) -> u64 {
        self.model_bytes_per_token
            .get(model)
            .map_or(3, |ratio| ratio.get())
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The entry for a collection, matched by id first, then by name.
    pub fn lookup(&self, tenant: &str, id: &str, name: &str) -> Option<&CollectionPolicy> {
        self.entries
            .get(&(tenant.to_string(), id.to_string()))
            .or_else(|| self.entries.get(&(tenant.to_string(), name.to_string())))
    }
}

/// One contributor as the MCP layer identifies it before dispatch: an
/// authorized collection the call draws from, with the index it is pinned
/// to when one is active.
#[derive(Debug, Clone)]
pub struct Contributor {
    pub collection_id: String,
    pub collection_name: String,
    pub index_version: Option<String>,
}

/// The resolved, shared policy for one invocation (A9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedPolicy {
    pub payer: String,
    /// The largest effective per-request allowance among the contributors.
    pub request_ceiling: u64,
    /// Every applicable daily scope, deduplicated: (kind, id) -> allowance.
    pub daily: BTreeMap<(String, String), u64>,
}

/// Resolve the contributors' policies into one shared policy, or the reason
/// generation is not authorized. Pure: the tests drive it directly.
pub fn resolve(
    policy: &GenerationPolicy,
    tenant: &str,
    contributors: &[Contributor],
) -> std::result::Result<SharedPolicy, String> {
    if contributors.is_empty() {
        return Err("no authorized contributing collection was identified for this call".into());
    }
    let mut payer: Option<String> = None;
    let mut ceiling = 0u64;
    let mut daily = BTreeMap::new();
    for c in contributors {
        let Some(p) = policy.lookup(tenant, &c.collection_id, &c.collection_name) else {
            return Err(format!(
                "collection '{}' has no MCP generation policy (A2: an operator enables generation, \
                 names a budget owner and sets both allowances)",
                c.collection_name
            ));
        };
        let (Some(owner), Some(request), Some(day)) =
            (p.budget_owner.as_ref(), p.request_tokens, p.daily_tokens)
        else {
            return Err(format!(
                "collection '{}' does not name a budget owner and both token allowances",
                c.collection_name
            ));
        };
        match &payer {
            None => payer = Some(owner.clone()),
            Some(existing) if existing == owner => {}
            Some(_) => {
                return Err(
                    "the contributing collections do not authorize a common budget owner".into(),
                )
            }
        }
        daily.insert(("collection".to_string(), c.collection_id.clone()), day);
        let effective_request = match c
            .index_version
            .as_ref()
            .and_then(|iv| p.indexes.get(iv).map(|v| (iv, v)))
        {
            Some((iv, (r, d))) => {
                daily.insert(("index".to_string(), iv.clone()), *d);
                *r
            }
            None => request,
        };
        ceiling = ceiling.max(effective_request);
    }
    Ok(SharedPolicy {
        payer: payer.unwrap_or_default(),
        request_ceiling: ceiling,
        daily,
    })
}

// ---------------------------------------------------------------------------
// the per-invocation context
// ---------------------------------------------------------------------------

/// What an MCP dispatch carries into the native handlers.
#[derive(Debug)]
pub struct CallContext {
    pub tenant: String,
    /// The original execution's invocation id: attempts and spend attach here.
    pub invocation_id: String,
    /// `MUNARIUM_MCP_PAID_TOOLS` for this call's form.
    pub paid_enabled: bool,
    /// The resolved policy, or why generation is not authorized.
    pub policy: std::result::Result<SharedPolicy, String>,
    /// Physical attempts admitted under this call (for tests and receipts).
    pub admitted: std::sync::atomic::AtomicU64,
    /// A failed settlement can leave the actual charge higher than the estimate.
    pub settlement_failed: std::sync::atomic::AtomicBool,
}

tokio::task_local! {
    static CALL: Arc<CallContext>;
}

/// Run `fut` (an inner native dispatch) under `ctx`.
pub async fn scope<F: std::future::Future>(ctx: Arc<CallContext>, fut: F) -> F::Output {
    CALL.scope(ctx, fut).await
}

/// The context of the MCP call this task serves, if any.
pub fn current() -> Option<Arc<CallContext>> {
    CALL.try_with(|c| c.clone()).ok()
}

/// A reserved physical attempt.
#[derive(Debug)]
pub struct Attempt {
    pub attempt_id: String,
}

/// The reservation estimate for one completion request: serialized bytes
/// divided by the configured ratio and rounded up, framing, and the permitted output (which
/// includes reasoning tokens on every adapter that bounds them by
/// `max_tokens`).
pub fn bound_for(
    policy: &GenerationPolicy,
    request: &munarium_core::provider::CompletionRequest,
    schema: Option<&serde_json::Value>,
) -> Result<u64> {
    let bytes = serde_json::to_vec(&(request, schema))
        .map_err(|_| KernelError::InvalidInput("cannot size the completion request".into()))?;
    (bytes.len() as u64)
        .div_ceil(policy.bytes_per_token(&request.model))
        .checked_add(FRAMING_TOKENS)
        .and_then(|n| n.checked_add(u64::from(request.max_tokens.max(1))))
        .ok_or_else(|| KernelError::InvalidInput("completion bound overflow".into()))
}

fn lock_key(tenant: &str, what: &str) -> String {
    format!("mcp-alloc/{tenant}\u{1f}{what}")
}

/// Reserve one physical attempt (A8/A9) for an MCP call. Every refusal
/// happens before the HTTP submission and leaves nothing reserved. Called by
/// the provider attempt observer (`money_api`) at the actual retry boundary,
/// so adapter-level retries are separate attempts too.
pub async fn admit_attempt(
    pool: Option<&sqlx::PgPool>,
    ctx: &CallContext,
    provider_config: &str,
    model: &str,
    bound: u64,
) -> Result<Attempt> {
    if !ctx.paid_enabled {
        return Err(not_authorized(
            "this MCP call may not generate: MUNARIUM_MCP_PAID_TOOLS is disabled, or the tool \
             is not a generating tool",
        ));
    }
    let policy = ctx.policy.as_ref().map_err(|why| not_authorized(why))?;
    if ctx
        .settlement_failed
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err(accounting("an earlier attempt's settlement failed"));
    }
    let Some(pool) = pool else {
        return Err(accounting("no durable store"));
    };
    let attempt_id = format!("mga-{}", uuid::Uuid::now_v7().simple());
    let mut tx = pool.begin().await.map_err(accounting)?;
    // Lock order: the invocation, then every daily scope in sorted order, so
    // two replicas reserving overlapping scopes cannot deadlock.
    let mut locks: BTreeSet<String> = BTreeSet::new();
    locks.insert(lock_key(&ctx.tenant, &format!("inv/{}", ctx.invocation_id)));
    let mut scope_locks: Vec<String> = policy
        .daily
        .keys()
        .map(|(k, id)| lock_key(&ctx.tenant, &format!("{k}/{id}")))
        .collect();
    scope_locks.sort();
    for key in locks.iter().chain(scope_locks.iter()) {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(key)
            .execute(&mut *tx)
            .await
            .map_err(accounting)?;
    }
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(COALESCE(settled_units, bound_units)), 0)::bigint
           FROM mcp_token_attempts WHERE tenant_id = $1 AND invocation_id = $2",
    )
    .bind(&ctx.tenant)
    .bind(&ctx.invocation_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(accounting)?;
    let used = used.max(0) as u64;
    if used.saturating_add(bound) > policy.request_ceiling {
        return Err(exhausted(&format!(
            "this request has {} of {} tokens left and the next model call reserves {bound}",
            policy.request_ceiling.saturating_sub(used),
            policy.request_ceiling
        )));
    }
    for ((kind, id), allowance) in &policy.daily {
        let day_used: i64 = sqlx::query_scalar(
            "SELECT COALESCE(sum(COALESCE(a.settled_units, a.bound_units)), 0)::bigint
               FROM mcp_token_attempt_scopes s
               JOIN mcp_token_attempts a ON a.attempt_id = s.attempt_id
              WHERE s.tenant_id = $1 AND s.scope_kind = $2 AND s.scope_id = $3
                AND s.day = (now() AT TIME ZONE 'UTC')::date",
        )
        .bind(&ctx.tenant)
        .bind(kind)
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(accounting)?;
        if (day_used.max(0) as u64).saturating_add(bound) > *allowance {
            // Scoped to the caller's authority: contributors are collections
            // this caller is authorized to use, but the detail stays generic.
            return Err(exhausted(&format!(
                "a contributing {kind}'s daily allowance cannot cover the next model call \
                ({bound} reserved tokens); the window resets at midnight UTC"
            )));
        }
    }
    sqlx::query(
        "INSERT INTO mcp_token_attempts
            (attempt_id, invocation_id, tenant_id, payer, day, provider_config, model,
             request_ceiling, bound_units, state)
         VALUES ($1, $2, $3, $4, (now() AT TIME ZONE 'UTC')::date, $5, $6, $7, $8, 'reserved')",
    )
    .bind(&attempt_id)
    .bind(&ctx.invocation_id)
    .bind(&ctx.tenant)
    .bind(&policy.payer)
    .bind(provider_config)
    .bind(model)
    .bind(policy.request_ceiling as i64)
    .bind(bound as i64)
    .execute(&mut *tx)
    .await
    .map_err(accounting)?;
    for (kind, id) in policy.daily.keys() {
        sqlx::query(
            "INSERT INTO mcp_token_attempt_scopes (attempt_id, tenant_id, scope_kind, scope_id, day)
             VALUES ($1, $2, $3, $4, (now() AT TIME ZONE 'UTC')::date)",
        )
        .bind(&attempt_id)
        .bind(&ctx.tenant)
        .bind(kind)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(accounting)?;
    }
    tx.commit().await.map_err(accounting)?;
    ctx.admitted
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(Attempt { attempt_id })
}

/// A reservation that was never dispatched (a later native check refused it
/// first): released to zero. Best effort; a failure leaves it reserved, the
/// conservative direction.
pub async fn release(pool: Option<&sqlx::PgPool>, attempt: &Attempt) {
    let Some(pool) = pool else { return };
    let _ = sqlx::query(
        "UPDATE mcp_token_attempts SET state = 'released', settled_units = 0, settled_at = now()
          WHERE attempt_id = $1 AND state = 'reserved'",
    )
    .bind(&attempt.attempt_id)
    .execute(pool)
    .await;
}

/// Settle once by attempt id and return the recorded excess, if any.
/// Missing usage leaves the reservation charged at its estimate.
pub async fn settle(
    pool: Option<&sqlx::PgPool>,
    attempt: &Attempt,
    observed: Option<u64>,
) -> Result<(bool, u64)> {
    let Some(pool) = pool else {
        return Ok((false, 0));
    };
    match observed {
        Some(units) => {
            let excess: Option<i64> = sqlx::query_scalar(
                "WITH settled AS (
                    UPDATE mcp_token_attempts SET state = 'settled', settled_units = $2,
                        settled_at = now()
                     WHERE attempt_id = $1 AND state IN ('reserved', 'uncertain')
                    RETURNING attempt_id, tenant_id, provider_config, model, bound_units, settled_units
                 ), overage AS (
                    INSERT INTO mcp_bound_violations
                        (attempt_id, tenant_id, provider_config, model, bound_units, observed_units)
                    SELECT attempt_id, tenant_id, provider_config, model, bound_units, settled_units
                      FROM settled WHERE settled_units > bound_units
                    RETURNING observed_units - bound_units AS excess
                 )
                 SELECT COALESCE((SELECT excess FROM overage), 0) FROM settled",
            )
            .bind(&attempt.attempt_id)
            .bind(units.min(i64::MAX as u64) as i64)
            .fetch_optional(pool)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?;
            if let Some(excess) = excess.filter(|n| *n > 0) {
                tracing::warn!(
                    attempt = %attempt.attempt_id,
                    excess,
                    "MCP generation usage exceeded its reservation estimate"
                );
            }
            Ok((excess.is_some(), excess.unwrap_or(0) as u64))
        }
        None => {
            let changed = sqlx::query(
                "UPDATE mcp_token_attempts SET state = 'uncertain', settled_at = now()
                  WHERE attempt_id = $1 AND state = 'reserved'",
            )
            .bind(&attempt.attempt_id)
            .execute(pool)
            .await
            .map_err(|e| KernelError::Storage(e.to_string()))?
            .rows_affected()
                > 0;
            Ok((changed, 0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contributor(name: &str, index: Option<&str>) -> Contributor {
        Contributor {
            collection_id: format!("col-{name}"),
            collection_name: name.into(),
            index_version: index.map(String::from),
        }
    }

    const DOC: &str = r#"{"version":1,"collections":[
        {"tenant":"t","collection":"legal","budget_owner":"shared","request_tokens":2000,"daily_tokens":50000,
         "indexes":[{"index_version":"iv-legal-2","request_tokens":3000,"daily_tokens":9000}]},
        {"tenant":"t","collection":"finance","budget_owner":"shared","request_tokens":8000,"daily_tokens":50000},
        {"tenant":"t","collection":"research","budget_owner":"lab","request_tokens":64000,"daily_tokens":90000},
        {"tenant":"t","collection":"draft","request_tokens":100,"daily_tokens":100}
    ]}"#;

    /// A9: the largest effective request allowance among the contributors
    /// governs a shared call; allowances never add; Legal alone keeps its own.
    #[test]
    fn a9_the_largest_contributor_ceiling_governs_a_shared_call() {
        let p = GenerationPolicy::parse(DOC).unwrap();
        let both = resolve(
            &p,
            "t",
            &[contributor("legal", None), contributor("finance", None)],
        )
        .unwrap();
        assert_eq!(both.request_ceiling, 8000);
        assert_eq!(both.payer, "shared");
        assert_eq!(both.daily.len(), 2);
        let legal = resolve(&p, "t", &[contributor("legal", None)]).unwrap();
        assert_eq!(legal.request_ceiling, 2000);
        // A repeated scope is counted once.
        let twice = resolve(
            &p,
            "t",
            &[contributor("legal", None), contributor("legal", None)],
        )
        .unwrap();
        assert_eq!(twice.daily.len(), 1);
    }

    /// A8 inheritance recommendation: a pinned index without an override
    /// inherits the collection pair; a complete override supplies its own
    /// ceiling and adds its own daily scope beside the collection's.
    #[test]
    fn a8_index_overrides_are_complete_pairs_beside_the_collection_day() {
        let p = GenerationPolicy::parse(DOC).unwrap();
        let inherit = resolve(&p, "t", &[contributor("legal", Some("iv-legal-1"))]).unwrap();
        assert_eq!(inherit.request_ceiling, 2000);
        assert_eq!(inherit.daily.len(), 1);
        let own = resolve(&p, "t", &[contributor("legal", Some("iv-legal-2"))]).unwrap();
        assert_eq!(own.request_ceiling, 3000);
        assert_eq!(
            own.daily.get(&("index".into(), "iv-legal-2".into())),
            Some(&9000)
        );
        assert_eq!(
            own.daily.get(&("collection".into(), "col-legal".into())),
            Some(&50000)
        );
        let incomplete = r#"{"version":1,"collections":[{"tenant":"t","collection":"x","budget_owner":"o",
            "request_tokens":1,"daily_tokens":1,"indexes":[{"index_version":"iv","request_tokens":5}]}]}"#;
        assert!(GenerationPolicy::parse(incomplete)
            .unwrap_err()
            .contains("overrides only one allowance"));
    }

    /// A2/A9 refusals: no entry, an entry without an owner, or no common
    /// payer all refuse; an unrelated higher-limit collection cannot raise
    /// a call it does not contribute to.
    #[test]
    fn a2_a9_missing_owner_unknown_contributor_and_mixed_payers_refuse() {
        let p = GenerationPolicy::parse(DOC).unwrap();
        assert!(resolve(&p, "t", &[contributor("unlisted", None)]).is_err());
        assert!(resolve(&p, "t", &[contributor("draft", None)])
            .unwrap_err()
            .contains("budget owner"));
        assert!(resolve(
            &p,
            "t",
            &[contributor("legal", None), contributor("research", None)]
        )
        .unwrap_err()
        .contains("common budget owner"));
        assert!(resolve(&p, "other-tenant", &[contributor("legal", None)]).is_err());
        assert!(resolve(&p, "t", &[]).is_err());
        assert_eq!(
            resolve(&p, "t", &[contributor("legal", None)])
                .unwrap()
                .request_ceiling,
            2000,
            "research (64000) is merely available, not a contributor"
        );
    }

    #[test]
    fn policy_documents_are_closed_and_versioned() {
        assert!(GenerationPolicy::parse(r#"{"version":2}"#).is_err());
        assert!(GenerationPolicy::parse(r#"{"version":1,"extra":1}"#).is_err());
        assert!(GenerationPolicy::parse(r#"{"version":1,"collections":[{"tenant":"t","collection":"a"},{"tenant":"t","collection":"a"}]}"#).is_err());
        assert!(GenerationPolicy::parse(r#"{"version":1}"#)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn reservation_rounds_up_using_the_resolved_models_ratio() {
        let req = munarium_core::provider::CompletionRequest {
            model: "m".into(),
            system: Some("s".repeat(100)),
            prompt: "p".repeat(1000),
            max_tokens: 500,
            temperature: None,
            tools: None,
        };
        let schema = serde_json::json!({"type": "object"});
        let bytes = serde_json::to_vec(&(&req, &schema)).unwrap().len() as u64;
        let p =
            GenerationPolicy::parse(r#"{"version":1,"model_bytes_per_token":{"m":5,"other":2}}"#)
                .unwrap();
        for (policy, ratio) in [(&p, 5), (&GenerationPolicy::default(), 3)] {
            assert_eq!(
                bound_for(policy, &req, Some(&schema)).unwrap(),
                bytes.div_ceil(ratio) + FRAMING_TOKENS + 500
            );
        }
        assert!(
            GenerationPolicy::parse(r#"{"version":1,"model_bytes_per_token":{"m":0}}"#).is_err()
        );
    }
}
