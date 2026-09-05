# SPDX-License-Identifier: Apache-2.0
"""The server's 7 MMP conformance scenarios, ported to the black-box API
surface (same names as server/conformance/src/lib.rs so CI output is
comparable across languages).

Two scenarios differ from the Rust suite by design:
- `composer.budget-degradation` and `digests.rebuilt-under-pin` assert via
  the server's ComposeContext (the Rust suite composes client-side through
  munarium-core, which Python does not link);
- `gates.chronology-certain-only` is a pure-kernel check (client-side
  `check_chronology` over declarative rules, never an API call) — SCENARIOS.md
  marks it kernel-only; no client port carries it, marked SKIP here.
"""

from __future__ import annotations

from typing import Any

import pytest

from munarium_client import HeadConflictError


async def test_ledger_append_head_conflict(client: Any) -> None:
    v = await client.commands.create_version()
    out = await client.commands.propose_claim(
        v, expected_head=0, subject="hero", key="eyes", value="green"
    )
    assert out.claim.seq == 1, "first append must get seq 1"

    with pytest.raises(HeadConflictError):
        await client.commands.propose_claim(
            v, expected_head=0, subject="hero", key="home", value="harbor"
        )
    assert await client.query.head(v) == 1, "failed append must not advance head"


async def test_ledger_origin_round_trips(client: Any) -> None:
    # S-4.1: a connector claim's origin survives the round trip on both
    # transports; a claim proposed without one reads back without one.
    v = await client.commands.create_version()
    origin = {
        "kind": "connector",
        "source_id": "crm",
        "mapping_version": "captable-holdings@1",
        "row_key": "holder_id=43",
        "event_position": "lsn/0/1A2B",
        "observed_at": "2026-08-28T09:15:00Z",
        "evidence_id": "ev-batch-0001",
    }
    out = await client.commands.propose_claim(
        v, subject="shareholder.43", key="shares", value="90500", origin=origin
    )
    assert out.claim.origin is not None, "the write echo must carry origin"
    assert out.claim.origin.model_dump(exclude_none=True) == origin

    read = await client.query.get_claim(out.claim.id)
    assert read.claim.origin is not None and read.claim.origin.row_key == "holder_id=43", (
        "a fresh read must return the origin, not just the write echo"
    )

    plain = await client.commands.propose_claim(v, subject="shareholder.43", key="class", value="A")
    assert plain.claim.origin is None, "no origin in, no origin out"


async def test_ledger_supersession_pin(client: Any) -> None:
    v1 = await client.commands.create_version()
    original = await client.commands.propose_claim(v1, subject="hero", key="eyes", value="green")
    await client.commands.propose_claim(v1, subject="hero", key="home", value="harbor")

    # correction in a CHILD version supersedes across the lineage
    v2 = await client.commands.create_version(parent_version_id=v1)
    await client.commands.propose_claim(
        v2,
        subject="hero",
        key="eyes",
        value="blue",
        claim_type="correction",
        supersedes_id=original.claim.id,
    )

    head_facts = (await client.query.facts(v2)).facts
    eyes = [f for f in head_facts if f.key == "eyes"]
    assert len(eyes) == 1 and eyes[0].value == "blue", "head must read the correction"

    pinned = (await client.query.facts(v2, as_of_seq=2)).facts
    eyes = [f for f in pinned if f.key == "eyes"]
    assert len(eyes) == 1 and eyes[0].value == "green", (
        "claim superseded after the pin must read as current at the pin"
    )


async def test_pins_one_pin_bounds_all_stores(client: Any) -> None:
    v = await client.commands.create_version()
    await client.commands.propose_claim(v, subject="hero", key="eyes", value="green")  # seq 1
    await client.commands.open_promise(
        v,
        key="reveal",
        kind="setup",
        description="open the letter",
        origin_scope="ch1",
        due_scope="ch3",
    )  # seq 2 (registration advances the clock)
    await client.commands.lock_anchor(
        v, subject="hero", key="eyes", value="green", scope_path="ch1"
    )  # seq 3
    await client.commands.record_counts(
        v, key="flashback", scope_path="ch1", count=1, budget=2
    )  # seq 4
    await client.commands.propose_claim(v, subject="hero", key="home", value="harbor")  # seq 5
    await client.commands.fulfill_promise(v, "reveal")  # fulfilled_seq 6

    # pin at 1: only claim 1 exists — nothing registered later may leak back
    assert await client.query.anchors(v, as_of_seq=1) == [], (
        "anchor stamped at seq 3 must be invisible at pin 1"
    )
    assert await client.query.counters(v, as_of_seq=1) == [], (
        "counter stamped at seq 4 must be invisible at pin 1"
    )
    assert await client.query.promises(v, as_of_seq=1) == [], (
        "promise registered at seq 2 must be invisible at pin 1"
    )

    # pin at 2: promise registered and OPEN; anchor and counter still ahead
    assert await client.query.anchors(v, as_of_seq=2) == [], (
        "anchor stamped at seq 3 must be invisible at pin 2"
    )
    assert await client.query.counters(v, as_of_seq=2) == [], (
        "counter stamped at seq 4 must be invisible at pin 2"
    )
    promises = await client.query.promises(v, as_of_seq=2)
    assert len(promises) == 1, "promise registered at seq 2 must be visible at pin 2"
    assert promises[0].status == "open", "post-pin fulfillment must read back OPEN"

    # head: everything visible, promise fulfilled
    assert await client.query.anchors(v) != [], "anchor at head"
    head_promises = await client.query.promises(v)
    assert head_promises[0].status == "fulfilled", "promise fulfilled at head"


async def test_gates_block_records_disputed(client: Any) -> None:
    v = await client.commands.create_version()
    await client.commands.propose_claim(v, subject="hero", key="eyes", value="green")

    # the command path IS the governance path: the conflicting plain claim
    # comes back SUCCESS with status disputed + the gate finding.
    out = await client.commands.propose_claim(
        v, subject="hero", key="eyes", value="blue", scope_path="ch2"
    )
    assert out.is_disputed, "conflicting plain claim must be recorded disputed"
    assert any(
        f.rule_id == "gate.ledger-conflict" and f.severity == "block" for f in out.findings
    ), f"expected gate.ledger-conflict block, got {out.findings}"

    accepted = (await client.query.facts(v)).facts
    eyes = [f for f in accepted if f.key == "eyes"]
    assert len(eyes) == 1 and eyes[0].value == "green", "canon must be unchanged"

    disputed = (await client.query.facts(v, statuses=("disputed",))).facts
    assert any(f.value == "blue" for f in disputed), (
        "the blocked claim must be recorded disputed, not dropped"
    )


async def test_composer_budget_degradation(client: Any) -> None:
    v = await client.commands.create_version()
    for i in range(1, 21):
        await client.commands.propose_claim(
            v,
            subject="hero",
            key=f"k{i}",
            value=f"value-{i} with prose attached",
            scope_path="book.ch1" if i <= 10 else "book.ch2",
        )
    full = await client.query.compose_context(v, scope="book.ch1")
    budget = full.estimated_tokens - 20
    degraded = await client.query.compose_context(v, scope="book.ch1", budget_tokens=budget)
    assert degraded.estimated_tokens <= budget, "budget must hold"
    facts_section = next((sec for sec in degraded.sections if sec.title == "Accepted facts"), None)
    kept = len(facts_section.body.splitlines()) if facts_section else 0
    assert kept == 20, f"digests must degrade BEFORE facts trim (facts kept: {kept})"


async def test_digests_rebuilt_under_pin(client: Any) -> None:
    from munarium_client.models import Digest

    v = await client.commands.create_version()
    await client.commands.propose_claim(
        v, subject="hero", key="eyes", value="green", scope_path="ch1"
    )  # seq 1
    await client.commands.propose_claim(
        v, subject="hero", key="home", value="harbor", scope_path="ch1"
    )  # seq 2

    # store a HEAD-shaped digest, then pin before seq 2: the stored rung
    # (which mentions "home") must never be served under the pin.
    await client.commands.upsert_digest(
        Digest(
            version_id=v,
            tier=0,
            scope_path="ch1",
            content="[ch1] hero eyes green; hero home harbor",
            content_hash="head-shaped",
            built_from_seq=2,
        )
    )
    pinned = await client.query.compose_context(v, as_of_seq=1)
    assert "home" not in pinned.text, "stored head digests must never be served under a pin"


async def test_gates_chronology_certain_only(client: Any) -> None:
    pytest.skip(
        "pure-kernel scenario (client-side check_chronology over declarative "
        "rules; no API surface) — SCENARIOS.md marks it kernel-only and no "
        "client port carries it"
    )
