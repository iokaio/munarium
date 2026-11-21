# SPDX-License-Identifier: Apache-2.0
"""Offline unit tests for the Server 1.5 (ClearView) wire fields on the typed
facade: C1 ``collection_availability``/``coverage``, R1
``disclose_execution``/``execution``, the IR-3 turn ``execution`` and R11
``retryable``/``retry_action``.

Each new request option is sent only when the caller sets it, so a default
call keeps the v1.4 request bytes; each new response member is optional, so a
v1.4 server's response (which never carries it) still decodes.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

import httpx  # noqa: E402
from google.rpc import error_details_pb2  # noqa: E402

from munarium_client import (  # noqa: E402
    ClientOptions,
    HeadConflictError,
    InvalidInputError,
    OverloadedError,
)
from munarium_client import _specs as specs  # noqa: E402
from munarium_client._errors import error_from_grpc, error_from_problem  # noqa: E402
from munarium_client._grpc_common import parse_turn_response  # noqa: E402
from munarium_client._proto.mmp.v1 import common_pb2, retrieval_pb2, session_pb2  # noqa: E402
from munarium_client._sse import SseEvent, classify_turn_event  # noqa: E402
from munarium_client.grpc_transport import (  # noqa: E402
    SyncGrpcRetrieval,
    SyncGrpcSessions,
)
from munarium_client.models import SearchResult, TurnProgress, TurnResult  # noqa: E402
from munarium_client.rest import SyncRestTransport  # noqa: E402
from munarium_client.rest_planes import RestRetrieval, RestSessions  # noqa: E402

_LEGACY_TURN: dict[str, object] = {
    "session_id": "s-1",
    "ordinal": 1,
    "collections_searched": ["contracts", "letters"],
    "skipped": [],
    "hits": [],
    "envelopes": [],
}

_COVERAGE: dict[str, object] = {
    "policy": "report",
    "complete": False,
    "members": [
        {"collection": "contracts", "stage": "search", "outcome": "complete", "hits": 3},
        {"collection": "memos", "stage": "search", "outcome": "empty", "hits": 0},
        {
            "collection": "letters",
            "stage": "probe",
            "outcome": "unavailable",
            "reason": "no_active_index",
        },
    ],
    "withheld_claims": ["absence", "exhaustive_coverage"],
}

_ENVELOPE: dict[str, object] = {
    "chunk_ids": [],
    "source_ids": [],
    "source_paths": [],
    "source_content_hashes": [],
    "index_version": "iv-1",
    "event_watermark": 9,
}

_EXECUTION: dict[str, object] = {
    "version": 1,
    "engine": "postgresql",
    "lexical_domain": "ts_rank",
    "legs": [
        {"leg": "lexical", "candidate_limit": 50, "fetched": 12, "exhausted": True},
        {"leg": "vector", "candidate_limit": 50, "fetched": None, "exhausted": None},
    ],
    "stages": [
        {
            "stage": "reranking",
            "policy": "prefer",
            "policy_revision": 4,
            "applied": False,
            "state": "unavailable",
            "fallback": "fused_order",
        }
    ],
}


class _Responder:
    """Answers with one canned body, recording the requests it saw."""

    def __init__(self, body: dict[str, object]) -> None:
        self.body = body
        self.seen: list[httpx.Request] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.seen.append(request)
        return httpx.Response(200, json=self.body)


def _rest(responder: _Responder) -> SyncRestTransport:
    return SyncRestTransport(
        ClientOptions("http://test", token="t", uid="u"),
        transport=httpx.MockTransport(responder),
    )


class _Stub:
    """Stands in for a grpc stub method; only identity matters."""


class _FakeCore:
    """Records the message each typed gRPC call would send."""

    def __init__(self, response: Any) -> None:
        self.response = response
        self.sent: list[Any] = []
        self.sessions = type("S", (), {"Turn": _Stub()})()
        self.retrieval = type("R", (), {"HybridSearch": _Stub()})()

    def call(self, _fn: Any, msg: Any, **_kw: Any) -> Any:
        self.sent.append(msg)
        return self.response


# ---------------------------------------------------------------------------
# C1: the request option
# ---------------------------------------------------------------------------


def test_turn_body_omits_collection_availability_unless_set() -> None:
    assert specs.turn_body("q", None, None, None) == {"query": "q"}
    body = specs.turn_body("q", None, None, None, None, "report")
    assert body == {"query": "q", "collection_availability": "report"}


def test_rest_turn_sends_the_policy_only_when_selected() -> None:
    responder = _Responder(_LEGACY_TURN)
    sessions = RestSessions(_rest(responder))
    sessions.turn("s-1", query="q")
    sessions.turn("s-1", query="q", collection_availability="require")
    plain, selected = (json.loads(r.content) for r in responder.seen)
    assert "collection_availability" not in plain
    assert selected["collection_availability"] == "require"


def test_grpc_turn_sends_the_policy_only_when_selected() -> None:
    core = _FakeCore(session_pb2.TurnResponse(session_id="s-1", ordinal=1))
    sessions = SyncGrpcSessions(core)  # type: ignore[arg-type]
    sessions.turn("s-1", query="q")
    sessions.turn("s-1", query="q", collection_availability="report")
    plain, selected = core.sent
    assert plain.collection_availability == ""
    # proto3 puts no bytes on the wire for "": the v1.4 request exactly.
    assert (
        plain.SerializeToString()
        == session_pb2.TurnRequest(session_id="s-1", query="q").SerializeToString()
    )
    assert selected.collection_availability == "report"


# ---------------------------------------------------------------------------
# C1: the response member
# ---------------------------------------------------------------------------


def _assert_coverage(result: TurnResult) -> None:
    c = result.coverage
    assert c is not None
    assert (c.policy, c.complete) == ("report", False)
    assert c.withheld_claims == ["absence", "exhaustive_coverage"]
    complete, empty, unavailable = c.members
    assert (complete.collection, complete.outcome, complete.hits) == ("contracts", "complete", 3)
    assert complete.layer is None and complete.reason is None
    # An authorized empty member is a successful outcome with a zero count.
    assert (empty.outcome, empty.hits) == ("empty", 0)
    assert (unavailable.stage, unavailable.outcome) == ("probe", "unavailable")
    assert unavailable.hits is None and unavailable.reason == "no_active_index"


def test_rest_turn_decodes_coverage() -> None:
    responder = _Responder({**_LEGACY_TURN, "coverage": _COVERAGE})
    result = RestSessions(_rest(responder)).turn("s-1", query="q", collection_availability="report")
    _assert_coverage(result)


def test_a_v14_turn_without_coverage_decodes_to_none() -> None:
    result = TurnResult.model_validate(_LEGACY_TURN)
    assert result.coverage is None
    assert result.model_dump(exclude_none=True) == _LEGACY_TURN


def test_coverage_round_trips_through_json() -> None:
    result = TurnResult.model_validate({**_LEGACY_TURN, "coverage": _COVERAGE})
    _assert_coverage(TurnResult.model_validate_json(result.model_dump_json()))
    assert result.model_dump(exclude_none=True)["coverage"] == _COVERAGE


def test_sse_done_carries_coverage() -> None:
    item = classify_turn_event(
        SseEvent("done", json.dumps({**_LEGACY_TURN, "coverage": _COVERAGE}))
    )
    assert isinstance(item, TurnResult)
    _assert_coverage(item)
    legacy = classify_turn_event(SseEvent("done", json.dumps(_LEGACY_TURN)))
    assert isinstance(legacy, TurnResult) and legacy.coverage is None


def test_sse_member_progress_keeps_outcome_and_reason() -> None:
    payload = {
        "stage": "retrieval",
        "collection": "letters",
        "hits": 0,
        "skipped": False,
        "outcome": "unavailable",
        "reason": "engine_unavailable",
    }
    item = classify_turn_event(SseEvent("progress", json.dumps(payload)))
    assert isinstance(item, TurnProgress)
    assert (item.model_extra or {})["outcome"] == "unavailable"
    assert (item.model_extra or {})["reason"] == "engine_unavailable"


def test_grpc_coverage_parses_like_the_rest_twin() -> None:
    resp = session_pb2.TurnResponse(
        session_id="s-1",
        ordinal=1,
        collections_searched=["contracts", "letters"],
        coverage=session_pb2.TurnCoverage(
            policy="report",
            complete=False,
            members=[
                session_pb2.CollectionOutcome(
                    collection="contracts", stage="search", outcome="complete", hits=3
                ),
                session_pb2.CollectionOutcome(
                    collection="memos", stage="search", outcome="empty", hits=0
                ),
                session_pb2.CollectionOutcome(
                    collection="letters",
                    stage="probe",
                    outcome="unavailable",
                    reason="no_active_index",
                ),
            ],
            withheld_claims=["absence", "exhaustive_coverage"],
        ),
    )
    grpc_result = parse_turn_response(resp)
    _assert_coverage(grpc_result)
    rest_result = TurnResult.model_validate({**_LEGACY_TURN, "coverage": _COVERAGE})
    assert grpc_result.coverage == rest_result.coverage


def test_grpc_turn_without_coverage_parses_to_none() -> None:
    assert parse_turn_response(session_pb2.TurnResponse(session_id="s-1")).coverage is None


# ---------------------------------------------------------------------------
# R1: disclose_execution / execution
# ---------------------------------------------------------------------------

_SEARCH: dict[str, object] = {"hits": [], "envelope": _ENVELOPE}


def test_rest_search_sends_disclose_execution_only_when_set() -> None:
    responder = _Responder({**_SEARCH, "execution": _EXECUTION})
    retrieval = RestRetrieval(_rest(responder))
    retrieval.search(query="q", shape_ref="docs")
    result = retrieval.search(query="q", shape_ref="docs", disclose_execution=True)
    plain, disclosed = (json.loads(r.content) for r in responder.seen)
    assert "disclose_execution" not in plain
    assert disclosed["disclose_execution"] is True
    e = result.execution
    assert e is not None and (e.version, e.engine) == (1, "postgresql")
    lexical, vector = e.legs
    assert (lexical.fetched, lexical.exhausted) == (12, True)
    # null is "not measured": unknown, never zero.
    assert vector.fetched is None and vector.exhausted is None
    assert e.stages[0].fallback == "fused_order" and e.stages[0].reason is None


def test_a_v14_search_without_execution_decodes_to_none() -> None:
    result = SearchResult.model_validate(_SEARCH)
    assert result.execution is None
    assert "execution" not in result.model_dump(exclude_none=True)


def test_grpc_search_sends_and_parses_execution() -> None:
    envelope = common_pb2.ProvenanceEnvelope(index_version="iv-1", event_watermark=9)
    execution = retrieval_pb2.SearchExecution(
        version=1,
        engine="postgresql",
        lexical_domain="ts_rank",
        legs=[
            retrieval_pb2.SearchLegExecution(leg="lexical", candidate_limit=50, fetched=0),
            retrieval_pb2.SearchLegExecution(leg="vector"),
        ],
    )
    core = _FakeCore(retrieval_pb2.HybridSearchResponse(envelope=envelope, execution=execution))
    retrieval = SyncGrpcRetrieval(core)  # type: ignore[arg-type]
    result = retrieval.search(query="q", shape_ref="docs", disclose_execution=True)
    assert core.sent[0].disclose_execution is True
    e = result.execution
    assert e is not None and e.stages == []
    lexical, vector = e.legs
    # A measured zero stays zero; an unset optional stays unknown.
    assert (lexical.candidate_limit, lexical.fetched, lexical.exhausted) == (50, 0, None)
    assert (vector.candidate_limit, vector.fetched, vector.exhausted) == (None, None, None)

    core = _FakeCore(retrieval_pb2.HybridSearchResponse(envelope=envelope))
    plain = SyncGrpcRetrieval(core).search(query="q", shape_ref="docs")  # type: ignore[arg-type]
    assert core.sent[0].disclose_execution is False and plain.execution is None


# ---------------------------------------------------------------------------
# IR-3: the multi-collection turn execution
# ---------------------------------------------------------------------------

_TURN_EXECUTION: dict[str, object] = {
    "version": 1,
    "stages": [
        {
            "stage": "reranking",
            "policy": "require",
            "policy_revision": 2,
            "applied": True,
            "state": "ready",
            "identity": "rr-a",
            "scored": 8,
            "collection": "contracts",
        },
        {
            "stage": "reranking",
            "policy": "prefer",
            "policy_revision": 5,
            "applied": False,
            "state": "unavailable",
            "fallback": "fused_order",
            "collection": "letters",
        },
    ],
}


def _assert_turn_execution(result: TurnResult) -> None:
    e = result.execution
    assert e is not None and e.version == 1
    contracts, letters = e.stages
    assert (contracts.collection, contracts.applied, contracts.scored) == ("contracts", True, 8)
    assert contracts.identity == "rr-a" and contracts.fallback is None
    assert (letters.collection, letters.fallback) == ("letters", "fused_order")


def test_rest_turn_decodes_execution() -> None:
    body = {**_LEGACY_TURN, "coverage": _COVERAGE, "execution": _TURN_EXECUTION}
    result = RestSessions(_rest(_Responder(body))).turn("s-1", query="q")
    _assert_turn_execution(result)
    _assert_coverage(result)
    assert result.model_dump(exclude_none=True)["execution"] == _TURN_EXECUTION


def test_a_turn_without_execution_or_with_null_decodes_to_none() -> None:
    for body in (_LEGACY_TURN, {**_LEGACY_TURN, "execution": None}):
        result = TurnResult.model_validate(body)
        assert result.execution is None
        assert result.model_dump(exclude_none=True) == _LEGACY_TURN


def test_sse_done_carries_execution() -> None:
    item = classify_turn_event(
        SseEvent("done", json.dumps({**_LEGACY_TURN, "execution": _TURN_EXECUTION}))
    )
    assert isinstance(item, TurnResult)
    _assert_turn_execution(item)
    legacy = classify_turn_event(SseEvent("done", json.dumps(_LEGACY_TURN)))
    assert isinstance(legacy, TurnResult) and legacy.execution is None


def test_grpc_turn_carries_no_execution() -> None:
    assert parse_turn_response(session_pb2.TurnResponse(session_id="s-1")).execution is None


def test_single_search_stage_collection_is_optional() -> None:
    named = json.loads(json.dumps(_EXECUTION))
    named["stages"][0]["collection"] = "contracts"
    result = SearchResult.model_validate({**_SEARCH, "execution": named})
    assert result.execution is not None
    assert result.execution.stages[0].collection == "contracts"
    plain = SearchResult.model_validate({**_SEARCH, "execution": _EXECUTION})
    assert plain.execution is not None and plain.execution.stages[0].collection is None
    stage = plain.model_dump(exclude_none=True)["execution"]["stages"][0]
    assert "collection" not in stage


# ---------------------------------------------------------------------------
# R11: retryable / retry_action
# ---------------------------------------------------------------------------


def _problem(slug: str, status: int, **ext: object) -> dict[str, object]:
    return {
        "type": f"https://munarium.ioka.io/problems/{slug}",
        "title": slug,
        "status": status,
        "detail": "d",
        **ext,
    }


def test_rest_problem_decodes_retry_guidance() -> None:
    err = error_from_problem(
        503, _problem("overloaded", 503, retryable=True, retry_action="repeat")
    )
    assert isinstance(err, OverloadedError)
    assert (err.retryable, err.retry_action) == (True, "repeat")
    err = error_from_problem(
        409,
        _problem(
            "head-conflict", 409, expected=1, actual=2, retryable=False, retry_action="reread"
        ),
    )
    assert isinstance(err, HeadConflictError) and (err.expected, err.actual) == (1, 2)
    assert (err.retryable, err.retry_action) == (False, "reread")


def test_a_v14_problem_carries_no_retry_guidance() -> None:
    err = error_from_problem(400, _problem("invalid-input", 400))
    assert isinstance(err, InvalidInputError)
    assert err.retryable is None and err.retry_action is None


def test_grpc_error_info_decodes_retry_guidance() -> None:
    info = error_details_pb2.ErrorInfo(reason="run-locked", domain="mmp.ioka.io")
    info.metadata["retryable"] = "false"
    info.metadata["retry_action"] = "retry_later"
    err = error_from_grpc("ABORTED", "locked", info)
    assert (err.slug, err.retryable, err.retry_action) == ("run-locked", False, "retry_later")

    bare = error_details_pb2.ErrorInfo(reason="invalid-input", domain="mmp.ioka.io")
    err = error_from_grpc("INVALID_ARGUMENT", "bad", bare)
    assert err.retryable is None and err.retry_action is None
