# SPDX-License-Identifier: Apache-2.0
"""Real mTLS/REST/gRPC authority checks against an explicitly selected local binary.

Set MUNARIUM_PLATFORM_TEST_BINARY and, for persistence, an isolated
MUNARIUM_PLATFORM_TEST_DATABASE_URL. All certificates and signing keys are temporary.
"""

from __future__ import annotations

import base64
import hashlib
import ipaddress
import json
import os
import socket
import ssl
import subprocess
import tempfile
import time
import uuid
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from datetime import UTC, datetime, timedelta
from pathlib import Path

import grpc
import httpx
import pytest

from munarium_client import ApiRequest, ClientOptions, ServerApiClient
from munarium_client._errors import ForbiddenError, HeadConflictError, IdempotencyMismatchError

pytestmark = pytest.mark.skipif(
    not os.environ.get("MUNARIUM_PLATFORM_TEST_BINARY"),
    reason="requires explicit locally built Server binary and isolated test resources",
)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def digest(domain, value):
    return "sha256:" + hashlib.sha256(domain.encode() + b"\0" + canonical(value)).hexdigest()


def b64(value):
    return base64.urlsafe_b64encode(value).rstrip(b"=").decode()


def port():
    with socket.socket() as stream:
        stream.bind(("127.0.0.1", 0))
        return stream.getsockname()[1]


@contextmanager
def deployment(database, service_peers=()):
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec, ed25519
    from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

    with tempfile.TemporaryDirectory(prefix="munarium-platform-mtls-") as temporary:
        directory = Path(temporary)
        now = datetime.now(UTC)
        ca_key = ec.generate_private_key(ec.SECP256R1())
        ca_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "isolated-stage1-ca")])
        ca = (
            x509.CertificateBuilder()
            .subject_name(ca_name)
            .issuer_name(ca_name)
            .public_key(ca_key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - timedelta(minutes=1))
            .not_valid_after(now + timedelta(hours=1))
            .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
            .add_extension(
                x509.SubjectKeyIdentifier.from_public_key(ca_key.public_key()), critical=False
            )
            .add_extension(
                x509.KeyUsage(False, False, False, False, False, True, True, False, False),
                critical=True,
            )
            .sign(ca_key, hashes.SHA256())
        )
        ca_pem = ca.public_bytes(serialization.Encoding.PEM)
        (directory / "ca.pem").write_bytes(ca_pem)

        def certificate(name, server=False):
            key = ec.generate_private_key(ec.SECP256R1())
            cert = (
                x509.CertificateBuilder()
                .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, name)]))
                .issuer_name(ca_name)
                .public_key(key.public_key())
                .serial_number(x509.random_serial_number())
                .not_valid_before(now - timedelta(minutes=1))
                .not_valid_after(now + timedelta(hours=1))
                .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
                .add_extension(
                    x509.AuthorityKeyIdentifier.from_issuer_public_key(ca_key.public_key()),
                    critical=False,
                )
                .add_extension(
                    x509.SubjectAlternativeName(
                        [
                            x509.DNSName("localhost"),
                            x509.IPAddress(ipaddress.ip_address("127.0.0.1")),
                        ]
                    ),
                    critical=False,
                )
                .add_extension(
                    x509.ExtendedKeyUsage(
                        [
                            *([ExtendedKeyUsageOID.SERVER_AUTH] if server else []),
                            ExtendedKeyUsageOID.CLIENT_AUTH,
                        ]
                    ),
                    critical=False,
                )
                .sign(ca_key, hashes.SHA256())
            )
            cert_path, key_path = directory / f"{name}.pem", directory / f"{name}.key"
            cert_path.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
            key_path.write_bytes(
                key.private_bytes(
                    serialization.Encoding.PEM,
                    serialization.PrivateFormat.PKCS8,
                    serialization.NoEncryption(),
                )
            )
            return cert_path, key_path, cert.fingerprint(hashes.SHA256()).hex()

        server = certificate("server", server=True)
        operator = certificate("operator")
        writer = certificate("writer")
        impostor = certificate("unenrolled")
        signer = ed25519.Ed25519PrivateKey.generate()
        successor = ed25519.Ed25519PrivateKey.generate()
        tenant = "platform-" + uuid.uuid4().hex
        identities = {name: certificate(name, server=True) for name in service_peers}
        http_port, grpc_port, ops_port = port(), port(), port()
        checkpoint = directory / "checkpoint.json"
        key = {
            "public_key": b64(signer.public_key().public_bytes_raw()),
            "issuer": "operator-issuer",
            "presenter": "operator",
            "subjects": ["enrolled-human"],
        }
        config = {
            "certificate_file": str(server[0]),
            "private_key_file": str(server[1]),
            "client_ca_file": str(directory / "ca.pem"),
            "peers": {
                operator[2]: {
                    "service": "operator",
                    "tenants": [tenant],
                    "scopes": ["read", "govern"],
                },
                writer[2]: {"service": "writer", "tenants": [tenant], "scopes": ["read", "record"]},
            },
            "tenants": {
                tenant: {
                    "checkpoint_file": str(checkpoint),
                    "authority": {
                        "deployment": "stage1-live",
                        "tenant": tenant,
                        "audience": "svc-server",
                        "epoch": 1,
                        "bootstrap_keys": {"bootstrap": key},
                    },
                }
            },
        }
        config_path = directory / "config.json"
        for name, identity in identities.items():
            if name not in ("svc-warden", "svc-registry", "svc-gate"):
                continue
            config["peers"][identity[2]] = {
                "service": name,
                "tenants": [tenant],
                "scopes": ["read", "record"] if name == "svc-gate" else ["read"],
            }
        config_path.write_bytes(canonical(config))
        env = {
            name: os.environ[name]
            for name in ("PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP")
            if name in os.environ
        }
        env.update(
            MUNARIUM_HTTP_ADDR=f"127.0.0.1:{http_port}",
            MUNARIUM_GRPC_ADDR=f"127.0.0.1:{grpc_port}",
            MUNARIUM_OPS_ADDR=f"127.0.0.1:{ops_port}",
            MUNARIUM_AUTH_MODE="static",
            MUNARIUM_STATIC_TOKENS=f"synthetic-writer:{tenant}:rw",
            MUNARIUM_STORE=database,
            MUNARIUM_SOURCE_STORE="pg" if database == "postgres" else "mem",
            MUNARIUM_REQUIRE_UID="false",
            MUNARIUM_AUTHORITY_PROFILE="platform-v1",
            MUNARIUM_PLATFORM_CONFIG_FILE=str(config_path),
        )
        if database == "postgres":
            env["MUNARIUM_DATABASE_URL"] = os.environ["MUNARIUM_PLATFORM_TEST_DATABASE_URL"]
        binary = str(Path(os.environ["MUNARIUM_PLATFORM_TEST_BINARY"]).resolve(strict=True))
        enrolled = subprocess.run(
            [binary, "platform-enroll"], env=env, capture_output=True, timeout=30
        )
        assert enrolled.returncode == 0, "isolated enrollment failed"

        def http(identity):
            context = ssl.create_default_context(cadata=ca_pem.decode())
            if identity is not None:
                context.load_cert_chain(str(identity[0]), str(identity[1]))
            return httpx.Client(verify=context, timeout=5, trust_env=False, follow_redirects=False)

        raw_http = http(operator)
        endpoint = f"https://127.0.0.1:{http_port}"
        process = subprocess.Popen(
            [binary],
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        channel = grpc.secure_channel(
            f"127.0.0.1:{grpc_port}",
            grpc.ssl_channel_credentials(
                ca_pem, operator[1].read_bytes(), operator[0].read_bytes()
            ),
        )
        api = ServerApiClient(ClientOptions(endpoint), http_client=raw_http)
        rpc = ServerApiClient(
            ClientOptions(f"https://127.0.0.1:{grpc_port}"), grpc_transport=True, channel=channel
        )
        try:
            deadline = time.monotonic() + 15
            last_health = "no response"
            while time.monotonic() < deadline:
                assert process.poll() is None, "Server exited during isolated startup"
                try:
                    response = raw_http.get(endpoint + "/healthz", timeout=1)
                    last_health = f"HTTP {response.status_code}"
                    if response.status_code == 200:
                        break
                except httpx.TransportError as error:
                    last_health = type(error).__name__ + ": " + str(error)
                time.sleep(0.05)
            else:
                pytest.fail("isolated mTLS Server did not become ready: " + last_health)
            grpc.channel_ready_future(channel).result(timeout=10)
            yield dict(
                tenant=tenant,
                api=api,
                rpc=rpc,
                http=http,
                writer=writer,
                impostor=impostor,
                endpoint=endpoint,
                signer=signer,
                successor=successor,
                key=key,
                checkpoint=checkpoint,
                ca=ca_pem,
                grpc_endpoint=f"127.0.0.1:{grpc_port}",
                directory=directory,
                identities=identities,
            )
        finally:
            api.close()
            rpc.close()
            raw_http.close()
            channel.close()
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)


def signed(d, state, artifact, nonce, *, key=None, kid="bootstrap", changes=None):
    now = int(time.time())
    payload = {
        "schema_version": 1,
        "deployment": "stage1-live",
        "tenant": d["tenant"],
        "audience": "svc-server",
        "issuer": "operator-issuer",
        "subject": "enrolled-human",
        "subject_kind": "human",
        "action": "install-governance",
        "artifact_digest": digest("munarium:governance-artifact:v1", artifact),
        "expected_revision": state["revision"],
        "expected_head": state["head"],
        "epoch": 1,
        "nonce": nonce,
        "iat": now - 1,
        "nbf": now - 1,
        "exp": now + 60,
    }
    payload.update(changes or {})
    header = {"alg": "Ed25519", "kid": kid, "typ": "munarium-authority+jws"}
    message = b64(canonical(header)) + "." + b64(canonical(payload))
    return {
        "attestation": message + "." + b64((key or d["signer"]).sign(message.encode())),
        "artifact": artifact,
    }


@pytest.mark.parametrize("database", ["memory", "postgres"])
def test_platform_records_require_current_principals_and_keep_reserved_custody(database):
    from cryptography.hazmat.primitives.asymmetric import ed25519

    if database == "postgres" and not os.environ.get("MUNARIUM_PLATFORM_TEST_DATABASE_URL"):
        pytest.skip("isolated PostgreSQL URL not supplied")
    with deployment(database) as d:
        signer = ed25519.Ed25519PrivateKey.generate()
        path = {"tenant": d["tenant"]}
        now = int(time.time())
        restriction = dict(
            digest="sha256:" + "a" * 64,
            nbf=now - 10,
            exp=now + 120,
            scopes=["read", "propose"],
            resources=["records:" + d["tenant"]],
        )
        identity = {
            "keys": {
                "warden": {
                    "public_key": b64(signer.public_key().public_bytes_raw()),
                    "issuer": "warden",
                    "decision": True,
                }
            },
            "peers": {"writer": {"task": restriction, "policy": restriction, "maximum_depth": 0}},
            "registrations": [],
        }
        artifact = {
            "schema_version": 1,
            "bindings": {"identity:svc-server": identity},
            "retire_bootstrap": False,
            "successor_keys": {},
        }
        state = d["api"].get_platform_authority(ApiRequest(path=path)).json()
        d["api"].transition_platform_authority(
            ApiRequest.json(signed(d, state, artifact, "identity"), path=path)
        )
        payload = {
            "schema_version": 1,
            "deployment": "stage1-live",
            "tenant": d["tenant"],
            "issuer": "warden",
            "audience": "svc-server",
            "origin": "recorder",
            "actor": "recorder",
            "origin_kind": "service",
            "service": "writer",
            "purpose": "decision",
            "scopes": ["read", "propose"],
            "resources": ["records:" + d["tenant"]],
            "iat": now - 3,
            "nbf": now - 3,
            "exp": now + 50,
            "parent_digest": None,
            "bootstrap": None,
        }

        def principal(changes=None):
            body = dict(payload, **(changes or {}))
            message = (
                b64(canonical({"alg": "Ed25519", "kid": "warden", "typ": "munarium-principal+jws"}))
                + "."
                + b64(canonical(body))
            )
            return message + "." + b64(signer.sign(message.encode()))

        chain = [principal()]
        fixture = (
            Path(__file__).resolve().parents[3]
            / "server/contract/platform-stage1/record-vectors.json"
        )
        event = json.loads(fixture.read_text())["examples"]["event"]
        event["tenant"] = event["payload"]["tenant"] = d["tenant"]
        for lineage in event["payload"]["lineage"]:
            lineage["tenant"] = d["tenant"]
        event["source_service"] = "writer"
        event["payload_digest"] = digest("munarium:decision-event-payload:v1", event["payload"])
        channel = grpc.secure_channel(
            d["grpc_endpoint"],
            grpc.ssl_channel_credentials(
                d["ca"], d["writer"][1].read_bytes(), d["writer"][0].read_bytes()
            ),
        )
        with d["http"](d["writer"]) as http:
            api = ServerApiClient(ClientOptions(d["endpoint"]), http_client=http)
            rpc = ServerApiClient(
                ClientOptions("https://" + d["grpc_endpoint"]), grpc_transport=True, channel=channel
            )
            try:
                body = {
                    "chain": chain,
                    "action": {"operation": "append", "event": canonical(event).decode()},
                }
                ack = api.platform_records(ApiRequest.json(body, path=path)).json()
                assert rpc.platform_records(ApiRequest.json(body, path=path)).json() == ack
                lookup = {
                    "chain": chain,
                    "action": {"operation": "lookup", "operation_id": event["operation_id"]},
                }
                assert rpc.platform_records(ApiRequest.json(lookup, path=path)).json()[
                    "events"
                ] == [event]
                # Ordinary data-plane credentials cannot read the reserved physical tenant/version.
                response = http.get(
                    d["endpoint"] + "/v1/versions/" + ack["ledger_id"] + "/head",
                    headers={"authorization": "Bearer synthetic-writer"},
                )
                assert response.status_code == 404
                for changed in (
                    {"service": "operator"},
                    {"audience": "svc-gate"},
                    {"origin_kind": "human"},
                    {"tenant": "foreign"},
                    {"scopes": ["read"]},
                ):
                    with pytest.raises(ForbiddenError):
                        rpc.platform_records(
                            ApiRequest.json(dict(body, chain=[principal(changed)]), path=path)
                        )
                with pytest.raises(ForbiddenError):
                    api.platform_records(ApiRequest.json(dict(body, chain=[]), path=path))
                with pytest.raises(ForbiddenError):
                    api.platform_records(ApiRequest.json(lookup, path={"tenant": "foreign"}))
                state = d["api"].get_platform_authority(ApiRequest(path=path)).json()
                identity["keys"]["warden"]["decision"] = False
                d["api"].transition_platform_authority(
                    ApiRequest.json(signed(d, state, artifact, "revoke"), path=path)
                )
                for client in (api, rpc):
                    with pytest.raises(ForbiddenError):
                        client.platform_records(ApiRequest.json(lookup, path=path))
            finally:
                api.close()
                rpc.close()
                channel.close()


@pytest.mark.parametrize("database", ["memory", "postgres"])
def test_platform_authority_over_real_mtls_and_both_transports(database):
    if database == "postgres" and not os.environ.get("MUNARIUM_PLATFORM_TEST_DATABASE_URL"):
        pytest.skip("isolated PostgreSQL URL not supplied")
    with deployment(database) as d:
        path = {"tenant": d["tenant"]}
        state = d["api"].get_platform_authority(ApiRequest(path=path)).json()
        assert d["rpc"].get_platform_authority(ApiRequest(path=path)).json() == state
        artifact = {
            "schema_version": 1,
            "bindings": {},
            "retire_bootstrap": False,
            "successor_keys": {},
        }
        for changes in (
            {"subject_kind": "agent"},
            {"subject": "invented-human"},
            {"audience": "svc-registry"},
            {"tenant": "foreign"},
            {"epoch": 2},
            {"exp": 1},
            {"issuer": "untrusted"},
        ):
            body = signed(d, state, artifact, "rejected", changes=changes)
            for api in (d["api"], d["rpc"]):
                with pytest.raises(ForbiddenError):
                    api.transition_platform_authority(ApiRequest.json(body, path=path))
            assert d["api"].get_platform_authority(ApiRequest(path=path)).json()["head"] == 0
        body = signed(d, state, artifact, "first")
        with d["http"](d["writer"]) as writer:
            response = writer.post(
                d["endpoint"] + f"/v1/platform/{d['tenant']}/authority",
                json=body,
                headers={"authorization": "Bearer synthetic-writer", "x-munarium-peer": "operator"},
            )
            assert response.status_code == 403
            for route in (
                "/v1/shapes",
                "/v1/runbooks",
                "/v1/collections/fictional/activate-index",
                "/v1.2/collections/fictional/governance",
            ):
                method = "PUT" if route.endswith("governance") else "POST"
                assert (
                    writer.request(
                        method,
                        d["endpoint"] + route,
                        json={},
                        headers={"authorization": "Bearer synthetic-writer"},
                    ).status_code
                    == 403
                )
        for identity in (None, d["impostor"]):
            with d["http"](identity) as untrusted, pytest.raises(httpx.TransportError):
                untrusted.get(d["endpoint"] + f"/v1/platform/{d['tenant']}/authority")
        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = [
                pool.submit(api.transition_platform_authority, ApiRequest.json(body, path=path))
                for api in (d["api"], d["rpc"])
            ]
            receipts = [future.result().json() for future in futures]
        assert receipts[0] == receipts[1]
        assert receipts[0]["head"] == 1
        changed = signed(d, state, artifact, "first", changes={"exp": int(time.time()) + 80})
        with pytest.raises(IdempotencyMismatchError):
            d["rpc"].transition_platform_authority(ApiRequest.json(changed, path=path))
        stale = signed(d, state, artifact, "stale")
        with pytest.raises(HeadConflictError):
            d["api"].transition_platform_authority(ApiRequest.json(stale, path=path))
        state = d["api"].get_platform_authority(ApiRequest(path=path)).json()
        successor_key = dict(
            d["key"], public_key=b64(d["successor"].public_key().public_bytes_raw())
        )
        retired = dict(artifact, retire_bootstrap=True, successor_keys={"successor": successor_key})
        retirement = signed(d, state, retired, "retire")
        receipt = (
            d["rpc"].transition_platform_authority(ApiRequest.json(retirement, path=path)).json()
        )
        assert receipt["bootstrap_retired"]
        assert (
            d["api"].transition_platform_authority(ApiRequest.json(retirement, path=path)).json()
            == receipt
        )
        state = d["rpc"].get_platform_authority(ApiRequest(path=path)).json()
        with pytest.raises(ForbiddenError):
            d["api"].transition_platform_authority(
                ApiRequest.json(signed(d, state, retired, "retired-signer"), path=path)
            )
        final = signed(d, state, retired, "successor", key=d["successor"], kid="successor")
        assert (
            d["api"].transition_platform_authority(ApiRequest.json(final, path=path)).json()["head"]
            == 3
        )
        checkpoint = json.loads(d["checkpoint"].read_bytes())
        assert checkpoint["fence"]["minimum_head"] == 3 and checkpoint["fence"]["bootstrap_retired"]
        assert checkpoint["prepared"] is None
