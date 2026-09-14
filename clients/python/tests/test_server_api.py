# SPDX-License-Identifier: Apache-2.0
import json

import httpx
import pytest

from munarium_client import ApiRequest, AsyncServerApiClient, ClientOptions, ServerApiClient
from munarium_client._errors import InvalidInputError, ProviderError


def test_complete_api_preserves_null_large_integers_and_encodes_path():
    seen = []

    def handler(request):
        seen.append(request)
        return httpx.Response(200, json={"revision": 9007199254740993, "auto_generate": None})

    client = ServerApiClient(ClientOptions("http://fixture.invalid", token="test", uid="tester"))
    client.http.close()
    client.http = httpx.Client(transport=httpx.MockTransport(handler))
    try:
        response = client.replace_collection_vocabulary(
            ApiRequest.json(
                {"revision": 9007199254740993, "auto_generate": None}, path={"id": "a/b?x=1"}
            )
        )
        assert response.json() == {"revision": 9007199254740993, "auto_generate": None}
        assert seen[0].url.raw_path == b"/v1.2/collections/a%2Fb%3Fx%3D1/vocabulary"
        assert json.loads(seen[0].content)["auto_generate"] is None
        assert seen[0].headers["x-munarium-uid"] == "tester"
        assert seen[0].headers["authorization"] == "Bearer test"
    finally:
        client.close()


def test_paid_call_is_not_replayed_and_keeps_typed_problem():
    calls = []

    def handler(request):
        calls.append(request)
        return httpx.Response(
            502,
            json={
                "type": "https://munarium.ioka.io/problems/provider-error",
                "detail": "provider unavailable",
            },
        )

    client = ServerApiClient(ClientOptions("http://fixture.invalid", read_retries=5))
    client.http.close()
    client.http = httpx.Client(transport=httpx.MockTransport(handler))
    try:
        # Use the registry's actual provider slug; an upstream failure cannot
        # cause a second completion and charge.
        with pytest.raises(ProviderError):
            client.compose_answer(ApiRequest.json({"question": "fixture", "sources": []}))
        assert len(calls) == 1
    finally:
        client.close()


@pytest.mark.asyncio
async def test_async_api_uses_same_revision_and_disable_payload():
    async def handler(request):
        assert json.loads(request.content) == {"revision": 7, "enabled": False}
        return httpx.Response(200, json={"revision": 8, "enabled": False})

    client = AsyncServerApiClient(ClientOptions("http://fixture.invalid"))
    await client.http.aclose()
    client.http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    try:
        result = await client.update_collection_vocabulary(
            ApiRequest.json({"revision": 7, "enabled": False}, path={"id": "fixture"})
        )
        assert result.json()["revision"] == 8
    finally:
        await client.close()


def test_caller_cannot_replace_credentials_through_source_headers():
    client = ServerApiClient(ClientOptions("http://fixture.invalid", token="private"))
    try:
        with pytest.raises(InvalidInputError):
            client.get_vocabulary_settings(ApiRequest(source_headers={"authorization": "other"}))
    finally:
        client.close()
