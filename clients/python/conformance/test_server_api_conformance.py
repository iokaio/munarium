# SPDX-License-Identifier: Apache-2.0
"""Complete API conformance against the same Server build, without paid calls."""

import base64
import hashlib
import json
import os
import re
import uuid
from pathlib import Path

import grpc
import pytest

from munarium_client import (
    TARGET_SERVER_VERSION,
    ApiRequest,
    AsyncServerApiClient,
    ClientOptions,
    ServerApiClient,
)
from munarium_client._errors import InvalidInputError, MunariumError, NotFoundError
from munarium_client._grpc_common import target_from_endpoint
from munarium_client._proto.mmp.v1 import server_api_pb2 as pb
from munarium_client._proto.mmp.v1 import server_api_pb2_grpc as stubs


def options(grpc_transport):
    endpoint = os.environ.get("MUNARIUM_GRPC_URL" if grpc_transport else "MUNARIUM_REST_URL")
    if not endpoint:
        pytest.skip("live Server endpoint is unset")
    return ClientOptions(
        endpoint, token=os.environ.get("MUNARIUM_TOKEN", "devtoken"), uid="api-conformance"
    )


@pytest.mark.parametrize("grpc_transport", [False, True])
def test_vocabulary_source_identity_and_old_platform_gaps(grpc_transport):
    client = ServerApiClient(options(grpc_transport), grpc_transport=grpc_transport)
    name = "api-" + uuid.uuid4().hex
    try:
        assert client.version_info().json()["version"] == TARGET_SERVER_VERSION
        shape = (
            "apiVersion: munarium.ioka.io/v1\nkind: Shape\n"
            "metadata: {name: api-docs, version: 1}\nspec:\n"
            "  fact:\n    schema: {type: object}\n"
        )
        client.apply_shape(ApiRequest(body=shape.encode(), content_type="text/yaml"))
        collection = client.create_collection(
            ApiRequest.json(
                {"name": name, "shape_ref": "api-docs@1", "access_level": 0, "compartments": []}
            )
        ).json()
        path = {"id": collection["id"]}
        first = client.get_collection_vocabulary(ApiRequest(path=path)).json()
        value = {
            "revision": first["revision"],
            "enabled": True,
            "auto_generate": False,
            "sampling": None,
            "groups": [["purchase order", "procurement request"]],
        }
        saved = client.replace_collection_vocabulary(ApiRequest.json(value, path=path)).json()
        assert saved["groups"] == value["groups"]
        assert saved["auto_generate"] is False
        with pytest.raises(InvalidInputError):
            client.replace_collection_vocabulary(ApiRequest.json(value, path=path))
        off = client.update_collection_vocabulary(
            ApiRequest.json({"revision": saved["revision"], "enabled": False}, path=path)
        ).json()
        assert off["enabled"] is False
        assert (
            client.get_vocabulary_revision(ApiRequest(path=path)).json()["revision"]
            == off["revision"]
        )
        # Neither a revoked collection nor a made-up sample may reach a model.
        with pytest.raises(MunariumError):
            client.refresh_collection_vocabulary(
                ApiRequest.json(
                    {"revision": off["revision"], "source_ids": ["src-absent"]}, path=path
                )
            )
        text = b"A purchase order requires supervisor approval."
        filename = name + "/ordering.txt"
        source = client.ingest_file(
            ApiRequest.json(
                {
                    "filename": filename,
                    "media_type": "text/plain",
                    "content_base64": base64.b64encode(text).decode(),
                    "collections": [name],
                }
            )
        ).json()
        metadata = client.get_source(ApiRequest(path={"source_id": source["source_id"]})).json()
        assert metadata["filename"] == filename
        assert source["sha256"] == hashlib.sha256(text).hexdigest()
        assert "content_base64" not in metadata
        # Missing-source errors retain their typed category on the new gRPC surface.
        with pytest.raises(NotFoundError):
            client.get_source(ApiRequest(path={"source_id": "src-absent"}))
        # Streaming is an actual RPC, not UNIMPLEMENTED, including typed initial errors.
        with pytest.raises(MunariumError):
            list(
                client.turn_stream(
                    ApiRequest.json(
                        {"question": "fixture", "retrieval_only": True}, path={"id": "ses-absent"}
                    )
                )
            )
    finally:
        client.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("grpc_transport", [False, True])
async def test_async_complete_surface_reads(grpc_transport):
    client = AsyncServerApiClient(options(grpc_transport), grpc_transport=grpc_transport)
    try:
        assert (await client.version_info()).json()["version"] == TARGET_SERVER_VERSION
        assert "sampling" in (await client.get_vocabulary_settings()).json()
    finally:
        await client.close()


def test_every_documented_operation_has_a_served_rpc():
    config = options(True)
    target, tls = target_from_endpoint(config.endpoint)
    channel = (
        grpc.secure_channel(target, grpc.ssl_channel_credentials())
        if tls
        else grpc.insecure_channel(target)
    )
    stub = stubs.ServerApiServiceStub(channel)
    catalog = json.loads((Path(__file__).resolve().parents[2] / "server-api.json").read_text())
    try:
        for op in catalog["operations"]:
            request = pb.ServerApiRequest(
                body=b"{}",
                path_parameters={key: "absent" for key in re.findall(r"\{([^}]+)\}", op["path"])},
            )
            try:
                response = getattr(stub, op["rpc"])(
                    request,
                    metadata=(
                        ("authorization", "Bearer invalid"),
                        ("munarium-uid", "api-conformance"),
                    ),
                    timeout=10,
                )
                if op["streaming"]:
                    list(response)
            except grpc.RpcError as error:
                assert error.code() != grpc.StatusCode.UNIMPLEMENTED, op["rpc"]
    finally:
        channel.close()
