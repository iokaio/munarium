# SPDX-License-Identifier: Apache-2.0
"""Conformance fixtures: the same scenario set runs over four client
variants (rest/grpc x sync/async). Scenarios are written async-style; sync
clients ride a passthrough adapter. Requires a running server:

    MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_GRPC_URL=127.0.0.1:15051 \
    MUNARIUM_TOKEN=devtoken pytest conformance/
"""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import Any

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from munarium_client import AsyncMunariumClient, ClientOptions, MunariumClient  # noqa: E402

REST_URL = os.environ.get("MUNARIUM_REST_URL")
GRPC_URL = os.environ.get("MUNARIUM_GRPC_URL")
TOKEN = os.environ.get("MUNARIUM_TOKEN", "devtoken")


from munarium_client import _Threaded  # noqa: E402  (the production adapter)


class SyncClientAsAsync:
    """Async-shaped view of a sync client, built on the PRODUCTION
    _Threaded adapter so the harness exercises the same wrapping the
    grpc-async facade uses."""

    def __init__(self, inner: MunariumClient) -> None:
        self.commands = _Threaded(inner.commands)
        self.query = _Threaded(inner.query)
        self.ingest = _Threaded(inner.ingest)
        self.retrieval = _Threaded(inner.retrieval)
        self.runbooks = _Threaded(inner.runbooks)
        self.providers = _Threaded(inner.providers)
        self._inner = inner

    async def propose_claim_with_retry(self, *args: Any, **kwargs: Any) -> Any:
        return self._inner.propose_claim_with_retry(*args, **kwargs)

    async def close(self) -> None:
        self._inner.close()


VARIANTS = ["rest-sync", "rest-async", "grpc-sync", "grpc-async"]


def _make(variant: str) -> Any:
    transport, mode = variant.split("-")
    if transport == "rest":
        if REST_URL is None:
            pytest.skip("MUNARIUM_REST_URL not set")
        options = ClientOptions(REST_URL, token=TOKEN, uid="conformance")
        return (
            AsyncMunariumClient.rest(options)
            if mode == "async"
            else SyncClientAsAsync(MunariumClient.rest(options))
        )
    if GRPC_URL is None:
        pytest.skip("MUNARIUM_GRPC_URL not set")
    options = ClientOptions(GRPC_URL, token=TOKEN, uid="conformance")
    return (
        AsyncMunariumClient.grpc(options)
        if mode == "async"
        else SyncClientAsAsync(MunariumClient.grpc(options))
    )


@pytest.fixture(params=VARIANTS)
async def client(request: pytest.FixtureRequest) -> Any:
    c = _make(request.param)
    yield c
    await c.close()


@pytest.fixture
def rest_client() -> Any:
    if REST_URL is None:
        pytest.skip("MUNARIUM_REST_URL not set")
    with MunariumClient.rest(ClientOptions(REST_URL, token=TOKEN, uid="conformance")) as c:
        yield c


@pytest.fixture
def grpc_client() -> Any:
    if GRPC_URL is None:
        pytest.skip("MUNARIUM_GRPC_URL not set")
    with MunariumClient.grpc(ClientOptions(GRPC_URL, token=TOKEN, uid="conformance")) as c:
        yield c
