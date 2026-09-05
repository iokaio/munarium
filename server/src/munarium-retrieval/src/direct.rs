// SPDX-License-Identifier: Apache-2.0
//! The direct build: one extraction pass, two indexes, and the
//! `idx2-` identity that records what actually happened.
//!
//! The mirror re-reads chunks PostgreSQL committed; the direct build extracts
//! ONCE and fans the same prepared chunks to both sinks — the PostgreSQL
//! chunk tables (through a short, insert-only transaction) and the datastore
//! artifact (through the same attempt-leased publication path the mirror
//! uses). No provider or extraction work is duplicated, and no transaction
//! spans extraction (§7.2).
//!
//! ## The identity
//!
//! A direct build's version id is `idx2-` + sha256(canonical `BuildSpec`),
//! and the spec is a RECORD, not a reconstruction: real per-source extraction
//! outcomes, real snapshot, `reconstructed: false`. That is why extraction
//! runs before the id exists — the id is a hash of what extraction produced.
//! Engine and physical knobs stay out of it, so an engine upgrade re-binds
//! artifacts without touching the version a session pinned.
//!
//! ## What this does NOT do
//!
//! Activation. A built version is inert until an operator (or the caller,
//! explicitly) activates it — and activation for datastore-served scopes goes
//! through the guarded coordinator path, which refuses to activate a version
//! whose `serving` binding does not exist (§7.3 logical activation step 3).

use munarium_core::retrieval::IndexVersion;
use munarium_core::{KernelError, Result};
use munarium_datastore::model::{
    Chunker, Embedder, ExtractionOutcome, ExtractionStatus, Extractor, IndexOptions,
    LexicalAnalysis, Metric, Normalization, ShapeRef, Snapshot, SourceRef, StopTerms,
};
use munarium_datastore::PreparedChunk;
use munarium_retrieval_pg::direct::PreparedBuild;
use munarium_retrieval_pg::PgRetrieval;

use crate::mirror::{direct_index, direct_plan, MirrorContext, MirrorOutcome, MirrorTarget};

/// What a direct build produced, all identities included.
#[derive(Debug)]
pub struct DirectBuildOutcome {
    /// The `idx2-` logical version — the spec's own hash.
    pub index_version_id: String,
    /// Whether THIS call committed the PostgreSQL rows (false = the identity
    /// already existed, which is convergence, not failure).
    pub committed: bool,
    /// What the artifact publication did.
    pub artifact: MirrorOutcome,
    /// The version that was active when the build STARTED — the
    /// `expected_active_version` a §7.3 CAS activation should pass, so a
    /// concurrent activation surfaces as superseded rather than clobbered.
    pub expected_active: Option<String>,
    pub version: IndexVersion,
}

/// Build a collection directly: extract once, commit the PostgreSQL rows in a
/// short transaction, and publish the datastore artifact from the same
/// chunks.
pub async fn build_collection_direct(
    ctx: &MirrorContext,
    pg: &PgRetrieval,
    collection_id: &str,
    max_chars: usize,
    watermark_seq: u64,
) -> Result<DirectBuildOutcome> {
    // §7.2 step 1: record what is active BEFORE the work, so activation can
    // be a compare-and-swap against it.
    let expected_active = pg.current_active_collection_index(collection_id).await?;

    // The slow half — no transaction anywhere near it.
    let build = pg
        .extract_collection_prepared(collection_id, max_chars)
        .await?;

    // The REAL spec, from what extraction actually did.
    let spec = direct_spec(
        collection_id,
        watermark_seq,
        &build,
        &pg.extractor_version(),
    );
    let index_version_id = spec
        .index_version_id()
        .map_err(|e| KernelError::InvalidInput(format!("canonical spec: {e}")))?;

    let manifest = serde_json::json!({
        "collection_id": collection_id,
        "collection_name": build.info.name,
        "shape_ref": build.info.shape_ref,
        "chunker": munarium_retrieval_pg::CHUNKER_VERSION,
        "extractors": pg.extractor_version(),
        "embedder": {
            "provider": "local",
            "model": munarium_retrieval_pg::LOCAL_EMBEDDER,
            "dims": munarium_retrieval_pg::EMBED_DIMS,
        },
        "source_set": build.sources.iter().map(|s| s.content_hash.clone()).collect::<Vec<_>>(),
        "max_chars": max_chars,
        // What distinguishes this row from an `idx-` build at a glance, and
        // the statement that its id is a spec hash rather than a recipe hash.
        "direct": true,
        "spec_version": spec.spec_version,
    });

    // The short half.
    let committed = pg
        .commit_prepared_index(
            collection_id,
            &index_version_id,
            &manifest,
            watermark_seq,
            &build,
        )
        .await?;

    // The artifact, from the SAME chunks — extraction never runs twice.
    let chunks: Vec<PreparedChunk> = build
        .chunks
        .iter()
        .map(|c| {
            use sha2::Digest as _;
            PreparedChunk {
                chunk_id: c.chunk_id.clone(),
                source_id: c.source_id.clone(),
                source_path: c.source_path.clone(),
                node_id: None,
                ordinal: c.ordinal,
                text: c.text.clone(),
                text_sha256: sha2::Sha256::digest(c.text.as_bytes()).into(),
                embedding: Some(c.embedding.clone()),
                metadata: Default::default(),
            }
        })
        .collect();
    // The direct build is where the chunk count is finally known BEFORE the
    // plan is fixed, so the exact/approximate threshold is decided here and
    // recorded with the observed count (§6.3). The mirror path never takes
    // this branch: it is the reference reconstruction and stays exact.
    let plan = direct_plan(true, chunks.len() as u64, &ctx.vector_policy);
    let artifact = direct_index(
        ctx,
        pg,
        MirrorTarget::Collection { collection_id },
        &index_version_id,
        &spec,
        &plan,
        chunks,
    )
    .await?;

    let version = pg.index_version_by_id(&index_version_id).await?;
    Ok(DirectBuildOutcome {
        index_version_id,
        committed,
        artifact,
        expected_active,
        version,
    })
}

/// The direct build's spec: a record of the inputs, never a reconstruction.
fn direct_spec(
    collection_id: &str,
    watermark_seq: u64,
    build: &PreparedBuild,
    extractor_version: &str,
) -> munarium_datastore::model::BuildSpec {
    munarium_datastore::model::BuildSpec {
        spec_version: 1,
        scope: munarium_datastore::model::Scope {
            kind: munarium_datastore::model::ScopeKind::Collection,
            id: collection_id.to_string(),
        },
        sources: build
            .sources
            .iter()
            .map(|s| SourceRef {
                source_id: s.source_id.clone(),
                logical_path: s.filename.clone(),
                media_type: s.media_type.clone(),
                content_sha256: s.content_hash.clone(),
                revision: None,
            })
            .collect(),
        snapshot: Snapshot { watermark_seq },
        shape: ShapeRef {
            shape_ref: build.info.shape_ref.clone(),
            version: 1,
        },
        chunker: Chunker {
            name: "para".into(),
            version: munarium_retrieval_pg::CHUNKER_VERSION.into(),
            params: Default::default(),
        },
        extractor: Extractor {
            name: "munarium-extract".into(),
            version: extractor_version.to_string(),
            config: Default::default(),
            per_source: build
                .outcomes
                .iter()
                .map(|o| ExtractionOutcome {
                    source_id: o.source_id.clone(),
                    outcome: match o.status {
                        "extracted" => ExtractionStatus::Extracted,
                        "empty" => ExtractionStatus::Empty,
                        _ => ExtractionStatus::Failed,
                    },
                    extracted_text_sha256: o.extracted_text_sha256.clone(),
                    method: o.method.clone(),
                })
                .collect(),
        },
        embedder: Some(Embedder {
            model: munarium_retrieval_pg::LOCAL_EMBEDDER.into(),
            dimensions: munarium_retrieval_pg::EMBED_DIMS as u32,
            normalization: Normalization::L2,
            metric: Metric::Cosine,
        }),
        lexical_analysis: LexicalAnalysis {
            contract_version: munarium_datastore::lexical::ANALYZER_CONTRACT_VERSION,
            tokenizer: munarium_datastore::lexical::TOKENIZER_ID.into(),
            stemmer: "snowball-english".into(),
            stop_terms_ref: StopTerms {
                list_ref: "pg16/english".into(),
                sha256: munarium_datastore::lexical::stop_terms_sha256(),
            },
            index_options: IndexOptions {
                positions: true,
                case_folding: Some("lowercase".into()),
                accent_folding: Some("none".into()),
            },
        },
        reconstructed: false,
    }
}
