# SPDX-License-Identifier: Apache-2.0
"""Offline unit tests for the S-3.5 evidence-hierarchy surface.

The governing invariant of the whole S-3.x line is that a caller who does
NOT name a research profile sees byte-identical request and response
behaviour, so most of what follows pins the absence of a change: the turn
body grows no key, a legacy turn response round-trips without one, and the
gRPC request serializes to the same bytes it always did. The rest covers
what is genuinely new — the typed decision on both transports, the six
appended SSE stages, and the two management reports.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

import httpx  # noqa: E402
import pytest  # noqa: E402

from munarium_client import ClientOptions, UnsupportedError  # noqa: E402
from munarium_client import _specs as specs  # noqa: E402
from munarium_client._grpc_common import parse_turn_response  # noqa: E402
from munarium_client._proto.mmp.v1 import session_pb2  # noqa: E402
from munarium_client._sse import SseEvent, classify_turn_event  # noqa: E402
from munarium_client.grpc_transport import SyncGrpcReports  # noqa: E402
from munarium_client.models import TurnProgress, TurnResult  # noqa: E402
from munarium_client.rest import SyncRestTransport  # noqa: E402
from munarium_client.rest_planes import RestReports  # noqa: E402

# ---------------------------------------------------------------------------
# 1. the turn request: no profile, no change
# ---------------------------------------------------------------------------


def test_turn_body_gains_no_key_when_no_profile_is_named() -> None:
    body = specs.turn_body("who signed?", None, True, None)
    assert "research_profile" not in body
    # Key ORDER too, not just membership: the invariant is byte-identical
    # request bytes, and json.dumps preserves insertion order.
    assert list(body) == ["query", "complete"]
    assert json.dumps(body) == '{"query": "who signed?", "complete": true}'


def test_turn_body_carries_the_profile_when_one_is_named() -> None:
    body = specs.turn_body("who signed?", 5, True, None, "counterparty_check")
    assert body["research_profile"] == "counterparty_check"
    # Appended last, so an existing caller's four keys keep their positions.
    assert list(body) == ["query", "top_k", "complete", "research_profile"]


def test_turn_spec_threads_the_profile_into_the_json_body() -> None:
    plain = specs.turn("s-1", "q", None, None, None, None)
    assert plain.json == {"query": "q"}, "a profile-less turn posts what it always did"
    assert plain.path == "/v1/sessions/s-1/turns"

    profiled = specs.turn("s-1", "q", None, None, None, "counterparty_check")
    assert profiled.json == {"query": "q", "research_profile": "counterparty_check"}
    # The paid-call posture is untouched by the new field.
    assert profiled.retry == "write" and profiled.timeout == "exempt"


# ---------------------------------------------------------------------------
# 2. the turn response: the decision, and its absence
# ---------------------------------------------------------------------------

_LEGACY_TURN: dict[str, object] = {
    "session_id": "s-1",
    "ordinal": 1,
    "collections_searched": ["docs"],
    "skipped": [],
    "hits": [],
    "envelopes": [],
}

_HIERARCHY: dict[str, object] = {
    "profile": "counterparty_check",
    "intent_kind": "enumerate",
    "intent_explicit": True,
    "layers": [
        {
            "layer": "register",
            "role": "controlling",
            "requirement": "required",
            "block": "complete_table",
            "evidence_id": "ev-1",
            "supports_completeness": True,
            "elapsed_ms": 42,
        },
        {
            "layer": "documents",
            "role": "supporting",
            "requirement": "optional",
            "block": "refusal",
            "supports_completeness": False,
            "refusal_code": "evidence-expired",
            "elapsed_ms": 7,
        },
    ],
    "completeness_available": True,
    "disclosed_conflicts": 1,
    "conflicts_policy": "disclose",
}


def test_a_turn_response_without_a_hierarchy_round_trips_without_the_key() -> None:
    result = TurnResult.model_validate(_LEGACY_TURN)
    assert result.hierarchy is None
    # exclude_none is what a caller re-serializing a stored transcript uses;
    # the new optional must not make a legacy turn's JSON grow a key.
    assert "hierarchy" not in result.model_dump(exclude_none=True)
    assert result.model_dump(exclude_none=True) == _LEGACY_TURN


def test_a_hierarchy_decision_parses_into_typed_layers() -> None:
    result = TurnResult.model_validate({**_LEGACY_TURN, "hierarchy": _HIERARCHY})
    h = result.hierarchy
    assert h is not None
    assert (h.profile, h.intent_kind, h.intent_explicit) == (
        "counterparty_check",
        "enumerate",
        True,
    )
    assert h.disclosed_conflicts == 1 and h.conflicts_policy == "disclose"
    register, documents = h.layers
    assert register.block == "complete_table" and register.supports_completeness
    assert register.evidence_id == "ev-1" and register.refusal_code is None
    # A refusing layer is still a layer that ran: it reports its cost.
    assert documents.refusal_code == "evidence-expired" and documents.elapsed_ms == 7
    assert not documents.supports_completeness


def test_disclosed_conflicts_defaults_when_the_server_omits_it() -> None:
    # The server declares it `#[serde(default)]`, so the client must too —
    # otherwise a lean body fails validation on a field nobody set.
    lean = {k: v for k, v in _HIERARCHY.items() if k != "disclosed_conflicts"}
    result = TurnResult.model_validate({**_LEGACY_TURN, "hierarchy": lean})
    assert result.hierarchy is not None and result.hierarchy.disclosed_conflicts == 0


# ---------------------------------------------------------------------------
# 3. gRPC parity
# ---------------------------------------------------------------------------


def test_grpc_turn_request_bytes_are_unchanged_without_a_profile() -> None:
    # proto3 does not put a default-valued scalar on the wire, so the empty
    # string the transport sends for None is literally no bytes at all.
    before = session_pb2.TurnRequest(session_id="s-1", query="q")
    after = session_pb2.TurnRequest(session_id="s-1", query="q", research_profile="")
    assert before.SerializeToString() == after.SerializeToString()
    assert (
        session_pb2.TurnRequest(
            session_id="s-1", query="q", research_profile="p"
        ).SerializeToString()
        != before.SerializeToString()
    )


def test_grpc_turn_response_without_a_hierarchy_parses_to_none() -> None:
    resp = session_pb2.TurnResponse(session_id="s-1", ordinal=1)
    assert parse_turn_response(resp).hierarchy is None


def test_grpc_hierarchy_parses_empty_strings_back_to_none() -> None:
    resp = session_pb2.TurnResponse(
        session_id="s-1",
        ordinal=1,
        hierarchy=session_pb2.EvidenceHierarchyDecision(
            profile="counterparty_check",
            intent_explicit=False,
            layers=[
                session_pb2.LayerOutcome(
                    layer="documents",
                    role="supporting",
                    requirement="fallback",
                    block="document_hits",
                    supports_completeness=False,
                    elapsed_ms=3,
                )
            ],
            completeness_available=False,
            conflicts_policy="disclose",
        ),
    )
    h = parse_turn_response(resp).hierarchy
    assert h is not None
    # Both transports must produce the SAME TurnResult, so proto3's unset
    # strings have to come back as the None the REST twin parses.
    assert h.intent_kind is None
    assert h.layers[0].evidence_id is None and h.layers[0].refusal_code is None


# ---------------------------------------------------------------------------
# 4. the appended SSE stages
# ---------------------------------------------------------------------------

_NEW_STAGES: list[dict[str, object]] = [
    {
        "stage": "profile",
        "profile": "counterparty_check",
        "layers": ["register", "documents"],
        "intent_kind": "enumerate",
        "intent_explicit": True,
    },
    {"stage": "layer_start", "layer": "register", "role": "controlling", "requirement": "required"},
    {"stage": "layer_source", "layer": "register", "source": "holdings", "provider": "matrix"},
    {
        "stage": "layer_complete",
        "layer": "register",
        "block": "complete_table",
        "supports_completeness": True,
        "refusal_code": None,
        "elapsed_ms": 42,
    },
    {"stage": "coverage", "completeness_available": True, "disclosed_conflicts": 1},
    {"stage": "compose", "layers_used": 2, "context_chars": 8192, "layers_dropped": ["documents"]},
]


@pytest.mark.parametrize("payload", _NEW_STAGES, ids=[str(p["stage"]) for p in _NEW_STAGES])
def test_the_hierarchy_stages_survive_classification_with_their_fields(
    payload: dict[str, object],
) -> None:
    item = classify_turn_event(SseEvent("progress", json.dumps(payload)))
    assert isinstance(item, TurnProgress) and item.stage == payload["stage"]
    # TurnProgress declares only `stage`; the per-stage fields ride as
    # pydantic extras, so an operator can read them without a typed model
    # per stage — and a stage this build cannot name still gets through.
    assert item.model_extra == {k: v for k, v in payload.items() if k != "stage"}


def test_verify_gains_an_optional_layer_without_disturbing_the_legacy_shape() -> None:
    legacy = classify_turn_event(
        SseEvent("progress", json.dumps({"stage": "verify", "attempt": 0, "violations": 0}))
    )
    assert isinstance(legacy, TurnProgress) and "layer" not in (legacy.model_extra or {})

    layered = classify_turn_event(
        SseEvent(
            "progress",
            json.dumps({"stage": "verify", "attempt": 0, "violations": 0, "layer": "register"}),
        )
    )
    assert isinstance(layered, TurnProgress)
    assert (layered.model_extra or {})["layer"] == "register"


def test_a_stage_this_build_cannot_name_is_still_yielded() -> None:
    # Progress is informational and the server is free to grow: an unknown
    # stage must never end iteration, which is what would happen if the
    # model enumerated stages instead of accepting extras.
    item = classify_turn_event(SseEvent("progress", '{"stage": "some_future_stage", "n": 1}'))
    assert isinstance(item, TurnProgress) and item.stage == "some_future_stage"


# ---------------------------------------------------------------------------
# 5. the two management reports
# ---------------------------------------------------------------------------


class _Responder:
    """Answers with one canned body, recording the requests it saw."""

    def __init__(self, body: dict[str, object]) -> None:
        self.body = body
        self.seen: list[httpx.Request] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.seen.append(request)
        return httpx.Response(200, json=self.body)


def _reports(responder: _Responder) -> RestReports:
    return RestReports(
        SyncRestTransport(
            ClientOptions("http://test", token="t", uid="u"),
            transport=httpx.MockTransport(responder),
        )
    )


_EVIDENCE_BODY: dict[str, object] = {
    "window": "7d",
    "hierarchy_turns": 12,
    "legacy_turns": 340,
    "completeness_available": 9,
    "layers": [
        {
            "profile": "counterparty_check",
            "layer": "register",
            "turns": 12,
            "refusals": 5,
            "complete": 7,
            "refusal_codes": ["matrix-unavailable", "evidence-expired"],
            "p50_ms": 41,
            "p95_ms": 190,
        }
    ],
}


def test_evidence_report_hits_the_route_and_types_the_layer_stats() -> None:
    responder = _Responder(_EVIDENCE_BODY)
    report = _reports(responder).evidence(window="7d")
    assert responder.seen[0].url.path == "/v1/reports/evidence"
    assert dict(responder.seen[0].url.params) == {"window": "7d"}
    assert (report.window, report.hierarchy_turns, report.legacy_turns) == ("7d", 12, 340)
    assert report.completeness_available == 9
    (layer,) = report.layers
    assert layer.profile == "counterparty_check" and layer.layer == "register"
    assert layer.refusals == 5 and layer.refusal_codes[0] == "matrix-unavailable"
    assert (layer.p50_ms, layer.p95_ms) == (41, 190)


def test_evidence_report_omits_the_window_when_unset() -> None:
    responder = _Responder(_EVIDENCE_BODY)
    _reports(responder).evidence()
    assert not responder.seen[0].url.params, "no window = let the server default stand"


_MATRIX_BODY: dict[str, object] = {
    "configured": True,
    "circuit_open": False,
    "consecutive_failures": 0,
    "data_views": [
        {
            "runbook_ref": "diligence@3",
            "name": "holdings",
            "contract": "holdings_by_company",
            "access_level": 2,
        }
    ],
}


def test_matrix_report_hits_the_route_and_types_the_data_views() -> None:
    responder = _Responder(_MATRIX_BODY)
    report = _reports(responder).matrix()
    assert responder.seen[0].url.path == "/v1/reports/matrix"
    assert not responder.seen[0].url.params, "the route takes none"
    assert report.configured and not report.circuit_open
    (view,) = report.data_views
    assert view.runbook_ref == "diligence@3" and view.contract == "holdings_by_company"
    assert view.access_level == 2


def test_an_unwired_matrix_plane_is_not_a_failing_one() -> None:
    # configured=False and circuit_open=True are different operational
    # facts; the model keeps them separate so a dashboard cannot conflate
    # "never set up" with "tripped".
    responder = _Responder(
        {"configured": False, "circuit_open": False, "consecutive_failures": 0, "data_views": []}
    )
    report = _reports(responder).matrix()
    assert not report.configured and not report.circuit_open and not report.data_views


def test_both_reports_are_rest_only_on_grpc() -> None:
    reports = SyncGrpcReports()
    with pytest.raises(UnsupportedError):
        reports.evidence(window="24h")
    with pytest.raises(UnsupportedError):
        reports.matrix()
