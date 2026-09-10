# SPDX-License-Identifier: Apache-2.0
"""Opt-in Server 1.1.1 qualification through real clients and a controlled provider.

Requires the normal conformance environment plus MUNARIUM_OLLAMA_FIXTURE_URL
(test runner's /calls origin) and MUNARIUM_OLLAMA_SERVER_ENDPOINT (Server's
origin for the same fixture). No paid calls or model downloads. Run serially
on an isolated test tenant; the fixture records requests across test cases.
"""

from __future__ import annotations

import base64
import os
from collections.abc import AsyncIterator, Iterator
from types import SimpleNamespace
from typing import Any
from uuid import uuid4

import httpx
import pytest

from munarium_client import (
    AsyncMunariumClient,
    ClientOptions,
    MunariumClient,
    MunariumError,
    _Threaded,
)
from munarium_client.models import TurnProgress, TurnResult

REST = os.environ.get("MUNARIUM_REST_URL", "")
GRPC = os.environ.get("MUNARIUM_GRPC_URL", "")
TOKEN = os.environ.get("MUNARIUM_TOKEN", "devtoken")
FIXTURE = os.environ.get("MUNARIUM_OLLAMA_FIXTURE_URL", "")
ENDPOINT = os.environ.get("MUNARIUM_OLLAMA_SERVER_ENDPOINT", "")
pytestmark = pytest.mark.skipif(
    not (REST and GRPC and FIXTURE and ENDPOINT),
    reason="Server 1.1.1 qualification needs REST/gRPC and the isolated Ollama fixture",
)


def calls() -> list[dict[str, Any]]:
    response = httpx.get(FIXTURE + "/calls", timeout=10)
    response.raise_for_status()
    return list(response.json())


@pytest.fixture(scope="module")
def routing() -> Iterator[dict[str, str]]:
    with MunariumClient.rest(ClientOptions(REST, token=TOKEN, uid="qualification")) as ops:
        version = ops.server_version()
        assert (version.name, version.version) == ("munarium-server", "1.1.1")
        prefix = "client111-" + uuid4().hex[:12]
        names = {
            key: prefix + "-" + key for key in ("baseline", "selected", "shape", "docs", "runbook")
        }
        for family in ("baseline", "selected"):
            ops.providers.apply_config(f"""apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata: {{ name: {names[family]} }}
spec:
  provider: ollama
  endpoint: {ENDPOINT}
  models:
    complete: [{family}-fast, {family}-capable, explicit]
    embed: [embed]
    fast: {family}-fast
    capable: {family}-capable
""")
        ops.runbooks.apply_shape(f"""apiVersion: munarium.ioka.io/v1
kind: Shape
metadata: {{ name: {names["shape"]}, version: 1 }}
spec:
  fact:
    schema: {{ type: object }}
""")
        ops.runbooks.apply_runbook(f"""apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: {{ name: {names["runbook"]}, version: 1 }}
spec:
  collections:
    - name: {names["docs"]}
      shape: {names["shape"]}@1
      sources: {{ filenamePrefix: "{prefix}/" }}
  retrieval:
    modelQueryExpansion: {{ maxTerms: 4, maxTokens: 64, required: false }}
  models:
    allowOverrides: [{names["selected"]}]
    tasks:
      query_expansion: {{ provider: {names["baseline"]}, tier: fast }}
      completion: {{ provider: {names["baseline"]}, tier: capable }}
  completion: {{ promptTemplate: "{{query}}\\n{{context}}", maxTokens: 256 }}
  steps:
    - resolveSources: {{}}
    - buildIndex: {{}}
    - verify: {{}}
    - cutover: {{ approval: required }}
""")
        ops.ingest.ingest(
            {
                "filename": prefix + "/procedure.md",
                "media_type": "text/markdown",
                "content_base64": base64.b64encode(
                    b"The journey procedure requires a review."
                ).decode(),
            }
        )
        run = ops.runbooks.run_runbook(names["runbook"])
        status = ops.runbooks.get_run(run.run_id)
        step = next(s for s in status.steps if s.state == "awaiting_approval")
        ops.runbooks.approve_step(run.run_id, step.ordinal)
        assert ops.runbooks.get_run(run.run_id).state == "done"
        inventory = {p.name: p for p in ops.providers.list()}
        assert inventory[names["selected"]].provider == "ollama"
        assert inventory[names["selected"]].credential_ok
        yield names


@pytest.fixture(params=["rest-sync", "rest-async", "grpc-sync", "grpc-async"])
async def variant(request: pytest.FixtureRequest) -> AsyncIterator[Any]:
    transport, mode = request.param.split("-")
    options = ClientOptions(REST if transport == "rest" else GRPC, token=TOKEN, uid="qualification")
    if mode == "sync":
        with getattr(MunariumClient, transport)(options) as client:
            yield SimpleNamespace(
                providers=_Threaded(client.providers),
                sessions=_Threaded(client.sessions),
                commands=_Threaded(client.commands),
            )
    else:
        async with getattr(AsyncMunariumClient, transport)(options) as client:
            yield client


async def test_named_ollama(variant: Any, routing: dict[str, str]) -> None:
    providers = variant.providers
    before = len(calls())
    health = await providers.health(routing["selected"])
    assert health.healthy and health.provider == "ollama"
    assert len(calls()) == before, "named health must not run inference"
    version = await variant.commands.create_version()
    out = await providers.complete(
        routing["selected"], prompt="Say OK.", tier="capable", version_id=version
    )
    assert (out.provider, out.model, out.input_tokens, out.output_tokens) == (
        "ollama",
        "selected-capable",
        9,
        3,
    )
    assert len(calls()) == before + 1
    assert out.invocation_event_id
    selected = await providers.complete(
        "default", prompt="Say OK.", provider="ollama", model="explicit"
    )
    assert (selected.provider, selected.model) == ("ollama", "explicit")
    inputs = [uuid4().hex, "second input"]
    embedded = await providers.embed(routing["selected"], inputs=inputs, version_id=version)
    assert embedded.provider == "ollama" and embedded.model == "embed"
    assert embedded.dimensions == 3 and embedded.vectors == [[1.0, 1.0, 0.5], [1.0, 2.0, 0.5]]
    assert not embedded.cache_hit
    assert embedded.invocation_event_id
    replay = await providers.embed(routing["selected"], inputs=inputs)
    assert replay.cache_hit and replay.vectors == embedded.vectors


async def test_override_routing(variant: Any, routing: dict[str, str]) -> None:
    sessions = variant.sessions
    session = await sessions.create(routing["runbook"])
    try:
        for override, expected in [
            (None, ["baseline-fast", "baseline-capable"]),
            ({}, ["baseline-fast", "baseline-capable"]),
            ({"provider": routing["selected"], "tier": "capable"}, ["selected-capable"] * 2),
            (
                {"provider": routing["selected"], "model": "explicit", "tier": "fast"},
                ["explicit"] * 2,
            ),
        ]:
            before = len(calls())
            result = await sessions.turn(
                session.session_id, query="journey", complete=True, model_override=override
            )
            assert [call["model"] for call in calls()[before:]] == expected
            assert result.completion.model == expected[-1]
            assert result.completion.was_override == bool(override)
        for complete, override in [
            (True, {"provider": routing["baseline"]}),
            (True, {"provider": routing["selected"], "tier": "invalid"}),
            (False, {"provider": routing["selected"]}),
        ]:
            before = len(calls())
            with pytest.raises(MunariumError):
                await sessions.turn(
                    session.session_id, query="journey", complete=complete, model_override=override
                )
            assert len(calls()) == before, "rejection must precede any provider request"
    finally:
        await sessions.close(session.session_id)


def test_stream_exposes_actual_models(routing: dict[str, str]) -> None:
    with MunariumClient.rest(ClientOptions(REST, token=TOKEN, uid="qualification")) as client:
        session = client.sessions.create(routing["runbook"])
        try:
            events = list(
                client.sessions.turn_stream(
                    session.session_id,
                    query="travel procedure",
                    complete=True,
                    model_override={"provider": routing["selected"], "tier": "capable"},
                )
            )
            expansion = next(
                e for e in events if isinstance(e, TurnProgress) and e.stage == "expansion"
            )
            assert (expansion.provider, expansion.model) == ("ollama", "selected-capable")
            assert expansion.terms == ["journey"]
            assert (expansion.input_tokens, expansion.output_tokens) == (9, 3)
            assert isinstance(events[-1], TurnResult)
            assert events[-1].completion is not None
            assert events[-1].completion.model == "selected-capable"
            assert events[-1].completion.was_override
        finally:
            client.sessions.close(session.session_id)
