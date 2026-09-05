// SPDX-License-Identifier: Apache-2.0
//! The retrieval coordinator.
//!
//! This crate is the Server integration boundary for retrieval. `munarium-server`
//! depends on it and, from stage 1 onward, never on a storage backend directly
//! (the datastore design).
//!
//! # Why a facade at all
//!
//! `RetrievalBackend` in `munarium-core` has two methods and covers only the
//! legacy shape-scoped path. Every collection operation that matters —
//! searching, building, verifying, activating, retiring — was an inherent method
//! on the concrete `PgRetrieval`, and `AppState::retrieval_for` returned that
//! concrete type. So there was no seam: adding a second implementation would
//! have left PostgreSQL as the real interface (§3.3).
//!
//! # What this is not, yet
//!
//! In stage 1 this forwards to PostgreSQL and nothing else. `RetrievalMode`
//! parses but is pinned to `Postgres`, the query-preparation step is not here
//! yet, and there is no second backend to dispatch to. That is deliberate: the
//! plan's stage 1 exit is "PostgreSQL remains the only implementation but is
//! genuinely behind the coordinator", proven by every existing test and public
//! behaviour being unchanged. Introducing dispatch and preparation in the same
//! commit as the boundary move would make it impossible for a reviewer to tell
//! the mechanical part from the meaningful part.
//!
//! # The split that matters
//!
//! Not everything here is destined for a trait. Collections, membership, active
//! pointers and source ingress are **PostgreSQL's job in every mode** — the
//! datastore crate explicitly does not own them (§4.1). Only the *search* path
//! gets a second implementation. The methods below are grouped to make that
//! visible, so the eventual trait is obvious rather than negotiated.

pub mod backfill;
pub mod build_metrics;
pub mod capabilities;
pub mod config;
pub mod direct;
pub mod executor;
pub mod merge;
pub mod mirror;
pub mod routing;
pub mod serving;
pub mod shadow;
pub mod shadow_candidate;
pub mod shadow_exec;

use munarium_core::retrieval::{
    CollectionInfo, HybridQuery, IndexVersion, PreparedSearchQuery, RetrievalBackend, SearchParams,
    SearchResult, SourceInfo,
};
use munarium_core::Result;
use munarium_retrieval_pg::PgRetrieval;

/// The cross-collection merge — the engine-neutral pooled fusion, adapted.
///
/// This closed the stage 5 gate the decision log set ("cross-engine fusion is
/// a merge hazard"): the raw-score merge in `munarium-retrieval-pg` is no
/// longer the call site, and the equivalence tests in [`merge`] prove the
/// swap changed nothing in `postgres` mode — every score bit-identical, not
/// merely every top-k.
pub use merge::{merge_hits, merge_hits_weighted};

/// Engine-neutral query-policy helpers.
///
/// Pure functions with no SQL in them, re-exported here so Server names ONE
/// retrieval crate rather than two.
pub use munarium_retrieval_pg::{
    expand_query, number_query_digits, pairs_tsquery, select_collection_indices, tsquery_lexemes,
};

/// Which versions a scope must be able to serve, and why. Re-exported so
/// Server reads the policy vocabulary from the coordinator rather than from the
/// PostgreSQL backend.
pub use munarium_retrieval_pg::required::{RequiredReason, RequiredVersionsPolicy};

/// The embedding width the built-in embedder produces.
pub use munarium_retrieval_pg::EMBED_DIMS;

/// Which engine serves user traffic. Parsed from `MUNARIUM_RETRIEVAL_MODE`.
///
/// `Postgres` is the default and the rollback path, and it is what a missing or
/// invalid configuration resolves to — a retrieval mode that fails open onto an
/// unproven engine would be the wrong kind of convenience (§9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RetrievalMode {
    /// Existing PostgreSQL only.
    #[default]
    Postgres,
    /// PostgreSQL serves; a verified datastore artifact is also built.
    Mirror,
    /// As `Mirror`, plus sampled datastore queries for comparison. The
    /// PostgreSQL response is still what the caller receives.
    Shadow,
    /// Datastore serves the scopes the rollout selector routes to it.
    Datastore,
}

impl RetrievalMode {
    /// Parse, falling back to `Postgres`. An unrecognised value is a
    /// configuration error the caller should surface, but it must never
    /// silently select a non-default engine, so this returns the default and
    /// reports whether the input was recognised.
    pub fn parse(raw: &str) -> (Self, bool) {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "postgres" => (Self::Postgres, true),
            "mirror" => (Self::Mirror, true),
            "shadow" => (Self::Shadow, true),
            "datastore" => (Self::Datastore, true),
            _ => (Self::Postgres, false),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mirror => "mirror",
            Self::Shadow => "shadow",
            Self::Datastore => "datastore",
        }
    }
}

/// A cloneable, tenant-scoped retrieval handle.
///
/// Cheap to clone, like the `PgRetrieval` it currently wraps — the session
/// plane fans one out per collection to search concurrently.
#[derive(Clone)]
pub struct Retrieval {
    pg: PgRetrieval,
    mode: RetrievalMode,
    /// Present only in `datastore` mode with the plane's prerequisites met.
    /// Absent, every search forwards to PostgreSQL — which is also what an
    /// UNSELECTED scope does in datastore mode; the difference is that a
    /// selected scope with no plane fails rather than falling back.
    serving: Option<std::sync::Arc<serving::ServingPlane>>,
}

impl Retrieval {
    /// Build a coordinator over the PostgreSQL reference backend.
    pub fn new(pg: PgRetrieval, mode: RetrievalMode) -> Self {
        Self {
            pg,
            mode,
            serving: None,
        }
    }

    /// Attach the datastore serving plane. Only meaningful in
    /// `datastore` mode; in any other mode the dispatch never consults it.
    pub fn with_serving(mut self, plane: std::sync::Arc<serving::ServingPlane>) -> Self {
        self.serving = Some(plane);
        self
    }

    pub fn mode(&self) -> RetrievalMode {
        self.mode
    }

    /// Escape hatch to the reference backend.
    ///
    /// It exists because stage 1 moves the boundary without moving every
    /// behaviour across it in one commit, and a half-moved boundary with no
    /// escape hatch produces worse code than an honest one. Each remaining
    /// caller is a stage 1 follow-up; when there are none, this goes.
    pub fn reference(&self) -> &PgRetrieval {
        &self.pg
    }

    /// The letter-prefixed number lexemes the permitted collections' own
    /// indexes hold for these digit runs (2026-08-30, §13.5 entry 25).
    ///
    /// Forwarded rather than reached around: the caller is the turn pipeline,
    /// and it talks to the coordinator.
    pub async fn number_form_lexemes(
        &self,
        collections: &[CollectionInfo],
        digits: &[String],
    ) -> Result<Vec<String>> {
        self.pg.number_form_lexemes(collections, digits).await
    }

    /// Scan a freshly built index for its letter-prefixed number lexemes.
    ///
    /// Called at cutover so the table is warm; a failure is a warning, not a
    /// build failure, because the lookup populates lazily.
    pub async fn record_number_lexemes(&self, collection_id: &str, index_id: &str) -> Result<()> {
        self.pg.record_number_lexemes(collection_id, index_id).await
    }

    /// The same, for the lexeme-frequency table the stop-term fraction reads.
    pub async fn record_lexeme_frequency(&self, collection_id: &str, index_id: &str) -> Result<()> {
        self.pg
            .record_lexeme_frequency(collection_id, index_id)
            .await
    }

    // -- derived-index builds --------------------------------------
    //
    // A mirror reads chunks PostgreSQL already committed, so these are
    // PostgreSQL-shaped by definition. They live on the coordinator anyway,
    // because the alternative is Server naming `munarium_retrieval_pg` to run a
    // build — which is exactly the boundary stage 1 exists to hold.

    /// Mirror every serving-required version of one collection.
    pub async fn backfill_collection(
        &self,
        ctx: &crate::mirror::MirrorContext,
        collection_id: &str,
        policy: RequiredVersionsPolicy,
        pin_horizon_secs: i64,
    ) -> Result<crate::backfill::BackfillReport> {
        crate::backfill::backfill_collection(ctx, &self.pg, collection_id, policy, pin_horizon_secs)
            .await
    }

    /// Mirror one index version, collection-scoped or legacy shape-scoped.
    ///
    /// Which one it is comes from the version's OWN row, not from the caller: a
    /// caller that had to know would be able to get it wrong, and the two read
    /// different chunk tables.
    pub async fn rebuild_version(
        &self,
        ctx: &crate::mirror::MirrorContext,
        index_version_id: &str,
    ) -> Result<crate::mirror::MirrorOutcome> {
        let facts = self.pg.version_facts(index_version_id).await?;
        let target = match &facts.collection_id {
            Some(collection_id) => crate::mirror::MirrorTarget::Collection { collection_id },
            None => crate::mirror::MirrorTarget::LegacyShape {
                shape_ref: &facts.shape_ref,
            },
        };
        crate::backfill::backfill_one(ctx, &self.pg, target, index_version_id).await
    }

    // -- search -------------------------------------------------------------
    // The only group that gets a second implementation. When the datastore
    // backend lands, these dispatch on the rollout selector; everything below
    // stays PostgreSQL in every mode.

    /// Prepare a query ONCE for a whole request.
    ///
    /// The coordinator owns this, not a backend, for two reasons. The cheap
    /// one: `search_collection` used to derive the expansion and the query
    /// vector internally, so an N-collection fan-out embedded the same query N
    /// times. The one that matters: in shadow mode PostgreSQL and Datastore
    /// answer the SAME request, and if each derived its own plan and its own
    /// vector, a difference in results could not be attributed to the engine --
    /// which is the only thing shadow mode exists to measure.
    ///
    /// "Owns" here is orchestration, not implementation: the embedder is
    /// injected, so provider credentials stay in Server and the datastore crate
    /// never depends on them.
    pub fn prepare_query(&self, query: &str, params: &SearchParams) -> PreparedSearchQuery {
        munarium_retrieval_pg::PgRetrieval::prepare_query(
            query,
            params,
            &munarium_retrieval_pg::LocalHashEmbedder,
        )
    }

    /// Search one collection with an already prepared query.
    ///
    /// The fan-out path: prepare once, then call this per collection. In
    /// `datastore` mode this is where the dispatch happens: the rollout
    /// selector routes the scope, a selected scope is served from its
    /// `serving`-bound artifact with NO fallback (§9.1 — a failure there is
    /// `datastore-unavailable`, never a silent PostgreSQL answer), and an
    /// unselected scope continues on PostgreSQL by policy.
    pub async fn search_collection_prepared(
        &self,
        collection_id: &str,
        prepared: &PreparedSearchQuery,
        index_version: Option<&str>,
    ) -> Result<SearchResult> {
        if self.mode == RetrievalMode::Datastore {
            let Some(plane) = &self.serving else {
                // Fail closed, not over to PostgreSQL: without the selector
                // this handle cannot tell a selected scope from an unselected
                // one, and guessing would let a broken replica silently serve
                // a selected scope from the wrong engine (§9.1). The replica
                // is unready for the same reason; a request that reaches it
                // anyway gets the truth.
                return Err(munarium_core::KernelError::DatastoreUnavailable(
                    "this replica is in datastore mode with no datastore plane".into(),
                ));
            };
            {
                if plane
                    .routes_to_datastore("collection", collection_id)
                    .await?
                {
                    // The LOGICAL version comes from the same control-plane
                    // read the PostgreSQL path performs; only the physical
                    // artifact is the datastore's choice.
                    let (version_id, watermark) = self
                        .pg
                        .resolve_index_version(collection_id, index_version)
                        .await?;
                    let prepared = std::sync::Arc::new(prepared.clone());
                    return plane
                        .search(
                            &self.pg,
                            ("collection", collection_id),
                            &version_id,
                            watermark as u64,
                            &prepared,
                        )
                        .await;
                }
            }
        }
        self.pg
            .search_collection_prepared(collection_id, prepared, index_version)
            .await
    }

    /// Search one collection, preparing inline.
    ///
    /// The single-collection path (one REST search, one legacy query). A
    /// fan-out must use `prepare_query` + `search_collection_prepared` instead,
    /// or it pays for the embedding once per collection.
    ///
    /// A thin wrapper over the prepared path so the datastore dispatch is the
    /// same one: this used to call PostgreSQL directly, which in `datastore`
    /// mode answered a selected scope from one engine over REST and from the
    /// other over the session plane — the two-engines-under-one-scope
    /// situation §9.1 forbids.
    pub async fn search_collection(
        &self,
        collection_id: &str,
        query: &str,
        params: SearchParams,
        index_version: Option<&str>,
    ) -> Result<SearchResult> {
        let prepared = self.prepare_query(query, &params);
        self.search_collection_prepared(collection_id, &prepared, index_version)
            .await
    }

    /// The legacy shape-scoped hybrid search, via `RetrievalBackend`.
    ///
    /// In `datastore` mode the same dispatch as the collection path applies:
    /// the selector routes on scope kind `shape`, a selected shape is served
    /// from its `serving`-bound artifact with no fallback, and the version
    /// resolution stays PostgreSQL's (`resolve_index` — the active LEGACY
    /// version, `collection_id IS NULL`, never a collection's).
    pub async fn hybrid_search(&self, q: HybridQuery) -> Result<SearchResult> {
        if self.mode == RetrievalMode::Datastore {
            let Some(plane) = &self.serving else {
                return Err(munarium_core::KernelError::DatastoreUnavailable(
                    "this replica is in datastore mode with no datastore plane".into(),
                ));
            };
            if plane.routes_to_datastore("shape", &q.shape_ref).await? {
                let (version_id, watermark) = self
                    .pg
                    .resolve_index(&q.shape_ref, q.index_version.as_deref())
                    .await?;
                // The legacy path's fixed knobs, restated: candidate pools of
                // 50 per leg, RRF at 60, the caller's top_k. The query is
                // prepared through the same embedder the artifact was built
                // with; legacy searches carry no expansion rules.
                let params = SearchParams {
                    top_k: if q.top_k == 0 { 10 } else { q.top_k },
                    ..SearchParams::default()
                };
                let prepared = std::sync::Arc::new(self.prepare_query(&q.query, &params));
                return plane
                    .search(
                        &self.pg,
                        ("shape", &q.shape_ref),
                        &version_id,
                        watermark,
                        &prepared,
                    )
                    .await;
            }
        }
        self.pg.hybrid_search(q).await
    }

    /// The active index version for a shape.
    pub async fn index_version(&self, shape_ref: &str) -> Result<IndexVersion> {
        self.pg.index_version(shape_ref).await
    }

    /// Normalized query lexemes, as `plainto_tsquery` prints them.
    ///
    /// Backend-specific today: it round-trips through PostgreSQL. stage 1's
    /// query-preparation step lifts this into a `LexicalQueryPlan` the
    /// coordinator builds once and hands to every backend, at which point this
    /// stops being a retrieval method at all.
    pub async fn query_lexemes(&self, text: &str) -> Result<Vec<String>> {
        self.pg.query_lexemes(text).await
    }

    // -- collections: PostgreSQL in every mode ------------------------------

    pub async fn ensure_collection(
        &self,
        name: &str,
        shape_ref: &str,
        access_level: i32,
        compartments: &[String],
        description: Option<&str>,
    ) -> Result<CollectionInfo> {
        self.pg
            .ensure_collection(name, shape_ref, access_level, compartments, description)
            .await
    }

    pub async fn collection_by_id(&self, id: &str) -> Result<CollectionInfo> {
        self.pg.collection_by_id(id).await
    }

    pub async fn collection_by_name(&self, name: &str) -> Result<CollectionInfo> {
        self.pg.collection_by_name(name).await
    }

    pub async fn list_collections(&self) -> Result<Vec<CollectionInfo>> {
        self.pg.list_collections().await
    }

    pub async fn bind_source(
        &self,
        collection_id: &str,
        source_id: &str,
        bound_by_uid: Option<&str>,
    ) -> Result<bool> {
        self.pg
            .bind_source(collection_id, source_id, bound_by_uid)
            .await
    }

    /// Bind many sources in one statement; see `PgRetrieval::bind_sources`.
    pub async fn bind_sources(
        &self,
        collection_id: &str,
        source_ids: &[String],
        bound_by_uid: Option<&str>,
    ) -> Result<u64> {
        self.pg
            .bind_sources(collection_id, source_ids, bound_by_uid)
            .await
    }

    pub async fn collection_source_count(&self, collection_id: &str) -> Result<i64> {
        self.pg.collection_source_count(collection_id).await
    }

    // -- index lifecycle: PostgreSQL in every mode --------------------------
    // Activation changes the collection's logical active-version pointer, which
    // is control-plane truth. A physical binding is a separate operation that
    // does not exist yet (§7.3) and must never be conflated with this one.

    pub async fn active_collection_index(&self, collection_id: &str) -> Result<Option<String>> {
        self.pg.active_collection_index(collection_id).await
    }

    pub async fn build_collection_index(
        &self,
        collection_id: &str,
        max_chars: usize,
        watermark_seq: u64,
        activate: bool,
    ) -> Result<IndexVersion> {
        if activate {
            self.guard_build_and_activate("collection", collection_id)
                .await?;
        }
        self.pg
            .build_collection_index(collection_id, max_chars, watermark_seq, activate)
            .await
    }

    pub async fn activate_collection_index(
        &self,
        collection_id: &str,
        index_id: &str,
    ) -> Result<()> {
        self.guard_activation("collection", collection_id, index_id)
            .await?;
        self.pg
            .activate_collection_index(collection_id, index_id)
            .await
    }

    /// §7.3 logical activation, as a compare-and-swap: activate `index_id`
    /// only if the CURRENT active version is still `expected_active`.
    /// `Ok(false)` is the superseded-build outcome — the pointer moved under
    /// this build, nothing was changed, and the built version stays valid.
    pub async fn activate_collection_index_cas(
        &self,
        collection_id: &str,
        index_id: &str,
        expected_active: Option<&str>,
    ) -> Result<bool> {
        self.guard_activation("collection", collection_id, index_id)
            .await?;
        self.pg
            .activate_collection_index_cas(collection_id, index_id, expected_active)
            .await
    }

    /// Whether `scope` is one the rollout selector routes to the datastore
    /// on THIS replica — `Ok(false)` in every other mode, `Err` for a
    /// datastore-mode replica with no plane (it cannot tell, and guessing is
    /// how a broken replica activates an unservable version).
    async fn scope_is_datastore_served(&self, scope_kind: &str, scope_id: &str) -> Result<bool> {
        if self.mode != RetrievalMode::Datastore {
            return Ok(false);
        }
        let Some(plane) = &self.serving else {
            return Err(munarium_core::KernelError::DatastoreUnavailable(
                "this replica is in datastore mode with no datastore plane; activation cannot verify the serving binding it requires"
                    .into(),
            ));
        };
        plane.routes_to_datastore(scope_kind, scope_id).await
    }

    /// §7.3 logical activation step 3: for a scope the rollout selector
    /// routes to the datastore, the candidate version must hold a `serving`
    /// binding whose artifact is verified — a verified artifact with no
    /// binding is not servable, and activating it would make the scope
    /// unservable at the moment of cutover. PostgreSQL-served scopes are
    /// untouched: "PostgreSQL mode does not consult bindings."
    ///
    /// `scope_kind` is `collection` or `shape`: the legacy shape path is
    /// routed by the same selector (`hybrid_search`) and so needs the same
    /// guard — an unguarded legacy activation on a datastore-served shape
    /// was the one way left to activate a version with no binding.
    async fn guard_activation(
        &self,
        scope_kind: &str,
        scope_id: &str,
        index_id: &str,
    ) -> Result<()> {
        if !self.scope_is_datastore_served(scope_kind, scope_id).await? {
            return Ok(());
        }
        let plane = self
            .serving
            .as_ref()
            .expect("scope_is_datastore_served returned true only with a plane");
        match plane.executor.catalog.binding(index_id, munarium_store_pg::artifacts::BindingSlot::Serving).await? {
            None => Err(munarium_core::KernelError::InvalidInput(format!(
                "{scope_kind} {scope_id} is datastore-served and version {index_id} has no serving binding; build and promote its artifact before activating"
            ))),
            Some(b) => {
                let verified = plane
                    .executor
                    .catalog
                    .artifact(index_id, &b.artifact_id)
                    .await?
                    .map(|r| r.state == munarium_store_pg::artifacts::ArtifactState::Verified)
                    .unwrap_or(false);
                if verified {
                    Ok(())
                } else {
                    Err(munarium_core::KernelError::InvalidInput(format!(
                        "{scope_kind} {scope_id} is datastore-served and version {index_id}'s serving-bound artifact is not verified; re-verify or re-promote first"
                    )))
                }
            }
        }
    }

    /// The build-then-activate-inline paths (`build_index` /
    /// `build_collection_index` with `activate: true`) activate a version
    /// that does not exist until the build finishes, so there is no binding
    /// to inspect up front — and a PostgreSQL-only build can never produce
    /// one. On a datastore-served scope that combination is refused before
    /// any work is done; the caller builds without activating, mirrors or
    /// promotes the artifact, and activates through the guarded path.
    async fn guard_build_and_activate(&self, scope_kind: &str, scope_id: &str) -> Result<()> {
        if self.scope_is_datastore_served(scope_kind, scope_id).await? {
            return Err(munarium_core::KernelError::InvalidInput(format!(
                "{scope_kind} {scope_id} is datastore-served; a PostgreSQL build cannot activate inline because the version would have no serving binding — build with activate=false, promote its artifact, then activate"
            )));
        }
        Ok(())
    }

    /// Build a collection DIRECTLY: one extraction pass fans to the
    /// PostgreSQL chunk tables and the datastore artifact, and the version's
    /// identity is the `idx2-` hash of the real build spec.
    pub async fn build_collection_direct(
        &self,
        ctx: &crate::mirror::MirrorContext,
        collection_id: &str,
        max_chars: usize,
        watermark_seq: u64,
    ) -> Result<crate::direct::DirectBuildOutcome> {
        crate::direct::build_collection_direct(
            ctx,
            &self.pg,
            collection_id,
            max_chars,
            watermark_seq,
        )
        .await
    }

    pub async fn verify_collection_index(&self, index_id: &str) -> Result<serde_json::Value> {
        self.pg.verify_collection_index(index_id).await
    }

    pub async fn retire_old_collection(&self, collection_id: &str, keep: u32) -> Result<u64> {
        self.pg.retire_old_collection(collection_id, keep).await
    }

    // -- legacy shape-scoped path -------------------------------------------
    // Supported until legacy retirement is separately approved (§9.1).

    pub async fn build_index(
        &self,
        shape_ref: &str,
        max_chars: usize,
        watermark_seq: u64,
        activate: bool,
    ) -> Result<IndexVersion> {
        if activate {
            self.guard_build_and_activate("shape", shape_ref).await?;
        }
        self.pg
            .build_index(shape_ref, max_chars, watermark_seq, activate)
            .await
    }

    pub async fn activate_index(&self, shape_ref: &str, index_id: &str) -> Result<()> {
        self.guard_activation("shape", shape_ref, index_id).await?;
        self.pg.activate_index(shape_ref, index_id).await
    }

    pub async fn verify_index(&self, index_id: &str) -> Result<serde_json::Value> {
        self.pg.verify_index(index_id).await
    }

    pub async fn retire_old(&self, shape_ref: &str, keep: u32) -> Result<u64> {
        self.pg.retire_old(shape_ref, keep).await
    }

    pub async fn source_count(&self, shape_ref: &str) -> Result<i64> {
        self.pg.source_count(shape_ref).await
    }

    pub async fn index_version_by_id(&self, index_id: &str) -> Result<IndexVersion> {
        self.pg.index_version_by_id(index_id).await
    }

    // -- source ingress: PostgreSQL in every mode ---------------------------

    pub async fn put_source(
        &self,
        declared_sha256: &str,
        media_type: &str,
        filename: &str,
        shape_ref: Option<&str>,
        bytes: &[u8],
    ) -> Result<(String, String, bool)> {
        self.pg
            .put_source(declared_sha256, media_type, filename, shape_ref, bytes)
            .await
    }

    pub async fn source_info(&self, source_id: &str) -> Result<SourceInfo> {
        self.pg.source_info(source_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parses_the_four_names() {
        for (raw, want) in [
            ("postgres", RetrievalMode::Postgres),
            ("MIRROR", RetrievalMode::Mirror),
            (" shadow ", RetrievalMode::Shadow),
            ("datastore", RetrievalMode::Datastore),
        ] {
            let (mode, known) = RetrievalMode::parse(raw);
            assert_eq!(mode, want, "{raw}");
            assert!(known, "{raw} should be recognised");
        }
    }

    #[test]
    fn an_unknown_mode_falls_back_to_postgres_and_says_so() {
        // The fallback is the point: a typo must not select an engine, and the
        // caller must be able to tell a typo from a deliberate default.
        let (mode, known) = RetrievalMode::parse("datastor");
        assert_eq!(mode, RetrievalMode::Postgres);
        assert!(!known);
    }

    #[test]
    fn missing_configuration_is_postgres_and_is_not_an_error() {
        let (mode, known) = RetrievalMode::parse("");
        assert_eq!(mode, RetrievalMode::Postgres);
        assert!(known, "an unset mode is the documented default, not a typo");
    }

    #[test]
    fn the_default_is_postgres() {
        assert_eq!(RetrievalMode::default(), RetrievalMode::Postgres);
        assert_eq!(RetrievalMode::default().as_str(), "postgres");
    }
}
