// SPDX-License-Identifier: Apache-2.0
//! Building, sealing, opening and querying one artifact.
//!
//! The seal is where identity is created, so the ordering here is load-bearing:
//! components are written and hashed first, the manifest is computed from those
//! hashes last, and `artifact_id` is the hash of the manifest. Nothing
//! attempt-specific — no timestamp, no builder, no hostname — enters the
//! manifest, which is what makes a byte-identical rebuild converge on one id
//! instead of colliding.

use sha2::{Digest, Sha256};

use crate::canonical::canonical_bytes;
use crate::fusion::{fuse, FusedHit, FusionWeights};
use crate::model::*;
use crate::records::{read_records, write_records, ChunkRecord, RecordsBlob};
use crate::store::ArtifactStore;
use crate::vector::{Candidate, FlatVectorIndex, VectorIndex};
use crate::verify::{validate_manifest, verify_component, Limits, ReaderCapabilities};
use crate::{Error, PreparedChunk};

/// Component paths. Fixed rather than configurable: a manifest names them, and
/// a second way to spell the same file is a second thing to get wrong.
pub const RECORDS_BODY: &str = "records/chunks.bin";
pub const RECORDS_INDEX: &str = "records/chunks.idx";
pub const VECTOR_DATA: &str = "vector/flat.bin";
pub const VECTOR_DISKANN_DATA: &str = "vector/diskann.bin";
pub const LEXICAL_INDEX: &str = "lexical/index.bin";
pub const BUILD_SPEC: &str = "build-spec.canonical.json";
pub const ARTIFACT_PLAN: &str = "artifact-plan.canonical.json";
pub const MANIFEST: &str = "manifest.json";

/// Accumulates prepared chunks, then seals them into an artifact.
pub struct ShardWriter {
    records: Vec<ChunkRecord>,
    vectors: Option<FlatVectorIndex>,
    seen: std::collections::HashSet<String>,
}

impl ShardWriter {
    /// `dimensions` is `None` for a lexical-only corpus, which is legitimate.
    pub fn new(dimensions: Option<usize>) -> Self {
        Self {
            records: Vec::new(),
            vectors: dimensions.map(FlatVectorIndex::new),
            seen: std::collections::HashSet::new(),
        }
    }

    /// Add one prepared chunk.
    ///
    /// Refuses a duplicate chunk id: the id is the citation target and the
    /// fusion key, so two chunks sharing one would make a citation ambiguous
    /// and let fusion silently merge different content.
    pub fn add(&mut self, chunk: PreparedChunk) -> Result<(), Error> {
        if !self.seen.insert(chunk.chunk_id.clone()) {
            return Err(Error::Invalid(format!(
                "duplicate chunk id {:?}; ids are citation targets and must be unique",
                chunk.chunk_id
            )));
        }
        match (&mut self.vectors, &chunk.embedding) {
            (Some(ix), Some(_)) if ix.dimensions() == 0 => {
                // A zero-dimension index accepts only empty embeddings and
                // serializes `dims = 0`, which `from_bytes` and
                // `validate_manifest` both refuse — so it would seal an
                // artifact no reader can open. Refused at the first chunk
                // rather than after the whole corpus has been streamed in.
                return Err(Error::Invalid(format!(
                    "chunk {:?}: this shard declares a zero-dimension vector index, which \
                     no reader can open; a lexical-only shard declares no dimensions",
                    chunk.chunk_id
                )));
            }
            (Some(ix), Some(e)) => ix.push(&chunk.chunk_id, e)?,
            (Some(_), None) => {
                return Err(Error::Invalid(format!(
                    "chunk {:?} has no embedding but this shard declares vectors; a partial \
                     vector leg would rank some chunks and silently exclude others",
                    chunk.chunk_id
                )))
            }
            (None, Some(_)) => {
                return Err(Error::Invalid(format!(
                    "chunk {:?} carries an embedding but this shard declares none",
                    chunk.chunk_id
                )))
            }
            (None, None) => {}
        }
        self.records.push(ChunkRecord {
            chunk_id: chunk.chunk_id,
            source_id: chunk.source_id,
            source_path: chunk.source_path,
            node_id: chunk.node_id,
            ordinal: chunk.ordinal,
            text: chunk.text,
            text_sha256: hex::encode(chunk.text_sha256),
        });
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Write every component, then compute the manifest from what was written.
    ///
    /// Returns the manifest and its `artifact_id`. The manifest is NOT written
    /// here: publication writes it last, after the components are durable, so a
    /// visible manifest always describes components that exist (§7.1 step 9).
    pub fn seal(
        self,
        spec: &BuildSpec,
        plan: &ArtifactBuildPlan,
        store: &dyn ArtifactStore,
    ) -> Result<SealedArtifact, Error> {
        if self.records.is_empty() {
            return Err(Error::Invalid(
                "refusing to seal an artifact with no chunks; an empty index answers every \
                 query with nothing and looks identical to a broken one"
                    .into(),
            ));
        }

        let mut components = Vec::new();
        let mut put = |path: &str, purpose: ComponentPurpose, bytes: &[u8], required: bool| {
            store.put_component(path, bytes)?;
            components.push(Component {
                path: path.to_string(),
                purpose,
                bytes_len: bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(bytes)),
                required,
            });
            Ok::<(), Error>(())
        };

        // The canonical sidecars, so an artifact carries the documents whose
        // hashes name it and can be audited without the catalog.
        let spec_bytes = canonical_bytes(spec)?;
        let plan_bytes = canonical_bytes(plan)?;
        put(
            BUILD_SPEC,
            ComponentPurpose::ManifestSidecar,
            &spec_bytes,
            true,
        )?;
        put(
            ARTIFACT_PLAN,
            ComponentPurpose::ManifestSidecar,
            &plan_bytes,
            true,
        )?;

        let RecordsBlob { body, index } = write_records(&self.records)?;
        put(RECORDS_BODY, ComponentPurpose::Records, &body, true)?;
        put(RECORDS_INDEX, ComponentPurpose::Records, &index, true)?;

        // The lexical index. Written here rather than left implied: a manifest
        // that names a lexical engine while carrying no lexical files describes
        // an artifact that cannot answer a lexical query, and every checksum
        // would still pass.
        //
        // A binary without the engine compiled in REFUSES to seal rather than
        // producing that artifact. Silently omitting the component would let a
        // reduced build publish something a full build would later open and
        // find hollow.
        #[cfg(feature = "lexical-tantivy")]
        {
            let bytes = crate::lexical::build(&self.records)?;
            put(LEXICAL_INDEX, ComponentPurpose::Lexical, &bytes, true)?;
        }
        #[cfg(not(feature = "lexical-tantivy"))]
        {
            return Err(Error::Unsupported(format!(
                "this build has no lexical engine compiled in, but the plan names {:?}; sealing would produce an artifact that claims an engine it does not carry",
                plan.lexical.engine_id
            )));
        }

        let mut engines = vec![EngineRef {
            role: EngineRole::Records,
            engine_id: "munarium-records".into(),
            engine_revision: "0.1.0".into(),
        }];
        // The vector leg. The PLAN names the engine and the seal materializes
        // exactly that engine (§6.3: the plan records physical decisions; the
        // seal is where corpus size is finally known, so the plan was built
        // with the count in hand). Every disagreement between the staged
        // chunks and the plan is a refusal, because the alternatives are an
        // artifact that lies about its own engine or one with a half-present
        // leg.
        let (vectors, dimensions) = match (&self.vectors, &plan.vector) {
            (None, None) => (None, None),
            (None, Some(v)) => {
                return Err(Error::Invalid(format!(
                    "the plan names vector engine {:?} but no chunk carried an embedding",
                    v.engine_id
                )));
            }
            (Some(_), None) => {
                return Err(Error::Invalid(
                    "chunks carried embeddings but the plan names no vector engine".into(),
                ));
            }
            (Some(ix), Some(v)) => {
                match (v.engine_id.as_str(), v.kind) {
                    ("munarium-flat", crate::model::VectorKind::Exact) => {
                        let bytes = ix.to_bytes()?;
                        put(VECTOR_DATA, ComponentPurpose::Vector, &bytes, false)?;
                    }
                    ("diskann", crate::model::VectorKind::Approximate) => {
                        #[cfg(feature = "vector-diskann")]
                        {
                            let params = crate::vector_diskann::GraphParams::from_plan_map(
                                v.graph.as_ref().ok_or_else(|| {
                                    Error::Invalid(
                                        "a diskann plan must carry its graph parameters".into(),
                                    )
                                })?,
                            )?;
                            let entries: Vec<(String, Vec<f32>)> = ix
                                .entries()
                                .map(|(id, e)| (id.to_string(), e.to_vec()))
                                .collect();
                            let graph = crate::vector_diskann::DiskAnnVectorIndex::build(
                                ix.dimensions(),
                                &entries,
                                params,
                            )?;
                            let bytes = graph.to_bytes()?;
                            put(VECTOR_DISKANN_DATA, ComponentPurpose::Vector, &bytes, false)?;
                        }
                        #[cfg(not(feature = "vector-diskann"))]
                        {
                            return Err(Error::Unsupported(
                                "the plan names the diskann engine but this build has no \
                                 approximate vector engine compiled in; sealing would produce \
                                 an artifact that claims an engine it does not carry"
                                    .into(),
                            ));
                        }
                    }
                    (engine, kind) => {
                        return Err(Error::Invalid(format!(
                            "unknown vector engine/kind combination {engine:?}/{kind:?}; \
                             approved engines are munarium-flat (exact) and diskann \
                             (approximate)"
                        )));
                    }
                }
                engines.push(EngineRef {
                    role: EngineRole::Vector,
                    engine_id: v.engine_id.clone(),
                    engine_revision: v.engine_revision.clone(),
                });
                (Some(ix.len() as u64), Some(ix.dimensions() as u32))
            }
        };
        engines.push(EngineRef {
            role: EngineRole::Lexical,
            engine_id: plan.lexical.engine_id.clone(),
            engine_revision: plan.lexical.engine_revision.clone(),
        });
        // Sorted so the manifest is a function of content, not of the order
        // this function happened to push in.
        engines.sort_by(|a, b| {
            a.role
                .cmp(&b.role)
                .then_with(|| a.engine_id.cmp(&b.engine_id))
        });
        components.sort_by(|a, b| a.path.cmp(&b.path));

        let documents = {
            let mut ids: Vec<&str> = self.records.iter().map(|r| r.source_id.as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            ids.len() as u64
        };

        // A probe over the artifact's own content: the first record, by id.
        // It proves the index ANSWERS, which a checksum cannot -- a correctly
        // transferred but wrongly built index passes every checksum.
        let mut sorted_ids: Vec<&str> = self.records.iter().map(|r| r.chunk_id.as_str()).collect();
        sorted_ids.sort_unstable();
        let probes = vec![Probe {
            id: "probe-record-first".into(),
            kind: ProbeKind::Record,
            query: None,
            expect: ProbeExpectation {
                chunk_ids: vec![sorted_ids[0].to_string()],
                result_sha256: None,
            },
        }];

        let manifest = ArtifactManifest {
            manifest_version: 1,
            format_version: plan.envelope.format_version,
            build_spec_sha256: hex::encode(Sha256::digest(&spec_bytes)),
            artifact_plan_sha256: hex::encode(Sha256::digest(&plan_bytes)),
            engines,
            components,
            range_map: None,
            counts: Counts {
                chunks: self.records.len() as u64,
                documents,
                // Term counting belongs to the lexical engine; until the
                // Tantivy adapter reports it, this is the record count rather
                // than a guess dressed as a measurement.
                terms: self.records.len() as u64,
                vectors,
                dimensions,
            },
            reader: ReaderRange {
                min_version: 1,
                max_version: plan.envelope.format_version,
                required_features: plan.envelope.feature_bits.clone(),
            },
            probes,
        };
        let artifact_id = manifest.artifact_id()?;
        Ok(SealedArtifact {
            manifest,
            artifact_id,
        })
    }
}

/// What a seal produced: the manifest and the id it hashes to.
#[derive(Debug, Clone)]
pub struct SealedArtifact {
    pub manifest: ArtifactManifest,
    pub artifact_id: String,
}

impl SealedArtifact {
    /// Canonical manifest bytes — what publication writes last and what a
    /// reader hashes to check the id.
    pub fn manifest_bytes(&self) -> Result<Vec<u8>, Error> {
        canonical_bytes(&self.manifest)
    }

    /// Write the manifest, completing publication.
    pub fn publish_manifest(&self, store: &dyn ArtifactStore) -> Result<(), Error> {
        store.put_component(MANIFEST, &self.manifest_bytes()?)
    }
}

/// An opened, verified artifact.
///
/// `Debug` deliberately omits the records and the vectors: this type holds
/// corpus TEXT, and a debug-printed shard in a log or a test failure would spill
/// document content. The identity and the shape are what a reader needs.
pub struct OpenShard {
    pub artifact_id: String,
    pub manifest: ArtifactManifest,
    records: Vec<ChunkRecord>,
    /// `chunk_id` → position in `records`. `record` sits on the per-query
    /// path (once per fused hit, and once per candidate when demotions are
    /// on), and a linear scan over a 100k-chunk shard there was millions of
    /// string compares per query per shard.
    record_index: std::collections::HashMap<String, usize>,
    vectors: Option<Box<dyn VectorIndex + Send + Sync>>,
    #[cfg(feature = "lexical-tantivy")]
    lexical: Option<crate::lexical::TantivyLexicalIndex>,
}

impl std::fmt::Debug for OpenShard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenShard")
            .field("artifact_id", &self.artifact_id)
            .field("chunks", &self.records.len())
            .field("vectors", &self.vectors.as_ref().map(|v| v.len()))
            .field("lexical", &self.has_lexical())
            .finish_non_exhaustive()
    }
}

impl OpenShard {
    /// Open an artifact, verifying everything before trusting anything.
    ///
    /// Order matters and is the security property: the manifest BYTES are
    /// hashed and checked against the expected id first, so a substituted
    /// manifest is caught before its own contents are used to decide what to
    /// read. Only then is it parsed, validated, and its components verified.
    pub fn open(
        store: &dyn ArtifactStore,
        expected_artifact_id: &str,
        reader: &ReaderCapabilities,
        limits: &Limits,
    ) -> Result<Self, Error> {
        let bytes = store.get_component(MANIFEST, None)?;
        crate::verify::verify_manifest_bytes(&bytes, expected_artifact_id)?;

        let manifest: ArtifactManifest = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Invalid(format!("manifest does not parse: {e}")))?;
        validate_manifest(&manifest, reader, limits)?;

        let mut records = Vec::new();
        let mut vectors = None;
        let mut body = None;
        let mut index = None;
        #[cfg(feature = "lexical-tantivy")]
        let mut lexical = None;

        for c in &manifest.components {
            // An optional component that is absent is fine; a REQUIRED one that
            // is absent makes the artifact unusable, and both are decided from
            // the manifest rather than from what happens to be on disk.
            if !store.exists(&c.path)? {
                if c.required {
                    return Err(Error::Integrity(format!(
                        "required component {:?} is missing",
                        c.path
                    )));
                }
                continue;
            }
            let got = store.get_component(&c.path, None)?;
            verify_component(c, &got)?;
            match c.path.as_str() {
                RECORDS_BODY => body = Some(got),
                RECORDS_INDEX => index = Some(got),
                VECTOR_DATA => {
                    vectors = Some(Box::new(FlatVectorIndex::from_bytes(&got)?)
                        as Box<dyn VectorIndex + Send + Sync>)
                }
                VECTOR_DISKANN_DATA => {
                    #[cfg(feature = "vector-diskann")]
                    {
                        vectors = Some(Box::new(
                            crate::vector_diskann::DiskAnnVectorIndex::from_bytes(&got)?,
                        )
                            as Box<dyn VectorIndex + Send + Sync>);
                    }
                    // Same rule as the lexical arm: refuse rather than serve
                    // the artifact vector-blind. In practice the envelope
                    // feature bit already refused at verify; this is the
                    // second lock on the same door.
                    #[cfg(not(feature = "vector-diskann"))]
                    {
                        return Err(Error::Unsupported(
                            "artifact carries a diskann vector index but this build has no \
                             approximate vector engine compiled in"
                                .into(),
                        ));
                    }
                }
                LEXICAL_INDEX => {
                    #[cfg(feature = "lexical-tantivy")]
                    {
                        lexical = Some(crate::lexical::TantivyLexicalIndex::open(&got)?);
                    }
                    // A reader without the engine must REFUSE rather than serve
                    // the artifact lexically-blind: half an index answering
                    // half a query is worse than an honest refusal.
                    #[cfg(not(feature = "lexical-tantivy"))]
                    {
                        return Err(Error::Unsupported(
                            "artifact carries a lexical index but this build has no lexical engine compiled in"
                                .into(),
                        ));
                    }
                }
                _ => {}
            }
        }

        if let (Some(b), Some(i)) = (body, index) {
            records = read_records(&b, &i)?;
        }
        if records.len() as u64 != manifest.counts.chunks {
            return Err(Error::Integrity(format!(
                "manifest declares {} chunks, records hold {}",
                manifest.counts.chunks,
                records.len()
            )));
        }
        // The vector leg is cross-checked against `counts` the way the
        // records are: a component that verified byte-for-byte can still be
        // the wrong component if the manifest's counts say something else,
        // and a leg holding fewer vectors than declared would answer vector
        // queries over a silently partial index.
        if let Some(ix) = vectors.as_ref() {
            if let Some(declared) = manifest.counts.vectors {
                if ix.len() as u64 != declared {
                    return Err(Error::Integrity(format!(
                        "manifest declares {declared} vectors, the vector index holds {}",
                        ix.len()
                    )));
                }
            }
            if let Some(dims) = manifest.counts.dimensions {
                if ix.dimensions() as u64 != dims as u64 {
                    return Err(Error::Integrity(format!(
                        "manifest declares {dims} dimensions, the vector index holds {}",
                        ix.dimensions()
                    )));
                }
            }
        }

        let record_index = records
            .iter()
            .enumerate()
            .map(|(i, r)| (r.chunk_id.clone(), i))
            .collect();
        let shard = Self {
            artifact_id: expected_artifact_id.to_string(),
            manifest,
            records,
            record_index,
            vectors,
            #[cfg(feature = "lexical-tantivy")]
            lexical,
        };
        shard.run_probes()?;
        Ok(shard)
    }

    /// Run the manifest's probes. Checksums show the bytes arrived; probes show
    /// the index answers.
    fn run_probes(&self) -> Result<(), Error> {
        for probe in &self.manifest.probes {
            match probe.kind {
                ProbeKind::Record => {
                    for want in &probe.expect.chunk_ids {
                        if self.record(want).is_none() {
                            return Err(Error::Integrity(format!(
                                "probe {:?} expects chunk {want:?}, which the artifact does not hold",
                                probe.id
                            )));
                        }
                    }
                }
                // Lexical and vector probes arrive with those adapters; a probe
                // kind this reader cannot run is a refusal, never a silent skip
                // -- a probe that quietly does nothing is worse than no probe,
                // because it reports success.
                ProbeKind::Lexical | ProbeKind::Vector => {
                    return Err(Error::Unsupported(format!(
                        "probe {:?} is of kind {:?}, which this reader cannot execute",
                        probe.id, probe.kind
                    )))
                }
            }
        }
        Ok(())
    }

    /// Whether this shard can answer a lexical query.
    pub fn has_lexical(&self) -> bool {
        #[cfg(feature = "lexical-tantivy")]
        {
            self.lexical.is_some()
        }
        #[cfg(not(feature = "lexical-tantivy"))]
        {
            false
        }
    }

    /// Lexical candidates for a plan, with content demotions applied.
    ///
    /// Demotion happens HERE rather than inside the index, because the marker
    /// is tested against the record text and the lexical index deliberately
    /// does not store it -- keeping the text in one place halves the artifact
    /// and leaves one copy of the truth.
    #[cfg(feature = "lexical-tantivy")]
    pub fn lexical_candidates(
        &self,
        plan: &crate::lexical::LexicalPlan,
        limit: usize,
    ) -> Result<Vec<Candidate>, Error> {
        use crate::lexical::LexicalIndex as _;
        let Some(ix) = self.lexical.as_ref() else {
            return Err(Error::Unsupported(
                "this artifact carries no lexical index".into(),
            ));
        };
        // Over-fetch when demotions are in play: a demoted hit that would
        // have made the top-k must be able to fall out of it, and one just
        // below the cut must be able to rise into it. Fetching exactly
        // `limit` and demoting afterwards can only reorder WITHIN the top-k,
        // which is a no-op on the boundary demotion exists to move — and the
        // PostgreSQL path demotes over its whole candidate pool, so anything
        // less here is recorded by the shadow comparison as an engine
        // difference that is really a truncation.
        let fetch = if plan.demotions.is_empty() {
            limit
        } else {
            limit.saturating_mul(4).max(10)
        };
        let mut candidates = ix.lexical_candidates(plan, fetch)?;
        if !plan.demotions.is_empty() {
            crate::lexical::apply_demotions(&mut candidates, plan, &|id| {
                self.record(id).map(|r| r.text.clone())
            });
            // Scores changed; the engine's order no longer holds. Same
            // deterministic order the engine uses: score descending, then
            // chunk id.
            candidates.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.chunk_id.cmp(&b.chunk_id))
            });
        }
        candidates.truncate(limit);
        Ok(candidates)
    }

    /// Analyze text through this artifact's OWN lexical analyzer.
    ///
    /// Exposed because a query must be tokenized by the analyzer that built
    /// the index it searches — feeding another engine's lexemes to Tantivy
    /// would measure the mismatch of the two tokenizers, not the engines. The
    /// shadow candidate path uses this to derive its plan terms from the same
    /// query STRING the reference searched.
    #[cfg(feature = "lexical-tantivy")]
    pub fn analyze(&self, text: &str) -> Result<Vec<String>, Error> {
        let Some(ix) = self.lexical.as_ref() else {
            return Err(Error::Unsupported(
                "this artifact carries no lexical index to analyze with".into(),
            ));
        };
        ix.analyze(text)
    }

    pub fn record(&self, chunk_id: &str) -> Option<&ChunkRecord> {
        self.record_index
            .get(chunk_id)
            .and_then(|&i| self.records.get(i))
    }

    pub fn records(&self) -> &[ChunkRecord] {
        &self.records
    }

    /// Vector candidates, or an empty list when the artifact has no vector leg.
    ///
    /// Empty rather than an error: a lexical-only artifact is valid, and a
    /// caller asking both legs of a hybrid query should get the legs that
    /// exist rather than a failure.
    pub fn vector_candidates(
        &self,
        embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<Candidate>, Error> {
        match &self.vectors {
            None => Ok(Vec::new()),
            Some(ix) => {
                // Stored vectors are refused non-finite at `push`; the query
                // gets the same rule here, once, whatever engine answers it.
                // The flat scan would otherwise return every distance as NaN
                // and fall through to chunk-id order, and DiskANN would map
                // NaN to 1.0 and traverse arbitrarily — both "results" for a
                // query that means nothing.
                if embedding.iter().any(|v| !v.is_finite()) {
                    return Err(Error::Invalid(
                        "query embedding holds a non-finite value".into(),
                    ));
                }
                ix.vector_candidates(embedding, limit)
            }
        }
    }

    /// Hybrid search over this shard.
    ///
    /// A convenience over the separate legs, not a place fusion hides: the legs
    /// are produced independently and fused by the engine-neutral code, and a
    /// caller that wants per-leg diagnostics can call the legs directly.
    pub fn hybrid_search(
        &self,
        lexical: &[Candidate],
        embedding: Option<&[f32]>,
        weights: &FusionWeights,
        top_k: usize,
    ) -> Result<Vec<FusedHit>, Error> {
        let vector = match embedding {
            Some(e) => self.vector_candidates(e, top_k.max(1) * 4)?,
            None => Vec::new(),
        };
        let mut hits = fuse(lexical, &vector, weights);
        hits.truncate(top_k);
        Ok(hits)
    }
}
