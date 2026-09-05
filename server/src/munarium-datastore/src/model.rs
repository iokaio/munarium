// SPDX-License-Identifier: Apache-2.0
//! The three canonical documents, mirroring `server/contract/datastore/`.
//!
//! Field names and shapes match the JSON Schemas exactly, because the contract
//! examples and identity vectors are deserialized by tests here — a rename that
//! drifts from the schema fails rather than silently producing a different
//! `artifact_id`.
//!
//! Identity, in one place (§5.1):
//!
//! - `index_version_id` = `idx2-` + sha256(canonical `BuildSpec`). Logical.
//!   Excludes the engine, its revision, the envelope format and every physical
//!   knob, so an engine upgrade does NOT invalidate a session's pin.
//! - `artifact_plan_sha256` = sha256(canonical `ArtifactBuildPlan`). Physical.
//! - `artifact_id` = sha256(canonical `manifest.json`). The ONE physical
//!   content identifier — do not introduce a second.
//!
//! ## No `skip_serializing_if`, anywhere
//!
//! Every nullable field serializes explicitly as `null`. In a document whose
//! hash IS its identity, an omitted field and a null field are different
//! documents with different ids, so `skip_serializing_if` is not a formatting
//! convenience here — it silently changes what an artifact is called. The
//! contract's Python reference emits nulls, and the cross-implementation test
//! in `tests/contract_vectors.rs` is what caught this.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::canonical::canonical_sha256;
use crate::Error;

// --- BuildSpec: the logical indexed corpus ----------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildSpec {
    pub spec_version: u32,
    pub scope: Scope,
    /// Ordered, and the order is part of the identity: it is the stable
    /// source_id ordering the builder streams in, so two builds of the same
    /// SET cannot disagree by permutation.
    pub sources: Vec<SourceRef>,
    pub snapshot: Snapshot,
    pub shape: ShapeRef,
    pub chunker: Chunker,
    pub extractor: Extractor,
    /// `None` is a lexical-only corpus, and legitimate.
    pub embedder: Option<Embedder>,
    pub lexical_analysis: LexicalAnalysis,
    /// `true` for specs reassembled from an existing PostgreSQL `idx-` version
    /// during a mirror build. Such a spec's hash is NOT the source of the
    /// version id, and it must never be used as replay input: it is a best
    /// reconstruction of inputs nobody recorded, not a record of them.
    pub reconstructed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scope {
    pub kind: ScopeKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Collection,
    LegacyShape,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRef {
    pub source_id: String,
    pub logical_path: String,
    pub media_type: String,
    pub content_sha256: String,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub watermark_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapeRef {
    #[serde(rename = "ref")]
    pub shape_ref: String,
    pub version: u32,
}

/// Scalar values a spec may carry. Deliberately has no float variant: the
/// canonicalizer would refuse one, and refusing at the type is better than
/// refusing at the hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Param {
    Bool(bool),
    Int(i64),
    Text(String),
    Null,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunker {
    pub name: String,
    pub version: String,
    /// Every EFFECTIVE parameter, resolved: a default that was not written
    /// down still changes the chunks. `BTreeMap` so serialization is ordered
    /// before canonicalization ever sees it.
    pub params: BTreeMap<String, Param>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Extractor {
    pub name: String,
    pub version: String,
    pub config: BTreeMap<String, Param>,
    /// Without the per-source record, two builds whose extraction silently
    /// differed would share a logical id.
    pub per_source: Vec<ExtractionOutcome>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractionOutcome {
    pub source_id: String,
    pub outcome: ExtractionStatus,
    pub extracted_text_sha256: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionStatus {
    Extracted,
    Empty,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Embedder {
    pub model: String,
    pub dimensions: u32,
    pub normalization: Normalization,
    pub metric: Metric,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    L2,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Cosine,
    L2,
    InnerProduct,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexicalAnalysis {
    pub contract_version: u32,
    pub tokenizer: String,
    pub stemmer: String,
    pub stop_terms_ref: StopTerms,
    pub index_options: IndexOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopTerms {
    #[serde(rename = "ref")]
    pub list_ref: String,
    /// Carried by HASH, not by reference alone: a reference would let the list
    /// change under a fixed logical id.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexOptions {
    /// Must be true for any corpus the current shaping serves: the phrase and
    /// substring demotions read positions.
    pub positions: bool,
    #[serde(default)]
    pub case_folding: Option<String>,
    #[serde(default)]
    pub accent_folding: Option<String>,
}

impl BuildSpec {
    /// `idx2-` + the full SHA-256 of the canonical document.
    ///
    /// Nothing is concatenated outside the document — the spec already carries
    /// the scope and the snapshot, and a concatenation would be a second format
    /// to get wrong.
    pub fn index_version_id(&self) -> Result<String, Error> {
        Ok(format!("idx2-{}", canonical_sha256(self)?))
    }
}

// --- ArtifactBuildPlan: one physical realization ----------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactBuildPlan {
    pub plan_version: u32,
    pub envelope: Envelope,
    pub lexical: LexicalEngine,
    /// `None` when the spec has no embedder. A plan must not declare a vector
    /// leg the spec cannot supply.
    pub vector: Option<VectorEngine>,
    pub records: RecordsFormat,
    #[serde(default)]
    pub range_map: Option<RangeMapPlan>,
    pub shaper: Shaper,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub format_version: u32,
    /// Sorted and deduplicated by the builder.
    pub feature_bits: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexicalEngine {
    pub engine_id: String,
    /// A pinned semver or an immutable git rev. A floating range or a branch
    /// name is not a revision.
    pub engine_revision: String,
    pub positions: bool,
    #[serde(default)]
    pub segments: Option<u32>,
    #[serde(default)]
    pub compression: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorEngine {
    pub engine_id: String,
    pub engine_revision: String,
    pub kind: VectorKind,
    #[serde(default)]
    pub quantization: Option<String>,
    #[serde(default)]
    pub graph: Option<BTreeMap<String, Param>>,
    #[serde(default)]
    pub rescore_depth: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorKind {
    /// The correctness oracle and the small-index fast path.
    Exact,
    /// Requires the recall gate against `Exact`.
    Approximate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordsFormat {
    pub format: String,
    #[serde(default)]
    pub compression: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RangeMapPlan {
    pub block_bytes: u64,
    pub hash_algorithm: String,
}

/// How the physical settings were CHOSEN. Recording the decision inputs is what
/// makes a build policy auditable rather than a story about what someone
/// probably ran — and settings must never be chosen from transient runtime load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shaper {
    pub policy_version: u32,
    pub decisions: Vec<ShaperDecision>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShaperDecision {
    pub setting: String,
    pub chosen: Param,
    pub because: String,
    #[serde(default)]
    pub threshold: Option<Param>,
    #[serde(default)]
    pub observed: Option<Param>,
}

impl ArtifactBuildPlan {
    pub fn plan_sha256(&self) -> Result<String, Error> {
        canonical_sha256(self)
    }
}

// --- ArtifactManifest: a pure function of sealed content --------------------

/// Content-pure by construction: there is no field for a build timestamp,
/// builder identity, attempt id or hostname, and none for a tenant or a logical
/// version. The first four are non-content metadata that belongs to the catalog
/// and attempt rows; the last two are AUTHORITY, and putting authority in a
/// content hash would make it pretend to be an authorization boundary.
///
/// Purity is what makes two byte-identical rebuilds converge on one
/// `artifact_id` instead of colliding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub manifest_version: u32,
    pub format_version: u32,
    pub build_spec_sha256: String,
    pub artifact_plan_sha256: String,
    pub engines: Vec<EngineRef>,
    pub components: Vec<Component>,
    #[serde(default)]
    pub range_map: Option<RangeMapRef>,
    pub counts: Counts,
    pub reader: ReaderRange,
    pub probes: Vec<Probe>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineRef {
    pub role: EngineRole,
    pub engine_id: String,
    pub engine_revision: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineRole {
    Lexical,
    Vector,
    Records,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Component {
    pub path: String,
    pub purpose: ComponentPurpose,
    pub bytes_len: u64,
    pub sha256: String,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentPurpose {
    ManifestSidecar,
    Records,
    Lexical,
    Vector,
    Filters,
    RangeMap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RangeMapRef {
    pub path: String,
    pub block_bytes: u64,
    pub blocks: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Counts {
    pub chunks: u64,
    pub documents: u64,
    pub terms: u64,
    #[serde(default)]
    pub vectors: Option<u64>,
    #[serde(default)]
    pub dimensions: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReaderRange {
    pub min_version: u32,
    pub max_version: u32,
    /// A reader that does not recognise one of these refuses the artifact;
    /// unknown OPTIONAL features are ignored.
    pub required_features: Vec<String>,
}

/// A probe turns "the hashes matched" into "the index answers", which is a
/// different claim: a correctly transferred but wrongly built index passes
/// checksums.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    pub id: String,
    pub kind: ProbeKind,
    #[serde(default)]
    pub query: Option<String>,
    pub expect: ProbeExpectation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    Lexical,
    Vector,
    Record,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeExpectation {
    pub chunk_ids: Vec<String>,
    #[serde(default)]
    pub result_sha256: Option<String>,
}

impl ArtifactManifest {
    /// The artifact's identity: sha256 of its own canonical bytes, bare hex.
    pub fn artifact_id(&self) -> Result<String, Error> {
        canonical_sha256(self)
    }
}
