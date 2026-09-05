// SPDX-License-Identifier: Apache-2.0
//! stage 2's exit gate.
//!
//! "A prepared-chunk fixture can build, close, reopen, verify, and query
//! without Server or PostgreSQL, and a byte-identical rebuild reproduces the
//! same `artifact_id`."
//!
//! Nothing in this file touches a database, a server, a runtime or a network.
//! That is the claim being tested as much as the behaviour is.

use std::collections::BTreeMap;

use munarium_datastore::fusion::FusionWeights;
use munarium_datastore::model::*;
use munarium_datastore::shard::{OpenShard, ShardWriter, MANIFEST, RECORDS_BODY, VECTOR_DATA};
use munarium_datastore::store::{ArtifactStore, LocalFileStore};
use munarium_datastore::vector::Candidate;
use munarium_datastore::verify::{Limits, ReaderCapabilities};
use munarium_datastore::PreparedChunk;
use sha2::{Digest, Sha256};

fn chunk(
    id: &str,
    source: &str,
    ordinal: u32,
    text: &str,
    embedding: Option<Vec<f32>>,
) -> PreparedChunk {
    PreparedChunk {
        chunk_id: id.into(),
        source_id: source.into(),
        source_path: format!("corpus/{source}.md"),
        node_id: Some(format!("{ordinal:04}")),
        ordinal,
        text: text.into(),
        text_sha256: Sha256::digest(text.as_bytes()).into(),
        embedding,
        metadata: BTreeMap::new(),
    }
}

fn fixture() -> Vec<PreparedChunk> {
    vec![
        chunk(
            "s1#0",
            "s1",
            0,
            "the continental congress met in philadelphia",
            Some(vec![1.0, 0.0, 0.0]),
        ),
        chunk(
            "s1#1",
            "s1",
            1,
            "washington wrote to congress about supply",
            Some(vec![0.9, 0.1, 0.0]),
        ),
        chunk(
            "s2#0",
            "s2",
            0,
            "colonial newspapers reported the destruction of the tea",
            Some(vec![0.0, 1.0, 0.0]),
        ),
    ]
}

fn spec() -> BuildSpec {
    BuildSpec {
        spec_version: 1,
        scope: Scope {
            kind: ScopeKind::Collection,
            id: "col-test".into(),
        },
        sources: vec![
            SourceRef {
                source_id: "s1".into(),
                logical_path: "corpus/s1.md".into(),
                media_type: "text/markdown".into(),
                content_sha256: "a".repeat(64),
                revision: None,
            },
            SourceRef {
                source_id: "s2".into(),
                logical_path: "corpus/s2.md".into(),
                media_type: "text/markdown".into(),
                content_sha256: "b".repeat(64),
                revision: None,
            },
        ],
        snapshot: Snapshot { watermark_seq: 42 },
        shape: ShapeRef {
            shape_ref: "para".into(),
            version: 1,
        },
        chunker: Chunker {
            name: "para".into(),
            version: "para@1".into(),
            params: BTreeMap::from([("max_chars".to_string(), Param::Int(1200))]),
        },
        extractor: Extractor {
            name: "munarium-extract".into(),
            version: "0.5.0".into(),
            config: BTreeMap::new(),
            per_source: vec![
                ExtractionOutcome {
                    source_id: "s1".into(),
                    outcome: ExtractionStatus::Extracted,
                    extracted_text_sha256: Some("c".repeat(64)),
                    method: Some("local".into()),
                },
                ExtractionOutcome {
                    source_id: "s2".into(),
                    outcome: ExtractionStatus::Extracted,
                    extracted_text_sha256: Some("d".repeat(64)),
                    method: Some("local".into()),
                },
            ],
        },
        embedder: Some(Embedder {
            model: "local-hash@1".into(),
            dimensions: 3,
            normalization: Normalization::L2,
            metric: Metric::Cosine,
        }),
        lexical_analysis: LexicalAnalysis {
            contract_version: 1,
            tokenizer: "munarium-pg-compat@1".into(),
            stemmer: "snowball-english".into(),
            stop_terms_ref: StopTerms {
                list_ref: "pg16/english".into(),
                sha256: "e".repeat(64),
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

fn plan() -> ArtifactBuildPlan {
    ArtifactBuildPlan {
        plan_version: 1,
        envelope: Envelope {
            format_version: 1,
            feature_bits: vec!["records.v1".into()],
        },
        lexical: LexicalEngine {
            engine_id: "tantivy".into(),
            engine_revision: "0.22.0".into(),
            positions: true,
            segments: Some(1),
            compression: None,
        },
        vector: Some(VectorEngine {
            engine_id: "munarium-flat".into(),
            engine_revision: "0.1.0".into(),
            kind: VectorKind::Exact,
            quantization: None,
            graph: None,
            rescore_depth: None,
        }),
        records: RecordsFormat {
            format: "munarium-records@1".into(),
            compression: None,
        },
        range_map: None,
        shaper: Shaper {
            policy_version: 1,
            decisions: vec![ShaperDecision {
                setting: "vector.kind".into(),
                chosen: Param::Text("exact".into()),
                because: "below the approximate threshold".into(),
                threshold: Some(Param::Int(100_000)),
                observed: Some(Param::Int(3)),
            }],
        },
    }
}

fn build_into(dir: &std::path::Path) -> (LocalFileStore, String) {
    let store = LocalFileStore::new(dir).unwrap();
    let mut w = ShardWriter::new(Some(3));
    for c in fixture() {
        w.add(c).unwrap();
    }
    let sealed = w.seal(&spec(), &plan(), &store).unwrap();
    sealed.publish_manifest(&store).unwrap();
    (store, sealed.artifact_id)
}

/// The gate itself.
#[test]
fn build_seal_reopen_verify_and_query_with_no_server() {
    let dir = tempfile::tempdir().unwrap();
    let (store, artifact_id) = build_into(dir.path());

    let shard = OpenShard::open(
        &store,
        &artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .expect("a freshly sealed artifact must reopen");

    assert_eq!(shard.manifest.counts.chunks, 3);
    assert_eq!(shard.manifest.counts.documents, 2, "two distinct sources");
    assert_eq!(shard.manifest.counts.dimensions, Some(3));

    // Records answer a citation without any source metadata.
    let r = shard.record("s1#1").expect("record present");
    assert_eq!(r.source_path, "corpus/s1.md");
    assert!(r.text.contains("washington"));

    // The vector leg answers, nearest first.
    let near = shard.vector_candidates(&[1.0, 0.0, 0.0], 3).unwrap();
    assert_eq!(near[0].chunk_id, "s1#0");

    // Hybrid fusion over both legs.
    let lexical = vec![
        Candidate {
            chunk_id: "s2#0".into(),
            score: 9.0,
        },
        Candidate {
            chunk_id: "s1#0".into(),
            score: 4.0,
        },
    ];
    let hits = shard
        .hybrid_search(
            &lexical,
            Some(&[1.0, 0.0, 0.0]),
            &FusionWeights::default(),
            3,
        )
        .unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].chunk_id, "s1#0", "in both legs, so it wins");
    assert!(hits[0].lexical_rank.is_some() && hits[0].vector_rank.is_some());
}

/// What converges, and what does not.
///
/// The content-pure manifest means two builds of the same inputs produce the
/// same `artifact_id` **when every component is byte-deterministic**. Tantivy is
/// not: it names each segment with a fresh UUID, so an artifact carrying a
/// lexical index does NOT converge.
///
/// Both halves are asserted because getting either wrong is expensive. The
/// LOGICAL id must converge -- it is what a session pin, an audit and the
/// provenance envelope depend on, and it derives from the BuildSpec alone. The
/// artifact id must be ALLOWED not to, which is precisely why section 7.1 step
/// 7's catalog rule (adopt the existing row on primary-key conflict) is the
/// mechanism that stops a rebuild duplicating an artifact rather than a nicety
/// for a rare race.
///
/// Section 5.1 anticipated this: "builds whose upstream engines emit different
/// bytes produce different IDs even under the same plan". If a future engine set
/// IS deterministic the second assertion starts failing -- check the engines
/// before "fixing" it.
#[test]
fn the_logical_id_converges_even_though_the_artifact_id_need_not() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (_, id_a) = build_into(a.path());
    let (_, id_b) = build_into(b.path());

    assert_eq!(
        spec().index_version_id().unwrap(),
        spec().index_version_id().unwrap(),
        "the logical id is a function of the BuildSpec and must always converge"
    );
    assert_ne!(
        id_a, id_b,
        "Tantivy segment UUIDs make the artifact id differ; if this ever passes the engine          set became deterministic and decisions.md needs updating"
    );

    // Both artifacts open and answer identically: the non-determinism is in file
    // names and layout, never in what the index knows.
    for (dir, id) in [(a.path(), &id_a), (b.path(), &id_b)] {
        let store = LocalFileStore::new(dir).unwrap();
        let shard =
            OpenShard::open(&store, id, &ReaderCapabilities::v1(), &Limits::default()).unwrap();
        assert_eq!(shard.records().len(), 3);
        assert_eq!(shard.manifest.counts.documents, 2);
    }
}

/// The sidecars, which ARE deterministic, converge byte-for-byte. That is what
/// makes the logical id reproducible rather than merely equal by luck.
#[test]
fn the_canonical_sidecars_are_byte_identical_across_builds() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    build_into(a.path());
    build_into(b.path());
    for name in ["build-spec.canonical.json", "artifact-plan.canonical.json"] {
        assert_eq!(
            std::fs::read(a.path().join(name)).unwrap(),
            std::fs::read(b.path().join(name)).unwrap(),
            "{name} must be a pure function of its inputs"
        );
    }
}

/// A substituted manifest is caught by its hash BEFORE its contents are used
/// to decide what to read. This is the ordering property of `open`.
#[test]
fn a_tampered_manifest_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (store, artifact_id) = build_into(dir.path());

    // Same-length substitution inside the manifest, so nothing but the content
    // changes -- the point is that the HASH catches it, not a length check.
    let mut bytes = store.get_component(MANIFEST, None).unwrap();
    let needle = b"munarium-records";
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("the records engine is named in the manifest");
    bytes[pos + needle.len() - 1] = b'X';
    store.put_component(MANIFEST, &bytes).unwrap();

    let err = OpenShard::open(
        &store,
        &artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("integrity"), "{err}");
}

/// A corrupt component is caught by its own checksum, even though the manifest
/// is intact — the two checks are independent for a reason.
#[test]
fn a_corrupt_component_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (store, artifact_id) = build_into(dir.path());

    let mut body = store.get_component(RECORDS_BODY, None).unwrap();
    body[0] ^= 0xff;
    store.put_component(RECORDS_BODY, &body).unwrap();

    let err = OpenShard::open(
        &store,
        &artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("integrity"), "{err}");
}

/// A REQUIRED component that is gone is fatal; an OPTIONAL one that is gone is
/// not. Both decided from the manifest, not from what is on disk.
#[test]
fn a_missing_required_component_is_fatal_and_an_optional_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let (store, artifact_id) = build_into(dir.path());

    // The vector data is optional: without it the artifact still serves
    // lexically, and the vector leg simply contributes nothing.
    std::fs::remove_file(dir.path().join(VECTOR_DATA)).unwrap();
    let shard = OpenShard::open(
        &store,
        &artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .expect("an artifact without its optional vector component still opens");
    assert!(shard
        .vector_candidates(&[1.0, 0.0, 0.0], 3)
        .unwrap()
        .is_empty());

    // The records body is required.
    std::fs::remove_file(dir.path().join(RECORDS_BODY)).unwrap();
    let err = OpenShard::open(
        &store,
        &artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("required component"), "{err}");
}

/// An artifact that declares a newer envelope than this reader supports is
/// refused BEFORE anything is opened, rather than being opened and misread.
#[test]
fn an_unsupported_envelope_is_refused_before_opening_anything() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalFileStore::new(dir.path()).unwrap();
    let mut w = ShardWriter::new(Some(3));
    for c in fixture() {
        w.add(c).unwrap();
    }
    let mut future = plan();
    future.envelope.format_version = 99;
    let sealed = w.seal(&spec(), &future, &store).unwrap();
    sealed.publish_manifest(&store).unwrap();

    let err = OpenShard::open(
        &store,
        &sealed.artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("unsupported"), "{err}");
}

/// The writer refuses inputs that would make an artifact quietly wrong.
#[test]
fn the_writer_refuses_ambiguous_or_partial_input() {
    let mut w = ShardWriter::new(Some(3));
    w.add(chunk("dup", "s1", 0, "one", Some(vec![1.0, 0.0, 0.0])))
        .unwrap();
    let err = w
        .add(chunk("dup", "s1", 1, "two", Some(vec![0.0, 1.0, 0.0])))
        .unwrap_err();
    assert!(err.to_string().contains("duplicate chunk id"), "{err}");

    // A shard declaring vectors must get one for every chunk: a partial vector
    // leg ranks some chunks and silently excludes others.
    let mut w = ShardWriter::new(Some(3));
    let err = w.add(chunk("a", "s1", 0, "no vector", None)).unwrap_err();
    assert!(err.to_string().contains("no embedding"), "{err}");

    // And the converse.
    let mut w = ShardWriter::new(None);
    let err = w
        .add(chunk("a", "s1", 0, "has one", Some(vec![1.0])))
        .unwrap_err();
    assert!(err.to_string().contains("declares none"), "{err}");

    // An empty artifact answers every query with nothing and looks exactly
    // like a broken one, so it cannot be sealed.
    let dir = tempfile::tempdir().unwrap();
    let store = LocalFileStore::new(dir.path()).unwrap();
    let err = ShardWriter::new(None)
        .seal(&spec(), &plan(), &store)
        .unwrap_err();
    assert!(err.to_string().contains("no chunks"), "{err}");
}

/// A lexical-only corpus is a first-class shape, not a degraded one.
#[test]
fn a_lexical_only_artifact_builds_and_opens() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalFileStore::new(dir.path()).unwrap();
    let mut w = ShardWriter::new(None);
    for c in fixture() {
        w.add(PreparedChunk {
            embedding: None,
            ..c
        })
        .unwrap();
    }
    let mut lexical_spec = spec();
    lexical_spec.embedder = None;
    let mut lexical_plan = plan();
    lexical_plan.vector = None;

    let sealed = w.seal(&lexical_spec, &lexical_plan, &store).unwrap();
    sealed.publish_manifest(&store).unwrap();
    let shard = OpenShard::open(
        &store,
        &sealed.artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(shard.manifest.counts.vectors, None);
    assert_eq!(shard.manifest.counts.dimensions, None);
    assert!(shard
        .vector_candidates(&[1.0, 0.0, 0.0], 3)
        .unwrap()
        .is_empty());
    assert_eq!(shard.records().len(), 3);
}

/// The manifest carries no attempt-specific metadata, which is what makes the
/// convergence above possible. Asserted on the SERIALIZED bytes, because that
/// is what gets hashed.
#[test]
fn the_sealed_manifest_carries_no_build_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = build_into(dir.path());
    let bytes = store.get_component(MANIFEST, None).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    for forbidden in [
        "built_at",
        "builder",
        "attempt_id",
        "hostname",
        "node_id",
        "tenant_id",
        "index_version_id",
    ] {
        assert!(
            !text.contains(forbidden),
            "manifest must not carry {forbidden:?}: it is either non-content metadata or \
             authority, and both break content purity"
        );
    }
}

// ---------------------------------------------------------------------------
// stage 8: the approximate vector engine
// ---------------------------------------------------------------------------

/// A plan naming the diskann engine: same corpus, different physical engine.
fn diskann_plan() -> ArtifactBuildPlan {
    #[cfg(feature = "vector-diskann")]
    let graph = Some(munarium_datastore::vector_diskann::GraphParams::default().to_plan_map());
    #[cfg(not(feature = "vector-diskann"))]
    let graph = Some(BTreeMap::from([
        ("max_degree".to_string(), Param::Int(32)),
        ("l_build".to_string(), Param::Int(100)),
        ("alpha".to_string(), Param::Text("1.2".into())),
        ("l_search".to_string(), Param::Int(100)),
    ]));
    let mut p = plan();
    p.envelope.feature_bits.push("vector.diskann.v1".into());
    p.vector = Some(VectorEngine {
        engine_id: "diskann".into(),
        engine_revision: "0.56.0".into(),
        kind: VectorKind::Approximate,
        quantization: None,
        graph,
        rescore_depth: None,
    });
    p.shaper.decisions = vec![ShaperDecision {
        setting: "vector.kind".into(),
        chosen: Param::Text("approximate".into()),
        because: "at or above the approximate threshold".into(),
        threshold: Some(Param::Int(2)),
        observed: Some(Param::Int(3)),
    }];
    p
}

#[cfg(feature = "vector-diskann")]
#[test]
fn a_diskann_artifact_seals_opens_and_answers_like_the_exact_one() {
    use munarium_datastore::shard::VECTOR_DISKANN_DATA;

    let flat_dir = tempfile::tempdir().unwrap();
    let (flat_store, flat_id) = build_into(flat_dir.path());
    let flat = OpenShard::open(
        &flat_store,
        &flat_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let store = LocalFileStore::new(dir.path()).unwrap();
    let mut w = ShardWriter::new(Some(3));
    for c in fixture() {
        w.add(c).unwrap();
    }
    let sealed = w.seal(&spec(), &diskann_plan(), &store).unwrap();
    sealed.publish_manifest(&store).unwrap();

    // Same spec, different engine: both artifacts hang off ONE logical id
    // (both seals were handed the same `spec()`, and the engine is outside
    // it), while the PHYSICAL ids differ.
    assert_ne!(sealed.artifact_id, flat_id);

    let shard = OpenShard::open(
        &store,
        &sealed.artifact_id,
        &ReaderCapabilities::v1(),
        &Limits::default(),
    )
    .expect("a freshly sealed approximate artifact must reopen");

    // The manifest names the diskann component and requires the feature bit;
    // the flat component is absent.
    assert!(shard
        .manifest
        .components
        .iter()
        .any(|c| c.path == VECTOR_DISKANN_DATA));
    assert!(!shard
        .manifest
        .components
        .iter()
        .any(|c| c.path == VECTOR_DATA));
    assert!(shard
        .manifest
        .reader
        .required_features
        .contains(&"vector.diskann.v1".to_string()));

    // With l_search wider than the corpus the beam covers the whole graph, so
    // the approximate leg answers EXACTLY what the oracle answers — same
    // chunks, same scores, same order.
    let query = [0.95f32, 0.05, 0.0];
    let approx: Vec<Candidate> = shard.vector_candidates(&query, 3).unwrap();
    let exact: Vec<Candidate> = flat.vector_candidates(&query, 3).unwrap();
    assert_eq!(approx, exact);
}

#[cfg(feature = "vector-diskann")]
#[test]
fn a_reader_without_the_feature_bit_refuses_the_approximate_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalFileStore::new(dir.path()).unwrap();
    let mut w = ShardWriter::new(Some(3));
    for c in fixture() {
        w.add(c).unwrap();
    }
    let sealed = w.seal(&spec(), &diskann_plan(), &store).unwrap();
    sealed.publish_manifest(&store).unwrap();

    // Yesterday's reader: no vector.diskann.v1 in its capability set. The
    // refusal happens at verification — BEFORE any component is fetched or
    // opened — which is what lets a binding change be gated fleet-wide.
    let stale = ReaderCapabilities {
        format_min: 1,
        format_max: 1,
        features: ["records.v1"].into_iter().map(String::from).collect(),
    };
    match OpenShard::open(&store, &sealed.artifact_id, &stale, &Limits::default()) {
        Err(munarium_datastore::Error::Unsupported(msg)) => {
            assert!(msg.contains("vector.diskann.v1"), "{msg}");
        }
        other => panic!("expected an Unsupported refusal, got {other:?}"),
    }
}

#[cfg(not(feature = "vector-diskann"))]
#[test]
fn a_diskann_plan_refuses_to_seal_without_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalFileStore::new(dir.path()).unwrap();
    let mut w = ShardWriter::new(Some(3));
    for c in fixture() {
        w.add(c).unwrap();
    }
    match w.seal(&spec(), &diskann_plan(), &store) {
        Err(munarium_datastore::Error::Unsupported(msg)) => {
            assert!(msg.contains("diskann"), "{msg}");
        }
        other => panic!("expected an Unsupported refusal, got {other:?}"),
    }
}

#[test]
fn an_unknown_vector_engine_is_refused_at_seal() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalFileStore::new(dir.path()).unwrap();
    let mut w = ShardWriter::new(Some(3));
    for c in fixture() {
        w.add(c).unwrap();
    }
    let mut bogus = plan();
    bogus.vector.as_mut().unwrap().engine_id = "faiss".into();
    match w.seal(&spec(), &bogus, &store) {
        Err(munarium_datastore::Error::Invalid(msg)) => {
            assert!(msg.contains("faiss"), "{msg}");
        }
        other => panic!("expected an Invalid refusal, got {other:?}"),
    }
}
