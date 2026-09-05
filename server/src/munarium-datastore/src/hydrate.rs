// SPDX-License-Identifier: Apache-2.0
//! L1 hydration and eviction.
//!
//! Brings an artifact from L2 to a local directory, verifying as it goes, and
//! reclaims space when the cache grows past its high watermark.
//!
//! ## Everything keys on the full [`ArtifactCacheKey`]
//!
//! Single-flight, residency, leases, eviction and quarantine all key on
//! `{isolation_domain, logical_version_id, artifact_id}` — never on the
//! artifact id alone. Identical content in two tenants legitimately produces
//! one content hash, so coalescing on it would let one tenant's request warm,
//! evict, or quarantine another's cache entry, and would let a caller reach
//! another domain's bytes by presenting a hash it happened to know.
//!
//! ## The publication order is the safety property
//!
//! Download into the key's `.partial` directory (one per key; single-flight
//! guarantees one hydration per key at a time), verify every component
//! against the manifest, fsync each file, then atomically rename to the
//! sealed leaf and write `COMPLETE` last. A reader therefore never observes a
//! half-written artifact at a sealed path, and a crash leaves a `.partial`
//! directory that startup reconciliation removes rather than a
//! plausible-looking broken artifact.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

use crate::model::ArtifactManifest;
use crate::store::ArtifactStore;
use crate::verify::{
    validate_manifest, verify_component, verify_manifest_bytes, Limits, ReaderCapabilities,
};
use crate::{ArtifactCacheKey, Error};

/// Written last, after everything else is durable.
pub const COMPLETE_MARKER: &str = "COMPLETE";
const PARTIAL_SUFFIX: &str = ".partial";

/// Why an artifact is resident, which decides whether it may be evicted.
///
/// These are separately budgeted on purpose (§9.1): staged prewarm must never
/// be able to starve the serving-required set, and neither may evict an
/// artifact an open request is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Residency {
    /// A cache miss brought it in. First to be evicted.
    Opportunistic,
    /// A `staged` binding being prewarmed. Evictable before required, never
    /// before an open request.
    StagedPrewarm,
    /// In the scope's serving-required set. Evicting it would withdraw the
    /// replica's readiness.
    ServingRequired,
    /// An open request or a session pin is reading it right now.
    Pinned,
}

/// What the cache holds for one key.
#[derive(Debug, Clone)]
pub struct ResidentArtifact {
    pub key: ArtifactCacheKey,
    pub path: PathBuf,
    pub bytes: u64,
    pub residency: Residency,
    pub last_access: SystemTime,
}

/// High and low watermarks, not "delete one file when full".
///
/// Evicting exactly enough to fit the next arrival guarantees the next arrival
/// evicts again; going down to a low watermark amortizes the work and stops the
/// cache spending its life at the boundary.
#[derive(Debug, Clone, Copy)]
pub struct CacheBudget {
    pub high_watermark_bytes: u64,
    pub low_watermark_bytes: u64,
}

impl CacheBudget {
    pub fn new(high: u64, low: u64) -> Result<Self, Error> {
        if low >= high {
            return Err(Error::Invalid(format!(
                "low watermark {low} must be below high watermark {high}; equal or inverted \
                 watermarks make eviction either continuous or impossible"
            )));
        }
        Ok(Self {
            high_watermark_bytes: high,
            low_watermark_bytes: low,
        })
    }
}

/// The local artifact cache.
pub struct L1Cache {
    root: PathBuf,
    budget: CacheBudget,
    state: Mutex<CacheState>,
    /// Signalled whenever a hydration finishes, so waiters wake instead of
    /// polling. Paired with `CacheState::in_flight`.
    finished: Condvar,
}

#[derive(Default)]
struct CacheState {
    resident: HashMap<ArtifactCacheKey, ResidentArtifact>,
    /// Keys whose bytes were found corrupt. Quarantine is per KEY, so one
    /// tenant's corrupt copy never suppresses another tenant's identical
    /// content, and never reveals that the other tenant holds it.
    quarantined: HashSet<ArtifactCacheKey>,
    /// Keys currently being hydrated, so concurrent misses inside ONE
    /// authorized domain share the work instead of racing.
    in_flight: HashSet<ArtifactCacheKey>,
}

impl L1Cache {
    pub fn new(root: impl Into<PathBuf>, budget: CacheBudget) -> Result<Self, Error> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            budget,
            state: Mutex::new(CacheState::default()),
            finished: Condvar::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn sealed_path(&self, key: &ArtifactCacheKey) -> Result<PathBuf, Error> {
        Ok(self.root.join(key.l1_relative_path()?))
    }

    pub fn resident(&self, key: &ArtifactCacheKey) -> Option<ResidentArtifact> {
        self.state.lock().unwrap().resident.get(key).cloned()
    }

    pub fn is_quarantined(&self, key: &ArtifactCacheKey) -> bool {
        self.state.lock().unwrap().quarantined.contains(key)
    }

    pub fn used_bytes(&self) -> u64 {
        self.state
            .lock()
            .unwrap()
            .resident
            .values()
            .map(|r| r.bytes)
            .sum()
    }

    /// Hydrate an artifact from a store into L1.
    ///
    /// Idempotent: an already-resident key returns immediately with its
    /// residency raised if the caller asked for a stronger one — a serving
    /// requirement must be able to promote something a miss brought in.
    ///
    /// Single-flight by the FULL cache key: a concurrent caller for the same
    /// key blocks until the download in progress finishes and then returns its
    /// result, rather than fetching the same bytes twice. Concurrent callers
    /// for DIFFERENT keys never block each other, including two domains holding
    /// identical content.
    ///
    /// Blocking is correct here: this is a synchronous API run on a bounded
    /// blocking pool, and the alternative — duplicating a multi-gigabyte fetch
    /// under exactly the load that makes it expensive — is worse.
    pub fn hydrate(
        &self,
        key: &ArtifactCacheKey,
        store: &dyn ArtifactStore,
        reader: &ReaderCapabilities,
        limits: &Limits,
        residency: Residency,
    ) -> Result<ResidentArtifact, Error> {
        {
            let mut st = self.state.lock().unwrap();
            if st.quarantined.contains(key) {
                return Err(Error::Integrity(format!(
                    "{key} is quarantined; it will not be re-fetched until an operator or a \
                     verified republish clears it"
                )));
            }
            if let Some(existing) = st.resident.get_mut(key) {
                existing.residency = existing.residency.max(residency);
                existing.last_access = SystemTime::now();
                return Ok(existing.clone());
            }
            // True single-flight: a second caller WAITS for the download in
            // progress rather than starting its own or being turned away.
            // Racing would fetch the same bytes twice under exactly the load
            // that makes that expensive -- a cold replica admitting traffic --
            // and refusing would make a concurrent miss look like a failure.
            //
            // The wait re-checks the predicate rather than breaking out on the
            // first wake. `Condvar::wait` may wake spuriously, and a wake that
            // was not this key finishing would otherwise fall straight through
            // and start the duplicate download this exists to prevent.
            while st.in_flight.contains(key) {
                st = self
                    .finished
                    .wait(st)
                    .expect("the cache lock is not poisoned on any path we recover from");
            }

            // The hydration this call waited on has finished. Take its result.
            if st.quarantined.contains(key) {
                return Err(Error::Integrity(format!(
                    "{key} was quarantined by the hydration this call waited on"
                )));
            }
            if let Some(existing) = st.resident.get_mut(key) {
                existing.residency = existing.residency.max(residency);
                existing.last_access = SystemTime::now();
                return Ok(existing.clone());
            }
            // Neither resident nor quarantined: it failed transiently -- a
            // timeout, say. This caller tries for itself, because one caller's
            // transient failure is not every caller's.
            st.in_flight.insert(key.clone());
        }

        // The in-flight mark is released by a guard, not by straight-line
        // code after the download: a panic inside `hydrate_inner` (a store
        // implementation that panics, a poisoned lock it holds) would
        // otherwise leave the key in `in_flight` forever, and every later
        // caller for it would wait on the condvar for a wake that never comes.
        let outcome = {
            let _guard = InFlightGuard { cache: self, key };
            self.hydrate_inner(key, store, reader, limits, residency)
        };
        {
            let mut st = self.state.lock().unwrap();
            // Quarantine BEFORE waking, so a waiter observes the final state
            // rather than a window in which the key is neither in flight nor
            // yet marked bad. (The guard removed the in-flight mark; waiters
            // re-check the predicate under this same lock.)
            if let Err(Error::Integrity(_)) = &outcome {
                st.quarantined.insert(key.clone());
            }
        }
        self.finished.notify_all();

        match outcome {
            Ok(resident) => {
                // Make room, but never by evicting what was JUST brought in:
                // the new entry has the freshest access time but may have the
                // weakest residency (an opportunistic shadow fetch beside a
                // serving-required set already over the low watermark), and
                // the residency-first victim order would then pick it every
                // time — download, self-evict, hand the caller a path that no
                // longer exists. An eviction failure is a filesystem error on
                // some OTHER key's leaf; the hydration itself succeeded and is
                // accounted, so it is reported as the success it is.
                let _ = self.evict_to_low_watermark_except(Some(key));
                Ok(resident)
            }
            // Integrity failures quarantined above, before waking waiters. A
            // timeout or a missing file does not quarantine: doing so would let
            // one flaky fetch permanently unserve an artifact that is fine.
            Err(e) => Err(e),
        }
    }

    fn hydrate_inner(
        &self,
        key: &ArtifactCacheKey,
        store: &dyn ArtifactStore,
        reader: &ReaderCapabilities,
        limits: &Limits,
        residency: Residency,
    ) -> Result<ResidentArtifact, Error> {
        let sealed = self.sealed_path(key)?;
        let partial = sealed.with_extension(PARTIAL_SUFFIX.trim_start_matches('.'));
        if partial.exists() {
            // Left by a crash. Removed rather than resumed: a partial directory
            // carries no record of which components were verified, so resuming
            // would mean trusting bytes nobody checked.
            std::fs::remove_dir_all(&partial)?;
        }
        std::fs::create_dir_all(&partial)?;

        let result = (|| -> Result<u64, Error> {
            // The manifest is fetched and hash-checked BEFORE it is parsed, so
            // a substituted manifest cannot direct what gets read.
            let manifest_bytes = store.get_component(crate::shard::MANIFEST, None)?;
            verify_manifest_bytes(&manifest_bytes, &key.artifact_id)?;
            let manifest: ArtifactManifest = serde_json::from_slice(&manifest_bytes)
                .map_err(|e| Error::Invalid(format!("manifest does not parse: {e}")))?;
            validate_manifest(&manifest, reader, limits)?;

            let declared: u64 = manifest.components.iter().map(|c| c.bytes_len).sum();
            if declared > self.budget.high_watermark_bytes {
                return Err(Error::Limit(format!(
                    "artifact declares {declared} bytes, over the whole cache high watermark \
                     of {}; it can never be resident and waiting for space would never end",
                    self.budget.high_watermark_bytes
                )));
            }

            let mut written = 0u64;
            for c in &manifest.components {
                if !store.exists(&c.path)? {
                    if c.required {
                        // Absence is NOT an integrity failure: no byte was
                        // observed to disagree with the manifest. An
                        // eventually-consistent listing, or a read that lands
                        // between a component put and a republished manifest,
                        // must not quarantine a key that is fine — the
                        // quarantine is reserved for a hash or length that
                        // was checked and did not match.
                        return Err(Error::Io(std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            format!("required component {:?} is absent from the store", c.path),
                        )));
                    }
                    continue;
                }
                let bytes = store.get_component(&c.path, None)?;
                verify_component(c, &bytes)?;
                let target = partial.join(&c.path);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                write_durably(&target, &bytes)?;
                written += bytes.len() as u64;
            }
            write_durably(&partial.join(crate::shard::MANIFEST), &manifest_bytes)?;
            written += manifest_bytes.len() as u64;
            Ok(written)
        })();

        let bytes = match result {
            Ok(b) => b,
            Err(e) => {
                // A failed download is excluded from accounting and never
                // opened; leaving it would let a corrupt tree be mistaken for
                // a sealed one after a restart.
                let _ = std::fs::remove_dir_all(&partial);
                return Err(e);
            }
        };

        if let Some(parent) = sealed.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if sealed.exists() {
            std::fs::remove_dir_all(&sealed)?;
        }
        std::fs::rename(&partial, &sealed)?;
        // COMPLETE is written last and is still not trusted on its own: a
        // reader verifies the manifest regardless. It marks a directory as
        // having finished, not as having been checked.
        std::fs::write(sealed.join(COMPLETE_MARKER), b"")?;

        let resident = ResidentArtifact {
            key: key.clone(),
            path: sealed,
            bytes,
            residency,
            last_access: SystemTime::now(),
        };
        self.state
            .lock()
            .unwrap()
            .resident
            .insert(key.clone(), resident.clone());
        Ok(resident)
    }

    /// Raise or lower why an artifact is resident.
    pub fn set_residency(&self, key: &ArtifactCacheKey, residency: Residency) -> Result<(), Error> {
        let mut st = self.state.lock().unwrap();
        let entry = st
            .resident
            .get_mut(key)
            .ok_or_else(|| Error::Invalid(format!("{key} is not resident")))?;
        entry.residency = residency;
        Ok(())
    }

    /// Evict least-recently-used evictable artifacts until below the low
    /// watermark.
    ///
    /// Returns the keys evicted. If nothing evictable remains, this returns
    /// without error: refusing a HYDRATION for lack of space is the caller's
    /// decision to make with a specific resource-exhausted error, and deleting
    /// an in-use shard to make room would be worse than being full.
    pub fn evict_to_low_watermark(&self) -> Result<Vec<ArtifactCacheKey>, Error> {
        self.evict_to_low_watermark_except(None)
    }

    /// `evict_to_low_watermark`, sparing one key — the one a hydration has
    /// just brought in and is about to hand to its caller.
    fn evict_to_low_watermark_except(
        &self,
        except: Option<&ArtifactCacheKey>,
    ) -> Result<Vec<ArtifactCacheKey>, Error> {
        let mut evicted = Vec::new();
        loop {
            let victim = {
                let st = self.state.lock().unwrap();
                let used: u64 = st.resident.values().map(|r| r.bytes).sum();
                if used <= self.budget.low_watermark_bytes {
                    break;
                }
                // Weakest residency first, then least recently used. Sorting by
                // residency before recency is what stops a busy prewarm from
                // evicting the serving-required set.
                st.resident
                    .values()
                    .filter(|r| r.residency < Residency::Pinned)
                    .filter(|r| except != Some(&r.key))
                    .min_by(|a, b| {
                        a.residency
                            .cmp(&b.residency)
                            .then_with(|| a.last_access.cmp(&b.last_access))
                    })
                    .map(|r| r.key.clone())
            };
            let Some(key) = victim else { break };
            self.evict(&key)?;
            evicted.push(key);
        }
        Ok(evicted)
    }

    /// Remove one artifact from L1. Its bytes remain in L2, so this is a cache
    /// operation and never data loss.
    pub fn evict(&self, key: &ArtifactCacheKey) -> Result<(), Error> {
        let path = {
            let mut st = self.state.lock().unwrap();
            match st.resident.remove(key) {
                Some(r) => r.path,
                None => return Ok(()),
            }
        };
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        Ok(())
    }

    /// Clear a quarantine, after an operator has looked or a verified
    /// republish has happened.
    pub fn clear_quarantine(&self, key: &ArtifactCacheKey) {
        self.state.lock().unwrap().quarantined.remove(key);
    }

    /// Reconcile in-memory state against the filesystem at startup.
    ///
    /// Removes every `.partial` directory, because a crash leaves one with no
    /// record of what was verified — and every sealed leaf too. This process
    /// has not verified those, and trusting a stale cache index is precisely
    /// what §10.3 says not to do; they are not adopted, and a leaf that is
    /// never adopted is disk the watermarks cannot see. Left in place, a
    /// restart's re-hydration of the same set would hold the old copy beside
    /// the new one for every key it had not yet re-requested — up to twice
    /// the high watermark on the replica's ephemeral disk. Re-fetching from
    /// L2 is the cost either way; this just does not pay it in disk as well.
    pub fn reconcile_startup(&self) -> Result<usize, Error> {
        let mut removed = 0;
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                if !path.is_dir() {
                    continue;
                }
                let is_partial = path
                    .extension()
                    .is_some_and(|x| x == PARTIAL_SUFFIX.trim_start_matches('.'));
                // A sealed leaf is recognizable by what hydration writes
                // there and nowhere else: the manifest and the marker.
                let is_sealed_leaf = path.join(COMPLETE_MARKER).exists()
                    || path.join(crate::shard::MANIFEST).exists();
                if is_partial || is_sealed_leaf {
                    std::fs::remove_dir_all(&path)?;
                    removed += 1;
                } else {
                    stack.push(path);
                }
            }
        }
        Ok(removed)
    }
}

/// Releases a key's in-flight mark when dropped — on the normal path and on a
/// panic alike. The condvar is signalled by `hydrate` after it has recorded
/// the outcome; on a panic there is no outcome, and the unwinding thread's
/// waiters are woken by the next `notify_all` or by their own re-check.
struct InFlightGuard<'a> {
    cache: &'a L1Cache,
    key: &'a ArtifactCacheKey,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut st) = self.cache.state.lock() {
            st.in_flight.remove(self.key);
        }
        if std::thread::panicking() {
            self.cache.finished.notify_all();
        }
    }
}

/// Write a file and flush it to stable storage before returning.
///
/// The publication order — components, manifest, rename, `COMPLETE` — only
/// protects against a crash if each step is durable before the next; a
/// buffered write that the rename outruns leaves a `COMPLETE`-marked leaf
/// whose files are truncated. A reader re-hashes every component, so this is
/// never silent, but a leaf that verifies after a crash beats one that has to
/// be re-fetched.
fn write_durably(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// A cloneable handle, since the cache is shared across request tasks.
pub type SharedL1Cache = Arc<L1Cache>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::shard::ShardWriter;
    use crate::store::LocalFileStore;
    use crate::PreparedChunk;
    use sha2::{Digest, Sha256};
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

    /// Build a small artifact into a store and return its id.
    fn seed(dir: &Path, n: usize) -> (LocalFileStore, String) {
        let store = LocalFileStore::new(dir).unwrap();
        let mut w = ShardWriter::new(None);
        for i in 0..n {
            let text = format!("chunk number {i} with some words in it");
            w.add(PreparedChunk {
                chunk_id: format!("c{i}"),
                source_id: "s".into(),
                source_path: "p.md".into(),
                node_id: None,
                ordinal: i as u32,
                text: text.clone(),
                text_sha256: Sha256::digest(text.as_bytes()).into(),
                embedding: None,
                metadata: BTreeMap::new(),
            })
            .unwrap();
        }
        let sealed = w.seal(&spec(), &plan(), &store).unwrap();
        sealed.publish_manifest(&store).unwrap();
        (store, sealed.artifact_id)
    }

    fn key(domain: &str, id: &str) -> ArtifactCacheKey {
        ArtifactCacheKey::new(domain, "idx2-v1", id)
    }

    fn cache(root: &Path) -> L1Cache {
        L1Cache::new(root, CacheBudget::new(10_000_000, 5_000_000).unwrap()).unwrap()
    }

    #[test]
    fn hydrates_verifies_and_seals() {
        let src = tempfile::tempdir().unwrap();
        let l1 = tempfile::tempdir().unwrap();
        let (store, id) = seed(src.path(), 3);
        let c = cache(l1.path());
        let k = key("dom", &id);

        let r = c
            .hydrate(
                &k,
                &store,
                &ReaderCapabilities::v1(),
                &Limits::default(),
                Residency::ServingRequired,
            )
            .unwrap();
        assert!(
            r.path.join(COMPLETE_MARKER).exists(),
            "COMPLETE written last"
        );
        assert!(r.path.join("manifest.json").exists());
        assert!(r.bytes > 0);
        // No .partial left behind.
        assert!(!r.path.with_extension("partial").exists());
    }

    #[test]
    fn hydrating_twice_is_idempotent_and_can_raise_residency() {
        let src = tempfile::tempdir().unwrap();
        let l1 = tempfile::tempdir().unwrap();
        let (store, id) = seed(src.path(), 2);
        let c = cache(l1.path());
        let k = key("dom", &id);

        c.hydrate(
            &k,
            &store,
            &ReaderCapabilities::v1(),
            &Limits::default(),
            Residency::Opportunistic,
        )
        .unwrap();
        let again = c
            .hydrate(
                &k,
                &store,
                &ReaderCapabilities::v1(),
                &Limits::default(),
                Residency::ServingRequired,
            )
            .unwrap();
        assert_eq!(
            again.residency,
            Residency::ServingRequired,
            "raised, not reset"
        );
    }

    /// The isolation property, at the cache layer. Identical CONTENT in two
    /// domains is two entries, two directories, and two independent lifetimes.
    #[test]
    fn identical_content_in_two_domains_is_cached_separately() {
        let src = tempfile::tempdir().unwrap();
        let l1 = tempfile::tempdir().unwrap();
        let (store, id) = seed(src.path(), 2);
        let c = cache(l1.path());
        let a = key("dom-a", &id);
        let b = key("dom-b", &id);

        let ra = c
            .hydrate(
                &a,
                &store,
                &ReaderCapabilities::v1(),
                &Limits::default(),
                Residency::Opportunistic,
            )
            .unwrap();
        let rb = c
            .hydrate(
                &b,
                &store,
                &ReaderCapabilities::v1(),
                &Limits::default(),
                Residency::Opportunistic,
            )
            .unwrap();
        assert_ne!(ra.path, rb.path);

        // Evicting one leaves the other entirely alone.
        c.evict(&a).unwrap();
        assert!(c.resident(&a).is_none());
        assert!(c.resident(&b).is_some());
        assert!(rb.path.exists());
    }

    /// Corruption quarantines the KEY, not the content hash: one domain's bad
    /// copy must never suppress another's good one, nor reveal that the other
    /// holds it.
    #[test]
    fn corruption_quarantines_one_domain_only() {
        let src = tempfile::tempdir().unwrap();
        let l1 = tempfile::tempdir().unwrap();
        let (store, id) = seed(src.path(), 2);
        let c = cache(l1.path());

        // Corrupt a component in the shared store.
        let mut body = store
            .get_component(crate::shard::RECORDS_BODY, None)
            .unwrap();
        body[0] ^= 0xff;
        store
            .put_component(crate::shard::RECORDS_BODY, &body)
            .unwrap();

        let a = key("dom-a", &id);
        let err = c
            .hydrate(
                &a,
                &store,
                &ReaderCapabilities::v1(),
                &Limits::default(),
                Residency::Opportunistic,
            )
            .unwrap_err();
        assert!(format!("{err}").contains("integrity"), "{err}");
        assert!(c.is_quarantined(&a));
        // A different domain is NOT quarantined by A's experience.
        assert!(!c.is_quarantined(&key("dom-b", &id)));

        // And a re-request in the quarantined domain is refused without a fetch.
        let err = c
            .hydrate(
                &a,
                &store,
                &ReaderCapabilities::v1(),
                &Limits::default(),
                Residency::Opportunistic,
            )
            .unwrap_err();
        assert!(format!("{err}").contains("quarantined"), "{err}");
        c.clear_quarantine(&a);
        assert!(!c.is_quarantined(&a));
    }

    /// Eviction takes the weakest residency first, so a prewarm cannot displace
    /// the serving-required set, and nothing displaces a pin.
    #[test]
    fn eviction_respects_residency_before_recency() {
        let l1 = tempfile::tempdir().unwrap();
        // Watermarks low enough that everything must be evicted down to one.
        let c = L1Cache::new(l1.path(), CacheBudget::new(300, 150).unwrap()).unwrap();

        let insert = |name: &str, bytes: u64, residency: Residency| {
            let k = key("dom", name);
            let path = c.sealed_path(&k).unwrap();
            std::fs::create_dir_all(&path).unwrap();
            c.state.lock().unwrap().resident.insert(
                k.clone(),
                ResidentArtifact {
                    key: k.clone(),
                    path,
                    bytes,
                    residency,
                    last_access: SystemTime::now(),
                },
            );
            k
        };
        let opportunistic = insert("opp", 100, Residency::Opportunistic);
        let prewarm = insert("pre", 100, Residency::StagedPrewarm);
        let required = insert("req", 100, Residency::ServingRequired);
        let pinned = insert("pin", 100, Residency::Pinned);

        let evicted = c.evict_to_low_watermark().unwrap();
        assert_eq!(evicted[0], opportunistic, "weakest goes first");
        assert!(evicted.contains(&prewarm));
        assert!(!evicted.contains(&pinned), "a pin is never evicted");
        assert!(c.resident(&pinned).is_some());
        // Required survives if the low watermark is reached before it is needed.
        let _ = required;
    }

    /// Nothing evictable and still over budget is NOT an error here: refusing a
    /// hydration is the caller's decision with a specific error, and deleting
    /// an in-use shard would be worse than being full.
    #[test]
    fn a_cache_of_only_pins_stops_evicting_rather_than_deleting_them() {
        let l1 = tempfile::tempdir().unwrap();
        let c = L1Cache::new(l1.path(), CacheBudget::new(100, 50).unwrap()).unwrap();
        let k = key("dom", "pinned");
        let path = c.sealed_path(&k).unwrap();
        std::fs::create_dir_all(&path).unwrap();
        c.state.lock().unwrap().resident.insert(
            k.clone(),
            ResidentArtifact {
                key: k.clone(),
                path,
                bytes: 999,
                residency: Residency::Pinned,
                last_access: SystemTime::now(),
            },
        );
        assert!(c.evict_to_low_watermark().unwrap().is_empty());
        assert!(c.resident(&k).is_some());
    }

    /// An artifact bigger than the whole cache is refused up front. Admitting
    /// it would evict everything and still not fit, so the wait would never end.
    #[test]
    fn an_artifact_larger_than_the_cache_is_refused_before_downloading() {
        let src = tempfile::tempdir().unwrap();
        let l1 = tempfile::tempdir().unwrap();
        let (store, id) = seed(src.path(), 5);
        let c = L1Cache::new(l1.path(), CacheBudget::new(64, 32).unwrap()).unwrap();
        let err = c
            .hydrate(
                &key("dom", &id),
                &store,
                &ReaderCapabilities::v1(),
                &Limits::default(),
                Residency::ServingRequired,
            )
            .unwrap_err();
        assert!(format!("{err}").contains("high watermark"), "{err}");
    }

    #[test]
    fn startup_reconciliation_removes_partial_directories() {
        let l1 = tempfile::tempdir().unwrap();
        let c = cache(l1.path());
        let stale = l1.path().join("dom").join("idx2-v1").join("abc.partial");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("half.bin"), b"incomplete").unwrap();

        assert_eq!(c.reconcile_startup().unwrap(), 1);
        assert!(!stale.exists());
    }

    /// A sealed leaf from a previous process is not adopted, so it is disk
    /// the watermarks cannot see; startup removes it rather than carrying an
    /// invisible copy beside the one the next hydration will fetch.
    #[test]
    fn startup_reconciliation_removes_unadopted_sealed_leaves() {
        let src = tempfile::tempdir().unwrap();
        let l1 = tempfile::tempdir().unwrap();
        let (store, id) = seed(src.path(), 2);
        let k = key("dom", &id);
        let leaf = {
            let c = cache(l1.path());
            c.hydrate(
                &k,
                &store,
                &ReaderCapabilities::v1(),
                &Limits::default(),
                Residency::ServingRequired,
            )
            .unwrap()
            .path
        };
        assert!(leaf.join(COMPLETE_MARKER).exists());

        // A fresh process over the same root.
        let c = cache(l1.path());
        assert!(c.resident(&k).is_none(), "never adopted");
        assert_eq!(c.reconcile_startup().unwrap(), 1);
        assert!(!leaf.exists(), "the sealed leaf is gone");
        // And a hydration afterwards simply fetches it again.
        c.hydrate(
            &k,
            &store,
            &ReaderCapabilities::v1(),
            &Limits::default(),
            Residency::ServingRequired,
        )
        .unwrap();
        assert!(leaf.join(COMPLETE_MARKER).exists());
    }

    /// The artifact a hydration just brought in must not be the victim of
    /// the eviction that follows it. With a low watermark below the size of
    /// ANY artifact, every hydration is over the watermark the moment it
    /// lands, and the residency-first victim order would pick the newest
    /// entry whenever it had the weakest residency — download, self-evict,
    /// hand back a path that no longer exists.
    #[test]
    fn a_just_hydrated_artifact_is_not_evicted_by_its_own_hydration() {
        let src = tempfile::tempdir().unwrap();
        let l1 = tempfile::tempdir().unwrap();
        let (store_a, id_a) = seed(src.path().join("a").as_path(), 3);
        let (store_b, id_b) = seed(src.path().join("b").as_path(), 4);
        let c = L1Cache::new(l1.path(), CacheBudget::new(1_000_000, 1).unwrap()).unwrap();

        let ka = key("dom", &id_a);
        let a = c
            .hydrate(
                &ka,
                &store_a,
                &ReaderCapabilities::v1(),
                &Limits::default(),
                Residency::ServingRequired,
            )
            .unwrap();
        assert!(a.path.exists(), "A survives its own hydration");
        assert!(c.resident(&ka).is_some());

        // An opportunistic fetch beside a serving-required set already over
        // the low watermark: the WEAKEST residency in the cache is the new
        // entry's own. It must still be the one that stays.
        let kb = key("dom", &id_b);
        let b = c
            .hydrate(
                &kb,
                &store_b,
                &ReaderCapabilities::v1(),
                &Limits::default(),
                Residency::Opportunistic,
            )
            .unwrap();
        assert!(b.path.exists(), "B survives its own hydration");
        assert!(c.resident(&kb).is_some());
        // Something else made room: A, the only other evictable entry.
        assert!(c.resident(&ka).is_none());
    }

    /// Absence is not corruption. A required component the store does not
    /// have fails the hydration but does NOT quarantine the key: no byte was
    /// checked and found wrong, and an eventually-consistent listing must not
    /// permanently unserve an artifact that is fine.
    #[test]
    fn a_missing_required_component_fails_without_quarantining() {
        let src = tempfile::tempdir().unwrap();
        let l1 = tempfile::tempdir().unwrap();
        let (store, id) = seed(src.path(), 2);
        let manifest: ArtifactManifest =
            serde_json::from_slice(&store.get_component(crate::shard::MANIFEST, None).unwrap())
                .unwrap();
        let required = manifest
            .components
            .iter()
            .find(|c| c.required)
            .expect("a required component");
        std::fs::remove_file(src.path().join(&required.path)).unwrap();

        let c = cache(l1.path());
        let k = key("dom", &id);
        let err = c
            .hydrate(
                &k,
                &store,
                &ReaderCapabilities::v1(),
                &Limits::default(),
                Residency::ServingRequired,
            )
            .unwrap_err();
        assert!(matches!(err, Error::Io(_)), "{err}");
        assert!(!c.is_quarantined(&k), "absence does not quarantine");
        assert!(c.resident(&k).is_none());
    }

    /// Single-flight, proven with real threads rather than asserted.
    ///
    /// Eight callers race for one key. Exactly one download happens, every
    /// caller gets a resident artifact, and nobody is turned away -- which is
    /// the behaviour the doc comment promises and the earlier implementation
    /// did not have: it returned an error to the second caller.
    #[test]
    fn concurrent_callers_for_one_key_share_a_single_download() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Counts how many times the bytes were actually fetched.
        struct CountingStore {
            inner: LocalFileStore,
            gets: AtomicUsize,
        }
        impl ArtifactStore for CountingStore {
            fn put_component(&self, p: &str, b: &[u8]) -> Result<(), Error> {
                self.inner.put_component(p, b)
            }
            fn get_component(
                &self,
                p: &str,
                r: Option<crate::store::ByteRange>,
            ) -> Result<Vec<u8>, Error> {
                self.gets.fetch_add(1, Ordering::SeqCst);
                // Slow enough that the other threads are certainly waiting.
                std::thread::sleep(std::time::Duration::from_millis(20));
                self.inner.get_component(p, r)
            }
            fn head_component(&self, p: &str) -> Result<u64, Error> {
                self.inner.head_component(p)
            }
            fn exists(&self, p: &str) -> Result<bool, Error> {
                self.inner.exists(p)
            }
        }

        let src = tempfile::tempdir().unwrap();
        let l1 = tempfile::tempdir().unwrap();
        let (inner, id) = seed(src.path(), 3);
        let store = CountingStore {
            inner,
            gets: AtomicUsize::new(0),
        };
        let c = Arc::new(cache(l1.path()));
        let k = key("dom", &id);

        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let c = Arc::clone(&c);
                    let k = k.clone();
                    let store = &store;
                    scope.spawn(move || {
                        c.hydrate(
                            &k,
                            store,
                            &ReaderCapabilities::v1(),
                            &Limits::default(),
                            Residency::ServingRequired,
                        )
                    })
                })
                .collect();
            for h in handles {
                let r = h.join().unwrap();
                assert!(r.is_ok(), "no caller may be turned away: {r:?}");
            }
        });

        // One artifact has 6 components plus the manifest. If every thread had
        // fetched, this would be eight times that.
        let gets = store.gets.load(Ordering::SeqCst);
        assert!(
            gets <= 8,
            "expected one download's worth of GETs, saw {gets} -- the callers raced"
        );
        assert!(c.resident(&k).is_some());
    }

    /// Two DOMAINS holding identical content must not block each other: the
    /// single-flight key is the whole cache key, not the content hash.
    #[test]
    fn concurrent_callers_for_different_domains_do_not_block_each_other() {
        let src = tempfile::tempdir().unwrap();
        let l1 = tempfile::tempdir().unwrap();
        let (store, id) = seed(src.path(), 2);
        let c = Arc::new(cache(l1.path()));

        std::thread::scope(|scope| {
            let handles: Vec<_> = ["dom-a", "dom-b", "dom-c"]
                .into_iter()
                .map(|d| {
                    let c = Arc::clone(&c);
                    let k = key(d, &id);
                    let store = &store;
                    scope.spawn(move || {
                        c.hydrate(
                            &k,
                            store,
                            &ReaderCapabilities::v1(),
                            &Limits::default(),
                            Residency::Opportunistic,
                        )
                        .map(|r| r.path)
                    })
                })
                .collect();
            let paths: Vec<_> = handles
                .into_iter()
                .map(|h| h.join().unwrap().unwrap())
                .collect();
            // Three distinct directories: identical content, separate lifetimes.
            let mut sorted = paths.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), 3, "domains must not share a cache entry");
        });
    }

    #[test]
    fn inverted_watermarks_are_refused() {
        assert!(CacheBudget::new(100, 100).is_err());
        assert!(CacheBudget::new(100, 200).is_err());
        assert!(CacheBudget::new(200, 100).is_ok());
    }
}
