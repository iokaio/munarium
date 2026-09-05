# SPDX-License-Identifier: Apache-2.0
"""Offline unit tests for the max-tokens budgets surface (``GET``/``POST``
``/v1/max-tokens``): the typed models, the sync AND async REST planes over
a mocked transport, the problem+json decodes, and the gRPC refusal. The
last section is a method-parity guard across every plane's sync, async and
gRPC classes — an async twin has been shipped short before, and a guard
that runs is cheaper than a defect a caller finds."""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

import httpx  # noqa: E402
import pytest  # noqa: E402
from pydantic import ValidationError  # noqa: E402

from munarium_client import (  # noqa: E402
    AsyncMunariumClient,
    ClientOptions,
    ForbiddenError,
    InvalidInputError,
    MunariumClient,
    UnsupportedError,
)
from munarium_client import grpc_transport as g  # noqa: E402
from munarium_client import rest_planes as p  # noqa: E402
from munarium_client.models import MaxTokensBudgets, MaxTokensResponse  # noqa: E402
from munarium_client.rest import AsyncRestTransport, SyncRestTransport  # noqa: E402

FIELDS = (
    "turn_completion",
    "query_expansion",
    "complete_default",
    "healthai_probe",
    "hierarchy_classifier",
    "hierarchy_intent",
    "runbook_advisory",
    "authoring_assist",
)

# Deliberately NOT the built-ins, so a decode that fell back to a default
# would be visible.
_BUDGETS: dict[str, int] = {
    "turn_completion": 4096,
    "query_expansion": 128,
    "complete_default": 2048,
    "healthai_probe": 256,
    "hierarchy_classifier": 48,
    "hierarchy_intent": 600,
    "runbook_advisory": 1024,
    "authoring_assist": 4096,
}
_TENANT_BODY: dict[str, object] = {
    **_BUDGETS,
    "source": "tenant",
    "updated_at": "2026-09-02T10:15:00Z",
}
_ENVIRONMENT_BODY: dict[str, object] = {**_BUDGETS, "source": "environment"}

_REST = ClientOptions("http://test", token="t", uid="u")
# An insecure channel is created lazily; nothing ever connects.
_GRPC = ClientOptions("127.0.0.1:1", token="t", uid="u")


def _problem(slug: str, status: int) -> dict[str, object]:
    return {
        "type": f"https://munarium.ioka.io/problems/{slug}",
        "title": slug,
        "status": status,
        "detail": f"{slug} detail",
    }


class _Responder:
    """Answers with one canned status + body, recording the requests it saw."""

    def __init__(self, body: dict[str, object], status: int = 200) -> None:
        self.body = body
        self.status = status
        self.seen: list[httpx.Request] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.seen.append(request)
        return httpx.Response(self.status, json=self.body)

    @property
    def only(self) -> httpx.Request:
        (request,) = self.seen
        return request


def _sync(responder: _Responder) -> p.RestProviders:
    return p.RestProviders(SyncRestTransport(_REST, transport=httpx.MockTransport(responder)))


def _async(responder: _Responder) -> p.AsyncRestProviders:
    return p.AsyncRestProviders(AsyncRestTransport(_REST, transport=httpx.MockTransport(responder)))


def _assert_tenant_shape(got: MaxTokensResponse) -> None:
    assert isinstance(got, MaxTokensBudgets), "a read result IS a budgets set (round-trip)"
    assert {f: getattr(got, f) for f in FIELDS} == _BUDGETS
    assert got.source == "tenant"
    assert got.updated_at == "2026-09-02T10:15:00Z"


# ---------------------------------------------------------------------------
# 1. GET /v1/max-tokens
# ---------------------------------------------------------------------------


def test_get_hits_the_route_and_decodes_the_flattened_shape() -> None:
    responder = _Responder(_TENANT_BODY)
    got = _sync(responder).max_tokens()
    req = responder.only
    assert (req.method, req.url.path) == ("GET", "/v1/max-tokens")
    assert not req.url.params, "the route takes none"
    assert req.headers["authorization"] == "Bearer t"
    assert req.headers["x-munarium-uid"] == "u"
    _assert_tenant_shape(got)


def test_get_environment_source_carries_no_updated_at() -> None:
    got = _sync(_Responder(_ENVIRONMENT_BODY)).max_tokens()
    assert got.source == "environment" and got.updated_at is None
    assert {f: getattr(got, f) for f in FIELDS} == _BUDGETS


# ---------------------------------------------------------------------------
# 2. POST /v1/max-tokens
# ---------------------------------------------------------------------------


def test_replace_sends_exactly_the_eight_fields_and_decodes_the_answer() -> None:
    responder = _Responder(_TENANT_BODY)
    got = _sync(responder).replace_max_tokens(MaxTokensBudgets(**_BUDGETS))
    req = responder.only
    assert (req.method, req.url.path) == ("POST", "/v1/max-tokens")
    assert req.headers["content-type"] == "application/json"
    sent = json.loads(req.content)
    assert sent == _BUDGETS
    assert set(sent) == set(FIELDS), "all eight, nothing else"
    # A whole-set replace is a write like apply_config: sent once, no
    # idempotency key minted for it.
    assert "idempotency-key" not in req.headers
    _assert_tenant_shape(got)


def test_replace_accepts_the_dict_shape() -> None:
    responder = _Responder(_TENANT_BODY)
    _sync(responder).replace_max_tokens(dict(_BUDGETS))
    assert json.loads(responder.only.content) == _BUDGETS


def test_a_read_result_edited_in_place_round_trips_without_source_or_updated_at() -> None:
    read = _sync(_Responder(_TENANT_BODY)).max_tokens()
    read.turn_completion = 8192
    responder = _Responder({**_TENANT_BODY, "turn_completion": 8192})
    got = _sync(responder).replace_max_tokens(read)
    sent = json.loads(responder.only.content)
    assert sent == {**_BUDGETS, "turn_completion": 8192}
    assert "source" not in sent and "updated_at" not in sent
    assert got.turn_completion == 8192


def test_dict_extras_do_not_reach_the_wire() -> None:
    # A dict copied from a GET body (source + updated_at riding along) is
    # the obvious caller shape; the server ignores extras, but the client
    # sends the replacement shape and nothing else regardless.
    responder = _Responder(_TENANT_BODY)
    _sync(responder).replace_max_tokens(dict(_TENANT_BODY))
    assert json.loads(responder.only.content) == _BUDGETS


def test_a_partial_set_is_refused_before_the_wire() -> None:
    # There is no partial update on the server; the model says so first.
    responder = _Responder(_TENANT_BODY)
    partial = {k: v for k, v in _BUDGETS.items() if k != "authoring_assist"}
    with pytest.raises(ValidationError, match="authoring_assist"):
        _sync(responder).replace_max_tokens(partial)
    assert not responder.seen


# ---------------------------------------------------------------------------
# 3. errors — problem+json through the slug registry
# ---------------------------------------------------------------------------


def test_an_out_of_range_value_is_the_servers_invalid_input() -> None:
    responder = _Responder(_problem("invalid-input", 400), status=400)
    with pytest.raises(InvalidInputError) as info:
        _sync(responder).replace_max_tokens({**_BUDGETS, "turn_completion": 1})
    assert info.value.slug == "invalid-input"
    assert not info.value.transient
    assert len(responder.seen) == 1, "a 400 is never retried"


def test_a_non_rw_replace_is_forbidden() -> None:
    responder = _Responder(_problem("forbidden", 403), status=403)
    with pytest.raises(ForbiddenError):
        _sync(responder).replace_max_tokens(_BUDGETS)


# ---------------------------------------------------------------------------
# 4. the async twin rides the same specs
# ---------------------------------------------------------------------------


async def test_async_get_and_replace_hit_the_same_routes() -> None:
    responder = _Responder(_TENANT_BODY)
    plane = _async(responder)
    _assert_tenant_shape(await plane.max_tokens())
    _assert_tenant_shape(await plane.replace_max_tokens(_BUDGETS))
    first, second = responder.seen
    assert (first.method, first.url.path) == ("GET", "/v1/max-tokens")
    assert (second.method, second.url.path) == ("POST", "/v1/max-tokens")
    assert json.loads(second.content) == _BUDGETS


async def test_async_400_decodes_the_same_way() -> None:
    responder = _Responder(_problem("invalid-input", 400), status=400)
    with pytest.raises(InvalidInputError):
        await _async(responder).replace_max_tokens(_BUDGETS)


# ---------------------------------------------------------------------------
# 5. gRPC: REST-only, refused out loud on both facades
# ---------------------------------------------------------------------------


def test_grpc_refuses_both_as_unsupported() -> None:
    with MunariumClient.grpc(_GRPC) as client:
        with pytest.raises(UnsupportedError, match="GET /v1/max-tokens"):
            client.providers.max_tokens()
        with pytest.raises(UnsupportedError, match="POST /v1/max-tokens"):
            client.providers.replace_max_tokens(_BUDGETS)


async def test_async_grpc_refuses_both_as_unsupported() -> None:
    async with AsyncMunariumClient.grpc(_GRPC) as client:
        with pytest.raises(UnsupportedError, match="GET /v1/max-tokens"):
            await client.providers.max_tokens()
        with pytest.raises(UnsupportedError, match="POST /v1/max-tokens"):
            await client.providers.replace_max_tokens(_BUDGETS)


# ---------------------------------------------------------------------------
# 6. surface parity: every plane, all three classes, the same method names
# ---------------------------------------------------------------------------

_PLANES: list[tuple[str, type, type, type]] = [
    ("commands", p.RestCommands, p.AsyncRestCommands, g.SyncGrpcCommands),
    ("query", p.RestQuery, p.AsyncRestQuery, g.SyncGrpcQuery),
    ("ingest", p.RestIngest, p.AsyncRestIngest, g.SyncGrpcIngest),
    ("retrieval", p.RestRetrieval, p.AsyncRestRetrieval, g.SyncGrpcRetrieval),
    ("runbooks", p.RestRunbooks, p.AsyncRestRunbooks, g.SyncGrpcRunbooks),
    ("providers", p.RestProviders, p.AsyncRestProviders, g.SyncGrpcProviders),
    ("sessions", p.RestSessions, p.AsyncRestSessions, g.SyncGrpcSessions),
    ("tokens", p.RestTokens, p.AsyncRestTokens, g.SyncGrpcTokens),
    ("reports", p.RestReports, p.AsyncRestReports, g.SyncGrpcReports),
    ("authoring", p.RestAuthoring, p.AsyncRestAuthoring, g.SyncGrpcAuthoring),
    ("evidence", p.RestEvidence, p.AsyncRestEvidence, g.SyncGrpcEvidence),
]


def _public_methods(cls: type) -> set[str]:
    return {n for n, v in vars(cls).items() if not n.startswith("_") and callable(v)}


@pytest.mark.parametrize(
    ("name", "rest", "async_rest", "grpc"), _PLANES, ids=[n for n, *_ in _PLANES]
)
def test_every_plane_exposes_the_same_methods_on_all_three_surfaces(
    name: str, rest: type, async_rest: type, grpc: type
) -> None:
    # The async gRPC facade wraps the sync gRPC plane via __getattr__, so
    # three classes cover all four surfaces. A method on one and not the
    # others is the past defect this guards against: a REST-only route
    # must be REFUSED on gRPC, never absent, and the async twin must not
    # trail the sync one.
    sync_methods = _public_methods(rest)
    assert sync_methods == _public_methods(async_rest), f"{name}: async REST twin drifted"
    assert sync_methods == _public_methods(grpc), f"{name}: gRPC plane drifted"


def test_the_providers_plane_carries_both_max_tokens_methods() -> None:
    assert {"max_tokens", "replace_max_tokens"} <= _public_methods(p.RestProviders)
