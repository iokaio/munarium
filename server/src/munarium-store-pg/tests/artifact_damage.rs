// SPDX-License-Identifier: Apache-2.0
//! The artifact damage ledger against a real PostgreSQL (ClearView R10, ART1,
//! ART2).
//!
//! Gated on `MUNARIUM_TEST_DATABASE_URL`, and skips loudly when it is unset.
//! Every test uses its own tenant, so the bounded history and the cross-tenant
//! repair enumeration never see another test's rows.

use munarium_store_pg::artifact_damage::{
    ArtifactDamageLedger, AuthoritativeObservation, Authority, Decision, ObjectPin, Outcome,
    ReleaseDecision, VerifierIdentity, MANIFEST_OBJECT,
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

    async fn observe(
        &self,
        verifier: &str,
        outcome: Outcome,
        attempt: Option<&str>,
    ) -> AuthoritativeObservation {
        AuthoritativeObservation {
            pin: self.pin(),
            verifier: VerifierIdentity {
                verifier_id: verifier.into(),
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
        }
    }

    async fn record(&self, o: &AuthoritativeObservation) -> Decision {
        self.ledger
            .record_authoritative(o, ENFORCE)
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
    renamed.verifier.verifier_id = "replica-b".into();
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
        ["replica-a".to_string(), "replica-b".to_string()]
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
        .record_authoritative(&second, OBSERVE)
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
        .record_authoritative(&foreign, ENFORCE)
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
        let r = ledger.record_authoritative(&filler, ENFORCE).await.unwrap();
        assert!(!r.history_full);
    }
    let before = ledger.history(10).await.unwrap();
    assert_eq!(before.len(), 3);

    let a = f.observe("replica-a", Outcome::Mismatch, None).await;
    let r = ledger.record_authoritative(&a, ENFORCE).await.unwrap();
    assert_eq!(r.decision, Decision::Candidate);
    assert!(r.history_full, "the history refused the entry");
    let b = f.observe("replica-b", Outcome::Mismatch, None).await;
    let r = ledger.record_authoritative(&b, ENFORCE).await.unwrap();
    assert!(matches!(r.decision, Decision::Blocked { .. }), "{r:?}");
    assert!(r.history_full);

    // Existing findings are preserved, none evicted, and the block's evidence
    // lives on the incident.
    assert_eq!(ledger.history(10).await.unwrap(), before);
    let incidents = ledger.incidents().await.unwrap();
    assert_eq!(
        incidents[0].confirming_verifiers,
        ["replica-a".to_string(), "replica-b".to_string()]
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
    replay.verifier.verifier_id = "replica-b".into();
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
            .record_authoritative(&withheld, OBSERVE)
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
        Some(["replica-a".to_string(), "replica-b".to_string()])
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

async fn decide(ledger: &ArtifactDamageLedger, o: &AuthoritativeObservation) -> Decision {
    ledger
        .record_authoritative(o, ENFORCE)
        .await
        .unwrap()
        .decision
}

#[tokio::test]
async fn single_replica_blocks_only_after_two_spaced_runs_and_fetches() {
    let url = guard!();
    let f = fixture(&url, "dmg-sr-block").await;
    let single = f.single();
    let now = f.ledger.db_now().await.unwrap();
    let mut first = f.observe("replica-s", Outcome::Mismatch, None).await;
    first.fetched_at = now - secs(INTERVAL + 60);
    assert_eq!(decide(&single, &first).await, Decision::Candidate);

    // One fetch replayed counts once, under any name or run.
    let mut replay = first.clone();
    replay.verifier.run_id = "another-run".into();
    assert_eq!(decide(&single, &replay).await, Decision::ReplayedFetch);
    // The same run with a fresh fetch is not a second run.
    let mut same_run = first.clone();
    same_run.fetch_id = uuid::Uuid::new_v4().to_string();
    same_run.fetched_at = now;
    assert_eq!(decide(&single, &same_run).await, Decision::SameVerifier);
    // A second run and fetch inside the interval does not count, nor does
    // one a second short of it, nor a fetch time in the future.
    for at in [
        first.fetched_at + secs(1),
        first.fetched_at + secs(INTERVAL - 1),
        now + secs(INTERVAL),
    ] {
        let soon = f.next_run(&first, at);
        assert_eq!(decide(&single, &soon).await, Decision::IntervalNotElapsed);
    }
    assert_eq!(f.blocked_by().await, None);
    assert_eq!(f.ledger.incidents().await.unwrap(), vec![]);

    // A second run and fetch at least the interval later blocks.
    let spaced = f.next_run(&first, now);
    let incident = match decide(&single, &spaced).await {
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
        ["replica-s".to_string(), "replica-s".to_string()]
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
    assert_eq!(decide(&single, &a).await, Decision::Candidate);
    let b = f.observe("replica-b", Outcome::Mismatch, None).await;
    assert!(matches!(
        decide(&single, &b).await,
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
    assert_eq!(decide(&single, &first).await, Decision::Candidate);
    let incident = match decide(&single, &f.next_run(&first, now)).await {
        Decision::Blocked { incident_id, .. } => incident_id,
        other => panic!("expected a block, got {other:?}"),
    };

    // Matches with no authorization are evidence only.
    let unauthorized = f.observe("replica-s", Outcome::Match, None).await;
    assert_eq!(decide(&single, &unauthorized).await, Decision::Recorded);

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
    assert_eq!(decide(&single, &one).await, Decision::RepairMatch);

    // The default rule never accepts the same verifier's second match.
    assert_eq!(
        f.record(&f.next_run(&one, now)).await,
        Decision::SameVerifier
    );
    // Too soon, or replayed, does not count.
    let soon = f.next_run(&one, one.fetched_at + secs(INTERVAL - 1));
    assert_eq!(decide(&single, &soon).await, Decision::IntervalNotElapsed);
    let mut replay = one.clone();
    replay.verifier.run_id = "another-run".into();
    assert_eq!(decide(&single, &replay).await, Decision::ReplayedFetch);
    assert_eq!(f.blocked_by().await.as_deref(), Some(incident.as_str()));

    // A second spaced run and fetch clears, once.
    let generation = match decide(&single, &f.next_run(&one, now)).await {
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
        Some(["replica-s".to_string(), "replica-s".to_string()])
    );
    assert_eq!(
        recorded.clearing_workflow_id.as_deref(),
        Some("repair-workflow")
    );
    let late = f.next_run(&one, now);
    assert!(matches!(
        decide(&single, &late).await,
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
    assert_eq!(decide(&single, &one).await, Decision::RepairMatch);
    let bad = f
        .observe("replica-s", Outcome::Mismatch, Some(&attempt.attempt_id))
        .await;
    assert_eq!(decide(&single, &bad).await, Decision::RepairInvalidated);
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
