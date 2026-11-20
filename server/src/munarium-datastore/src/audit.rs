// SPDX-License-Identifier: Apache-2.0
//! An authoritative digest audit of one stored artifact (ClearView ART1/ART2).
//!
//! Hydration and open stop at the first integrity failure and report it as a
//! message, which is right for serving and wrong for evidence. A durable
//! damage finding has to name the pinned object, the digest the trusted pin
//! expects and the digest actually observed, so two independent verifiers can
//! be compared on the same object. [`audit_artifact`] produces exactly that
//! and nothing more: it fetches directly from the store it is given, never
//! from a local cache, writes nothing and decides nothing.
//!
//! The trusted pin is the artifact id. The manifest bytes are hashed first;
//! only a manifest that hashes to the artifact id is parsed, so the component
//! digests the audit compares against are the pin's own, never a substituted
//! manifest's. An absent object or a failed read is [`AuditOutcome::Unavailable`],
//! not a mismatch: no byte was observed to disagree with the pin, and a
//! timeout must never count as damage. The one exception is a component the
//! manifest marks optional (`required: false`): when the store lists it as
//! absent, the audit skips it exactly as hydration does, so the artifact
//! hydration serves is the artifact the audit can find complete.

use sha2::{Digest, Sha256};

use crate::model::ArtifactManifest;
use crate::shard::MANIFEST;
use crate::store::ArtifactStore;
use crate::verify::{validate_manifest, Limits, ReaderCapabilities};
use crate::Error;

/// What one object's authoritative fetch showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    /// The observed bytes have the pinned length and digest.
    Match,
    /// Bytes were read and their length or digest differs from the pin.
    Mismatch,
    /// The object could not be read. Never evidence of damage.
    Unavailable,
}

/// One pinned object's comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectAudit {
    /// The artifact-relative object key: `manifest.json` or a component path.
    pub object_key: String,
    /// The digest the trusted pin expects (the artifact id for the manifest).
    pub expected_sha256: String,
    /// The declared length, when the pin declares one (components only).
    pub expected_bytes: Option<u64>,
    /// The digest of the bytes actually fetched, when any were.
    pub observed_sha256: Option<String>,
    pub observed_bytes: Option<u64>,
    /// The store's generation of the object the observed bytes came from,
    /// when the store reports one (an ETag or version id). `None` when it
    /// reports none or nothing was read. Two audits that saw different
    /// generations observed two different stored objects.
    pub storage_generation: Option<String>,
    pub outcome: AuditOutcome,
}

/// The audit of one artifact: the manifest first, then each component in
/// manifest order. A manifest mismatch ends the audit, because nothing it
/// declares can be trusted as the pin's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactAudit {
    pub artifact_id: String,
    pub objects: Vec<ObjectAudit>,
}

impl ArtifactAudit {
    /// The first mismatching object in canonical (manifest) order. Two
    /// verifiers that see the same damage therefore name the same object.
    pub fn first_mismatch(&self) -> Option<&ObjectAudit> {
        self.objects
            .iter()
            .find(|o| o.outcome == AuditOutcome::Mismatch)
    }

    /// The first object that could not be read.
    pub fn first_unavailable(&self) -> Option<&ObjectAudit> {
        self.objects
            .iter()
            .find(|o| o.outcome == AuditOutcome::Unavailable)
    }

    /// True only when every pinned object was read and matched.
    pub fn all_match(&self) -> bool {
        !self.objects.is_empty()
            && self
                .objects
                .iter()
                .all(|o| o.outcome == AuditOutcome::Match)
    }

    /// One object's comparison, by key.
    pub fn object(&self, key: &str) -> Option<&ObjectAudit> {
        self.objects.iter().find(|o| o.object_key == key)
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Fetch every pinned object of `expected_artifact_id` directly from `store`
/// and compare each with the pin.
///
/// Returns `Err` only when the pin itself cannot be interpreted: a manifest
/// that hashes to the artifact id but does not parse or validate for this
/// reader. Damage is reported in the returned audit, never as an error.
pub fn audit_artifact(
    store: &dyn ArtifactStore,
    expected_artifact_id: &str,
    reader: &ReaderCapabilities,
    limits: &Limits,
) -> Result<ArtifactAudit, Error> {
    let mut objects = Vec::new();
    let (manifest_bytes, manifest_generation) = match store.get_component_with_generation(MANIFEST)
    {
        Ok(read) => (read.bytes, read.generation),
        Err(_) => {
            objects.push(ObjectAudit {
                object_key: MANIFEST.to_string(),
                expected_sha256: expected_artifact_id.to_string(),
                expected_bytes: None,
                observed_sha256: None,
                observed_bytes: None,
                storage_generation: None,
                outcome: AuditOutcome::Unavailable,
            });
            return Ok(ArtifactAudit {
                artifact_id: expected_artifact_id.to_string(),
                objects,
            });
        }
    };
    let observed = digest(&manifest_bytes);
    let manifest_matches = observed == expected_artifact_id;
    objects.push(ObjectAudit {
        object_key: MANIFEST.to_string(),
        expected_sha256: expected_artifact_id.to_string(),
        expected_bytes: None,
        observed_sha256: Some(observed),
        observed_bytes: Some(manifest_bytes.len() as u64),
        storage_generation: manifest_generation,
        outcome: if manifest_matches {
            AuditOutcome::Match
        } else {
            AuditOutcome::Mismatch
        },
    });
    if !manifest_matches {
        return Ok(ArtifactAudit {
            artifact_id: expected_artifact_id.to_string(),
            objects,
        });
    }

    let manifest: ArtifactManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| Error::Invalid(format!("pinned manifest does not parse: {e}")))?;
    validate_manifest(&manifest, reader, limits)?;

    for c in &manifest.components {
        let fetched = match store.exists(&c.path) {
            Ok(true) => store.get_component_with_generation(&c.path).ok(),
            // An absent optional component is skipped, as hydration and open
            // skip it: the pin does not require it, so its absence neither
            // makes the artifact unusable nor leaves the audit incomplete. A
            // failed listing is still unavailable.
            Ok(false) if !c.required => continue,
            _ => None,
        };
        let (observed_sha256, observed_bytes, storage_generation, outcome) = match fetched {
            None => (None, None, None, AuditOutcome::Unavailable),
            Some(read) => {
                let bytes = read.bytes;
                let d = digest(&bytes);
                let ok = bytes.len() as u64 == c.bytes_len && d == c.sha256;
                (
                    Some(d),
                    Some(bytes.len() as u64),
                    read.generation,
                    if ok {
                        AuditOutcome::Match
                    } else {
                        AuditOutcome::Mismatch
                    },
                )
            }
        };
        objects.push(ObjectAudit {
            object_key: c.path.clone(),
            expected_sha256: c.sha256.clone(),
            expected_bytes: Some(c.bytes_len),
            observed_sha256,
            observed_bytes,
            storage_generation,
            outcome,
        });
    }
    Ok(ArtifactAudit {
        artifact_id: expected_artifact_id.to_string(),
        objects,
    })
}

#[cfg(all(test, feature = "lexical-tantivy"))]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::shard::ShardWriter;
    use crate::store::LocalFileStore;
    use crate::PreparedChunk;
    use std::collections::BTreeMap;

    fn spec() -> BuildSpec {
        BuildSpec {
            spec_version: 1,
            scope: Scope {
                kind: ScopeKind::Collection,
                id: "c".into(),
            },
            sources: vec![],
            snapshot: Snapshot { watermark_seq: 1 },
            shape: ShapeRef {
                shape_ref: "para".into(),
                version: 1,
            },
            chunker: Chunker {
                name: "para".into(),
                version: "para@1".into(),
                params: BTreeMap::new(),
            },
            extractor: Extractor {
                name: "x".into(),
                version: "1".into(),
                config: BTreeMap::new(),
                per_source: vec![],
            },
            embedder: None,
            lexical_analysis: LexicalAnalysis {
                contract_version: 1,
                tokenizer: "t".into(),
                stemmer: "s".into(),
                stop_terms_ref: StopTerms {
                    list_ref: "r".into(),
                    sha256: "e".repeat(64),
                },
                index_options: IndexOptions {
                    positions: true,
                    case_folding: None,
                    accent_folding: None,
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
                segments: None,
                compression: None,
            },
            vector: None,
            records: RecordsFormat {
                format: "munarium-records@1".into(),
                compression: None,
            },
            range_map: None,
            shaper: Shaper {
                policy_version: 1,
                decisions: vec![],
            },
        }
    }

    fn seed(dir: &std::path::Path) -> (LocalFileStore, String, ArtifactManifest) {
        let store = LocalFileStore::new(dir).unwrap();
        let mut w = ShardWriter::new(None);
        for i in 0..3u32 {
            let text = format!("chunk number {i} about harbour tea");
            w.add(PreparedChunk {
                chunk_id: format!("c{i}"),
                source_id: "s".into(),
                source_path: "p.md".into(),
                node_id: None,
                ordinal: i,
                text: text.clone(),
                text_sha256: Sha256::digest(text.as_bytes()).into(),
                embedding: None,
                metadata: BTreeMap::new(),
            })
            .unwrap();
        }
        let sealed = w.seal(&spec(), &plan(), &store).unwrap();
        sealed.publish_manifest(&store).unwrap();
        (store, sealed.artifact_id, sealed.manifest)
    }

    fn run(store: &LocalFileStore, id: &str) -> ArtifactAudit {
        audit_artifact(store, id, &ReaderCapabilities::v1(), &Limits::default()).unwrap()
    }

    #[test]
    fn a_clean_artifact_matches_every_pinned_object() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, manifest) = seed(dir.path());
        let audit = run(&store, &id);
        assert!(audit.all_match(), "{audit:?}");
        assert_eq!(audit.objects.len(), 1 + manifest.components.len());
        assert_eq!(audit.objects[0].object_key, MANIFEST);
        assert_eq!(audit.objects[0].expected_sha256, id);
        assert!(audit.first_mismatch().is_none());
    }

    /// A corrupted component is named with the pin's digest and the observed
    /// one, so two verifiers can be compared on the same object.
    #[test]
    fn a_corrupted_component_is_a_named_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, manifest) = seed(dir.path());
        let victim = manifest.components[0].clone();
        let mut bytes = store.get_component(&victim.path, None).unwrap();
        bytes[0] ^= 0xff;
        store.put_component(&victim.path, &bytes).unwrap();

        let audit = run(&store, &id);
        let bad = audit.first_mismatch().expect("a mismatch");
        assert_eq!(bad.object_key, victim.path);
        assert_eq!(bad.expected_sha256, victim.sha256);
        assert_ne!(bad.observed_sha256.as_deref(), Some(victim.sha256.as_str()));
        assert!(!audit.all_match());
    }

    /// A substituted manifest is a mismatch on the manifest itself, and the
    /// audit does not go on to trust the components it names.
    #[test]
    fn a_corrupted_manifest_stops_the_audit() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, _) = seed(dir.path());
        let mut bytes = store.get_component(MANIFEST, None).unwrap();
        bytes.push(b' ');
        store.put_component(MANIFEST, &bytes).unwrap();
        let audit = run(&store, &id);
        assert_eq!(audit.objects.len(), 1);
        assert_eq!(audit.objects[0].outcome, AuditOutcome::Mismatch);
    }

    /// An optional component the store does not hold is skipped, exactly as
    /// hydration skips it: the audit is complete and matches. A present
    /// optional component is still compared, and a missing required one is
    /// still unavailable.
    #[test]
    fn an_absent_optional_component_is_skipped_like_hydration() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _, mut manifest) = seed(dir.path());
        let optional = manifest.components[0].path.clone();
        manifest.components[0].required = false;
        let bytes = serde_json::to_vec(&manifest).unwrap();
        store.put_component(MANIFEST, &bytes).unwrap();
        let id = digest(&bytes);
        assert!(run(&store, &id).all_match());
        assert!(
            run(&store, &id).object(&optional).is_some(),
            "present: compared"
        );

        std::fs::remove_file(dir.path().join(&optional)).unwrap();
        let audit = run(&store, &id);
        assert!(audit.first_unavailable().is_none(), "{audit:?}");
        assert!(audit.all_match(), "{audit:?}");
        assert!(audit.object(&optional).is_none());
        assert_eq!(audit.objects.len(), manifest.components.len());

        // A required component that is absent is still unavailable.
        assert!(manifest.components[1].required);
        let required = manifest.components[1].path.clone();
        std::fs::remove_file(dir.path().join(&required)).unwrap();
        let audit = run(&store, &id);
        assert_eq!(
            audit.first_unavailable().map(|o| o.object_key.as_str()),
            Some(required.as_str())
        );
    }

    /// A store that reports generations: each object's audit carries the
    /// generation of the read that produced its digest. The local store
    /// reports none, so every object's is absent.
    #[test]
    fn each_object_carries_the_generation_its_bytes_were_read_at() {
        use crate::store::{ByteRange, StoredComponent};
        struct Versioned(LocalFileStore);
        impl ArtifactStore for Versioned {
            fn put_component(&self, p: &str, b: &[u8]) -> Result<(), Error> {
                self.0.put_component(p, b)
            }
            fn get_component(&self, p: &str, r: Option<ByteRange>) -> Result<Vec<u8>, Error> {
                self.0.get_component(p, r)
            }
            fn head_component(&self, p: &str) -> Result<u64, Error> {
                self.0.head_component(p)
            }
            fn exists(&self, p: &str) -> Result<bool, Error> {
                self.0.exists(p)
            }
            fn get_component_with_generation(&self, p: &str) -> Result<StoredComponent, Error> {
                Ok(StoredComponent {
                    bytes: self.0.get_component(p, None)?,
                    generation: Some(format!("etag:{p}")),
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (store, id, _) = seed(dir.path());
        let plain = run(&store, &id);
        assert!(plain.objects.iter().all(|o| o.storage_generation.is_none()));

        let versioned = Versioned(store);
        let audit = audit_artifact(
            &versioned,
            &id,
            &ReaderCapabilities::v1(),
            &Limits::default(),
        )
        .unwrap();
        assert!(audit.all_match());
        for o in &audit.objects {
            assert_eq!(
                o.storage_generation.as_deref(),
                Some(format!("etag:{}", o.object_key).as_str())
            );
        }
    }

    /// Absence is never damage: a missing component is unavailable.
    #[test]
    fn a_missing_component_is_unavailable_not_a_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id, manifest) = seed(dir.path());
        std::fs::remove_file(dir.path().join(&manifest.components[0].path)).unwrap();
        let audit = run(&store, &id);
        assert!(audit.first_mismatch().is_none());
        assert_eq!(
            audit.first_unavailable().map(|o| o.object_key.as_str()),
            Some(manifest.components[0].path.as_str())
        );
        let gone = tempfile::tempdir().unwrap();
        let empty = LocalFileStore::new(gone.path()).unwrap();
        let audit = run(&empty, &id);
        assert_eq!(audit.objects[0].outcome, AuditOutcome::Unavailable);
    }
}
