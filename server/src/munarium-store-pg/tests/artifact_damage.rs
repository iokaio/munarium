// SPDX-License-Identifier: Apache-2.0
//! The artifact damage ledger against a real PostgreSQL (ClearView R10, ART1,
//! ART2).
//!
//! Gated on `MUNARIUM_TEST_DATABASE_URL`, and skips loudly when it is unset.
//! Every test uses its own tenant, so the bounded history and the cross-tenant
//! repair enumeration never see another test's rows.
//!
//! Each named verifier ("replica-a", ...) is a disposable Ed25519 key that the
//! fixture generates and enrolls on first use; its verifier id is the key's
//! fingerprint. [`Fixture::record`] signs an observation just before
//! recording it, so a test may change any field first and the signature
//! covers the final value. Tests that need an unsigned or forged observation
//! call the ledger directly.

use std::collections::HashMap;
use std::sync::Mutex;

use munarium_core::artifact_verifier::VerifierSigningKey;
use munarium_store_pg::artifact_damage::{
    enroll_verifier, revoke_verifier, ArtifactDamageLedger, AuthoritativeObservation, Authority,
    Decision, ObjectPin, ObservationSignature, Outcome, ReleaseDecision, VerifierIdentity,
    MANIFEST_OBJECT,
};
use munarium_store_pg::artifacts::{ArtifactCatalog, NewArtifact};
use munarium_store_pg::PgStore;

const COMPONENT: &str = "records/chunks.bin";
const ENFORCE: Authority = Authority {
    may_block: true,
    may_clear: true,
};
const OBSERVE: Authority = Authority {
    may_block: false,
    may_clear: false,
};

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

fn hex(byte: char) -> String {
    byte.to_string().repeat(64)
}

struct Fixture {
    ledger: ArtifactDamageLedger,
    pool: sqlx::PgPool,
    tenant: String,
    version: String,
    artifact: String,
    component_sha: String,
    /// Disposable verifier keys by name, generated and enrolled on first use.
    keys: Mutex<HashMap<String, std::sync::Arc<VerifierSigningKey>>>,
}

async fn fixture(url: &str, prefix: &str) -> Fixture {
    let tenant = format!("{prefix}-{}", uuid::Uuid::new_v4().simple());
    let store = PgStore::connect(url, &tenant).await.unwrap();
    let pool = store.pool().clone();
    let version = format!("idx2-{}", uuid::Uuid::new_v4().simple());
    let artifact = hex('a');
    let component_sha = hex('c');
    ArtifactCatalog::new(pool.clone(), &tenant)
        .insert_sealed(&NewArtifact {
            index_version_id: version.clone(),
            artifact_id: artifact.clone(),
            engine_id: "tantivy".into(),
            format_version: 1,
            artifact_uri: format!("indexes/{version}/{artifact}"),
            artifact_plan: serde_json::json!({"plan_version": 1}),
            artifact_plan_sha256: hex('p'),
            artifact_manifest: serde_json::json!({
                "manifest_version": 1,
                "components": [{"path": COMPONENT, "sha256": component_sha, "bytes_len": 10}]
            }),
            bytes_len: 10,
            file_count: 2,
            built_by: Some("damage-test".into()),
            attempt_id: None,
        })
        .await
        .unwrap();
    Fixture {
        ledger: ArtifactDamageLedger::new(pool.clone(), &tenant),
        pool,
        tenant,
        version,
        artifact,
        component_sha,
        keys: Mutex::new(HashMap::new()),
    }
}

impl Fixture {
    fn pin(&self) -> ObjectPin {
        ObjectPin {
            index_version_id: self.version.clone(),
            artifact_id: self.artifact.clone(),
            object_key: COMPONENT.into(),
            expected_sha256: self.component_sha.clone(),
            storage_generation: None,
        }
    }

    /// The named verifier's key, generated and enrolled on first use.
    async fn key(&self, name: &str) -> std::sync::Arc<VerifierSigningKey> {
        let existing = self.keys.lock().unwrap().get(name).cloned();
        if let Some(key) = existing {
            return key;
        }
        let key = std::sync::Arc::new(VerifierSigningKey::generate());
        enroll_verifier(&self.pool, &key.public_key(), name, "damage-test")
            .await
            .unwrap();
        self.keys
            .lock()
            .unwrap()
            .insert(name.to_string(), key.clone());
        key
    }

    /// The named verifier's identity: its key's fingerprint.
    async fn fp(&self, name: &str) -> String {
        self.key(name).await.fingerprint()
    }

    /// Sign `o` with the key whose fingerprint is its verifier id, at the
    /// pin's current generation. An id that is no fixture key stays unsigned.
    async fn sign(&self, o: &mut AuthoritativeObservation) {
        let key = self
            .keys
            .lock()
            .unwrap()
            .values()
            .find(|k| k.fingerprint() == o.verifier.verifier_id)
            .cloned();
        let Some(key) = key else {
            o.signature = None;
            return;
        };
        let generation = self
            .ledger
            .admit(&o.pin.index_version_id, &o.pin.artifact_id)
            .await
            .unwrap()
            .generation;
        o.signature = Some(ObservationSignature {
            pin_generation: generation,
            signature: key.sign(&o.claims(&self.tenant, generation)),
        });
    }

    async fn signed(&self, o: &AuthoritativeObservation) -> AuthoritativeObservation {
        let mut o = o.clone();
        self.sign(&mut o).await;
        o
    }

    async fn observe(
        &self,
        verifier: &str,
        outcome: Outcome,
        attempt: Option<&str>,
    ) -> AuthoritativeObservation {
        AuthoritativeObservation {
            pin: self.pin(),
            verifier: VerifierIdentity {
                verifier_id: self.fp(verifier).await,
                run_id: uuid::Uuid::new_v4().to_string(),
            },
            fetch_id: uuid::Uuid::new_v4().to_string(),
            fetched_at: self.ledger.db_now().await.unwrap(),
            outcome,
            observed_sha256: match outcome {
                Outcome::Mismatch => Some(hex('b')),
                Outcome::Match => Some(self.component_sha.clone()),
                Outcome::Unavailable => None,
            },
            repair_attempt_id: attempt.map(str::to_string),
            detail: Some("test".into()),
            signature: None,
        }
    }

    async fn record(&self, o: &AuthoritativeObservation) -> Decision {
        self.ledger
            .record_authoritative(&self.signed(o).await, ENFORCE)
            .await
            .unwrap()
            .decision
    }

    async fn mismatch(&self, verifier: &str) -> Decision {
        let o = self.observe(verifier, Outcome::Mismatch, None).await;
        self.record(&o).await
    }

    async fn blocked_by(&self) -> Option<String> {
        self.ledger
            .admit(&self.version, &self.artifact)
            .await
            .unwrap()
            .blocked_by
    }

    /// Confirm a block with two independent verifiers; return the incident.
    async fn block(&self) -> String {
        assert_eq!(self.mismatch("replica-a").await, Decision::Candidate);
        match self.mismatch("replica-b").await {
            Decision::Blocked { incident_id, .. } => incident_id,
            other => panic!("expected a block, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// ART1: what does and does not confirm a fleet block
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_single_mismatch_is_a_candidate_and_never_blocks() {
    let url = guard!();
    let f = fixture(&url, "dmg-single").await;
    f.ledger
        .record_local_isolation(
            &f.version,
            &f.artifact,
            &VerifierIdentity {
                verifier_id: "replica-a".into(),
                run_id: "run-1".into(),
            },
            Some("hydration integrity failure"),
        )
        .await
        .unwrap();
    assert_eq!(f.mismatch("replica-a").await, Decision::Candidate);
    let admitted = f.ledger.admit(&f.version, &f.artifact).await.unwrap();
    assert_eq!(admitted.generation, 0);
    assert_eq!(admitted.blocked_by, None);
    assert_eq!(f.ledger.incidents().await.unwrap(), vec![]);
}

#[tokio::test]
async fn the_same_verifier_running_twice_counts_once() {
    let url = guard!();
    let f = fixture(&url, "dmg-twice").await;
    assert_eq!(f.mismatch("replica-a").await, Decision::Candidate);
    // A fresh run and a fresh fetch, but the same verifier identity.
    assert_eq!(f.mismatch("replica-a").await, Decision::SameVerifier);
    assert_eq!(f.mismatch("replica-a").await, Decision::SameVerifier);
    assert_eq!(f.blocked_by().await, None);
}

#[tokio::test]
async fn a_replayed_fetch_under_another_name_counts_once() {
    let url = guard!();
    let f = fixture(&url, "dmg-replay").await;
    let first = f.observe("replica-a", Outcome::Mismatch, None).await;
    assert_eq!(f.record(&first).await, Decision::Candidate);
    let mut renamed = first.clone();
    renamed.verifier.verifier_id = f.fp("replica-b").await;
    assert_eq!(f.record(&renamed).await, Decision::ReplayedFetch);
    assert_eq!(f.blocked_by().await, None);
}

#[tokio::test]
async fn a_timeout_or_a_different_pin_never_confirms() {
    let url = guard!();
    let f = fixture(&url, "dmg-pins").await;
    assert_eq!(f.mismatch("replica-a").await, Decision::Candidate);

    // A timeout from a second verifier is recorded, not counted.
    let timeout = f.observe("replica-b", Outcome::Unavailable, None).await;
    assert_eq!(f.record(&timeout).await, Decision::Recorded);

    // The same verifier compared against a different expected digest is not
    // the trusted pin at all.
    let mut wrong_digest = f.observe("replica-b", Outcome::Mismatch, None).await;
    wrong_digest.pin.expected_sha256 = hex('d');
    assert!(matches!(
        f.record(&wrong_digest).await,
        Decision::Rejected(_)
    ));

    // An object the pin does not contain.
    let mut wrong_object = f.observe("replica-b", Outcome::Mismatch, None).await;
    wrong_object.pin.object_key = "lexical/meta.json".into();
    assert!(matches!(
        f.record(&wrong_object).await,
        Decision::Rejected(_)
    ));

    // A different stored object generation is a different pinned object: it
    // starts its own candidate rather than confirming the first.
    let mut other_generation = f.observe("replica-b", Outcome::Mismatch, None).await;
    other_generation.pin.storage_generation = Some("etag-2".into());
    assert_eq!(f.record(&other_generation).await, Decision::Candidate);

    // The manifest's expected digest must be the artifact id.
    let mut manifest = f.observe("replica-b", Outcome::Mismatch, None).await;
    manifest.pin.object_key = MANIFEST_OBJECT.into();
    assert!(matches!(f.record(&manifest).await, Decision::Rejected(_)));

    assert_eq!(f.blocked_by().await, None);
    assert_eq!(f.ledger.incidents().await.unwrap(), vec![]);
}

#[tokio::test]
async fn two_independent_mismatches_on_one_pin_block_the_fleet_durably() {
    let url = guard!();
    let f = fixture(&url, "dmg-block").await;
    let incident = f.block().await;

    let admitted = f.ledger.admit(&f.version, &f.artifact).await.unwrap();
    assert_eq!(admitted.generation, 1);
    assert_eq!(admitted.blocked_by.as_deref(), Some(incident.as_str()));

    // A restarted reader: a fresh handle on a fresh pool sees the same block.
    let restarted = ArtifactDamageLedger::new(
        PgStore::connect(&url, &f.tenant)
            .await
            .unwrap()
            .pool()
            .clone(),
        &f.tenant,
    );
    let again = restarted.admit(&f.version, &f.artifact).await.unwrap();
    assert_eq!(again.blocked_by.as_deref(), Some(incident.as_str()));

    let incidents = f.ledger.incidents().await.unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(
        incidents[0].confirming_verifiers,
        [f.fp("replica-a").await, f.fp("replica-b").await]
    );
    assert_eq!(incidents[0].object_key, COMPONENT);
    assert_eq!(incidents[0].state, "blocked");

    // A third mismatch on a blocked pin is evidence, not a second incident.
    assert_eq!(f.mismatch("replica-c").await, Decision::Recorded);
    assert_eq!(f.ledger.incidents().await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_observe_mode_replica_records_but_never_blocks() {
    let url = guard!();
    let f = fixture(&url, "dmg-observe").await;
    assert_eq!(f.mismatch("replica-a").await, Decision::Candidate);
    let second = f.observe("replica-b", Outcome::Mismatch, None).await;
    let r = f
        .ledger
        .record_authoritative(&f.signed(&second).await, OBSERVE)
        .await
        .unwrap();
    assert_eq!(r.decision, Decision::BlockWithheld);
    assert_eq!(f.blocked_by().await, None);
    // The withheld confirmation leaves the candidate; an enforcing replica
    // can still confirm against it.
    assert!(matches!(
        f.mismatch("replica-c").await,
        Decision::Blocked { .. }
    ));
}

#[tokio::test]
async fn identical_bytes_in_another_tenant_are_not_blocked() {
    let url = guard!();
    let f = fixture(&url, "dmg-tenant-a").await;
    let g = fixture(&url, "dmg-tenant-b").await;
    assert_eq!(f.artifact, g.artifact, "same content hash in both tenants");
    f.block().await;
    assert!(f.blocked_by().await.is_some());
    let other = ArtifactDamageLedger::new(g.pool.clone(), &g.tenant);
    assert_eq!(
        other
            .admit(&g.version, &g.artifact)
            .await
            .unwrap()
            .blocked_by,
        None
    );
    // A verifier cannot reach the other tenant's pin through this ledger.
    let mut foreign = f.observe("replica-a", Outcome::Mismatch, None).await;
    foreign.pin.index_version_id = g.version.clone();
    assert!(f
        .ledger
        .record_authoritative(&f.signed(&foreign).await, ENFORCE)
        .await
        .is_err());
}

// ---------------------------------------------------------------------------
// R10: the bounded history
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_full_history_keeps_its_findings_and_still_blocks_a_new_pin() {
    let url = guard!();
    let f = fixture(&url, "dmg-full").await;
    let ledger = f.ledger.clone().with_history_cap(3);
    let mut filler = f.observe("replica-z", Outcome::Unavailable, None).await;
    for _ in 0..3 {
        filler.fetch_id = uuid::Uuid::new_v4().to_string();
        let r = ledger
            .record_authoritative(&f.signed(&filler).await, ENFORCE)
            .await
            .unwrap();
        assert!(!r.history_full);
    }
    let before = ledger.history(10).await.unwrap();
    assert_eq!(before.len(), 3);

    let a = f.observe("replica-a", Outcome::Mismatch, None).await;
    let r = ledger
        .record_authoritative(&f.signed(&a).await, ENFORCE)
        .await
        .unwrap();
    assert_eq!(r.decision, Decision::Candidate);
    assert!(r.history_full, "the history refused the entry");
    let b = f.observe("replica-b", Outcome::Mismatch, None).await;
    let r = ledger
        .record_authoritative(&f.signed(&b).await, ENFORCE)
        .await
        .unwrap();
    assert!(matches!(r.decision, Decision::Blocked { .. }), "{r:?}");
    assert!(r.history_full);

    // Existing findings are preserved, none evicted, and the block's evidence
    // lives on the incident.
    assert_eq!(ledger.history(10).await.unwrap(), before);
    let incidents = ledger.incidents().await.unwrap();
    assert_eq!(
        incidents[0].confirming_verifiers,
        [f.fp("replica-a").await, f.fp("replica-b").await]
    );
}

// ---------------------------------------------------------------------------
// The release fence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_block_committed_before_release_refuses_the_in_flight_read() {
    let url = guard!();
    let f = fixture(&url, "dmg-fence").await;
    let admitted = f.ledger.admit(&f.version, &f.artifact).await.unwrap();
    assert_eq!(admitted.blocked_by, None);
    f.block().await;
    assert!(matches!(
        f.ledger.release(&[admitted]).await.unwrap(),
        ReleaseDecision::Refused(_)
    ));
}

#[tokio::test]
async fn a_result_released_before_the_block_stays_authorized() {
    let url = guard!();
    let f = fixture(&url, "dmg-fence-rev").await;
    assert_eq!(f.mismatch("replica-a").await, Decision::Candidate);
    let admitted = f.ledger.admit(&f.version, &f.artifact).await.unwrap();
    assert_eq!(
        f.ledger
            .release(std::slice::from_ref(&admitted))
            .await
            .unwrap(),
        ReleaseDecision::Authorized
    );
    f.mismatch("replica-b").await;
    // The authorization already given stands; a later release of the same
    // admission is refused.
    assert!(matches!(
        f.ledger.release(&[admitted]).await.unwrap(),
        ReleaseDecision::Refused(_)
    ));
}

// ---------------------------------------------------------------------------
// ART2: clearance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unauthorized_stale_and_single_match_repairs_cannot_clear() {
    let url = guard!();
    let f = fixture(&url, "dmg-noclear").await;
    let incident = f.block().await;

    // Two matches with no repair attempt are ordinary evidence.
    for v in ["replica-a", "replica-b"] {
        let o = f.observe(v, Outcome::Match, None).await;
        assert_eq!(f.record(&o).await, Decision::Recorded);
    }
    // An attempt nobody authorized.
    let o = f
        .observe("replica-a", Outcome::Match, Some("rep-forged"))
        .await;
    assert!(matches!(f.record(&o).await, Decision::Rejected(_)));
    assert!(f.blocked_by().await.is_some());

    // Fetched before the authorization: pre-repair observations.
    let early_a = f.observe("replica-a", Outcome::Match, None).await;
    let early_b = f.observe("replica-b", Outcome::Match, None).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let attempt = f
        .ledger
        .authorize_repair(&incident, "repair-workflow", "operator", Some("restored"))
        .await
        .unwrap();
    for mut early in [early_a, early_b] {
        early.repair_attempt_id = Some(attempt.attempt_id.clone());
        assert!(matches!(f.record(&early).await, Decision::Rejected(_)));
    }

    // One fresh match, then the same verifier again: still blocked.
    let one = f
        .observe("replica-a", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert_eq!(f.record(&one).await, Decision::RepairMatch);
    let same = f
        .observe("replica-a", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert_eq!(f.record(&same).await, Decision::SameVerifier);
    let mut replay = one.clone();
    replay.verifier.verifier_id = f.fp("replica-b").await;
    assert_eq!(f.record(&replay).await, Decision::ReplayedFetch);

    // A timeout, and a match on an object that is not the incident's.
    let timeout = f
        .observe("replica-b", Outcome::Unavailable, Some(&attempt.attempt_id))
        .await;
    assert_eq!(f.record(&timeout).await, Decision::Recorded);
    let mut manifest = f
        .observe("replica-b", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    manifest.pin.object_key = MANIFEST_OBJECT.into();
    manifest.pin.expected_sha256 = f.artifact.clone();
    manifest.observed_sha256 = Some(f.artifact.clone());
    assert!(matches!(f.record(&manifest).await, Decision::Rejected(_)));

    // An observe-mode verifier may not clear.
    let withheld = f
        .observe("replica-b", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert_eq!(
        f.ledger
            .record_authoritative(&f.signed(&withheld).await, OBSERVE)
            .await
            .unwrap()
            .decision,
        Decision::ClearWithheld
    );

    assert_eq!(f.blocked_by().await.as_deref(), Some(incident.as_str()));
}

#[tokio::test]
async fn a_fresh_mismatch_invalidates_the_attempt() {
    let url = guard!();
    let f = fixture(&url, "dmg-invalidate").await;
    let incident = f.block().await;
    let attempt = f
        .ledger
        .authorize_repair(&incident, "repair-workflow", "operator", None)
        .await
        .unwrap();
    let a = f
        .observe("replica-a", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert_eq!(f.record(&a).await, Decision::RepairMatch);
    let bad = f
        .observe("replica-c", Outcome::Mismatch, Some(&attempt.attempt_id))
        .await;
    assert_eq!(f.record(&bad).await, Decision::RepairInvalidated);
    let b = f
        .observe("replica-b", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert!(matches!(f.record(&b).await, Decision::Rejected(_)));
    assert!(f.blocked_by().await.is_some());
}

#[tokio::test]
async fn a_verified_repair_clears_once_and_records_the_clearance() {
    let url = guard!();
    let f = fixture(&url, "dmg-clear").await;
    let incident = f.block().await;
    let in_flight = f.ledger.admit(&f.version, &f.artifact).await.unwrap();
    let attempt = f
        .ledger
        .authorize_repair(&incident, "repair-workflow", "operator", None)
        .await
        .unwrap();
    // Repeated workflow delivery returns the same attempt.
    let again = f
        .ledger
        .authorize_repair(&incident, "repair-workflow", "operator", None)
        .await
        .unwrap();
    assert_eq!(again.attempt_id, attempt.attempt_id);

    let a = f
        .observe("replica-a", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert_eq!(f.record(&a).await, Decision::RepairMatch);
    let b = f
        .observe("replica-b", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    let generation = match f.record(&b).await {
        Decision::Cleared {
            incident_id,
            generation,
        } => {
            assert_eq!(incident_id, incident);
            generation
        }
        other => panic!("expected clearance, got {other:?}"),
    };
    assert_eq!(generation, 2);
    assert_eq!(f.blocked_by().await, None);

    let recorded = &f.ledger.incidents().await.unwrap()[0];
    assert_eq!(recorded.state, "cleared");
    assert_eq!(recorded.cleared_generation, Some(2));
    assert_eq!(
        recorded.clearing_attempt_id.as_deref(),
        Some(attempt.attempt_id.as_str())
    );
    assert_eq!(
        recorded.clearing_workflow_id.as_deref(),
        Some("repair-workflow")
    );
    assert_eq!(
        recorded.clearing_verifiers,
        Some([f.fp("replica-a").await, f.fp("replica-b").await])
    );

    // Idempotent: a repeated delivery changes nothing.
    let c = f
        .observe("replica-c", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert!(matches!(f.record(&c).await, Decision::Rejected(_)));
    assert_eq!(
        f.ledger
            .admit(&f.version, &f.artifact)
            .await
            .unwrap()
            .generation,
        2
    );
    assert!(f
        .ledger
        .authorize_repair(&incident, "repair-workflow", "operator", None)
        .await
        .is_err());

    // A read admitted before the block still fails release after clearance.
    assert!(matches!(
        f.ledger.release(&[in_flight]).await.unwrap(),
        ReleaseDecision::Refused(_)
    ));
    let fresh = f.ledger.admit(&f.version, &f.artifact).await.unwrap();
    assert_eq!(
        f.ledger.release(&[fresh]).await.unwrap(),
        ReleaseDecision::Authorized
    );

    // A fresh mismatch starts a new incident; a late report against the old
    // attempt cannot touch it.
    let new_incident = f.block().await;
    assert_ne!(new_incident, incident);
    let late = f
        .observe("replica-d", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert!(matches!(f.record(&late).await, Decision::Rejected(_)));
    assert_eq!(f.blocked_by().await.as_deref(), Some(new_incident.as_str()));
}

#[tokio::test]
async fn an_attempt_for_an_earlier_incident_cannot_clear_the_current_block() {
    let url = guard!();
    let f = fixture(&url, "dmg-late").await;
    let first = f.block().await;
    let old = f
        .ledger
        .authorize_repair(&first, "workflow-1", "operator", None)
        .await
        .unwrap();
    for v in ["replica-a", "replica-b"] {
        let o = f.observe(v, Outcome::Match, Some(&old.attempt_id)).await;
        f.record(&o).await;
    }
    let second = f.block().await;
    // A second authorization by another workflow supersedes nothing here
    // (the old attempt is cleared), and the old attempt stays inert.
    let o = f
        .observe("replica-e", Outcome::Match, Some(&old.attempt_id))
        .await;
    assert!(matches!(f.record(&o).await, Decision::Rejected(_)));
    let newer = f
        .ledger
        .authorize_repair(&second, "workflow-2", "operator", None)
        .await
        .unwrap();
    let superseding = f
        .ledger
        .authorize_repair(&second, "workflow-3", "operator", None)
        .await
        .unwrap();
    assert_ne!(newer.attempt_id, superseding.attempt_id);
    let o = f
        .observe("replica-a", Outcome::Match, Some(&newer.attempt_id))
        .await;
    assert!(
        matches!(f.record(&o).await, Decision::Rejected(_)),
        "a superseded attempt cannot progress"
    );
    assert_eq!(f.blocked_by().await.as_deref(), Some(second.as_str()));
}

/// Defense in depth: even a repair attempt row forced back to `verifying`
/// (a restored backup, a manual edit) cannot clear a block that is not its
/// own incident.
#[tokio::test]
async fn a_verifying_attempt_for_a_superseded_incident_is_inert() {
    let url = guard!();
    let f = fixture(&url, "dmg-stale-row").await;
    let first = f.block().await;
    let old = f
        .ledger
        .authorize_repair(&first, "workflow-1", "operator", None)
        .await
        .unwrap();
    for v in ["replica-a", "replica-b"] {
        let o = f.observe(v, Outcome::Match, Some(&old.attempt_id)).await;
        f.record(&o).await;
    }
    let second = f.block().await;
    sqlx::query(
        "UPDATE artifact_repair_attempts
            SET state = 'verifying', first_match_verifier_id = NULL, first_match_run_id = NULL,
                first_match_fetch_id = NULL, first_match_fetched_at = NULL,
                first_match_observed_sha256 = NULL, first_match_storage_generation = NULL
          WHERE tenant_id = $1 AND attempt_id = $2",
    )
    .bind(&f.tenant)
    .bind(&old.attempt_id)
    .execute(&f.pool)
    .await
    .unwrap();
    for v in ["replica-c", "replica-d"] {
        let o = f.observe(v, Outcome::Match, Some(&old.attempt_id)).await;
        assert!(matches!(f.record(&o).await, Decision::Rejected(_)));
    }
    assert_eq!(f.blocked_by().await.as_deref(), Some(second.as_str()));
}

// ---------------------------------------------------------------------------
// Single-replica assurance (amendment of 20 November 2026)
// ---------------------------------------------------------------------------

const INTERVAL: i64 = munarium_store_pg::artifact_damage::DEFAULT_SINGLE_REPLICA_INTERVAL_SECS;

fn secs(n: i64) -> chrono::Duration {
    chrono::Duration::seconds(n)
}

impl Fixture {
    fn single(&self) -> ArtifactDamageLedger {
        self.ledger.clone().with_single_replica(INTERVAL).unwrap()
    }

    /// The same verifier's next run: a fresh run id and a fresh fetch.
    fn next_run(
        &self,
        first: &AuthoritativeObservation,
        fetched_at: chrono::DateTime<chrono::Utc>,
    ) -> AuthoritativeObservation {
        let mut o = first.clone();
        o.verifier.run_id = uuid::Uuid::new_v4().to_string();
        o.fetch_id = uuid::Uuid::new_v4().to_string();
        o.fetched_at = fetched_at;
        o
    }

    async fn incident(&self) -> munarium_store_pg::artifact_damage::Incident {
        self.ledger.incidents().await.unwrap().remove(0)
    }
}

impl Fixture {
    async fn decide(
        &self,
        ledger: &ArtifactDamageLedger,
        o: &AuthoritativeObservation,
    ) -> Decision {
        ledger
            .record_authoritative(&self.signed(o).await, ENFORCE)
            .await
            .unwrap()
            .decision
    }
}

#[tokio::test]
async fn single_replica_blocks_only_after_two_spaced_runs_and_fetches() {
    let url = guard!();
    let f = fixture(&url, "dmg-sr-block").await;
    let single = f.single();
    let now = f.ledger.db_now().await.unwrap();
    let mut first = f.observe("replica-s", Outcome::Mismatch, None).await;
    first.fetched_at = now - secs(INTERVAL + 60);
    assert_eq!(f.decide(&single, &first).await, Decision::Candidate);

    // One fetch replayed counts once, under any name or run.
    let mut replay = first.clone();
    replay.verifier.run_id = "another-run".into();
    assert_eq!(f.decide(&single, &replay).await, Decision::ReplayedFetch);
    // The same run with a fresh fetch is not a second run.
    let mut same_run = first.clone();
    same_run.fetch_id = uuid::Uuid::new_v4().to_string();
    same_run.fetched_at = now;
    assert_eq!(f.decide(&single, &same_run).await, Decision::SameVerifier);
    // A second run and fetch inside the interval does not count, nor does
    // one a second short of it, nor a fetch time in the future.
    for at in [
        first.fetched_at + secs(1),
        first.fetched_at + secs(INTERVAL - 1),
        now + secs(INTERVAL),
    ] {
        let soon = f.next_run(&first, at);
        assert_eq!(f.decide(&single, &soon).await, Decision::IntervalNotElapsed);
    }
    assert_eq!(f.blocked_by().await, None);
    assert_eq!(f.ledger.incidents().await.unwrap(), vec![]);

    // A second run and fetch at least the interval later blocks.
    let spaced = f.next_run(&first, now);
    let incident = match f.decide(&single, &spaced).await {
        Decision::Blocked {
            incident_id,
            generation,
        } => {
            assert_eq!(generation, 1);
            incident_id
        }
        other => panic!("expected a single-replica block, got {other:?}"),
    };
    assert_eq!(f.blocked_by().await.as_deref(), Some(incident.as_str()));
    let recorded = f.incident().await;
    assert_eq!(recorded.block_assurance, "single_replica");
    assert_eq!(
        recorded.confirming_verifiers,
        [f.fp("replica-s").await, f.fp("replica-s").await]
    );
    assert_eq!(recorded.clear_assurance, None);
}

#[tokio::test]
async fn the_default_ledger_never_counts_one_verifier_twice_however_spaced() {
    let url = guard!();
    let f = fixture(&url, "dmg-sr-default").await;
    assert_eq!(f.ledger.single_replica_interval_secs(), None);
    let now = f.ledger.db_now().await.unwrap();
    let mut first = f.observe("replica-s", Outcome::Mismatch, None).await;
    first.fetched_at = now - secs(6 * 60 * 60);
    assert_eq!(f.record(&first).await, Decision::Candidate);
    let spaced = f.next_run(&first, now);
    assert_eq!(f.record(&spaced).await, Decision::SameVerifier);
    assert_eq!(f.blocked_by().await, None);

    // Two verifiers block as before and record independent assurance.
    assert!(matches!(
        f.mismatch("replica-b").await,
        Decision::Blocked { .. }
    ));
    assert_eq!(f.incident().await.block_assurance, "independent");
}

#[tokio::test]
async fn single_replica_mode_still_records_two_verifiers_as_independent() {
    let url = guard!();
    let f = fixture(&url, "dmg-sr-indep").await;
    let single = f.single();
    let a = f.observe("replica-a", Outcome::Mismatch, None).await;
    assert_eq!(f.decide(&single, &a).await, Decision::Candidate);
    let b = f.observe("replica-b", Outcome::Mismatch, None).await;
    assert!(matches!(
        f.decide(&single, &b).await,
        Decision::Blocked { .. }
    ));
    assert_eq!(f.incident().await.block_assurance, "independent");
}

#[tokio::test]
async fn single_replica_clears_after_two_spaced_fresh_matches_with_authorization() {
    let url = guard!();
    let f = fixture(&url, "dmg-sr-clear").await;
    let single = f.single();
    let now = f.ledger.db_now().await.unwrap();
    let mut first = f.observe("replica-s", Outcome::Mismatch, None).await;
    first.fetched_at = now - secs(INTERVAL + 60);
    assert_eq!(f.decide(&single, &first).await, Decision::Candidate);
    let incident = match f.decide(&single, &f.next_run(&first, now)).await {
        Decision::Blocked { incident_id, .. } => incident_id,
        other => panic!("expected a block, got {other:?}"),
    };

    // Matches with no authorization are evidence only.
    let unauthorized = f.observe("replica-s", Outcome::Match, None).await;
    assert_eq!(f.decide(&single, &unauthorized).await, Decision::Recorded);

    let attempt = f
        .ledger
        .authorize_repair(&incident, "repair-workflow", "operator", None)
        .await
        .unwrap();
    // Open the verification window in the past so the two matches can be
    // spaced without waiting.
    sqlx::query(
        "UPDATE artifact_repair_attempts SET authorized_at = authorized_at - interval '1 hour'
          WHERE tenant_id = $1 AND attempt_id = $2",
    )
    .bind(&f.tenant)
    .bind(&attempt.attempt_id)
    .execute(&f.pool)
    .await
    .unwrap();
    let now = f.ledger.db_now().await.unwrap();
    let mut one = f
        .observe("replica-s", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    one.fetched_at = now - secs(INTERVAL + 30);
    assert_eq!(f.decide(&single, &one).await, Decision::RepairMatch);

    // The default rule never accepts the same verifier's second match.
    assert_eq!(
        f.record(&f.next_run(&one, now)).await,
        Decision::SameVerifier
    );
    // Too soon, or replayed, does not count.
    let soon = f.next_run(&one, one.fetched_at + secs(INTERVAL - 1));
    assert_eq!(f.decide(&single, &soon).await, Decision::IntervalNotElapsed);
    let mut replay = one.clone();
    replay.verifier.run_id = "another-run".into();
    assert_eq!(f.decide(&single, &replay).await, Decision::ReplayedFetch);
    assert_eq!(f.blocked_by().await.as_deref(), Some(incident.as_str()));

    // A second spaced run and fetch clears, once.
    let generation = match f.decide(&single, &f.next_run(&one, now)).await {
        Decision::Cleared {
            incident_id,
            generation,
        } => {
            assert_eq!(incident_id, incident);
            generation
        }
        other => panic!("expected clearance, got {other:?}"),
    };
    assert_eq!(generation, 2);
    assert_eq!(f.blocked_by().await, None);
    let recorded = f.incident().await;
    assert_eq!(recorded.state, "cleared");
    assert_eq!(recorded.block_assurance, "single_replica");
    assert_eq!(recorded.clear_assurance.as_deref(), Some("single_replica"));
    assert_eq!(
        recorded.clearing_verifiers,
        Some([f.fp("replica-s").await, f.fp("replica-s").await])
    );
    assert_eq!(
        recorded.clearing_workflow_id.as_deref(),
        Some("repair-workflow")
    );
    let late = f.next_run(&one, now);
    assert!(matches!(
        f.decide(&single, &late).await,
        Decision::Rejected(_)
    ));
}

#[tokio::test]
async fn a_mismatch_from_a_single_replica_still_invalidates_its_repair() {
    let url = guard!();
    let f = fixture(&url, "dmg-sr-inval").await;
    let incident = f.block().await;
    let single = f.single();
    let attempt = f
        .ledger
        .authorize_repair(&incident, "repair-workflow", "operator", None)
        .await
        .unwrap();
    let one = f
        .observe("replica-s", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert_eq!(f.decide(&single, &one).await, Decision::RepairMatch);
    let bad = f
        .observe("replica-s", Outcome::Mismatch, Some(&attempt.attempt_id))
        .await;
    assert_eq!(f.decide(&single, &bad).await, Decision::RepairInvalidated);
    assert!(f.blocked_by().await.is_some());
}

#[tokio::test]
async fn single_replica_intervals_outside_the_accepted_range_are_refused() {
    let url = guard!();
    let f = fixture(&url, "dmg-sr-range").await;
    for bad in [0, 59, 3601, -1] {
        assert!(f.ledger.clone().with_single_replica(bad).is_err(), "{bad}");
    }
    for good in [60, INTERVAL, 3600] {
        assert_eq!(
            f.ledger
                .clone()
                .with_single_replica(good)
                .unwrap()
                .single_replica_interval_secs(),
            Some(good)
        );
    }
}

/// Defense in depth: the schema refuses a same-verifier incident unless it
/// is marked single-replica with distinct runs and fetches at least 60
/// seconds apart, whatever code writes it.
#[tokio::test]
async fn the_schema_admits_a_same_verifier_block_only_when_marked_and_spaced() {
    let url = guard!();
    let f = fixture(&url, "dmg-sr-schema").await;
    f.ledger
        .record_local_isolation(
            &f.version,
            &f.artifact,
            &VerifierIdentity {
                verifier_id: "replica-s".into(),
                run_id: "r0".into(),
            },
            None,
        )
        .await
        .unwrap();
    for (assurance, runs, gap, accepted) in [
        ("independent", ("r1", "r2"), 600.0, false),
        ("single_replica", ("r1", "r1"), 600.0, false),
        ("single_replica", ("r1", "r2"), 30.0, false),
        ("weak", ("r1", "r2"), 600.0, false),
        ("single_replica", ("r1", "r2"), 600.0, true),
    ] {
        let inserted = sqlx::query(
            "INSERT INTO artifact_integrity_incidents
                 (tenant_id, incident_id, index_version_id, artifact_id, object_key,
                  expected_sha256, state, blocked_generation, first_verifier_id,
                  first_run_id, first_fetch_id, first_fetched_at, first_observed_sha256,
                  second_verifier_id, second_run_id, second_fetch_id, second_fetched_at,
                  second_observed_sha256, block_assurance)
             VALUES ($1, $2, $3, $4, 'x', $5, 'blocked', 1, 'replica-s', $6, 'f1',
                     now() - make_interval(secs => $8), $5, 'replica-s', $7, 'f2', now(),
                     $5, $9)",
        )
        .bind(&f.tenant)
        .bind(format!("inc-{}", uuid::Uuid::new_v4().simple()))
        .bind(&f.version)
        .bind(&f.artifact)
        .bind(hex('b'))
        .bind(runs.0)
        .bind(runs.1)
        .bind(gap)
        .bind(assurance)
        .execute(&f.pool)
        .await;
        assert_eq!(inserted.is_ok(), accepted, "{assurance} {runs:?} {gap}");
    }
}

// ---------------------------------------------------------------------------
// Authenticated verifier identity (ADR 0028 amendment, 20 November 2026)
// ---------------------------------------------------------------------------

/// Record without the fixture's signing step.
async fn raw(f: &Fixture, o: &AuthoritativeObservation) -> Decision {
    f.ledger
        .record_authoritative(o, ENFORCE)
        .await
        .unwrap()
        .decision
}

fn unauthenticated(d: &Decision) -> bool {
    matches!(d, Decision::Unauthenticated(_))
}

#[tokio::test]
async fn an_unsigned_or_forged_observation_counts_for_nothing() {
    let url = guard!();
    let f = fixture(&url, "dmg-auth-forged").await;
    // Unsigned, from an enrolled verifier's identity.
    let unsigned = f.observe("replica-a", Outcome::Mismatch, None).await;
    assert!(unauthenticated(&raw(&f, &unsigned).await));
    // A signature that does not verify.
    let mut forged = f
        .signed(&f.observe("replica-a", Outcome::Mismatch, None).await)
        .await;
    forged.signature.as_mut().unwrap().signature = "A".repeat(86);
    assert!(unauthenticated(&raw(&f, &forged).await));
    // Signed by another enrolled key, claiming replica-a's identity.
    let mut borrowed = f.observe("replica-a", Outcome::Mismatch, None).await;
    let other = f.key("replica-b").await;
    borrowed.signature = Some(ObservationSignature {
        pin_generation: 0,
        signature: other.sign(&borrowed.claims(&f.tenant, 0)),
    });
    assert!(unauthenticated(&raw(&f, &borrowed).await));
    // Validly signed, then changed: the digest it claims to have seen.
    let mut altered = f
        .signed(&f.observe("replica-a", Outcome::Mismatch, None).await)
        .await;
    altered.observed_sha256 = Some(hex('e'));
    assert!(unauthenticated(&raw(&f, &altered).await));
    // Signed for another tenant or another generation.
    let mut elsewhere = f.observe("replica-a", Outcome::Mismatch, None).await;
    let key = f.key("replica-a").await;
    elsewhere.signature = Some(ObservationSignature {
        pin_generation: 0,
        signature: key.sign(&elsewhere.claims("another-tenant", 0)),
    });
    assert!(unauthenticated(&raw(&f, &elsewhere).await));
    let mut later = f.observe("replica-a", Outcome::Mismatch, None).await;
    later.signature = Some(ObservationSignature {
        pin_generation: 1,
        signature: key.sign(&later.claims(&f.tenant, 1)),
    });
    assert!(unauthenticated(&raw(&f, &later).await));

    // None of them left a candidate: the first authenticated mismatch is
    // still only a candidate, and a second key blocks.
    assert_eq!(f.blocked_by().await, None);
    assert_eq!(f.mismatch("replica-a").await, Decision::Candidate);
    assert!(matches!(
        f.mismatch("replica-b").await,
        Decision::Blocked { .. }
    ));
    let history = f.ledger.history(20).await.unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|(_, _, decision)| decision == "unauthenticated")
            .count(),
        6
    );
}

#[tokio::test]
async fn an_unenrolled_key_never_counts() {
    let url = guard!();
    let f = fixture(&url, "dmg-auth-unenrolled").await;
    assert_eq!(f.mismatch("replica-a").await, Decision::Candidate);
    let stranger = VerifierSigningKey::generate();
    let mut o = f.observe("replica-a", Outcome::Mismatch, None).await;
    o.verifier.verifier_id = stranger.fingerprint();
    o.signature = Some(ObservationSignature {
        pin_generation: 0,
        signature: stranger.sign(&o.claims(&f.tenant, 0)),
    });
    assert!(unauthenticated(&raw(&f, &o).await));
    assert_eq!(f.blocked_by().await, None);
}

#[tokio::test]
async fn two_replicas_sharing_one_key_are_one_verifier() {
    let url = guard!();
    let f = fixture(&url, "dmg-auth-shared").await;
    assert_eq!(f.mismatch("replica-a").await, Decision::Candidate);
    // A second replica with a copy of the same key: a fresh run and fetch,
    // the same identity.
    let mut copy = f.observe("replica-a", Outcome::Mismatch, None).await;
    copy.verifier.run_id = "second-replica-run".into();
    assert_eq!(f.record(&copy).await, Decision::SameVerifier);
    assert_eq!(f.blocked_by().await, None);
}

#[tokio::test]
async fn a_revoked_key_counts_for_nothing_and_its_evidence_is_not_confirmed() {
    let url = guard!();
    let f = fixture(&url, "dmg-auth-revoked").await;
    assert_eq!(f.mismatch("replica-a").await, Decision::Candidate);
    let a = f.fp("replica-a").await;
    let revoked = revoke_verifier(&f.pool, &a, "operator", Some("compromised"))
        .await
        .unwrap();
    assert!(revoked.revoked_at.is_some());
    // Its new observations count for nothing.
    let o = f.observe("replica-a", Outcome::Mismatch, None).await;
    assert!(unauthenticated(&f.record(&o).await));
    // Its earlier candidate is not confirmed: replica-b's mismatch replaces
    // it, and a third key is needed.
    assert_eq!(f.mismatch("replica-b").await, Decision::Candidate);
    assert_eq!(f.blocked_by().await, None);
    let incident = match f.mismatch("replica-c").await {
        Decision::Blocked { incident_id, .. } => incident_id,
        other => panic!("expected a block, got {other:?}"),
    };
    assert_eq!(
        f.incident().await.confirming_verifiers,
        [f.fp("replica-b").await, f.fp("replica-c").await]
    );

    // The same during repair: a first match from a key revoked since is
    // replaced, never confirmed.
    let attempt = f
        .ledger
        .authorize_repair(&incident, "repair-workflow", "operator", None)
        .await
        .unwrap();
    let m = f
        .observe("replica-b", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert_eq!(f.record(&m).await, Decision::RepairMatch);
    revoke_verifier(&f.pool, &f.fp("replica-b").await, "operator", None)
        .await
        .unwrap();
    let m = f
        .observe("replica-c", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert_eq!(f.record(&m).await, Decision::RepairMatch);
    assert!(f.blocked_by().await.is_some());
    let m = f
        .observe("replica-d", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert!(matches!(f.record(&m).await, Decision::Cleared { .. }));
    assert_eq!(
        f.incident().await.clearing_verifiers,
        Some([f.fp("replica-c").await, f.fp("replica-d").await])
    );
    // Committed evidence keeps the revoked fingerprint it recorded.
    assert_eq!(
        f.incident().await.confirming_verifiers[0],
        f.fp("replica-b").await
    );
}

/// Begin revoking `fingerprint` on a connection of its own and leave the
/// revocation uncommitted, holding the key row's lock.
async fn revoking(f: &Fixture, fingerprint: &str) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut tx = f.pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE artifact_verifier_keys
            SET revoked_at = now(), revoked_by = 'operator', revoke_reason = 'race'
          WHERE fingerprint = $1 AND revoked_at IS NULL",
    )
    .bind(fingerprint)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx
}

/// Record `o` while a revocation of another key is in flight, and commit
/// that revocation while the record is (with the share lock) waiting for
/// it. Without the lock the record decides on the stale, unrevoked row and
/// commits before the revocation does.
async fn record_racing(
    f: &Fixture,
    o: &AuthoritativeObservation,
    revocation: sqlx::Transaction<'static, sqlx::Postgres>,
) -> Decision {
    let signed = f.signed(o).await;
    let (recorded, ()) = tokio::join!(
        f.ledger.record_authoritative(&signed, ENFORCE),
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(750)).await;
            revocation.commit().await.unwrap();
        }
    );
    recorded.unwrap().decision
}

/// A revocation of the stored first observation's key that commits before
/// the second observation's decision is honoured: the decision waits on
/// the key row, sees it revoked, and never confirms on it.
#[tokio::test]
async fn a_revocation_racing_a_confirmation_is_honoured() {
    let url = guard!();
    let f = fixture(&url, "dmg-auth-race").await;
    assert_eq!(f.mismatch("replica-a").await, Decision::Candidate);
    let o = f.observe("replica-b", Outcome::Mismatch, None).await;
    let revocation = revoking(&f, &f.fp("replica-a").await).await;
    assert_eq!(record_racing(&f, &o, revocation).await, Decision::Candidate);
    assert_eq!(f.blocked_by().await, None);
    // replica-b's mismatch is now the candidate: a third key confirms it.
    assert!(matches!(
        f.mismatch("replica-c").await,
        Decision::Blocked { .. }
    ));
    assert_eq!(
        f.incident().await.confirming_verifiers,
        [f.fp("replica-b").await, f.fp("replica-c").await]
    );
}

/// The same for a repair: a first match whose key's revocation commits
/// before the second match's decision is replaced, never cleared on.
#[tokio::test]
async fn a_revocation_racing_a_clearance_is_honoured() {
    let url = guard!();
    let f = fixture(&url, "dmg-auth-race-clear").await;
    let incident = f.block().await;
    let attempt = f
        .ledger
        .authorize_repair(&incident, "repair-workflow", "operator", None)
        .await
        .unwrap();
    let m = f
        .observe("replica-c", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    assert_eq!(f.record(&m).await, Decision::RepairMatch);
    let o = f
        .observe("replica-d", Outcome::Match, Some(&attempt.attempt_id))
        .await;
    let revocation = revoking(&f, &f.fp("replica-c").await).await;
    assert_eq!(
        record_racing(&f, &o, revocation).await,
        Decision::RepairMatch
    );
    assert!(f.blocked_by().await.is_some());
    let progress = f
        .ledger
        .repair_attempt(&attempt.attempt_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        progress.first_match_verifier_id,
        Some(f.fp("replica-d").await)
    );
}

#[tokio::test]
async fn enrollment_is_idempotent_and_revocation_is_permanent() {
    let url = guard!();
    let f = fixture(&url, "dmg-auth-enroll").await;
    let key = VerifierSigningKey::generate();
    let first = enroll_verifier(&f.pool, &key.public_key(), "replica-x", "operator")
        .await
        .unwrap();
    assert_eq!(first.fingerprint, key.fingerprint());
    let again = enroll_verifier(&f.pool, &key.public_key(), "replica-x", "operator")
        .await
        .unwrap();
    assert_eq!(again, first);
    for bad in ["", "not-a-key", &format!("{}=", key.public_key())] {
        assert!(enroll_verifier(&f.pool, bad, "x", "operator")
            .await
            .is_err());
    }
    revoke_verifier(&f.pool, &key.fingerprint(), "operator", None)
        .await
        .unwrap();
    assert!(
        enroll_verifier(&f.pool, &key.public_key(), "replica-x", "operator")
            .await
            .is_err(),
        "a revoked key cannot come back"
    );
    let unknown = VerifierSigningKey::generate();
    assert!(
        revoke_verifier(&f.pool, &unknown.fingerprint(), "operator", None)
            .await
            .is_err()
    );
}

// ---------------------------------------------------------------------------
// Storage generations (decision of 20 November 2026): two observations of
// different stored generations are different pinned objects, and an absent
// generation never combines with a known one.
// ---------------------------------------------------------------------------

impl Fixture {
    async fn at_generation(
        &self,
        verifier: &str,
        outcome: Outcome,
        attempt: Option<&str>,
        generation: Option<&str>,
    ) -> Decision {
        let mut o = self.observe(verifier, outcome, attempt).await;
        o.pin.storage_generation = generation.map(str::to_string);
        self.record(&o).await
    }

    async fn only_incident(&self) -> munarium_store_pg::artifact_damage::Incident {
        let mut all = self.ledger.incidents().await.unwrap();
        assert_eq!(all.len(), 1, "{all:?}");
        all.remove(0)
    }
}

#[tokio::test]
async fn mismatches_of_different_stored_generations_never_combine_toward_a_block() {
    let url = guard!();
    let f = fixture(&url, "dmg-gen-block").await;
    let m = Outcome::Mismatch;
    assert_eq!(
        f.at_generation("replica-a", m, None, Some("etag:1")).await,
        Decision::Candidate
    );
    // A second verifier on another generation starts its own candidate.
    assert_eq!(
        f.at_generation("replica-b", m, None, Some("etag:2")).await,
        Decision::Candidate
    );
    // Mixed: an absent generation is its own value and confirms neither.
    assert_eq!(
        f.at_generation("replica-c", m, None, None).await,
        Decision::Candidate
    );
    assert_eq!(f.blocked_by().await, None);
    assert_eq!(f.ledger.incidents().await.unwrap(), vec![]);

    // The same generation from an independent verifier confirms, and the
    // incident records the generation it was confirmed on.
    let d = f.at_generation("replica-d", m, None, Some("etag:2")).await;
    assert!(matches!(d, Decision::Blocked { .. }), "{d:?}");
    let incident = f.only_incident().await;
    assert_eq!(incident.storage_generation.as_deref(), Some("etag:2"));
    assert_eq!(incident.confirming_verifiers[0], f.fp("replica-b").await);
}

#[tokio::test]
async fn an_absent_generation_never_combines_with_a_known_one_toward_a_block() {
    let url = guard!();
    let f = fixture(&url, "dmg-gen-mixed").await;
    let m = Outcome::Mismatch;
    assert_eq!(
        f.at_generation("replica-a", m, None, None).await,
        Decision::Candidate
    );
    assert_eq!(
        f.at_generation("replica-b", m, None, Some("etag:1")).await,
        Decision::Candidate
    );
    assert_eq!(f.blocked_by().await, None);
    // Two absent generations still combine exactly as before the decision.
    let d = f.at_generation("replica-c", m, None, None).await;
    assert!(matches!(d, Decision::Blocked { .. }), "{d:?}");
    assert_eq!(f.only_incident().await.storage_generation, None);
}

#[tokio::test]
async fn repair_matches_of_different_stored_generations_never_clear() {
    let url = guard!();
    let f = fixture(&url, "dmg-gen-clear").await;
    let incident = f.block().await;
    let attempt = f
        .ledger
        .authorize_repair(&incident, "repair-workflow", "operator", None)
        .await
        .unwrap();
    let a = Some(attempt.attempt_id.as_str());
    let ok = Outcome::Match;
    assert_eq!(
        f.at_generation("replica-a", ok, a, Some("etag:7")).await,
        Decision::RepairMatch
    );
    // An independent match on another stored generation does not clear: it
    // replaces the first match, because the earlier one says nothing about
    // the object now stored.
    assert_eq!(
        f.at_generation("replica-b", ok, a, Some("etag:8")).await,
        Decision::RepairMatch
    );
    // Mixed: an absent generation replaces again and does not clear.
    assert_eq!(
        f.at_generation("replica-c", ok, a, None).await,
        Decision::RepairMatch
    );
    assert_eq!(
        f.at_generation("replica-a", ok, a, Some("etag:8")).await,
        Decision::RepairMatch
    );
    assert_eq!(f.blocked_by().await.as_deref(), Some(incident.as_str()));

    // Two independent matches on one generation clear, and the clearance
    // reports that generation.
    let d = f.at_generation("replica-b", ok, a, Some("etag:8")).await;
    assert!(matches!(d, Decision::Cleared { .. }), "{d:?}");
    assert_eq!(f.blocked_by().await, None);
    let cleared = f.only_incident().await;
    assert_eq!(cleared.state, "cleared");
    assert_eq!(cleared.clear_storage_generation.as_deref(), Some("etag:8"));
    assert_eq!(cleared.storage_generation, None, "blocked on an absent one");
}

#[tokio::test]
async fn absent_generation_repairs_clear_as_before() {
    let url = guard!();
    let f = fixture(&url, "dmg-gen-absent").await;
    let incident = f.block().await;
    let attempt = f
        .ledger
        .authorize_repair(&incident, "repair-workflow", "operator", None)
        .await
        .unwrap();
    let a = Some(attempt.attempt_id.as_str());
    assert_eq!(
        f.at_generation("replica-a", Outcome::Match, a, None).await,
        Decision::RepairMatch
    );
    let d = f.at_generation("replica-b", Outcome::Match, a, None).await;
    assert!(matches!(d, Decision::Cleared { .. }), "{d:?}");
    assert_eq!(f.only_incident().await.clear_storage_generation, None);
}
