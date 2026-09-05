# SPDX-License-Identifier: Apache-2.0
"""Offline unit tests for the SSE parser and the turn-stream event machine
(mirroring the Rust client's sse.rs tests, plus the classify/terminal
semantics the transports ride)."""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

import pytest  # noqa: E402

from munarium_client import (  # noqa: E402
    InvalidInputError,
    RunLockedError,
    UnexpectedError,
)
from munarium_client._sse import (  # noqa: E402
    MAX_EVENT_BYTES,
    SseEvent,
    SseOverflow,
    SseParser,
    TurnEventMachine,
    classify_turn_event,
)
from munarium_client.models import TurnProgress, TurnResult  # noqa: E402

# ---------------------------------------------------------------------------
# parser (ports of the sse.rs unit tests)
# ---------------------------------------------------------------------------


def test_parses_named_events_and_keepalives() -> None:
    p = SseParser()
    evs = p.push(b': keep-alive\n\nevent: progress\ndata: {"stage":"merge"}\n\n')
    assert evs == [SseEvent(event="progress", data='{"stage":"merge"}')]


def test_survives_arbitrary_chunk_boundaries() -> None:
    # The transport may split anywhere — including mid-line and between
    # CR and LF. Byte-by-byte is the adversarial version of that.
    wire = b'event: progress\r\ndata: {"n":1}\r\n\r\nevent: done\ndata: {}\n\n'
    p = SseParser()
    evs: list[SseEvent] = []
    for i in range(len(wire)):
        evs.extend(p.push(wire[i : i + 1]))
    assert len(evs) == 2
    assert evs[0].event == "progress" and evs[0].data == '{"n":1}'
    assert evs[1].event == "done"


def test_multi_line_data_joins_with_newlines() -> None:
    p = SseParser()
    evs = p.push(b"data: a\ndata: b\n\n")
    assert evs[0].data == "a\nb"
    assert evs[0].event == "", "default event name is empty"


def test_event_without_data_is_dropped_not_dispatched() -> None:
    p = SseParser()
    assert p.push(b"event: progress\n\n") == []
    # ...and the stale name does not leak into the next event.
    evs = p.push(b"data: x\n\n")
    assert evs[0].event == ""


def test_unknown_fields_and_no_colon_lines_are_ignored() -> None:
    p = SseParser()
    evs = p.push(b"id: 7\nretry: 100\nnonsense\ndata: ok\n\n")
    assert len(evs) == 1 and evs[0].data == "ok"


def test_a_neverending_event_overflows_instead_of_growing_forever() -> None:
    # No newline at all: the pending line buffer hits the cap.
    p = SseParser()
    chunk = b"x" * (1024 * 1024)
    with pytest.raises(SseOverflow):
        for _ in range(20):
            p.push(chunk)

    # Data lines that never dispatch (no blank line) also count.
    p = SseParser()
    line = b"data: " + b"y" * (1024 * 1024) + b"\n"
    with pytest.raises(SseOverflow):
        for _ in range(20):
            p.push(line)


def test_crlf_split_across_chunks_is_one_line_ending() -> None:
    p = SseParser()
    assert p.push(b"data: a\r") == []
    # The LF arriving in the NEXT chunk completes the same CRLF — it must
    # not read as an extra blank line (which would dispatch early).
    assert p.push(b"\n") == []
    evs = p.push(b"\n")
    assert evs == [SseEvent(event="", data="a")]


# ---------------------------------------------------------------------------
# classification + the machine (the turn-stream contract)
# ---------------------------------------------------------------------------


def _ev(name: str, data: object) -> SseEvent:
    return SseEvent(event=name, data=json.dumps(data))


def test_progress_decodes_with_extras_and_unknown_stage() -> None:
    # Forward-compat: unknown stages (and their unknown fields) must not
    # break iteration — progress is informational.
    item = classify_turn_event(_ev("progress", {"stage": "quantum", "qubits": 7}))
    assert isinstance(item, TurnProgress)
    assert item.stage == "quantum"


def test_undecodable_progress_is_skipped_not_fatal() -> None:
    assert classify_turn_event(SseEvent(event="progress", data="not json")) is None
    # Decodable JSON that is not a progress shape is skipped too.
    assert classify_turn_event(_ev("progress", {"no_stage": True})) is None


def test_done_is_terminal_and_carries_the_turn_result() -> None:
    body = {
        "session_id": "s-1",
        "ordinal": 1,
        "collections_searched": ["docs"],
        "hits": [],
        "envelopes": [],
    }
    item = classify_turn_event(_ev("done", body))
    assert isinstance(item, TurnResult), "the TurnResult IS the terminal marker"
    assert item.session_id == "s-1"


def test_undecodable_done_is_a_terminal_error() -> None:
    # The caller was owed a TurnResult — never a silent success.
    item = classify_turn_event(SseEvent(event="done", data="{broken"))
    assert isinstance(item, UnexpectedError)


def test_error_event_decodes_through_the_slug_registry() -> None:
    body = {
        "type": "https://munarium.ioka.io/problems/session-not-open",
        "title": "session not open",
        "status": 409,
        "detail": "session s-1 is closed",
    }
    item = classify_turn_event(_ev("error", body))
    assert isinstance(item, InvalidInputError)

    locked = {
        "type": "https://munarium.ioka.io/problems/run-locked",
        "status": 409,
        "detail": "run run-1 holds the lock",
    }
    item = classify_turn_event(_ev("error", locked))
    assert isinstance(item, RunLockedError)


def test_unknown_event_names_are_skipped() -> None:
    assert classify_turn_event(_ev("telemetry", {"x": 1})) is None


def _wire(*events: tuple[str, object]) -> bytes:
    return b"".join(
        f"event: {name}\ndata: {json.dumps(data)}\n\n".encode() for name, data in events
    )


_DONE = {
    "session_id": "s-1",
    "ordinal": 1,
    "collections_searched": [],
    "hits": [],
    "envelopes": [],
}


def test_machine_yields_progress_then_done_and_nothing_after() -> None:
    machine = TurnEventMachine()
    items = machine.feed(
        _wire(
            ("progress", {"stage": "retrieval", "collection": "docs", "hits": 2}),
            ("done", _DONE),
            ("progress", {"stage": "late"}),  # must be dropped
        )
    )
    assert [type(i) for i in items] == [TurnProgress, TurnResult]
    assert machine.terminal and machine.error is None
    assert machine.feed(_wire(("progress", {"stage": "later"}))) == []


def test_machine_surfaces_progress_before_a_terminal_error() -> None:
    # Progress events riding in the same chunk as the terminal error stay
    # visible — the caller yields them first, THEN raises.
    machine = TurnEventMachine()
    items = machine.feed(
        _wire(
            ("progress", {"stage": "merge", "hits": 3}),
            ("error", {"type": "x/run-locked", "status": 409, "detail": "locked"}),
        )
    )
    assert [type(i) for i in items] == [TurnProgress]
    assert machine.terminal
    assert isinstance(machine.error, RunLockedError)


def test_machine_overflow_is_a_typed_terminal_error() -> None:
    machine = TurnEventMachine()
    chunk = b"x" * (1024 * 1024)
    for _ in range(20):
        machine.feed(chunk)
        if machine.terminal:
            break
    assert machine.terminal
    assert isinstance(machine.error, UnexpectedError)
    assert str(MAX_EVENT_BYTES // (1024 * 1024)) in str(machine.error)
