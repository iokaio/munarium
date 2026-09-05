// SPDX-License-Identifier: Apache-2.0
//! Executing a prepared query against a bound artifact: resolve the slot's
//! binding, hydrate, open, run both legs, fuse.
//!
//! One executor serves two callers with two postures, and the differences are
//! PARAMETERS rather than copies:
//!
//! - The **shadow candidate** resolves the `shadow` slot at `Opportunistic`
//!   residency — a cache guest, first out under pressure, whose output feeds a
//!   comparison and never a user.
//! - The **serving path** resolves the `serving` slot at
//!   `ServingRequired` residency — evicting it would withdraw the replica's
//!   readiness — and its output IS the user's answer.
//!
//! What both share, and why it is structural: there is NO fallback anywhere in
//! this path. A missing binding, an unverified artifact, a quarantined key, an
//! unsupported reader — each refuses with its reason. For a shadow that keeps
//! the comparison honest; for serving it is §9.1's rule that a selected scope
//! fails with a specific error class rather than silently switching engines.
//! Everything downstream of the binding is verified before it is trusted:
//! hydration checks every component hash, and the open re-verifies the
//! manifest against the artifact id and runs the manifest's own probes.
//!
//! The query arrives PREPARED — same expansion, same query vector as the
//! reference engine, by construction. The one thing this engine derives for
//! itself is tokenization: the plan's expanded string is analyzed through the
//! artifact's own analyzer, because an index answers queries tokenized the way
//! it was built.

use std::sync::Arc;
use std::time::Instant;

use munarium_core::retrieval::{PreparedSearchQuery, SearchHit};
use munarium_datastore::fusion::FusionWeights;
use munarium_datastore::hydrate::{L1Cache, Residency};
use munarium_datastore::lexical::{Demotion, LexicalPlan, PlanTerm};
use munarium_datastore::shard::OpenShard;
use munarium_datastore::store::LocalFileStore;
use munarium_datastore::verify::{Limits, ReaderCapabilities};
use munarium_datastore::ArtifactCacheKey;
use munarium_store_pg::artifacts::{ArtifactCatalog, ArtifactState, BindingSlot};

use crate::mirror::ArtifactStoreFactory;
use crate::shadow::PhaseLatency;

/// L0: open shards, held in memory (§10.1's top tier).
///
/// The first benchmark baseline measured WHY this exists: without it every
/// query re-read the archive, re-verified every component and re-materialized
/// the Tantivy index — 42 ms of open cost per query on a corpus PostgreSQL
/// answered in 16 ms. An open shard is immutable (its cache key names content
/// hashes), so holding it costs memory and nothing else: there is no
/// staleness to manage, only capacity.
///
/// Bounded by SHARD COUNT rather than bytes, deliberately: the dominant
/// memory is the records and vectors, which are proportional to the artifact,
/// and the operator sizing L1 in bytes has already bounded what can be
/// resident; the count cap only stops a pathological many-artifact tenant mix
/// from holding every shard open at once. Eviction is
/// least-recently-inserted, which is enough at this cap.
pub struct L0Cache {
    max_open: usize,
    state: std::sync::Mutex<L0State>,
}

#[derive(Default)]
struct L0State {
    open: std::collections::HashMap<ArtifactCacheKey, Arc<crate::executor::SharedShard>>,
    order: std::collections::VecDeque<ArtifactCacheKey>,
}

/// An open shard, shared across queries and threads.
pub type SharedShard = munarium_datastore::shard::OpenShard;

impl L0Cache {
    pub fn new(max_open: usize) -> Self {
        Self {
            max_open: max_open.max(1),
            state: std::sync::Mutex::new(L0State::default()),
        }
    }

    pub fn get(&self, key: &ArtifactCacheKey) -> Option<Arc<SharedShard>> {
        self.state.lock().unwrap().open.get(key).cloned()
    }

    pub fn insert(&self, key: ArtifactCacheKey, shard: Arc<SharedShard>) {
        let mut st = self.state.lock().unwrap();
        if st.open.contains_key(&key) {
            return;
        }
        while st.open.len() >= self.max_open {
            let Some(evict) = st.order.pop_front() else {
                break;
            };
            st.open.remove(&evict);
        }
        st.order.push_back(key.clone());
        st.open.insert(key, shard);
    }

    /// Drop a shard whose bytes were found bad — quarantine reaches L0 too.
    pub fn remove(&self, key: &ArtifactCacheKey) {
        let mut st = self.state.lock().unwrap();
        st.open.remove(key);
        st.order.retain(|k| k != key);
    }

    pub fn len(&self) -> usize {
        self.state.lock().unwrap().open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Everything an execution needs, resolved once per process/tenant.
pub struct ArtifactExecutor {
    pub catalog: ArtifactCatalog,
    pub stores: Arc<dyn ArtifactStoreFactory>,
    pub cache: Arc<L1Cache>,
    /// The process-wide open-shard tier. SHARED like the L1 cache and for the
    /// same reason: per-request opens are the cost this exists to remove.
    pub l0: Arc<L0Cache>,
    pub reader: ReaderCapabilities,
    pub limits: Limits,
    /// Opaque, tenant-derived. Combined into the cache key so two tenants'
    /// identical content stays two residencies.
    pub isolation_domain: String,
}

impl std::fmt::Debug for ArtifactExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The isolation domain is tenant-derived; it has no business in logs.
        f.debug_struct("ArtifactExecutor").finish_non_exhaustive()
    }
}

/// How much of each hit the execution carries out.
///
/// Two callers, two needs, and the difference is deliberate rather than a
/// default: the shadow comparison reads only identities and hashes, so its
/// hits carry NO corpus text — a comparison that cannot hold text cannot leak
/// it into a log or a metric. The serving path's whole product is the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextPayload {
    /// Identity only: `text` stays empty. The shadow posture.
    Identity,
    /// Each hit carries its record's text. The serving posture.
    Served,
}

/// What an execution produced.
#[derive(Debug)]
pub enum ExecutionOutcome {
    /// The artifact could not serve, as a matter of state rather than
    /// incident: no binding in the slot, an unverified or quarantined
    /// artifact, an incompatible reader.
    Refused(String),
    /// The execution tried and broke.
    Failed(String),
    Executed(ArtifactExecution),
}

/// The artifact's answer, in the same shape the reference engine produces —
/// which is what lets one comparison consume both, and one serving path
/// return it to a caller.
#[derive(Debug)]
pub struct ArtifactExecution {
    pub artifact_id: String,
    pub engine: String,
    /// Fused, shaped hits — what this engine WOULD have returned.
    ///
    /// `source_content_hash` on these carries the CHUNK-TEXT sha256, and
    /// [`comparison`] normalizes the reference the same way, because "one
    /// chunk id resolving to two different texts" is the corruption the
    /// text-hash check exists to catch and chunk text is what both sides can
    /// actually hash. `text` is deliberately empty — nothing in a comparison
    /// needs corpus content, so nothing here can leak it.
    pub hits: Vec<SearchHit>,
    pub latency: PhaseLatency,
}

impl ArtifactExecutor {
    /// Resolve → hydrate → open → run both legs → fuse.
    ///
    /// Async at the seam only: catalog reads are async SQL, while hydration,
    /// opening and searching are synchronous CPU/IO work run on the blocking
    /// pool — an open materializes a lexical index, and doing that on the
    /// async runtime would stall unrelated requests.
    pub async fn execute(
        &self,
        index_version_id: &str,
        slot: BindingSlot,
        residency: Residency,
        payload: TextPayload,
        prepared: &Arc<PreparedSearchQuery>,
    ) -> ExecutionOutcome {
        let total = Instant::now();

        let binding = match self.catalog.binding(index_version_id, slot).await {
            Ok(Some(b)) => b,
            Ok(None) => {
                return ExecutionOutcome::Refused(format!(
                    "no {} binding for {index_version_id}",
                    slot.as_str()
                ))
            }
            Err(e) => return ExecutionOutcome::Failed(format!("binding read: {e}")),
        };

        let row = match self
            .catalog
            .artifact(index_version_id, &binding.artifact_id)
            .await
        {
            Ok(Some(r)) => r,
            Ok(None) => {
                return ExecutionOutcome::Refused(format!(
                    "{} binding names artifact {} which the catalog does not hold",
                    slot.as_str(),
                    binding.artifact_id
                ))
            }
            Err(e) => return ExecutionOutcome::Failed(format!("artifact read: {e}")),
        };
        if row.state != ArtifactState::Verified {
            // A binding normally asserts verification, but retirement and
            // failure can arrive after the bind; re-checking here is what
            // keeps a stale binding from serving a withdrawn artifact.
            return ExecutionOutcome::Refused(format!(
                "artifact {} is {}, not verified",
                row.artifact_id,
                row.state.as_str()
            ));
        }

        let store = match self.stores.store_for_prefix(&row.artifact_uri) {
            Ok(s) => s,
            Err(e) => return ExecutionOutcome::Failed(format!("store: {e}")),
        };
        let key = ArtifactCacheKey::new(
            self.isolation_domain.clone(),
            index_version_id,
            row.artifact_id.clone(),
        );

        let cache = Arc::clone(&self.cache);
        let l0 = Arc::clone(&self.l0);
        let reader = self.reader.clone();
        let limits = self.limits;
        let engine = row.engine_id.clone();
        let artifact_id = row.artifact_id.clone();
        let prepared = Arc::clone(prepared);

        let joined = tokio::task::spawn_blocking(move || {
            run_blocking(
                &cache,
                &l0,
                &key,
                store.as_ref(),
                &reader,
                &limits,
                &prepared,
                engine,
                artifact_id,
                residency,
                payload,
                total,
            )
        })
        .await;

        match joined {
            Ok(outcome) => outcome,
            Err(e) => ExecutionOutcome::Failed(format!("execution task: {e}")),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_blocking(
    cache: &L1Cache,
    l0: &L0Cache,
    key: &ArtifactCacheKey,
    store: &dyn munarium_datastore::store::ArtifactStore,
    reader: &ReaderCapabilities,
    limits: &Limits,
    prepared: &PreparedSearchQuery,
    engine: String,
    artifact_id: String,
    residency: Residency,
    payload: TextPayload,
    total: Instant,
) -> ExecutionOutcome {
    use munarium_datastore::Error as DsError;

    let refuse_or_fail = |e: DsError| match e {
        // States, not incidents: this reader cannot serve it, the key is
        // quarantined, a limit refused it before allocation.
        DsError::Unsupported(_) | DsError::Limit(_) => ExecutionOutcome::Refused(e.to_string()),
        DsError::Integrity(m) => ExecutionOutcome::Refused(format!("integrity (quarantined): {m}")),
        other => ExecutionOutcome::Failed(other.to_string()),
    };

    // The caller's residency is the posture: Opportunistic for a shadow (a
    // cache guest, never able to displace the serving-required set),
    // ServingRequired for the serving path (evicting it withdraws readiness).
    let resident = match cache.hydrate(key, store, reader, limits, residency) {
        Ok(r) => r,
        Err(e) => return refuse_or_fail(e),
    };

    // L0 first: an open shard for this key is immutable and reusable. The
    // miss path opens from the hydrated bytes and shares the result.
    let shard: Arc<OpenShard> = match l0.get(key) {
        Some(s) => s,
        None => {
            let local = match LocalFileStore::new(&resident.path) {
                Ok(s) => s,
                Err(e) => return ExecutionOutcome::Failed(format!("local store: {e}")),
            };
            match OpenShard::open(&local, &artifact_id, reader, limits) {
                Ok(s) => {
                    let s = Arc::new(s);
                    l0.insert(key.clone(), Arc::clone(&s));
                    s
                }
                Err(e) => {
                    // An integrity failure poisons any cached copy too.
                    if matches!(e, DsError::Integrity(_)) {
                        l0.remove(key);
                    }
                    return refuse_or_fail(e);
                }
            }
        }
    };

    // The lexical leg: plan terms come from analyzing the prepared EXPANDED
    // query through this artifact's own analyzer (module header). Selection
    // always uses the expanded formulation, mirroring the reference.
    let lex_started = Instant::now();
    let lexical = match &prepared.lexical {
        None => Vec::new(),
        Some(plan) => {
            let expanded_tokens = match shard.analyze(&plan.expanded) {
                Ok(t) => t,
                Err(e) => return refuse_or_fail(e),
            };
            let original_tokens: std::collections::HashSet<String> =
                match shard.analyze(&plan.original) {
                    Ok(t) => t.into_iter().collect(),
                    Err(e) => return refuse_or_fail(e),
                };
            let mut seen = std::collections::HashSet::new();
            let terms: Vec<PlanTerm> = expanded_tokens
                .into_iter()
                .filter(|t| seen.insert(t.clone()))
                .map(|t| {
                    if original_tokens.contains(&t) {
                        PlanTerm::user(t)
                    } else {
                        PlanTerm::expanded(t)
                    }
                })
                .collect();
            let ds_plan = LexicalPlan {
                terms,
                // No phrase groups: the reference's tsquery is OR (or the
                // pairs prefilter), never a phrase query, and an extra clause
                // on one side would measure the clause rather than the engine.
                phrases: Vec::new(),
                demotions: plan
                    .demotions
                    .iter()
                    .map(|d| Demotion {
                        contains: d.contains.clone(),
                        multiplier: d.lexical_multiplier as f32,
                    })
                    .collect(),
                minimum_should_match: plan.minimum_should_match,
            };
            match shard.lexical_candidates(&ds_plan, prepared.lexical_candidates.max(0) as usize) {
                Ok(c) => c,
                Err(e) => return refuse_or_fail(e),
            }
        }
    };
    let lexical_ms = lex_started.elapsed().as_secs_f64() * 1000.0;

    let vec_started = Instant::now();
    let vector = match prepared.embedding.as_deref() {
        None => Vec::new(),
        Some(embedding) => {
            match shard.vector_candidates(embedding, prepared.vector_candidates.max(0) as usize) {
                Ok(c) => c,
                // A lexical-only artifact answering a hybrid query serves the
                // leg it has; a dimension mismatch is a refusal.
                Err(e) => return refuse_or_fail(e),
            }
        }
    };
    let vector_ms = vec_started.elapsed().as_secs_f64() * 1000.0;

    let fuse_started = Instant::now();
    let fused = munarium_datastore::fusion::fuse(
        &lexical,
        &vector,
        &FusionWeights {
            lexical: 1.0,
            vector: 1.0,
            rrf_k: prepared.rrf_k,
        },
    );
    let fusion_ms = fuse_started.elapsed().as_secs_f64() * 1000.0;

    // `top_k == 0` means "the default of 10" on the PostgreSQL path
    // (`search_collection_prepared`, `hybrid_search`); the same prepared query
    // must mean the same answer size here, or a shadow comparison of such a
    // query reports a spurious fused difference between the engines.
    let top_k = if prepared.top_k == 0 {
        10
    } else {
        prepared.top_k
    };
    let mut hits = Vec::new();
    for f in fused.into_iter().take(top_k) {
        let Some(record) = shard.record(&f.chunk_id) else {
            // A fused id the records do not hold is an artifact contradicting
            // itself — §6.4 says reject the hit, and a shadow rejects the run.
            return ExecutionOutcome::Failed(format!(
                "fused chunk {} has no record; the artifact contradicts itself",
                f.chunk_id
            ));
        };
        hits.push(SearchHit {
            chunk_id: record.chunk_id.clone(),
            source_id: record.source_id.clone(),
            source_path: record.source_path.clone(),
            source_content_hash: record.text_sha256.clone(),
            text: match payload {
                TextPayload::Identity => String::new(),
                TextPayload::Served => record.text.clone(),
            },
            score: f.score,
            lexical_rank: f.lexical_rank,
            vector_rank: f.vector_rank,
            lexical_score: f.lexical_score.map(f64::from),
            vector_distance: f.vector_score.map(f64::from),
            metadata: None,
        });
    }

    ExecutionOutcome::Executed(ArtifactExecution {
        artifact_id,
        engine,
        hits,
        latency: PhaseLatency {
            lexical_ms,
            vector_ms,
            fusion_ms,
            total_ms: total.elapsed().as_secs_f64() * 1000.0,
        },
    })
}
