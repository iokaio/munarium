# SPDX-License-Identifier: Apache-2.0
"""The 2026-08-24 review-batch invariants, pinned offline.

Each test corresponds to a confirmed review finding: the async-gRPC
``turn_stream`` that was not iterable, whitespace in gRPC base64, the SSE
overflow that discarded a same-chunk ``done``, the surplus ingest results
that silently mis-paired, the REST command-retry rule (re-send on typed
``overloaded`` and connect-phase failures ONLY — never on a 502/504
``unexpected`` even though it is transient), the proto3 empty-list
sentinels, the single-file gRPC ``ingest()`` parity with REST's 400, and
bounded-memory JSON encoding for the large file/bulk REST surface.
"""

from __future__ import annotations

import base64
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

import httpx  # noqa: E402
import pytest  # noqa: E402

from munarium_client import (  # noqa: E402
    AsyncMunariumClient,
    ClientOptions,
    InvalidInputError,
    MunariumClient,
    OverloadedError,
    UnexpectedError,
    UnsupportedError,
)
from munarium_client import _specs as specs  # noqa: E402
from munarium_client._grpc_common import (  # noqa: E402
    prepare_ingest_files,
    reject_empty_list,
    splice_ingest_results,
)
from munarium_client._sse import SseOverflow, SseParser, TurnEventMachine  # noqa: E402
from munarium_client.models import IngestFile, IngestResult, TurnResult  # noqa: E402
from munarium_client.rest import SyncRestTransport, _iter_json_bytes  # noqa: E402

_DONE = {
    "session_id": "s-1",
    "ordinal": 1,
    "collections_searched": [],
    "hits": [],
    "envelopes": [],
}


def _file(name: str, content_b64: str, **kw: object) -> IngestFile:
    return IngestFile(filename=name, media_type="text/plain", content_base64=content_b64, **kw)  # type: ignore[arg-type]


# ---------------------------------------------------------------------------
# 1. async gRPC turn_stream is an async generator that raises on iteration
# ---------------------------------------------------------------------------


async def test_async_grpc_turn_stream_is_iterable_and_raises_unsupported() -> None:
    # Before the fix the _Threaded wrapper returned a coroutine, so
    # ``async for`` raised TypeError instead of the typed error.
    client = AsyncMunariumClient.grpc(ClientOptions("127.0.0.1:1", token="t", uid="u"))
    try:
        with pytest.raises(UnsupportedError, match="REST"):
            async for _ in client.sessions.turn_stream("s-1", query="x"):
                pass
    finally:
        await client.close()


# ---------------------------------------------------------------------------
# 2. gRPC base64 tolerates ASCII whitespace (the REST path trims)
# ---------------------------------------------------------------------------


def test_grpc_base64_strips_ascii_whitespace_like_the_rest_path() -> None:
    slots = prepare_ingest_files([_file("a.md", "aGVsbG8=\n"), _file("b.md", " aGVs\r\nbG8= \t")])
    assert all(not isinstance(s, IngestResult) for s in slots), slots
    assert slots[0].content == b"hello" and slots[1].content == b"hello"
    # Still strict: whitespace is the ONLY tolerance.
    bad = prepare_ingest_files([_file("c.md", "aGVsbG8")])  # missing padding
    assert isinstance(bad[0], IngestResult) and bad[0].error


# ---------------------------------------------------------------------------
# 3. SSE overflow keeps the events completed in the same chunk
# ---------------------------------------------------------------------------


def test_done_followed_by_oversized_trailing_bytes_still_yields_the_done() -> None:
    from munarium_client._sse import MAX_EVENT_BYTES

    chunk = f"event: done\ndata: {json.dumps(_DONE)}\n\n".encode() + b"x" * (MAX_EVENT_BYTES + 1)
    p = SseParser()
    events = p.push(chunk)
    assert [e.event for e in events] == ["done"], "the completed done must not be discarded"
    with pytest.raises(SseOverflow):
        p.push(b"")  # poisoned: the NEXT push raises

    # Through the machine: the TurnResult is yielded and the stream is
    # terminal on success — the overflow after it is irrelevant.
    machine = TurnEventMachine()
    items = machine.feed(chunk)
    assert [type(i) for i in items] == [TurnResult]
    assert machine.terminal and machine.error is None


def test_overflow_with_nothing_completed_raises_immediately_and_the_bound_is_exact() -> None:
    from munarium_client._sse import MAX_EVENT_BYTES

    p = SseParser()
    # Exactly the cap in one data line is fine...
    assert p.push(b"data: " + b"y" * MAX_EVENT_BYTES + b"\n") == []
    # ...one more byte of data would exceed it: raise, without appending.
    with pytest.raises(SseOverflow):
        p.push(b"data: y\n")


# ---------------------------------------------------------------------------
# 4. surplus server results are an error, not a silent drop
# ---------------------------------------------------------------------------


def test_surplus_ingest_results_raise_instead_of_mispairing() -> None:
    good = base64.b64encode(b"x").decode()
    slots = prepare_ingest_files([_file("a.md", good)])
    two = [
        IngestResult(filename="a.md", existed=False, bound_to=[]),
        IngestResult(filename="ghost.md", existed=False, bound_to=[]),
    ]
    with pytest.raises(UnexpectedError, match="ghost.md"):
        splice_ingest_results(slots, two)


# ---------------------------------------------------------------------------
# 7. the REST command-retry rule, on the wire (httpx.MockTransport)
# ---------------------------------------------------------------------------


def _problem(slug: str, status: int) -> dict[str, object]:
    return {
        "type": f"https://munarium.ioka.io/problems/{slug}",
        "title": slug,
        "status": status,
        "detail": f"{slug} detail",
    }


class _Script:
    """A scripted responder: one entry per attempt (an httpx.Response, or an
    exception to raise); records the attempts it saw."""

    def __init__(self, *steps: httpx.Response | Exception) -> None:
        self.steps = list(steps)
        self.seen: list[httpx.Request] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.seen.append(request)
        step = self.steps[len(self.seen) - 1]
        if isinstance(step, Exception):
            if isinstance(step, httpx.RequestError):
                step.request = request
            raise step
        return step


def _transport(script: _Script) -> SyncRestTransport:
    return SyncRestTransport(
        ClientOptions("http://test", token="t", uid="u", read_retries=2),
        transport=httpx.MockTransport(script),
    )


@pytest.fixture(autouse=True)
def _no_backoff(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr("munarium_client._retry.sleep_sync", lambda attempt: None)


_OK_VERSION = {"version_id": "v-1"}


def test_command_is_resent_with_the_same_key_on_typed_overloaded() -> None:
    script = _Script(
        httpx.Response(503, json=_problem("overloaded", 503)),
        httpx.Response(201, json=_OK_VERSION),
    )
    t = _transport(script)
    assert t.run(specs.create_version(None, None), idempotency_key="k-1") == "v-1"
    assert len(script.seen) == 2, "the server shed it before executing — safe to re-send"
    assert {r.headers["idempotency-key"] for r in script.seen} == {"k-1"}, "SAME key"


def test_command_is_resent_on_a_connect_phase_failure() -> None:
    script = _Script(httpx.ConnectError("refused"), httpx.Response(201, json=_OK_VERSION))
    t = _transport(script)
    assert t.run(specs.create_version(None, None)) == "v-1"
    assert len(script.seen) == 2, "provably never left the client — safe to re-send"


@pytest.mark.parametrize("status", [502, 504])
def test_command_is_not_resent_on_a_transient_gateway_unexpected(status: int) -> None:
    # The C10 review's cross-client find: a 502/504 means the command may
    # still be executing upstream, so a same-key re-send could execute it
    # twice — even though the error IS classified transient.
    script = _Script(
        httpx.Response(status, json={"error": "gateway"}),
        httpx.Response(201, json=_OK_VERSION),
    )
    t = _transport(script)
    with pytest.raises(UnexpectedError) as ei:
        t.run(specs.create_version(None, None))
    assert ei.value.transient and ei.value.status == status
    assert len(script.seen) == 1, "possibly delivered — never re-sent"


@pytest.mark.parametrize("status", [502, 504])
def test_read_still_retries_the_transient_gateway_unexpected(status: int) -> None:
    script = _Script(
        httpx.Response(status, json={"error": "gateway"}),
        httpx.Response(200, json={"head_seq": 7}),
    )
    t = _transport(script)
    assert t.run(specs.head("v-1")) == 7
    assert len(script.seen) == 2, "reads are idempotent — any transient retries"


def test_overloaded_command_retry_is_bounded_then_typed() -> None:
    shed = httpx.Response(503, json=_problem("overloaded", 503))
    script = _Script(shed, shed, shed)
    with pytest.raises(OverloadedError):
        _transport(script).run(specs.create_version(None, None))
    assert len(script.seen) == 3, "1 + read_retries attempts, then the typed error"


# ---------------------------------------------------------------------------
# proto3 empty-list sentinels (the cross-file tracer's find)
# ---------------------------------------------------------------------------


def test_explicit_empty_collections_is_rejected_on_grpc_but_none_is_fine() -> None:
    good = base64.b64encode(b"x").decode()
    with pytest.raises(InvalidInputError, match="collections"):
        prepare_ingest_files([_file("a.md", good, collections=[])])
    slots = prepare_ingest_files([_file("a.md", good, collections=None)])
    assert list(slots[0].collections) == []


def test_explicit_empty_runbook_refs_is_rejected_on_grpc_mint() -> None:
    client = MunariumClient.grpc(ClientOptions("127.0.0.1:1", token="t", uid="u"))
    try:
        with pytest.raises(InvalidInputError, match="runbook_refs"):
            client.tokens.mint(uid="u", scopes=["query"], runbook_refs=[])
    finally:
        client.close()
    reject_empty_list("x", None)  # None never trips it


# ---------------------------------------------------------------------------
# large REST JSON bodies stream with bounded chunks
# ---------------------------------------------------------------------------


def test_large_json_encoder_is_equivalent_and_bounded() -> None:
    value = {
        "files": [
            {
                "filename": 'quotes-"-and-unicode-ø.md',
                "content_base64": "a" * 200_000,
                "collections": ["support", "engineering"],
            }
        ],
        "label": None,
    }
    chunks = list(_iter_json_bytes(value))
    assert json.loads(b"".join(chunks)) == value
    assert max(map(len, chunks)) <= 64 * 1024 * 6, (
        "the encoder must not recreate a request-sized bytes object"
    )


def test_ingest_specs_select_streaming_json_only_for_large_body_routes() -> None:
    file = _file("a.md", base64.b64encode(b"hello").decode())
    assert specs.ingest(file).stream_json
    assert specs.ingest_batch([file]).stream_json
    assert specs.bulk_chunk("bulk-1", [file]).stream_json
    assert not specs.turn("s-1", "q", None, True, None).stream_json


# ---------------------------------------------------------------------------
# gRPC single-file ingest() matches REST's typed 400
# ---------------------------------------------------------------------------


def test_grpc_single_ingest_rejects_bad_base64_like_the_rest_400() -> None:
    client = MunariumClient.grpc(ClientOptions("127.0.0.1:1", token="t", uid="u"))
    try:
        with pytest.raises(InvalidInputError, match="base64"):
            client.ingest.ingest(_file("a.md", "%%not-base64%%"))
    finally:
        client.close()


# ---------------------------------------------------------------------------
# the shared query-param helper
# ---------------------------------------------------------------------------


def test_params_helper_drops_none_stringifies_and_renders_bools() -> None:
    assert specs._params(a=None, b=3, c="x", d=True, e=False, from_="t0") == {
        "b": "3",
        "c": "x",
        "d": "true",
        "e": "false",
        "from": "t0",
    }
    # Flag params: an explicit False is OMITTED (callers pass ``flag or None``).
    assert specs.validate_runbook("y", False, None, None, None).params == {}
    assert specs.validate_runbook("y", True, None, None, None).params == {"suggest": "true"}
    assert specs.list_runbooks(False).params == {}
    assert specs.bulk_status("b", False).params == {}
    # Tri-state carries both values.
    assert specs.list_tokens(None, False).params == {"active": "false"}
    assert specs.list_tokens("u", None).params == {"uid": "u"}
    assert specs.audit_report(None, None, None, "a", "b", 5, False, None).params == {
        "from": "a",
        "to": "b",
        "limit": "5",
    }
    assert specs.facts("v", None, 3, ("accepted", "disputed"), None).params == {
        "as_of_seq": "3",
        "statuses": "accepted,disputed",
    }
    assert specs.facts("v", None, None, (), None).params == {}
