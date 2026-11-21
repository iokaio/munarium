// SPDX-License-Identifier: Apache-2.0
//! ART1/ART2 on the serving path, over real artifacts in a corrupted
//! local-file artifact store and a real PostgreSQL.
//!
//! Each "replica" is its own `DatastoreParts` (its own L1 and L0 caches and
//! verifier identity) over one shared database and one shared L2 store, which
//! is what a fleet is. A "restarted" replica is a new `AppState` and new parts
//! with empty caches. These are deterministic, single-process stand-ins: the
//! multi-replica receipts on a real fleet are deferred cluster work.
//!
//! Each named replica signs with its own disposable Ed25519 verifier key,
//! generated and enrolled on first use, so its verifier id is that key's
//! fingerprint.
//!
//! Gated on `MUNARIUM_TEST_DATABASE_URL`; skips loudly when it is unset.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use munarium_core::artifact_verifier::VerifierSigningKey;
use munarium_core::retrieval::{PreparedSearchQuery, SearchParams};
use munarium_datastore::hydrate::{CacheBudget, L1Cache, Residency};
use munarium_datastore::model::*;
use munarium_datastore::shard::{ShardWriter, MANIFEST};
use munarium_datastore::store::LocalFileStore;
use munarium_datastore::{ArtifactCacheKey, PreparedChunk};
use munarium_retrieval::damage::{DamageGuard, DamageMode, FailureSite};
use munarium_retrieval::executor::{ExecutionOutcome, TextPayload};
use munarium_retrieval::mirror::{ArtifactStoreFactory, LocalStoreFactory};
use munarium_store_pg::artifact_damage::{ArtifactDamageLedger, Decision, ReleaseDecision};
use munarium_store_pg::artifacts::{ArtifactCatalog, BindingSlot, NewArtifact};
use sha2::Digest as _;

use crate::datastore_builds::{tenant_path_hash, ObjectStoreFactory};
use crate::datastore_serving::DatastoreParts;
use crate::state::AppState;

macro_rules! guard {
    () => {
        match std::env::var("MUNARIUM_TEST_DATABASE_URL") {
            Ok(url) => url,
            Err(_) => {
                eprintln!("SKIPPED: MUNARIUM_TEST_DATABASE_URL is unset");
                return;
            }
        }
    };
}

async fn app_state(url: &str, tenant: &str) -> Arc<AppState> {
    crate::providers_api::usage_tests::test_state_with_auth(
        Some(url.to_string()),
        crate::config::AuthMode::Static(vec![("admin".into(), tenant.into(), "mgmt".into())]),
    )
    .await
}

fn spec() -> BuildSpec {
    BuildSpec {
        spec_version: 1,
        scope: Scope {
            kind: ScopeKind::Collection,
            id: "damage".into(),
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

/// One tenant's artifact, published into a shared local-file L2 store and
/// bound into the `serving` slot.
struct Fleet {
    url: String,
    tenant: String,
    version: String,
    artifact: String,
    l2_root: tempfile::TempDir,
    l2_prefix: String,
    /// The component damaged by [`Fleet::corrupt`] and its original bytes.
    victim: (String, Vec<u8>),
    state: Arc<AppState>,
    /// Disposable verifier keys by replica name (encoded seeds, test-only).
    keys: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl Fleet {
    async fn new(url: String, prefix: &str) -> Self {
        let tenant = format!("{prefix}-{}", uuid::Uuid::new_v4().simple());
        let state = app_state(&url, &tenant).await;
        let pool = state.pg_pool().expect("postgres").clone();
        let version = format!("idx2-{}", uuid::Uuid::new_v4().simple());
        let l2_root = tempfile::tempdir().unwrap();
        let l2_prefix = format!("indexes/{}/{version}", tenant_path_hash(&tenant));
        let store = LocalFileStore::new(l2_root.path().join(&l2_prefix)).unwrap();
        let mut w = ShardWriter::new(None);
        for i in 0..4u32 {
            let text = format!("harbour tea record {i} from the boston archive");
            w.add(PreparedChunk {
                chunk_id: format!("c{i}"),
                source_id: format!("s{i}"),
                source_path: format!("corpus/s{i}.md"),
                node_id: None,
                ordinal: i,
                text: text.clone(),
                text_sha256: sha2::Sha256::digest(text.as_bytes()).into(),
                embedding: None,
                metadata: BTreeMap::from([("source_content_hash".to_string(), "f".repeat(64))]),
            })
            .unwrap();
        }
        let sealed = w.seal(&spec(), &plan(), &store).unwrap();
        sealed.publish_manifest(&store).unwrap();
        let victim_path = sealed
            .manifest
            .components
            .iter()
            .find(|c| c.purpose == ComponentPurpose::Records)
            .expect("a records component")
            .path
            .clone();
        let victim_bytes =
            std::fs::read(l2_root.path().join(&l2_prefix).join(&victim_path)).unwrap();

        let catalog = ArtifactCatalog::new(pool.clone(), &tenant);
        catalog
            .insert_sealed(&NewArtifact {
                index_version_id: version.clone(),
                artifact_id: sealed.artifact_id.clone(),
                engine_id: "tantivy".into(),
                format_version: 1,
                artifact_uri: l2_prefix.clone(),
                artifact_plan: serde_json::json!({"plan_version": 1}),
                artifact_plan_sha256: "p".repeat(64),
                artifact_manifest: serde_json::to_value(&sealed.manifest).unwrap(),
                bytes_len: 1,
                file_count: sealed.manifest.components.len() as i32 + 1,
                built_by: Some("damage-test".into()),
                attempt_id: None,
            })
            .await
            .unwrap();
        catalog
            .mark_verified(&version, &sealed.artifact_id, "damage-test")
            .await
            .unwrap();
        catalog
            .bind_new(
                &version,
                BindingSlot::Serving,
                &sealed.artifact_id,
                "damage-test",
                None,
            )
            .await
            .unwrap();
        Self {
            url,
            tenant,
            version,
            artifact: sealed.artifact_id,
            l2_root,
            l2_prefix,
            victim: (victim_path, victim_bytes),
            state,
            keys: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn pool(&self) -> sqlx::PgPool {
        self.state.pg_pool().expect("postgres").clone()
    }

    fn ledger(&self) -> ArtifactDamageLedger {
        ArtifactDamageLedger::new(self.pool(), &self.tenant)
    }

    fn victim_path(&self) -> PathBuf {
        self.l2_root
            .path()
            .join(&self.l2_prefix)
            .join(&self.victim.0)
    }

    /// Damage the authoritative copy.
    fn corrupt(&self) {
        let mut bytes = self.victim.1.clone();
        bytes[0] ^= 0xff;
        std::fs::write(self.victim_path(), bytes).unwrap();
    }

    /// The separately authorized byte repair.
    fn restore(&self) {
        std::fs::write(self.victim_path(), &self.victim.1).unwrap();
    }

    fn key(&self) -> ArtifactCacheKey {
        ArtifactCacheKey::new(
            tenant_path_hash(&self.tenant),
            self.version.clone(),
            self.artifact.clone(),
        )
    }

    /// The named replica's verifier key, generated and enrolled on first
    /// use. Each call returns a copy of the same key.
    async fn vkey(&self, name: &str) -> VerifierSigningKey {
        let existing = self.keys.lock().unwrap().get(name).cloned();
        let encoded = match existing {
            Some(encoded) => encoded,
            None => {
                let key = VerifierSigningKey::generate();
                munarium_store_pg::artifact_damage::enroll_verifier(
                    &self.pool(),
                    &key.public_key(),
                    name,
                    "damage-test",
                )
                .await
                .unwrap();
                let encoded = key.encode_secret();
                self.keys
                    .lock()
                    .unwrap()
                    .insert(name.to_string(), encoded.clone());
                encoded
            }
        };
        VerifierSigningKey::from_encoded(&encoded).unwrap()
    }

    /// The named replica's verifier identity.
    async fn fp(&self, name: &str) -> String {
        self.vkey(name).await.fingerprint()
    }

    /// One replica: its own caches and enrolled verifier key over the shared
    /// database and the shared L2 store.
    async fn replica(&self, verifier: &str, mode: DamageMode) -> Replica {
        self.replica_with(
            DamageGuard::new(mode, verifier).with_signing_key(self.vkey(verifier).await),
        )
    }

    /// A replica with an explicitly built damage guard.
    fn replica_with(&self, guard: DamageGuard) -> Replica {
        Self::replica_with_store(guard, Arc::new(LocalStoreFactory::new(self.l2_root.path())))
    }

    fn replica_with_store(guard: DamageGuard, stores: Arc<dyn ArtifactStoreFactory>) -> Replica {
        let l1 = tempfile::tempdir().unwrap();
        let cache = L1Cache::new(
            l1.path(),
            CacheBudget::new(512 * 1024 * 1024, 256 * 1024 * 1024).unwrap(),
        )
        .unwrap();
        Replica {
            parts: DatastoreParts::from_parts(Arc::new(cache), stores, 8, Arc::new(guard)),
            _l1: l1,
        }
    }
}

struct Replica {
    parts: Arc<DatastoreParts>,
    _l1: tempfile::TempDir,
}

fn prepared() -> Arc<PreparedSearchQuery> {
    let mut q = munarium_retrieval_pg::PgRetrieval::prepare_query(
        "harbour tea archive",
        &SearchParams::default(),
        &munarium_retrieval_pg::LocalHashEmbedder,
    );
    // The fixture artifact is lexical-only.
    q.embedding = None;
    Arc::new(q)
}

impl Replica {
    async fn execute(&self, f: &Fleet, pool: &sqlx::PgPool) -> ExecutionOutcome {
        self.parts
            .executor(pool, &f.tenant)
            .execute(
                &f.version,
                BindingSlot::Serving,
                Residency::ServingRequired,
                TextPayload::Served,
                &prepared(),
            )
            .await
    }

    async fn search(
        &self,
        f: &Fleet,
        state: &AppState,
    ) -> munarium_core::Result<munarium_core::retrieval::SearchResult> {
        let pool = state.pg_pool().expect("postgres").clone();
        let pg = state.retrieval_for(&f.tenant)?.reference().clone();
        self.parts
            .serving_plane(&pool, &f.tenant)
            .search(&pg, ("collection", "damage"), &f.version, 1, &prepared())
            .await
    }
}

fn refusal(outcome: &ExecutionOutcome) -> &str {
    match outcome {
        ExecutionOutcome::Refused(r) => r,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn executed(outcome: ExecutionOutcome) -> munarium_retrieval::executor::ArtifactExecution {
    match outcome {
        ExecutionOutcome::Executed(e) => e,
        other => panic!("expected an execution, got {other:?}"),
    }
}

/// Exercise the serving and repair paths using real S3 generation metadata.
/// The endpoint and database belong to the disposable rehearsal.
#[tokio::test]
#[ignore = "requires a disposable loopback S3 bucket and PostgreSQL"]
async fn s3_serving_and_repair_bind_observations_to_object_generation() {
    let url = std::env::var("MUNARIUM_TEST_DATABASE_URL").expect("disposable PostgreSQL");
    let endpoint = std::env::var("MUNARIUM_TEST_S3_ENDPOINT").expect("disposable S3 endpoint");
    let endpoint_url = reqwest::Url::parse(&endpoint).unwrap();
    assert_eq!(endpoint_url.host_str(), Some("127.0.0.1"));
    let store = munarium_store_objects::ObjectSourceStore::s3(munarium_store_objects::S3Config {
        bucket: "sources".into(),
        region: "us-east-1".into(),
        endpoint: Some(endpoint),
        force_path_style: true,
        access_key_id: Some(std::env::var("MUNARIUM_TEST_S3_ACCESS_KEY").expect("test S3 key")),
        secret_access_key: Some(
            std::env::var("MUNARIUM_TEST_S3_SECRET_KEY").expect("test S3 secret"),
        ),
    })
    .unwrap();
    let stores = Arc::new(ObjectStoreFactory {
        store: Arc::new(store),
    });
    let f = Fleet::new(url, "art-s3").await;
    let root = f.l2_root.path().join(&f.l2_prefix);
    let remote = stores.store_for_prefix(&f.l2_prefix).unwrap();
    let manifest: ArtifactManifest =
        serde_json::from_slice(&std::fs::read(root.join(MANIFEST)).unwrap()).unwrap();
    let upload = remote.clone();
    tokio::task::spawn_blocking(move || {
        for path in manifest
            .components
            .into_iter()
            .map(|c| c.path)
            .chain([MANIFEST.to_string()])
        {
            upload
                .put_component(&path, &std::fs::read(root.join(&path)).unwrap())
                .unwrap();
        }
    })
    .await
    .unwrap();
    let mut replicas = Vec::new();
    for name in ["s3-warm", "s3-a", "s3-b", "s3-c"] {
        replicas.push(Fleet::replica_with_store(
            DamageGuard::new(DamageMode::Enforce, name).with_signing_key(f.vkey(name).await),
            stores.clone(),
        ));
    }
    let [warm, a, b, c] = replicas.as_slice() else {
        unreachable!()
    };
    let pool = f.pool();
    let ledger = f.ledger();
    let warm_read = executed(warm.execute(&f, &pool).await);
    let admission = warm_read.admission.expect("read admitted before damage");

    let write = |bytes: Vec<u8>| {
        let remote = remote.clone();
        let victim = f.victim.0.clone();
        tokio::task::spawn_blocking(move || {
            remote.put_component(&victim, &bytes).unwrap();
            remote
                .get_component_with_generation(&victim)
                .unwrap()
                .generation
                .expect("S3 generation")
        })
    };
    let mut corrupt = f.victim.1.clone();
    corrupt[0] ^= 0xa1;
    let first = write(corrupt.clone()).await.unwrap();
    refusal(&a.execute(&f, &pool).await);
    corrupt[0] ^= 0xb2;
    let second = write(corrupt).await.unwrap();
    assert_ne!(first, second);
    refusal(&b.execute(&f, &pool).await);
    assert!(ledger.incidents().await.unwrap().is_empty());
    refusal(&c.execute(&f, &pool).await);
    let incident = ledger
        .incidents()
        .await
        .unwrap()
        .pop()
        .expect("two verifiers at one generation");
    assert_eq!(
        incident.storage_generation.as_deref(),
        Some(second.as_str())
    );
    assert!(matches!(
        ledger.release(&[admission]).await.unwrap(),
        ReleaseDecision::Refused(_)
    ));
    refusal(&warm.execute(&f, &pool).await);

    ledger
        .authorize_repair(&incident.incident_id, "s3-repair-test", "operator", None)
        .await
        .unwrap();
    let repaired = write(f.victim.1.clone()).await.unwrap();
    assert_ne!(repaired, second);
    crate::datastore_serving::verify_repairs(&b.parts, &pool).await;
    assert!(ledger
        .admit(&f.version, &f.artifact)
        .await
        .unwrap()
        .blocked_by
        .is_some());
    crate::datastore_serving::verify_repairs(&c.parts, &pool).await;
    let cleared = ledger.incidents().await.unwrap().pop().unwrap();
    assert_eq!(
        cleared.clear_storage_generation.as_deref(),
        Some(repaired.as_str())
    );
    assert!(!executed(warm.execute(&f, &pool).await).hits.is_empty());
}

/// ART1: one verifier isolates only; the same verifier again does not count;
/// a second independent verifier blocks the fleet; warm and restarted readers
/// refuse; an in-flight read admitted before the block cannot be released.
#[tokio::test]
async fn independent_confirmation_blocks_warm_and_restarted_readers() {
    let url = guard!();
    let f = Fleet::new(url, "art1-serving").await;
    let pool = f.pool();
    let ledger = f.ledger();

    // A warm replica with good local bytes, and an in-flight read admitted
    // before any damage.
    let warm = f.replica("replica-w", DamageMode::Enforce).await;
    let first = executed(warm.execute(&f, &pool).await);
    assert!(!first.hits.is_empty());
    let in_flight = first.admission.clone().expect("an admission was captured");
    assert_eq!(in_flight.generation, 0);
    assert!(
        warm.search(&f, &f.state).await.is_ok(),
        "released before damage"
    );

    f.corrupt();

    // Replica A: its hydration fails, it isolates locally and records one
    // authoritative mismatch. No fleet block.
    let a = f.replica("replica-a", DamageMode::Enforce).await;
    let refused = a.execute(&f, &pool).await;
    assert!(refusal(&refused).contains("integrity"), "{refused:?}");
    assert!(a.parts.cache.is_quarantined(&f.key()));
    assert!(a.parts.damage.is_isolated(&f.key()));
    assert_eq!(
        ledger
            .admit(&f.version, &f.artifact)
            .await
            .unwrap()
            .blocked_by,
        None
    );
    // The warm replica's good local copy still serves: local isolation is
    // local.
    executed(warm.execute(&f, &pool).await);

    // The same verifier running again does not count twice: neither another
    // refused read nor a deliberate second verifier run.
    refusal(&a.execute(&f, &pool).await);
    let again = a
        .parts
        .damage_context(&pool, &f.tenant)
        .unwrap()
        .verify(
            a.parts.stores.store_for_prefix(&f.l2_prefix).unwrap(),
            &f.version,
            &f.artifact,
            &a.parts.reader,
            &a.parts.limits,
            None,
        )
        .await
        .unwrap();
    assert_eq!(again.decision, Decision::SameVerifier);
    assert_eq!(
        ledger
            .admit(&f.version, &f.artifact)
            .await
            .unwrap()
            .blocked_by,
        None
    );

    // Replica B independently confirms: the fleet block commits.
    let b = f.replica("replica-b", DamageMode::Enforce).await;
    refusal(&b.execute(&f, &pool).await);
    let blocked = ledger.admit(&f.version, &f.artifact).await.unwrap();
    let incident = blocked.blocked_by.clone().expect("fleet block");
    assert_eq!(blocked.generation, 1);

    // The read admitted before the block cannot be released after it.
    assert!(matches!(
        ledger.release(&[in_flight]).await.unwrap(),
        ReleaseDecision::Refused(_)
    ));

    // The warm replica refuses despite good local bytes, on both paths.
    let warm_refusal = warm.execute(&f, &pool).await;
    assert!(
        refusal(&warm_refusal).contains(&incident),
        "{warm_refusal:?}"
    );
    let err = warm.search(&f, &f.state).await.unwrap_err();
    assert!(
        matches!(err, munarium_core::KernelError::DatastoreUnavailable(_)),
        "{err}"
    );

    // A restarted replica: a new AppState and empty caches. It refuses at
    // admission without fetching a byte.
    let restarted_state = app_state(&f.url, &f.tenant).await;
    let restarted = f.replica("replica-r", DamageMode::Enforce).await;
    let restarted_pool = restarted_state.pg_pool().unwrap().clone();
    let r = restarted.execute(&f, &restarted_pool).await;
    assert!(refusal(&r).contains(&incident), "{r:?}");
    assert_eq!(restarted.parts.cache.used_bytes(), 0);
    assert!(restarted.search(&f, &restarted_state).await.is_err());
    // An observe-mode reader honors the block too.
    let observer = f.replica("replica-o", DamageMode::Observe).await;
    refusal(&observer.execute(&f, &restarted_pool).await);

    let incidents = ledger.incidents().await.unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(
        incidents[0].confirming_verifiers,
        [f.fp("replica-a").await, f.fp("replica-b").await]
    );
    assert_eq!(incidents[0].object_key, f.victim.0);
}

/// ART1-T1: local cache damage with a clean authoritative fetch isolates and
/// recovers the local copy and issues no fleet block.
#[tokio::test]
async fn local_damage_with_a_clean_authoritative_fetch_does_not_block() {
    let url = guard!();
    let f = Fleet::new(url, "art1-local").await;
    let pool = f.pool();
    let a = f.replica("replica-a", DamageMode::Enforce).await;
    let store = a.parts.stores.store_for_prefix(&f.l2_prefix).unwrap();
    let resident = a
        .parts
        .cache
        .hydrate(
            &f.key(),
            store.as_ref(),
            &a.parts.reader,
            &a.parts.limits,
            Residency::ServingRequired,
        )
        .unwrap();
    let local = resident.path.join(&f.victim.0);
    let mut bytes = std::fs::read(&local).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(&local, bytes).unwrap();

    let refused = a.execute(&f, &pool).await;
    assert!(refusal(&refused).contains("integrity"), "{refused:?}");
    assert!(
        a.parts.cache.resident(&f.key()).is_none(),
        "the damaged local copy was isolated by eviction"
    );
    let ledger = f.ledger();
    assert_eq!(
        ledger
            .admit(&f.version, &f.artifact)
            .await
            .unwrap()
            .blocked_by,
        None
    );
    let history = ledger.history(10).await.unwrap();
    let a_fp = f.fp("replica-a").await;
    assert!(
        history.contains(&(a_fp.clone(), "match".into(), "recorded".into())),
        "{history:?}"
    );
    assert!(history.contains(&(a_fp, "isolated".into(), "recorded".into())));
    // The next read re-fetches and re-verifies from the clean source.
    executed(a.execute(&f, &pool).await);
}

/// The v1.4 default: with damage handling off nothing durable happens and
/// the in-process quarantine behaves as before.
#[tokio::test]
async fn the_default_mode_records_nothing_durable() {
    let url = guard!();
    let f = Fleet::new(url, "art-off").await;
    let pool = f.pool();
    f.corrupt();
    for verifier in ["replica-a", "replica-b"] {
        let r = f.replica(verifier, DamageMode::Off).await;
        assert!(r.parts.damage_context(&pool, &f.tenant).is_none());
        refusal(&r.execute(&f, &pool).await);
        assert!(r.parts.cache.is_quarantined(&f.key()));
    }
    let ledger = f.ledger();
    assert_eq!(
        ledger.pin_state(&f.version, &f.artifact).await.unwrap(),
        None
    );
    assert_eq!(ledger.history_len().await.unwrap(), 0);
}

/// ART2: only a preauthorized workflow with two fresh independent matches
/// clears the block, and the clearance is recorded; isolated copies are then
/// re-verified before they serve.
#[tokio::test]
async fn a_preauthorized_verified_repair_clears_and_readers_recover() {
    let url = guard!();
    let f = Fleet::new(url, "art2-serving").await;
    let pool = f.pool();
    let ledger = f.ledger();
    f.corrupt();
    let a = f.replica("replica-a", DamageMode::Enforce).await;
    let b = f.replica("replica-b", DamageMode::Enforce).await;
    refusal(&a.execute(&f, &pool).await);
    refusal(&b.execute(&f, &pool).await);
    let incident = ledger
        .admit(&f.version, &f.artifact)
        .await
        .unwrap()
        .blocked_by
        .expect("blocked");
    let in_flight_after_block = ledger.admit(&f.version, &f.artifact).await.unwrap();

    // The bytes are repaired, but nobody has authorized clearance: matches
    // are evidence only and the sweep has nothing to verify.
    f.restore();
    for r in [&a, &b] {
        let m = r
            .parts
            .damage_context(&pool, &f.tenant)
            .unwrap()
            .verify(
                r.parts.stores.store_for_prefix(&f.l2_prefix).unwrap(),
                &f.version,
                &f.artifact,
                &r.parts.reader,
                &r.parts.limits,
                None,
            )
            .await
            .unwrap();
        assert_eq!(m.decision, Decision::Recorded);
    }
    assert!(ledger
        .admit(&f.version, &f.artifact)
        .await
        .unwrap()
        .blocked_by
        .is_some());

    // The operator preauthorizes the repair workflow.
    let attempt = crate::artifact_damage::execute(
        &pool,
        &[
            "authorize-repair".into(),
            "--tenant".into(),
            f.tenant.clone(),
            "--incident".into(),
            incident.clone(),
            "--workflow".into(),
            "repair-wf".into(),
            "--actor".into(),
            "operator".into(),
        ],
    )
    .await
    .unwrap();
    let attempt_id = attempt["attempt_id"].as_str().unwrap().to_string();

    // One replica's verification is a single match: still blocked, and its
    // later sweeps do not count again.
    crate::datastore_serving::verify_repairs(&a.parts, &pool).await;
    crate::datastore_serving::verify_repairs(&a.parts, &pool).await;
    let progress = ledger.repair_attempt(&attempt_id).await.unwrap().unwrap();
    assert_eq!(progress.state, "verifying");
    assert_eq!(
        progress.first_match_verifier_id,
        Some(f.fp("replica-a").await)
    );
    assert!(ledger
        .admit(&f.version, &f.artifact)
        .await
        .unwrap()
        .blocked_by
        .is_some());
    refusal(&a.execute(&f, &pool).await);

    // The second independent replica's verification clears it.
    crate::datastore_serving::verify_repairs(&b.parts, &pool).await;
    let cleared = ledger.admit(&f.version, &f.artifact).await.unwrap();
    assert_eq!(cleared.blocked_by, None);
    assert_eq!(cleared.generation, 2);

    // A replica that isolates a copy while the pin cannot be read records an
    // unknown generation, never 0: the clearance above, which happened
    // before it isolated, must not release the isolation.
    let late = f.replica("replica-l", DamageMode::Enforce).await;
    let unreachable = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect_lazy("postgres://munarium@127.0.0.1:1/unreachable")
        .unwrap();
    late.parts
        .damage_context(&unreachable, &f.tenant)
        .unwrap()
        .on_integrity_failure(
            &f.version,
            &f.artifact,
            &f.key(),
            FailureSite::Hydrate,
            "the pin could not be read",
            &late.parts.cache,
            &late.parts.l0,
            late.parts.stores.store_for_prefix(&f.l2_prefix).unwrap(),
            &late.parts.reader,
            &late.parts.limits,
        )
        .await;
    assert!(late.parts.damage.is_isolated(&f.key()));
    for _ in 0..2 {
        let admitted = late
            .parts
            .damage_context(&pool, &f.tenant)
            .unwrap()
            .admit(
                &f.version,
                &f.artifact,
                &f.key(),
                &late.parts.cache,
                &late.parts.l0,
            )
            .await
            .unwrap();
        assert_eq!((admitted.generation, admitted.blocked_by), (2, None));
        assert!(
            late.parts.damage.is_isolated(&f.key()),
            "released without a clearance after the isolation"
        );
    }
    let listed = crate::artifact_damage::execute(
        &pool,
        &["list".into(), "--tenant".into(), f.tenant.clone()],
    )
    .await
    .unwrap();
    let recorded = &listed["incidents"][0];
    assert_eq!(recorded["state"], "cleared");
    assert_eq!(recorded["clearing_workflow_id"], "repair-wf");
    assert_eq!(recorded["clearing_attempt_id"], attempt_id.as_str());
    assert_eq!(
        recorded["clearing_verifiers"],
        serde_json::json!([f.fp("replica-a").await, f.fp("replica-b").await])
    );

    // A read admitted during the block still fails release after clearance.
    assert!(matches!(
        ledger.release(&[in_flight_after_block]).await.unwrap(),
        ReleaseDecision::Refused(_)
    ));

    // Replica A's quarantined copy is discarded and re-verified, not trusted.
    assert!(a.parts.cache.is_quarantined(&f.key()));
    let recovered = executed(a.execute(&f, &pool).await);
    assert!(!recovered.hits.is_empty());
    assert!(!a.parts.cache.is_quarantined(&f.key()));
    assert!(a.search(&f, &f.state).await.is_ok());
    let fresh = f.replica("replica-r", DamageMode::Enforce).await;
    executed(fresh.execute(&f, &pool).await);

    // A fresh mismatch after clearance starts a new incident under ART1.
    f.corrupt();
    let c = f.replica("replica-c", DamageMode::Enforce).await;
    let d = f.replica("replica-d", DamageMode::Enforce).await;
    refusal(&c.execute(&f, &pool).await);
    refusal(&d.execute(&f, &pool).await);
    let renewed = ledger.admit(&f.version, &f.artifact).await.unwrap();
    assert!(renewed.blocked_by.is_some());
    assert_ne!(renewed.blocked_by.as_deref(), Some(incident.as_str()));
    assert_eq!(renewed.generation, 3);
}

/// An unauthorized repair cannot be preauthorized through the command for
/// a pin that is not blocked, and a bad command line is refused.
#[tokio::test]
async fn the_repair_command_refuses_what_it_cannot_scope() {
    let url = guard!();
    let f = Fleet::new(url, "art2-cli").await;
    let pool = f.pool();
    for args in [
        vec![
            "authorize-repair",
            "--tenant",
            f.tenant.as_str(),
            "--incident",
            "inc-none",
            "--workflow",
            "wf",
            "--actor",
            "op",
        ],
        vec![
            "authorize-repair",
            "--tenant",
            f.tenant.as_str(),
            "--workflow",
            "wf",
        ],
        vec!["clear", "--tenant", f.tenant.as_str()],
        vec!["list"],
        vec![],
    ] {
        let args: Vec<String> = args.into_iter().map(String::from).collect();
        assert!(
            crate::artifact_damage::execute(&pool, &args).await.is_err(),
            "{args:?}"
        );
    }
}

/// Move this tenant's single-replica evidence `secs` into the past, standing
/// in for the interval elapsing: the candidate's fetch, the repair window
/// and the first repair match.
async fn backdate(f: &Fleet, pool: &sqlx::PgPool, secs: i64) {
    sqlx::query(
        "UPDATE artifact_integrity_candidates
            SET fetched_at = fetched_at - make_interval(secs => $2)
          WHERE tenant_id = $1",
    )
    .bind(&f.tenant)
    .bind(secs as f64)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE artifact_repair_attempts
            SET authorized_at = authorized_at - make_interval(secs => $2),
                first_match_fetched_at = first_match_fetched_at - make_interval(secs => $2)
          WHERE tenant_id = $1",
    )
    .bind(&f.tenant)
    .bind(secs as f64)
    .execute(pool)
    .await
    .unwrap();
}

/// The single-replica amendment end to end: the only replica blocks after its
/// own spaced second run, clears after its own spaced second match, records
/// the assurance on both, and the default replica does neither.
#[tokio::test]
async fn a_declared_single_replica_blocks_and_clears_with_spaced_runs() {
    let url = guard!();
    let f = Fleet::new(url, "art-single").await;
    let pool = f.pool();
    let ledger = f.ledger();
    let interval = munarium_store_pg::artifact_damage::DEFAULT_SINGLE_REPLICA_INTERVAL_SECS;
    f.corrupt();

    // A default replica records its candidate and, however long it waits,
    // never confirms it alone.
    let multi = f.replica("replica-m", DamageMode::Enforce).await;
    refusal(&multi.execute(&f, &pool).await);
    backdate(&f, &pool, interval + 60).await;
    let before = ledger.history_len().await.unwrap();
    crate::datastore_serving::verify_repairs(&multi.parts, &pool).await;
    assert_eq!(
        ledger.history_len().await.unwrap(),
        before,
        "a default replica does not re-verify its candidates"
    );
    assert_eq!(
        ledger
            .admit(&f.version, &f.artifact)
            .await
            .unwrap()
            .blocked_by,
        None
    );

    // The declared single replica, alone: drop the default replica's
    // candidate so only this replica's evidence counts. Its own second run
    // confirms only once its candidate is at least the interval old.
    let single = f.replica_with(
        DamageGuard::new(DamageMode::Enforce, "replica-s")
            .with_signing_key(f.vkey("replica-s").await)
            .with_single_replica(interval)
            .unwrap(),
    );
    let s = f.fp("replica-s").await;
    sqlx::query("DELETE FROM artifact_integrity_candidates WHERE tenant_id = $1")
        .bind(&f.tenant)
        .execute(&pool)
        .await
        .unwrap();
    refusal(&single.execute(&f, &pool).await);
    crate::datastore_serving::verify_repairs(&single.parts, &pool).await;
    assert_eq!(
        ledger
            .admit(&f.version, &f.artifact)
            .await
            .unwrap()
            .blocked_by,
        None,
        "not before the interval"
    );
    backdate(&f, &pool, interval + 60).await;
    crate::datastore_serving::verify_repairs(&single.parts, &pool).await;
    let blocked = ledger.admit(&f.version, &f.artifact).await.unwrap();
    let incident = blocked.blocked_by.clone().expect("single-replica block");
    assert_eq!(blocked.generation, 1);
    refusal(&single.execute(&f, &pool).await);

    // Repair, authorize, and verify twice with the interval between.
    f.restore();
    let attempt = crate::artifact_damage::execute(
        &pool,
        &[
            "authorize-repair".into(),
            "--tenant".into(),
            f.tenant.clone(),
            "--incident".into(),
            incident.clone(),
            "--workflow".into(),
            "repair-wf".into(),
            "--actor".into(),
            "operator".into(),
        ],
    )
    .await
    .unwrap();
    let attempt_id = attempt["attempt_id"].as_str().unwrap().to_string();
    crate::datastore_serving::verify_repairs(&single.parts, &pool).await;
    crate::datastore_serving::verify_repairs(&single.parts, &pool).await;
    let progress = ledger.repair_attempt(&attempt_id).await.unwrap().unwrap();
    assert_eq!(progress.state, "verifying", "one match, then a wait");
    assert_eq!(
        progress.first_match_verifier_id.as_deref(),
        Some(s.as_str())
    );
    // The single replica waits for the interval before its second match.
    assert!(ledger
        .admit(&f.version, &f.artifact)
        .await
        .unwrap()
        .blocked_by
        .is_some());
    backdate(&f, &pool, interval + 60).await;
    crate::datastore_serving::verify_repairs(&single.parts, &pool).await;
    let cleared = ledger.admit(&f.version, &f.artifact).await.unwrap();
    assert_eq!(cleared.blocked_by, None);
    assert_eq!(cleared.generation, 2);

    // A replica that isolates a copy while the pin cannot be read records an
    // unknown generation, never 0: the clearance above, which happened
    // before it isolated, must not release the isolation.
    let late = f.replica("replica-l", DamageMode::Enforce).await;
    let unreachable = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect_lazy("postgres://munarium@127.0.0.1:1/unreachable")
        .unwrap();
    late.parts
        .damage_context(&unreachable, &f.tenant)
        .unwrap()
        .on_integrity_failure(
            &f.version,
            &f.artifact,
            &f.key(),
            FailureSite::Hydrate,
            "the pin could not be read",
            &late.parts.cache,
            &late.parts.l0,
            late.parts.stores.store_for_prefix(&f.l2_prefix).unwrap(),
            &late.parts.reader,
            &late.parts.limits,
        )
        .await;
    assert!(late.parts.damage.is_isolated(&f.key()));
    for _ in 0..2 {
        let admitted = late
            .parts
            .damage_context(&pool, &f.tenant)
            .unwrap()
            .admit(
                &f.version,
                &f.artifact,
                &f.key(),
                &late.parts.cache,
                &late.parts.l0,
            )
            .await
            .unwrap();
        assert_eq!((admitted.generation, admitted.blocked_by), (2, None));
        assert!(
            late.parts.damage.is_isolated(&f.key()),
            "released without a clearance after the isolation"
        );
    }

    let listed = crate::artifact_damage::execute(
        &pool,
        &["list".into(), "--tenant".into(), f.tenant.clone()],
    )
    .await
    .unwrap();
    let recorded = &listed["incidents"][0];
    assert_eq!(recorded["state"], "cleared");
    assert_eq!(recorded["block_assurance"], "single_replica");
    assert_eq!(recorded["clear_assurance"], "single_replica");
    assert_eq!(recorded["confirming_verifiers"], serde_json::json!([s, s]));
    assert_eq!(recorded["clearing_verifiers"], serde_json::json!([s, s]));
    // The quarantined copy is re-verified and serves again.
    executed(single.execute(&f, &pool).await);
}

/// A two-verifier block lists its assurance as independent.
#[tokio::test]
async fn an_independent_block_lists_independent_assurance() {
    let url = guard!();
    let f = Fleet::new(url, "art-indep").await;
    let pool = f.pool();
    f.corrupt();
    refusal(
        &f.replica("replica-a", DamageMode::Enforce)
            .await
            .execute(&f, &pool)
            .await,
    );
    refusal(
        &f.replica("replica-b", DamageMode::Enforce)
            .await
            .execute(&f, &pool)
            .await,
    );
    let listed = crate::artifact_damage::execute(
        &pool,
        &["list".into(), "--tenant".into(), f.tenant.clone()],
    )
    .await
    .unwrap();
    assert_eq!(listed["incidents"][0]["block_assurance"], "independent");
    assert_eq!(
        listed["incidents"][0]["clear_assurance"],
        serde_json::Value::Null
    );
}

/// Without an enrolled key a replica isolates its copy and records what it
/// saw, but nothing it observes counts: two keyless replicas never block, and
/// neither do two replicas sharing one key.
#[tokio::test]
async fn keyless_and_key_sharing_replicas_only_isolate_locally() {
    let url = guard!();
    let f = Fleet::new(url, "art-keys").await;
    let pool = f.pool();
    let ledger = f.ledger();
    f.corrupt();
    for name in ["replica-a", "replica-b"] {
        let keyless = f.replica_with(DamageGuard::new(DamageMode::Enforce, name));
        assert!(!keyless.parts.damage.signs());
        let refused = keyless.execute(&f, &pool).await;
        assert!(refusal(&refused).contains("integrity"), "{refused:?}");
        assert!(keyless.parts.damage.is_isolated(&f.key()));
    }
    let history = ledger.history(10).await.unwrap();
    assert!(
        history
            .iter()
            .any(|(v, _, d)| v == "replica-a" && d == "unauthenticated"),
        "{history:?}"
    );
    assert!(history.iter().all(|(_, _, d)| d != "candidate"));

    // Two replicas with copies of one enrolled key are one verifier.
    for name in ["shared-1", "shared-2"] {
        let r = f.replica_with(
            DamageGuard::new(DamageMode::Enforce, name).with_signing_key(f.vkey("shared").await),
        );
        refusal(&r.execute(&f, &pool).await);
    }
    assert_eq!(
        ledger
            .admit(&f.version, &f.artifact)
            .await
            .unwrap()
            .blocked_by,
        None
    );
    let history = ledger.history(10).await.unwrap();
    let shared = f.fp("shared").await;
    assert!(history.contains(&(shared.clone(), "mismatch".into(), "candidate".into())));
    assert!(history.contains(&(shared, "mismatch".into(), "same_verifier".into())));

    // A distinct enrolled key confirms.
    refusal(
        &f.replica("replica-c", DamageMode::Enforce)
            .await
            .execute(&f, &pool)
            .await,
    );
    assert!(ledger
        .admit(&f.version, &f.artifact)
        .await
        .unwrap()
        .blocked_by
        .is_some());
}

/// Key generation, enrollment, listing and revocation through the operator
/// command. The private key stays in its file.
#[tokio::test]
async fn the_verifier_key_commands_enroll_and_revoke_without_exposing_the_key() {
    let url = guard!();
    let f = Fleet::new(url, "art-key-cli").await;
    let pool = f.pool();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("verifier.key");
    let path_s = path.to_string_lossy().to_string();
    let run = |args: Vec<&str>| {
        let args: Vec<String> = args.into_iter().map(String::from).collect();
        let pool = pool.clone();
        async move { crate::artifact_damage::execute(&pool, &args).await }
    };

    let generated = run(vec!["generate-verifier-key", "--out", &path_s])
        .await
        .unwrap();
    let secret = std::fs::read_to_string(&path).unwrap();
    assert!(!generated.to_string().contains(secret.trim()));
    let fingerprint = generated["fingerprint"].as_str().unwrap().to_string();
    let public_key = generated["public_key"].as_str().unwrap().to_string();
    // It never overwrites an existing key file.
    assert!(run(vec!["generate-verifier-key", "--out", &path_s])
        .await
        .is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), secret);
    let derived = run(vec!["verifier-public-key", "--key-file", &path_s])
        .await
        .unwrap();
    assert_eq!(derived["fingerprint"], fingerprint.as_str());
    assert_eq!(derived["public_key"], public_key.as_str());
    assert_eq!(
        munarium_retrieval::damage::load_verifier_key(&path_s)
            .unwrap()
            .fingerprint(),
        fingerprint
    );

    let enrolled = run(vec![
        "enroll-verifier",
        "--public-key",
        &public_key,
        "--label",
        "replica-1",
        "--actor",
        "operator",
    ])
    .await
    .unwrap();
    assert_eq!(enrolled["fingerprint"], fingerprint.as_str());
    assert_eq!(enrolled["revoked_at"], serde_json::Value::Null);
    let listed = run(vec!["list-verifiers"]).await.unwrap();
    assert!(listed["verifiers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k["fingerprint"] == fingerprint.as_str()));
    let revoked = run(vec![
        "revoke-verifier",
        "--fingerprint",
        &fingerprint,
        "--actor",
        "operator",
        "--reason",
        "rotated",
    ])
    .await
    .unwrap();
    assert!(revoked["revoked_at"].is_string());
    assert_eq!(revoked["revoke_reason"], "rotated");
    assert!(run(vec![
        "enroll-verifier",
        "--public-key",
        &public_key,
        "--label",
        "replica-1",
        "--actor",
        "operator",
    ])
    .await
    .is_err());
    for bad in [
        vec![
            "enroll-verifier",
            "--public-key",
            "nope",
            "--label",
            "x",
            "--actor",
            "op",
        ],
        vec!["enroll-verifier", "--label", "x", "--actor", "op"],
        vec!["revoke-verifier", "--actor", "op"],
        vec!["verifier-public-key", "--key-file", "does-not-exist.key"],
    ] {
        assert!(run(bad.clone()).await.is_err(), "{bad:?}");
    }
}
