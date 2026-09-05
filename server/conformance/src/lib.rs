// SPDX-License-Identifier: Apache-2.0
//! MMP conformance scenarios, written against the `StorageBackend` trait so
//! the SAME assertions run on the in-memory store (M0), the Postgres store
//! (M1), and — through the client adapters — the running server's REST and
//! gRPC planes (M2+, with cross-plane parity diffing).
//!
//! The pin fixtures are the crown jewels: they encode the one-pin-bounds-
//! everything semantic that defines the mesh.

pub mod clients;
pub mod cluster;
pub mod platform;

use munarium_core::composer::compose;
use munarium_core::gates::{blocked_claim_keys, run_gates};
use munarium_core::ledger::FactQuery;
use munarium_core::storage::{load_snapshot, NewClaim, StorageBackend};
use munarium_core::types::*;
use munarium_core::KernelError;

pub type ScenarioResult = Result<(), String>;

macro_rules! expect {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond { return Err(format!($($msg)+)); }
    };
}

/// All scenarios, name -> future. Add here; every backend runs the full set.
pub async fn run_all(store: &dyn StorageBackend) -> Vec<(&'static str, ScenarioResult)> {
    vec![
        (
            "ledger.append-head-conflict",
            ledger_append_head_conflict(store).await,
        ),
        (
            "ledger.supersession-pin",
            ledger_supersession_pin(store).await,
        ),
        (
            "pins.one-pin-bounds-all-stores",
            one_pin_bounds_all_stores(store).await,
        ),
        (
            "gates.block-records-disputed",
            gates_block_records_disputed(store).await,
        ),
        (
            "composer.budget-degradation",
            composer_budget_degradation(store).await,
        ),
        (
            "digests.rebuilt-under-pin",
            digests_rebuilt_under_pin(store).await,
        ),
        (
            "gates.chronology-certain-only",
            gates_chronology_certain_only(store).await,
        ),
        (
            "ledger.origin-round-trips",
            ledger_origin_round_trips(store).await,
        ),
    ]
}

/// A connector claim's origin survives the round trip on EVERY
/// backend: mem, pg, and — through the client adapters — REST and gRPC.
/// Running the same assertion on all four is what makes "origin is on the
/// wire" a checked statement rather than a documented intention.
async fn ledger_origin_round_trips(store: &dyn StorageBackend) -> ScenarioResult {
    let v = store
        .create_version(None, None)
        .await
        .map_err(|e| e.to_string())?;
    let origin = ClaimOrigin {
        kind: "connector".into(),
        source_id: "crm".into(),
        mapping_version: "captable-holdings@1".into(),
        row_key: "holder_id=43".into(),
        event_position: Some("lsn/0/1A2B".into()),
        observed_at: Some("2026-08-28T09:15:00Z".into()),
        evidence_id: Some("ev-batch-0001".into()),
    };
    let mut claim = NewClaim::fact("shareholder.43", "shares", "90500");
    claim.origin = Some(origin.clone());
    let stored = store
        .append_claim(&v, claim, None)
        .await
        .map_err(|e| e.to_string())?;
    expect!(
        stored.origin.as_ref() == Some(&origin),
        "the appended claim must carry its origin back: {:?}",
        stored.origin
    );

    // A fresh read, not the write's echo: the projection has to hold it.
    let read = store
        .get_claim(&stored.id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("get_claim({}) returned None", stored.id))?;
    expect!(
        read.origin.as_ref() == Some(&origin),
        "get_claim must return the origin field for field: {:?}",
        read.origin
    );

    // And a model-extracted claim beside it carries none — the field is
    // optional in fact, not just in type.
    let plain = store
        .append_claim(&v, NewClaim::fact("shareholder.43", "class", "A"), None)
        .await
        .map_err(|e| e.to_string())?;
    expect!(
        plain.origin.is_none(),
        "a claim proposed without an origin must read back without one"
    );
    Ok(())
}

async fn ledger_append_head_conflict(store: &dyn StorageBackend) -> ScenarioResult {
    let v = store
        .create_version(None, None)
        .await
        .map_err(|e| e.to_string())?;
    let c1 = store
        .append_claim(&v, NewClaim::fact("hero", "eyes", "green"), Some(0))
        .await
        .map_err(|e| e.to_string())?;
    expect!(c1.seq == 1, "first append must get seq 1, got {}", c1.seq);

    let err = store
        .append_claim(&v, NewClaim::fact("hero", "home", "harbor"), Some(0))
        .await;
    expect!(
        matches!(err, Err(KernelError::HeadConflict { .. })),
        "stale expected_head must be a HeadConflict, got {err:?}"
    );

    let head = store.head(&v).await.map_err(|e| e.to_string())?;
    expect!(head == 1, "failed append must not advance head, got {head}");
    Ok(())
}

async fn ledger_supersession_pin(store: &dyn StorageBackend) -> ScenarioResult {
    let v1 = store
        .create_version(None, None)
        .await
        .map_err(|e| e.to_string())?;
    let original = store
        .append_claim(&v1, NewClaim::fact("hero", "eyes", "green"), None)
        .await
        .map_err(|e| e.to_string())?;
    store
        .append_claim(&v1, NewClaim::fact("hero", "home", "harbor"), None)
        .await
        .map_err(|e| e.to_string())?;

    // correction in a CHILD version supersedes across the lineage
    let v2 = store
        .create_version(Some(&v1), None)
        .await
        .map_err(|e| e.to_string())?;
    let mut correction = NewClaim::fact("hero", "eyes", "blue");
    correction.claim_type = ClaimType::Correction;
    correction.supersedes_id = Some(original.id.clone());
    store
        .append_claim(&v2, correction, None)
        .await
        .map_err(|e| e.to_string())?;

    let head_facts = store
        .slice_facts(&v2, &FactQuery::default())
        .await
        .map_err(|e| e.to_string())?;
    let eyes: Vec<_> = head_facts.iter().filter(|f| f.key == "eyes").collect();
    expect!(
        eyes.len() == 1 && eyes[0].value == "blue",
        "head must read the correction"
    );

    // pinned before the correction: original still current AT THE PIN
    let pinned = store
        .slice_facts(
            &v2,
            &FactQuery {
                as_of_seq: Some(2),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    let eyes: Vec<_> = pinned.iter().filter(|f| f.key == "eyes").collect();
    expect!(
        eyes.len() == 1 && eyes[0].value == "green",
        "claim superseded after the pin must read as current at the pin (got {:?})",
        eyes.iter().map(|f| &f.value).collect::<Vec<_>>()
    );
    Ok(())
}

async fn one_pin_bounds_all_stores(store: &dyn StorageBackend) -> ScenarioResult {
    let v = store
        .create_version(None, None)
        .await
        .map_err(|e| e.to_string())?;
    store
        .append_claim(&v, NewClaim::fact("hero", "eyes", "green"), None)
        .await
        .map_err(|e| e.to_string())?; // seq 1
    store
        .register_promise(
            &v,
            "reveal",
            "setup",
            "open the letter",
            Some("ch1"),
            Some("ch3"),
        )
        .await
        .map_err(|e| e.to_string())?; // seq 2 (registration advances the clock)
    store
        .lock_anchor(&v, "hero", "eyes", "green", Some("ch1"), None)
        .await
        .map_err(|e| e.to_string())?; // seq 3
    store
        .record_counts(&v, "flashback", "ch1", 1, Some(2))
        .await
        .map_err(|e| e.to_string())?; // seq 4
    store
        .append_claim(&v, NewClaim::fact("hero", "home", "harbor"), None)
        .await
        .map_err(|e| e.to_string())?; // seq 5
    store
        .fulfill_promise(&v, "reveal")
        .await
        .map_err(|e| e.to_string())?; // fulfilled_seq 6

    // pin at 1: only claim 1 exists — nothing registered later may leak back
    expect!(
        store
            .anchors(&v, Some(1))
            .await
            .map_err(|e| e.to_string())?
            .is_empty(),
        "anchor stamped at seq 3 must be invisible at pin 1"
    );
    expect!(
        store
            .counter_totals(&v, Some(1))
            .await
            .map_err(|e| e.to_string())?
            .is_empty(),
        "counter stamped at seq 4 must be invisible at pin 1"
    );
    expect!(
        store
            .promises(&v, Some(1))
            .await
            .map_err(|e| e.to_string())?
            .is_empty(),
        "promise registered at seq 2 must be invisible at pin 1"
    );

    // pin at 2: promise registered and OPEN; anchor and counter still ahead
    let anchors = store
        .anchors(&v, Some(2))
        .await
        .map_err(|e| e.to_string())?;
    expect!(
        anchors.is_empty(),
        "anchor stamped at seq 3 must be invisible at pin 2"
    );
    let counters = store
        .counter_totals(&v, Some(2))
        .await
        .map_err(|e| e.to_string())?;
    expect!(
        counters.is_empty(),
        "counter stamped at seq 4 must be invisible at pin 2"
    );
    let promises = store
        .promises(&v, Some(2))
        .await
        .map_err(|e| e.to_string())?;
    expect!(
        promises.len() == 1,
        "promise registered at seq 2 must be visible at pin 2"
    );
    expect!(
        promises[0].status == PromiseStatus::Open,
        "post-pin fulfillment must read back OPEN, got {:?}",
        promises[0].status
    );

    // head: everything visible, promise fulfilled
    expect!(
        !store
            .anchors(&v, None)
            .await
            .map_err(|e| e.to_string())?
            .is_empty(),
        "anchor at head"
    );
    let head_promises = store.promises(&v, None).await.map_err(|e| e.to_string())?;
    expect!(
        head_promises[0].status == PromiseStatus::Fulfilled,
        "promise fulfilled at head"
    );
    Ok(())
}

async fn gates_block_records_disputed(store: &dyn StorageBackend) -> ScenarioResult {
    let v = store
        .create_version(None, None)
        .await
        .map_err(|e| e.to_string())?;
    store
        .append_claim(&v, NewClaim::fact("hero", "eyes", "green"), None)
        .await
        .map_err(|e| e.to_string())?;

    // the accept path: snapshot -> gates -> blocked claims recorded DISPUTED
    let snapshot = load_snapshot(store, &v, None, None, None)
        .await
        .map_err(|e| e.to_string())?;
    let candidate = Candidate {
        scope_path: Some("ch2".into()),
        claims: vec![ProposedClaim {
            claim_type: ClaimType::Fact,
            subject: "hero".into(),
            key: "eyes".into(),
            value: "blue".into(),
            supersedes_id: None,
        }],
        ..Default::default()
    };
    let findings = run_gates(&snapshot, &candidate);
    expect!(
        findings
            .iter()
            .any(|f| f.rule_id == "gate.ledger-conflict" && f.severity == Severity::Block),
        "conflicting plain claim must draw gate.ledger-conflict block, got {findings:?}"
    );

    let blocked = blocked_claim_keys(&findings);
    for p in &candidate.claims {
        let mut nc = NewClaim::fact(&p.subject, &p.key, &p.value);
        nc.scope_path = candidate.scope_path.clone();
        if blocked.contains(&p.claim_key()) {
            nc.status = ClaimStatus::Disputed; // blocked, never dropped
        }
        store
            .append_claim(&v, nc, None)
            .await
            .map_err(|e| e.to_string())?;
    }

    let accepted = store
        .slice_facts(&v, &FactQuery::default())
        .await
        .map_err(|e| e.to_string())?;
    let eyes: Vec<_> = accepted.iter().filter(|f| f.key == "eyes").collect();
    expect!(
        eyes.len() == 1 && eyes[0].value == "green",
        "canon must be unchanged"
    );

    let disputed = store
        .slice_facts(
            &v,
            &FactQuery {
                statuses: vec![ClaimStatus::Disputed],
                ..Default::default()
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    expect!(
        disputed.iter().any(|f| f.value == "blue"),
        "the blocked claim must be recorded disputed, not dropped"
    );
    Ok(())
}

async fn composer_budget_degradation(store: &dyn StorageBackend) -> ScenarioResult {
    let v = store
        .create_version(None, None)
        .await
        .map_err(|e| e.to_string())?;
    for i in 1..=20u64 {
        let mut c = NewClaim::fact(
            "hero",
            &format!("k{i}"),
            &format!("value-{i} with prose attached"),
        );
        c.scope_path = Some(if i <= 10 {
            "book.ch1".into()
        } else {
            "book.ch2".into()
        });
        store
            .append_claim(&v, c, None)
            .await
            .map_err(|e| e.to_string())?;
    }
    let snapshot = load_snapshot(store, &v, None, None, None)
        .await
        .map_err(|e| e.to_string())?;
    let full = compose(&snapshot, Some("book.ch1"), None);
    let budget = full.estimated_tokens() - 20;
    let degraded = compose(&snapshot, Some("book.ch1"), Some(budget));
    expect!(degraded.estimated_tokens() <= budget, "budget must hold");
    let facts = degraded
        .sections
        .iter()
        .find(|(t, _)| t == "Accepted facts")
        .map(|(_, b)| b.lines().count())
        .unwrap_or(0);
    expect!(
        facts == 20,
        "digests must degrade BEFORE facts trim (facts kept: {facts})"
    );
    Ok(())
}

async fn gates_chronology_certain_only(store: &dyn StorageBackend) -> ScenarioResult {
    use munarium_core::chrono_gate::{ChronologyRules, DeadlineRule, OrderRule};
    use munarium_core::gates::check_chronology;

    let v = store
        .create_version(None, None)
        .await
        .map_err(|e| e.to_string())?;
    store
        .append_claim(
            &v,
            NewClaim::fact("case", "filing_date", "2020-01-01"),
            None,
        )
        .await
        .map_err(|e| e.to_string())?;
    store
        .append_claim(&v, NewClaim::fact("hero", "birth_date", "circa 1950"), None)
        .await
        .map_err(|e| e.to_string())?;

    let rules = ChronologyRules {
        order: vec![OrderRule {
            before: "birth_date".into(),
            after: "death_date".into(),
            severity: "warn".into(),
        }],
        deadlines: vec![DeadlineRule {
            key: "response_date".into(),
            due_from: "filing_date".into(),
            within_days: 30,
            severity: "warn".into(),
        }],
        ..Default::default()
    };
    let snapshot = load_snapshot(store, &v, None, None, None)
        .await
        .map_err(|e| e.to_string())?;

    // candidate death before a CIRCA birth: uncertainty disables certainty
    let cand = Candidate {
        claims: vec![ProposedClaim {
            claim_type: ClaimType::Fact,
            subject: "hero".into(),
            key: "death_date".into(),
            value: "1940".into(),
            supersedes_id: None,
        }],
        ..Default::default()
    };
    let f = check_chronology(&snapshot, &cand, &rules, None);
    expect!(
        f.iter().all(|x| x.rule_id != "gate.chronology-order"),
        "circa date must never produce a certain order violation: {f:?}"
    );

    // late response in the candidate: deadline fires with the event chain
    let cand = Candidate {
        claims: vec![ProposedClaim {
            claim_type: ClaimType::Fact,
            subject: "case".into(),
            key: "response_date".into(),
            value: "2020-06-01".into(),
            supersedes_id: None,
        }],
        ..Default::default()
    };
    let f = check_chronology(&snapshot, &cand, &rules, None);
    expect!(
        f.iter().any(|x| x.rule_id == "gate.chronology-deadline"),
        "certainly-late response must fire gate.chronology-deadline: {f:?}"
    );
    let chain_len = f[0]
        .detail
        .as_ref()
        .and_then(|d| d.get("chain"))
        .and_then(|c| c.as_array())
        .map(|c| c.len())
        .unwrap_or(0);
    expect!(
        chain_len == 2,
        "finding must carry the complete event chain"
    );
    Ok(())
}

async fn digests_rebuilt_under_pin(store: &dyn StorageBackend) -> ScenarioResult {
    let v = store
        .create_version(None, None)
        .await
        .map_err(|e| e.to_string())?;
    let mut c = NewClaim::fact("hero", "eyes", "green");
    c.scope_path = Some("ch1".into());
    store
        .append_claim(&v, c, None)
        .await
        .map_err(|e| e.to_string())?; // seq 1
    let mut c = NewClaim::fact("hero", "home", "harbor");
    c.scope_path = Some("ch1".into());
    store
        .append_claim(&v, c, None)
        .await
        .map_err(|e| e.to_string())?; // seq 2

    // store a HEAD-shaped digest, then pin before seq 2
    let head_facts = store
        .slice_facts(&v, &FactQuery::default())
        .await
        .map_err(|e| e.to_string())?;
    for d in munarium_core::digests::build_ladder(&v, &head_facts) {
        store.upsert_digest(&d).await.map_err(|e| e.to_string())?;
    }
    let pinned = load_snapshot(store, &v, None, None, Some(1))
        .await
        .map_err(|e| e.to_string())?;
    let tier0: Vec<_> = pinned.digests.iter().filter(|d| d.tier == 0).collect();
    expect!(
        tier0.iter().all(|d| !d.content.contains("home")),
        "stored head digests must never be served under a pin"
    );
    Ok(())
}
