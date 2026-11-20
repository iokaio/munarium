# SPDX-License-Identifier: Apache-2.0
"""gRPC transports for the local v1.4 compatibility harness.

Runs inside a virtual environment that has the PUBLISHED ``munarium-client``
1.4.0 wheel installed: the generated v1.4 stubs inside that wheel are the old
strict readers this replay is meant to exercise. ``compat_wire`` supplies the
case runner and comparison, so this file only adds two transports:

* ``bridge``: ``mmp.v1.ServerApiService``, the native bridge. Each replay case is
  routed by its REST method and path template through the v1.4
  ``server_api.proto`` comments, and the response bytes are decoded exactly as
  a REST response would be.
* ``typed``: a fixed list of typed ``CommandService``, ``QueryService`` and
  ``SessionService`` calls, recorded with ``MessageToDict`` (field presence is
  kept) plus the gRPC status, ``ErrorInfo`` reason and metadata.

Usage::

    python grpc_driver.py --target 127.0.0.1:28051 --state <state.json> \\
        --proto <v1.4 server_api.proto> --out <transcript.json> --mode bridge|typed
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import compat_wire as cw  # noqa: E402

import grpc  # noqa: E402
from google.protobuf import json_format  # noqa: E402

from munarium_client._proto.mmp.v1 import (  # noqa: E402
    command_pb2,
    command_pb2_grpc,
    query_pb2,
    query_pb2_grpc,
    server_api_pb2,
    server_api_pb2_grpc,
    session_pb2,
    session_pb2_grpc,
)

TOKENS = {"rw": "compat-rw", "ro": "compat-ro", "mgmt": "compat-mgmt", "other": "compat-other-rw"}


def error_record(err: grpc.RpcError) -> dict[str, Any]:
    rec: dict[str, Any] = {"grpc_code": err.code().name, "details": err.details()}
    for key, value in err.trailing_metadata() or ():
        if key == "grpc-status-details-bin":
            try:
                from google.rpc import error_details_pb2, status_pb2

                status = status_pb2.Status.FromString(value)
                for any_detail in status.details:
                    info = error_details_pb2.ErrorInfo()
                    if any_detail.Unpack(info):
                        rec["reason"] = info.reason
                        rec["domain"] = info.domain
                        rec["metadata"] = dict(info.metadata)
            except Exception as exc:  # decoding failure is evidence too
                rec["status_details_error"] = str(exc)
    return rec


class BridgeTransport:
    name = "bridge"

    def __init__(self, target: str, proto_text: str) -> None:
        self.routes = cw.load_proto_routes(proto_text)
        self.stub = server_api_pb2_grpc.ServerApiServiceStub(grpc.insecure_channel(target))

    def call(self, req: dict[str, Any]) -> dict[str, Any]:
        route = self.routes.get((req["method"], req["template"]))
        if route is None:
            raise KeyError(f"no native bridge route for {req['method']} {req['template']}")
        rpc, streaming = route
        message = server_api_pb2.ServerApiRequest(
            path_parameters=req["params"],
            query_parameters=[server_api_pb2.ServerApiParameter(name=k, value=v) for k, v in req["query"].items()],
            body=req["body"] or b"",
            content_type=req["content_type"] or "",
        )
        metadata = []
        token = req["token"]
        if token and token != "none":
            metadata.append(("authorization", f"Bearer {TOKENS.get(token, token)}"))
        if req["uid"]:
            metadata.append(("munarium-uid", req["uid"]))
        for k, v in req["headers"].items():
            metadata.append((k.lower(), str(v)))
        method = getattr(self.stub, rpc)
        try:
            if streaming:
                parts = list(method(message, metadata=metadata, timeout=120))
                status = parts[0].status if parts else 0
                ctype = parts[0].content_type if parts else ""
                raw = b"".join(p.body for p in parts)
                out = cw.decode_body(status, ctype, raw)
                out["fragments"] = len(parts)
            else:
                resp = method(message, metadata=metadata, timeout=120)
                out = cw.decode_body(resp.status, resp.content_type, resp.body)
        except grpc.RpcError as err:
            out = {"status": 0, "media_type": "grpc-error", "bytes_len": 0, "sha256": "", "json": error_record(err)}
        out["rpc"] = rpc
        return out


TYPED_CASES: list[dict[str, Any]] = [
    {"id": "typed-head", "svc": "query", "rpc": "GetHead", "req": {"version_id": "${version_id}"}},
    {"id": "typed-claim", "svc": "query", "rpc": "GetClaim", "req": {"claim_id": "${claim_1}"}},
    {"id": "typed-claim-superseded", "svc": "query", "rpc": "GetClaim", "req": {"claim_id": "${claim_2}"}},
    {"id": "typed-claim-missing", "kind": "refusal", "svc": "query", "rpc": "GetClaim", "req": {"claim_id": "claim-00000000000000000000000000000000"}},
    {"id": "typed-facts", "svc": "query", "rpc": "SliceFacts", "req": {"version_id": "${version_id}"}},
    {"id": "typed-facts-pinned", "svc": "query", "rpc": "SliceFacts", "req": {"version_id": "${version_id}", "as_of_seq": 2}},
    {"id": "typed-lineage", "svc": "query", "rpc": "GetLineage", "req": {"version_id": "${version_id}"}},
    {"id": "typed-anchors", "svc": "query", "rpc": "ListAnchors", "req": {"version_id": "${version_id}"}},
    {"id": "typed-promises", "svc": "query", "rpc": "ListPromises", "req": {"version_id": "${version_id}"}},
    {"id": "typed-counters", "svc": "query", "rpc": "CounterTotals", "req": {"version_id": "${version_id}"}},
    {"id": "typed-compose", "svc": "query", "rpc": "ComposeContext", "req": {"version_id": "${version_id}"}, "exact": True},
    {"id": "typed-compose-budget", "svc": "query", "rpc": "ComposeContext", "req": {"version_id": "${version_id}", "budget_tokens": 12}, "exact": True},
    {"id": "typed-create-version", "svc": "command", "rpc": "CreateVersion", "req": {"parent_version_id": "${version_id}"}, "idem": "ty-01", "capture": {"tv": "/version_id"}},
    {"id": "typed-propose", "svc": "command", "rpc": "ProposeClaim", "req": {"version_id": "${tv}", "claim_type": "CLAIM_TYPE_FACT", "subject": "typed", "key": "k", "value": "v"}, "idem": "ty-02", "capture": {"tv_head": "/head_seq"}},
    {"id": "typed-propose-replay", "svc": "command", "rpc": "ProposeClaim", "req": {"version_id": "${tv}", "claim_type": "CLAIM_TYPE_FACT", "subject": "typed", "key": "k", "value": "v"}, "idem": "ty-02"},
    {"id": "typed-propose-mismatch", "kind": "conflict", "svc": "command", "rpc": "ProposeClaim", "req": {"version_id": "${tv}", "claim_type": "CLAIM_TYPE_FACT", "subject": "typed", "key": "k", "value": "other"}, "idem": "ty-02"},
    {"id": "typed-head-conflict", "kind": "conflict", "svc": "command", "rpc": "ProposeClaim", "req": {"version_id": "${tv}", "expected_head": 0, "claim_type": "CLAIM_TYPE_FACT", "subject": "typed", "key": "k2", "value": "v"}, "idem": "ty-03"},
    {"id": "typed-disputed", "svc": "command", "rpc": "ProposeClaim", "req": {"version_id": "${version_id}", "claim_type": "CLAIM_TYPE_FACT", "subject": "starter", "key": "region", "value": "east"}, "idem": "ty-04"},
    {"id": "typed-no-idempotency", "kind": "refusal", "svc": "command", "rpc": "ProposeClaim", "req": {"version_id": "${tv}", "claim_type": "CLAIM_TYPE_FACT", "subject": "typed", "key": "k3", "value": "v"}},
    {"id": "typed-ro-refused", "kind": "refusal", "svc": "command", "rpc": "ProposeClaim", "token": "ro", "req": {"version_id": "${tv}", "claim_type": "CLAIM_TYPE_FACT", "subject": "typed", "key": "k4", "value": "v"}, "idem": "ty-05"},
    {"id": "typed-no-token", "kind": "refusal", "svc": "query", "rpc": "GetHead", "token": "none", "req": {"version_id": "${version_id}"}},
    {"id": "typed-no-uid", "kind": "refusal", "svc": "query", "rpc": "GetHead", "uid": "", "req": {"version_id": "${version_id}"}},
    {"id": "typed-other-tenant", "kind": "refusal", "svc": "query", "rpc": "GetHead", "token": "other", "req": {"version_id": "${version_id}"}},
    {"id": "typed-session-create", "svc": "session", "rpc": "CreateSession", "req": {"runbook_name": "starter@1"}, "capture": {"ts": "/session_id"}},
    {"id": "typed-turn", "svc": "session", "rpc": "Turn", "req": {"session_id": "${ts}", "query": "Who handles invoice questions?"}},
    {"id": "typed-turn-complete", "svc": "session", "rpc": "Turn", "req": {"session_id": "${ts}", "query": "Who handles account access?", "complete": True}},
    {"id": "typed-session-get", "svc": "session", "rpc": "GetSession", "req": {"session_id": "${ts}"}},
    {"id": "typed-session-close", "svc": "session", "rpc": "CloseSession", "req": {"session_id": "${ts}"}},
    {"id": "typed-turn-closed", "kind": "conflict", "svc": "session", "rpc": "Turn", "req": {"session_id": "${ts}", "query": "x"}},
]


class TypedTransport:
    name = "typed"

    def __init__(self, target: str) -> None:
        channel = grpc.insecure_channel(target)
        self.stubs = {
            "command": (command_pb2_grpc.CommandServiceStub(channel), command_pb2),
            "query": (query_pb2_grpc.QueryServiceStub(channel), query_pb2),
            "session": (session_pb2_grpc.SessionServiceStub(channel), session_pb2),
        }

    def run(self, variables: dict[str, Any]) -> list[dict[str, Any]]:
        out = []
        for case in TYPED_CASES:
            entry: dict[str, Any] = {"id": case["id"], "kind": case.get("kind", "success")}
            if case.get("exact"):
                entry["exact"] = True
            try:
                req_dict = cw.substitute(case["req"], variables)
            except KeyError as err:
                entry["skipped"] = str(err)
                out.append(entry)
                continue
            stub, module = self.stubs[case["svc"]]
            method = getattr(stub, case["rpc"])
            message = json_format.ParseDict(req_dict, getattr(module, case["rpc"] + "Request")())
            metadata = []
            token = case.get("token", "rw")
            if token != "none":
                metadata.append(("authorization", f"Bearer {TOKENS[token]}"))
            uid = case.get("uid", "compat-user")
            if uid:
                metadata.append(("munarium-uid", uid))
            if "idem" in case:
                metadata.append(("idempotency-key", f"{variables['run']}-{case['idem']}"))
            try:
                resp = method(message, metadata=metadata, timeout=120)
                raw = resp.SerializeToString(deterministic=True)
                body = json_format.MessageToDict(resp, preserving_proto_field_name=True)
                entry["response"] = {"status": 0, "media_type": "application/grpc+proto", "bytes_len": len(raw),
                                     "sha256": hashlib.sha256(raw).hexdigest(), "json": body}
            except grpc.RpcError as err:
                entry["response"] = {"status": err.code().value[0], "media_type": "grpc-error", "bytes_len": 0,
                                     "sha256": "", "json": error_record(err)}
            missing = cw.capture(case, entry["response"], variables)
            if missing:
                entry["capture_missing"] = missing
            out.append(entry)
            print(case["id"], entry["response"]["status"])
        return out


def served_catalog(target: str, catalog: dict[str, Any]) -> dict[str, Any]:
    """Call every catalog operation with an invalid credential, as the Python
    API conformance test does, and list the RPCs the server does not serve.
    Calls go through generic method paths, so a catalog newer than the stubs
    in this environment can still be probed."""
    channel = grpc.insecure_channel(target)
    unimplemented, other = [], {}
    for op in catalog["operations"]:
        path = f"/mmp.v1.ServerApiService/{op['rpc']}"
        request = server_api_pb2.ServerApiRequest(
            body=b"{}", path_parameters={k: "absent" for k in re.findall(r"\{([^}]+)\}", op["path"])})
        metadata = (("authorization", "Bearer invalid"), ("munarium-uid", "api-conformance"))
        try:
            if op.get("streaming"):
                call = channel.unary_stream(path, request_serializer=server_api_pb2.ServerApiRequest.SerializeToString,
                                            response_deserializer=server_api_pb2.ServerApiResponse.FromString)
                list(call(request, metadata=metadata, timeout=10))
            else:
                call = channel.unary_unary(path, request_serializer=server_api_pb2.ServerApiRequest.SerializeToString,
                                           response_deserializer=server_api_pb2.ServerApiResponse.FromString)
                call(request, metadata=metadata, timeout=10)
        except grpc.RpcError as err:
            if err.code() == grpc.StatusCode.UNIMPLEMENTED:
                unimplemented.append(op["rpc"])
            else:
                other[err.code().name] = other.get(err.code().name, 0) + 1
    channel.close()
    return {"operations": len(catalog["operations"]), "unimplemented": sorted(unimplemented), "other_codes": other}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--target", required=True)
    ap.add_argument("--state", required=True)
    ap.add_argument("--cases")
    ap.add_argument("--proto")
    ap.add_argument("--pdf-b64-file")
    ap.add_argument("--out", required=True)
    ap.add_argument("--mode", choices=["bridge", "typed", "catalog"], required=True)
    ap.add_argument("--catalog", action="append", default=[], help="server-api.json (catalog mode; repeatable)")
    args = ap.parse_args()
    if args.mode == "catalog":
        cw.dump_json(args.out, {path: served_catalog(args.target, cw.load_json(path)) for path in args.catalog})
        return
    state = cw.load_json(args.state)
    variables = dict(state["seed"]["variables"])
    variables["run"] = args.mode
    if args.pdf_b64_file:
        variables["pdf_b64"] = Path(args.pdf_b64_file).read_text(encoding="ascii").strip()
    if args.mode == "bridge":
        transport = BridgeTransport(args.target, Path(args.proto).read_text(encoding="utf-8"))
        cases = cw.load_json(args.cases)["cases"]
        transcript = cw.run_cases(cases, transport, variables, print)
    else:
        transcript = TypedTransport(args.target).run(variables)
    cw.dump_json(args.out, transcript)


if __name__ == "__main__":
    main()
