// SPDX-License-Identifier: Apache-2.0
//! The seven MMP wire conformance scenarios, written against the client API.
//!
//! The contract text is `contract/conformance/SCENARIOS.md` (vendored from
//! `server/conformance/SCENARIOS.md`); the server's `mmp-conformance` crate is
//! the reference implementation and runs the same scenarios in-process and
//! black-box. These scenarios name only the client API — a public conformance
//! suite must not reach past the wire into the server's domain core, its
//! stores or its conversion crate. The eighth scenario,
//! `gates.chronology-certain-only`, is a kernel check with no wire surface and
//! stays on the server.
//!
//! Names match the server suite so CI output is comparable across languages.

use munarium_client::{dto, ContextQuery, FactsQuery, MunariumClient, MunariumError};

pub type ScenarioResult = Result<(), String>;

macro_rules! expect {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond { return Err(format!($($msg)+)); }
    };
}

fn e(err: MunariumError) -> String {
    err.to_string()
}

fn fact(subject: &str, key: &str, value: &str) -> dto::ProposeClaimRequest {
    dto::ProposeClaimRequest {
        expected_head: None,
        claim_type: dto::ClaimTypeDto::Fact,
        subject: subject.into(),
        key: key.into(),
        value: value.into(),
        scope_path: None,
        provenance: None,
        supersedes_id: None,
        entity_id: None,
        evidence: None,
        confidence: None,
        shape_ref: None,
        origin: None,
    }
}

async fn create_version(c: &MunariumClient, parent: Option<&str>) -> Result<String, String> {
    Ok(c.commands
        .create_version(
            dto::CreateVersionRequest {
                parent_version_id: parent.map(String::from),
                metadata: None,
            },
            None,
        )
        .await
        .map_err(e)?
        .version_id)
}

async fn propose(
    c: &MunariumClient,
    v: &str,
    req: dto::ProposeClaimRequest,
) -> Result<dto::ProposeClaimResponse, String> {
    c.commands.propose_claim(v, req, None).await.map_err(e)
}

async fn facts(c: &MunariumClient, v: &str, q: FactsQuery) -> Result<Vec<dto::ClaimDto>, String> {
    Ok(c.query.facts(v, q).await.map_err(e)?.facts)
}

/// All scenarios, name -> outcome; every transport runs the full set.
pub async fn run_all(c: &MunariumClient) -> Vec<(&'static str, ScenarioResult)> {
    vec![
        (
            "ledger.append-head-conflict",
            ledger_append_head_conflict(c).await,
        ),
        ("ledger.supersession-pin", ledger_supersession_pin(c).await),
        (
            "pins.one-pin-bounds-all-stores",
            pins_one_pin_bounds_all_stores(c).await,
        ),
        (
            "gates.block-records-disputed",
            gates_block_records_disputed(c).await,
        ),
        (
            "composer.budget-degradation",
            composer_budget_degradation(c).await,
        ),
        (
            "digests.rebuilt-under-pin",
            digests_rebuilt_under_pin(c).await,
        ),
        (
            "ledger.origin-round-trips",
            ledger_origin_round_trips(c).await,
        ),
    ]
}

/// SCENARIOS.md §1: a stale `expected_head` is refused and leaves the ledger untouched.
async fn ledger_append_head_conflict(c: &MunariumClient) -> ScenarioResult {
    let v = create_version(c, None).await?;
    let mut first = fact("hero", "eyes", "green");
    first.expected_head = Some(0);
    let c1 = propose(c, &v, first).await?;
    expect!(
        c1.claim.seq == 1,
        "first append must get seq 1, got {}",
        c1.claim.seq
    );

    let mut stale = fact("hero", "home", "harbor");
    stale.expected_head = Some(0);
    let err = c.commands.propose_claim(&v, stale, None).await;
    expect!(
        matches!(err, Err(MunariumError::HeadConflict { .. })),
        "stale expected_head must be a head conflict, got {err:?}"
    );

    let head = c.query.head(&v).await.map_err(e)?;
    expect!(head == 1, "failed append must not advance head, got {head}");
    Ok(())
}

/// SCENARIOS.md §2: a correction in a child version supersedes across the
/// lineage at the head; a pin before the correction still reads the original.
async fn ledger_supersession_pin(c: &MunariumClient) -> ScenarioResult {
    let v1 = create_version(c, None).await?;
    let original = propose(c, &v1, fact("hero", "eyes", "green")).await?;
    propose(c, &v1, fact("hero", "home", "harbor")).await?;

    let v2 = create_version(c, Some(&v1)).await?;
    let mut correction = fact("hero", "eyes", "blue");
    correction.claim_type = dto::ClaimTypeDto::Correction;
    correction.supersedes_id = Some(original.claim.id.clone());
    propose(c, &v2, correction).await?;

    let head_facts = facts(c, &v2, FactsQuery::default()).await?;
    let eyes: Vec<_> = head_facts.iter().filter(|f| f.key == "eyes").collect();
    expect!(
        eyes.len() == 1 && eyes[0].value == "blue",
        "head must read the correction, got {:?}",
        eyes.iter().map(|f| &f.value).collect::<Vec<_>>()
    );

    let pinned = facts(
        c,
        &v2,
        FactsQuery {
            as_of_seq: Some(2),
            ..Default::default()
        },
    )
    .await?;
    let eyes: Vec<_> = pinned.iter().filter(|f| f.key == "eyes").collect();
    expect!(
        eyes.len() == 1 && eyes[0].value == "green",
        "claim superseded after the pin must read as current at the pin, got {:?}",
        eyes.iter().map(|f| &f.value).collect::<Vec<_>>()
    );
    Ok(())
}

/// SCENARIOS.md §3: one pin bounds anchors, counters and promises, not only
/// facts; a fulfillment after the pin reads as still open.
async fn pins_one_pin_bounds_all_stores(c: &MunariumClient) -> ScenarioResult {
    let v = create_version(c, None).await?;
    propose(c, &v, fact("hero", "eyes", "green")).await?; // seq 1
    c.commands
        .open_promise(
            &v,
            dto::OpenPromiseRequest {
                key: "reveal".into(),
                kind: "setup".into(),
                description: "open the letter".into(),
                origin_scope: Some("ch1".into()),
                due_scope: Some("ch3".into()),
            },
            None,
        )
        .await
        .map_err(e)?; // seq 2 (registration advances the clock)
    c.commands
        .lock_anchor(
            &v,
            dto::LockAnchorRequest {
                subject: "hero".into(),
                key: "eyes".into(),
                value: "green".into(),
                scope_path: Some("ch1".into()),
                evidence: None,
            },
            None,
        )
        .await
        .map_err(e)?; // seq 3
    c.commands
        .record_counts(
            &v,
            dto::RecordCountsRequest {
                key: "flashback".into(),
                scope_path: "ch1".into(),
                count: 1,
                budget: Some(2),
            },
            None,
        )
        .await
        .map_err(e)?; // seq 4
    propose(c, &v, fact("hero", "home", "harbor")).await?; // seq 5
    c.commands
        .fulfill_promise(&v, "reveal", None)
        .await
        .map_err(e)?; // fulfilled_seq 6

    // pin at 1: only claim 1 exists — nothing registered later may leak back
    let anchors = c.query.anchors(&v, Some(1)).await.map_err(e)?.anchors;
    expect!(
        anchors.is_empty(),
        "anchor stamped at seq 3 must be invisible at pin 1"
    );
    let counters = c.query.counters(&v, Some(1)).await.map_err(e)?.counters;
    expect!(
        counters.is_empty(),
        "counter stamped at seq 4 must be invisible at pin 1"
    );
    let promises = c
        .query
        .promises(&v, Some(1), None)
        .await
        .map_err(e)?
        .promises;
    expect!(
        promises.is_empty(),
        "promise registered at seq 2 must be invisible at pin 1"
    );

    // pin at 2: promise registered and OPEN; anchor and counter still ahead
    let anchors = c.query.anchors(&v, Some(2)).await.map_err(e)?.anchors;
    expect!(
        anchors.is_empty(),
        "anchor stamped at seq 3 must be invisible at pin 2"
    );
    let counters = c.query.counters(&v, Some(2)).await.map_err(e)?.counters;
    expect!(
        counters.is_empty(),
        "counter stamped at seq 4 must be invisible at pin 2"
    );
    let promises = c
        .query
        .promises(&v, Some(2), None)
        .await
        .map_err(e)?
        .promises;
    expect!(
        promises.len() == 1,
        "promise registered at seq 2 must be visible at pin 2"
    );
    expect!(
        promises[0].status == "open",
        "post-pin fulfillment must read back OPEN, got {:?}",
        promises[0].status
    );

    // head: everything visible, promise fulfilled
    let anchors = c.query.anchors(&v, None).await.map_err(e)?.anchors;
    expect!(!anchors.is_empty(), "anchor at head");
    let promises = c.query.promises(&v, None, None).await.map_err(e)?.promises;
    expect!(
        promises.first().map(|p| p.status.as_str()) == Some("fulfilled"),
        "promise fulfilled at head, got {:?}",
        promises.first().map(|p| &p.status)
    );
    Ok(())
}

/// SCENARIOS.md §4: the command path is the governance path — a conflicting
/// claim draws a block finding, is recorded disputed, and canon is unchanged.
async fn gates_block_records_disputed(c: &MunariumClient) -> ScenarioResult {
    let v = create_version(c, None).await?;
    propose(c, &v, fact("hero", "eyes", "green")).await?;

    let mut conflicting = fact("hero", "eyes", "blue");
    conflicting.scope_path = Some("ch2".into());
    let out = propose(c, &v, conflicting).await?;
    expect!(
        out.claim.status == dto::ClaimStatusDto::Disputed,
        "conflicting plain claim must be recorded disputed, got {:?}",
        out.claim.status
    );
    expect!(
        out.findings
            .iter()
            .any(|f| f.rule_id == "gate.ledger-conflict" && f.severity == dto::SeverityDto::Block),
        "expected a gate.ledger-conflict block finding, got {:?}",
        out.findings
    );

    let accepted = facts(c, &v, FactsQuery::default()).await?;
    let eyes: Vec<_> = accepted.iter().filter(|f| f.key == "eyes").collect();
    expect!(
        eyes.len() == 1 && eyes[0].value == "green",
        "canon must be unchanged, got {:?}",
        eyes.iter().map(|f| &f.value).collect::<Vec<_>>()
    );

    let disputed = facts(
        c,
        &v,
        FactsQuery {
            statuses: vec![dto::ClaimStatusDto::Disputed],
            ..Default::default()
        },
    )
    .await?;
    expect!(
        disputed.iter().any(|f| f.value == "blue"),
        "the blocked claim must be recorded disputed, not dropped"
    );
    Ok(())
}

/// SCENARIOS.md §5: under a token budget the composer degrades digests before
/// it trims facts.
async fn composer_budget_degradation(c: &MunariumClient) -> ScenarioResult {
    let v = create_version(c, None).await?;
    for i in 1..=20u64 {
        let mut f = fact(
            "hero",
            &format!("k{i}"),
            &format!("value-{i} with prose attached"),
        );
        f.scope_path = Some(if i <= 10 { "book.ch1" } else { "book.ch2" }.into());
        propose(c, &v, f).await?;
    }
    let scope = || ContextQuery {
        scope: Some("book.ch1".into()),
        ..Default::default()
    };
    let full = c.query.compose_context(&v, scope()).await.map_err(e)?;
    let budget = full.estimated_tokens.saturating_sub(20);
    let degraded = c
        .query
        .compose_context(
            &v,
            ContextQuery {
                budget_tokens: Some(budget),
                ..scope()
            },
        )
        .await
        .map_err(e)?;
    expect!(
        degraded.estimated_tokens <= budget,
        "budget must hold: {} > {budget}",
        degraded.estimated_tokens
    );
    let kept = degraded
        .sections
        .iter()
        .find(|s| s.title == "Accepted facts")
        .map(|s| s.body.lines().count())
        .unwrap_or(0);
    expect!(
        kept == 20,
        "digests must degrade BEFORE facts trim (facts kept: {kept})"
    );
    Ok(())
}

/// SCENARIOS.md §6: a stored digest describes the head and is never served
/// under a pin.
async fn digests_rebuilt_under_pin(c: &MunariumClient) -> ScenarioResult {
    let v = create_version(c, None).await?;
    let mut f = fact("hero", "eyes", "green");
    f.scope_path = Some("ch1".into());
    propose(c, &v, f).await?; // seq 1
    let mut f = fact("hero", "home", "harbor");
    f.scope_path = Some("ch1".into());
    propose(c, &v, f).await?; // seq 2

    c.commands
        .upsert_digest(dto::DigestDto {
            version_id: v.clone(),
            tier: 0,
            scope_path: "ch1".into(),
            content: "[ch1] hero eyes green; hero home harbor".into(),
            content_hash: "head-shaped".into(),
            built_from_seq: 2,
        })
        .await
        .map_err(e)?;
    let pinned = c
        .query
        .compose_context(
            &v,
            ContextQuery {
                as_of_seq: Some(1),
                ..Default::default()
            },
        )
        .await
        .map_err(e)?;
    expect!(
        !pinned.text.contains("home"),
        "stored head digests must never be served under a pin"
    );
    Ok(())
}

/// SCENARIOS.md §7: a connector claim's origin survives the round trip; a
/// claim proposed without one reads back without one.
async fn ledger_origin_round_trips(c: &MunariumClient) -> ScenarioResult {
    let v = create_version(c, None).await?;
    let origin = dto::ClaimOriginDto {
        kind: "connector".into(),
        source_id: "crm".into(),
        mapping_version: "captable-holdings@1".into(),
        row_key: "holder_id=43".into(),
        event_position: Some("lsn/0/1A2B".into()),
        observed_at: Some("2026-08-28T09:15:00Z".into()),
        evidence_id: Some("ev-batch-0001".into()),
    };
    let expected = serde_json::to_value(&origin).map_err(|x| x.to_string())?;

    let mut with_origin = fact("shareholder.43", "shares", "90500");
    with_origin.origin = Some(origin);
    let stored = propose(c, &v, with_origin).await?;
    let echoed = serde_json::to_value(&stored.claim.origin).map_err(|x| x.to_string())?;
    expect!(
        echoed == expected,
        "the appended claim must carry its origin back, got {echoed}"
    );

    // A fresh read, not the write's echo: the projection has to hold it.
    let read = c.query.get_claim(&stored.claim.id).await.map_err(e)?;
    let read_origin = serde_json::to_value(&read.claim.origin).map_err(|x| x.to_string())?;
    expect!(
        read_origin == expected,
        "get_claim must return the origin, got {read_origin}"
    );

    let plain = propose(c, &v, fact("shareholder.43", "class", "A")).await?;
    expect!(
        plain.claim.origin.is_none(),
        "a claim proposed without an origin must read back without one"
    );
    Ok(())
}
