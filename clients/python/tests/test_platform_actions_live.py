# SPDX-License-Identifier: Apache-2.0
"""Stage 2 recording admission over actual enrolled mTLS and both transports."""

import json
import os
import time
from contextlib import ExitStack
from pathlib import Path

import grpc
import pytest
from test_platform_authority_live import b64, canonical, deployment, digest, signed

from munarium_client import ApiRequest, ClientOptions, ServerApiClient
from munarium_client._errors import ForbiddenError, IdempotencyMismatchError

pytestmark = pytest.mark.skipif(
    not os.environ.get("MUNARIUM_PLATFORM_TEST_BINARY"),
    reason="requires explicit local Server binary and isolated resources",
)


@pytest.mark.parametrize("database", ["memory", "postgres"])
def test_action_records_require_governed_roles_and_current_principals(database):
    from cryptography.hazmat.primitives.asymmetric import ed25519

    if database == "postgres" and not os.environ.get("MUNARIUM_PLATFORM_TEST_DATABASE_URL"):
        pytest.skip("isolated PostgreSQL URL not supplied")
    peers = ("svc-council", "svc-registry", "svc-gate")
    with deployment(database, service_peers=peers, record_peers=peers) as d, ExitStack() as stack:
        signer = ed25519.Ed25519PrivateKey.generate()
        now = int(time.time())
        path = {"tenant": d["tenant"]}
        resource = "action-records:" + d["tenant"]
        restriction = dict(
            digest="sha256:" + "a" * 64,
            nbf=now - 10,
            exp=now + 120,
            scopes=["read", "propose"],
            resources=[resource],
        )
        identity = {
            "keys": {
                "warden": {
                    "public_key": b64(signer.public_key().public_bytes_raw()),
                    "issuer": "warden",
                    "decision": True,
                }
            },
            "peers": {
                p: {"task": restriction, "policy": restriction, "maximum_depth": 0} for p in peers
            },
            "registrations": [],
        }
        fixture = (
            Path(__file__).resolve().parents[3] / "server/contract/platform-stage2-v1/vectors.json"
        )
        records = json.loads(fixture.read_text())["records"]
        scope = dict(
            domain="fixture-domain", tenant=d["tenant"], deployment="stage1-live", cell="cell-a"
        )
        activation = records["activation"]
        activation["transition"]["scope"] = scope
        activation["scope"] = scope
        activation["ratification"]["scope"] = scope
        event = records["activation-event"]
        event["scope"] = event["stream"]["scope"] = scope
        receipt = event["payload"]["receipt"]
        receipt["transition"]["scope"] = scope
        receipt["transition_digest"] = digest("munarium:stage2:activation:v1", activation)
        event["payload_digest"] = digest("munarium:stage2:event-payload:v1", event["payload"])
        policy = dict(
            schema_version=1,
            profile="stage2-single-cell-v1",
            scope=scope,
            streams=[
                dict(
                    stream_id="council-approvals",
                    producer="council",
                    service="svc-council",
                    generation=1,
                    kinds=["approval-recorded"],
                ),
                dict(
                    stream_id=event["stream"]["id"],
                    producer="registry",
                    service="svc-registry",
                    generation=1,
                    kinds=["activation-applied"],
                ),
            ],
            readers=list(peers),
            recovery=[],
        )
        artifact = dict(
            schema_version=1,
            bindings={"identity:svc-server": identity, "action-records:svc-server": policy},
            retire_bootstrap=False,
            successor_keys={},
        )

        def govern(nonce):
            state = d["api"].get_platform_authority(ApiRequest(path=path)).json()
            d["api"].transition_platform_authority(
                ApiRequest.json(signed(d, state, artifact, nonce), path=path)
            )

        def principal(peer, **changes):
            body = dict(
                schema_version=1,
                deployment="stage1-live",
                tenant=d["tenant"],
                issuer="warden",
                audience="svc-server",
                origin="recorder",
                actor="recorder",
                origin_kind="service",
                service=peer,
                purpose="decision",
                scopes=["read", "propose"],
                resources=[resource],
                iat=now - 3,
                nbf=now - 3,
                exp=now + 50,
                parent_digest=None,
                bootstrap=None,
            )
            body.update(changes)
            header = {"alg": "Ed25519", "kid": "warden", "typ": "munarium-principal+jws"}
            message = b64(canonical(header)) + "." + b64(canonical(body))
            return message + "." + b64(signer.sign(message.encode()))

        clients = {}
        for peer in peers:
            cert = d["identities"][peer]
            http = stack.enter_context(d["http"](cert))
            channel = grpc.secure_channel(
                d["grpc_endpoint"],
                grpc.ssl_channel_credentials(d["ca"], cert[1].read_bytes(), cert[0].read_bytes()),
            )
            stack.callback(channel.close)
            api = ServerApiClient(ClientOptions(d["endpoint"]), http_client=http)
            rpc = ServerApiClient(
                ClientOptions("https://" + d["grpc_endpoint"]), grpc_transport=True, channel=channel
            )
            stack.callback(api.close)
            stack.callback(rpc.close)
            clients[peer] = (api, rpc)

        def call(peer, action, transport=0, **changes):
            body = {"chain": [principal(peer, **changes)], "action": action}
            return (
                clients[peer][transport].platform_records(ApiRequest.json(body, path=path)).json()
            )

        # Enrollment alone and a raw assertion cannot create the governing binding.
        archive = dict(operation="action-archive", record=canonical(activation).decode())
        with pytest.raises(ForbiddenError):
            call("svc-council", archive)
        govern("stage2-policy")
        archived = call("svc-council", archive)
        assert call("svc-council", archive, 1) == archived
        assert "event_digest" not in archived
        append = dict(operation="action-append", event=canonical(event).decode())
        for transport in (0, 1):
            with pytest.raises(ForbiddenError):
                call("svc-gate", append, transport)
            for changes in (
                {"resources": ["records:" + d["tenant"]]},
                {"origin_kind": "agent"},
                {"exp": now + 90},
                {"service": "svc-council"},
                {"audience": "svc-gate"},
                {"scopes": ["read"]},
            ):
                with pytest.raises(ForbiddenError):
                    call("svc-registry", append, transport, **changes)
        ack = call("svc-registry", append)
        assert call("svc-registry", append, 1) == ack
        assert ack["event_digest"] == digest("munarium:stage2:accountability-event:v1", event)
        assert ack["payload_digest"] == event["payload_digest"]
        transition = dict(
            operation="action-transition", transition_id=activation["transition"]["id"]
        )
        assert (
            call("svc-gate", transition)
            == call("svc-gate", transition, 1)
            == dict(ledger_position=2, artifacts=[activation], events=[event])
        )
        head = dict(operation="action-source-head", stream_id=event["stream"]["id"], generation=1)
        assert (
            call("svc-gate", head)
            == call("svc-gate", head, 1)
            == dict(sequence=1, event_digest=ack["event_digest"])
        )
        lookup = dict(operation="action-lookup", operation_id="absent-operation")
        assert (
            call("svc-gate", lookup)
            == call("svc-gate", lookup, 1)
            == dict(ledger_position=2, artifacts=[], events=[])
        )
        changed = dict(event, occurred_at=999)
        for transport in (0, 1):
            with pytest.raises(IdempotencyMismatchError):
                call(
                    "svc-registry",
                    dict(operation="action-append", event=canonical(changed).decode()),
                    transport,
                )
        # A ratified binding removal applies immediately even to a previously acknowledged retry.
        del artifact["bindings"]["action-records:svc-server"]
        govern("remove-stage2-policy")
        for transport in (0, 1):
            with pytest.raises(ForbiddenError):
                call("svc-registry", append, transport)
            with pytest.raises(ForbiddenError):
                call("svc-gate", lookup, transport)
