# SPDX-License-Identifier: Apache-2.0
"""Offline unit tests: error decoding, path encoding, wire-sentinel guards."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from google.rpc import error_details_pb2  # noqa: E402

from munarium_client import (  # noqa: E402
    ForbiddenError,
    HeadConflictError,
    NotFoundError,
    OverloadedError,
    PolicyRejectionError,
    RateLimitedError,
    UnauthenticatedError,
    UnexpectedError,
)
from munarium_client._errors import error_from_grpc, error_from_problem  # noqa: E402
from munarium_client._grpc_common import reject_zero, yaml_hash  # noqa: E402
from munarium_client._specs import seg  # noqa: E402


def problem(slug: str, status: int, **ext: object) -> dict[str, object]:
    return {
        "type": f"https://munarium.ioka.io/problems/{slug}",
        "title": slug,
        "status": status,
        "detail": f"{slug} detail",
        **ext,
    }


def test_head_conflict_decodes_extensions() -> None:
    err = error_from_problem(409, problem("head-conflict", 409, expected=3, actual=7))
    assert isinstance(err, HeadConflictError)
    assert (err.expected, err.actual) == (3, 7)


def test_policy_rejection_carries_findings() -> None:
    err = error_from_problem(
        422,
        problem(
            "policy-rejection",
            422,
            gate_findings=[
                {"rule_id": "gate.ledger-conflict", "severity": "block", "message": "m"}
            ],
        ),
    )
    assert isinstance(err, PolicyRejectionError)
    assert err.findings[0].rule_id == "gate.ledger-conflict"
    assert err.findings_total == 1 and not err.findings_truncated


def test_auth_slugs_map_to_typed_errors() -> None:
    assert isinstance(
        error_from_problem(401, problem("unauthenticated", 401)), UnauthenticatedError
    )
    assert isinstance(error_from_problem(403, problem("forbidden", 403)), ForbiddenError)


def test_not_found_uses_kind_and_id() -> None:
    err = error_from_problem(404, problem("not-found", 404, kind="claim", id="c-1"))
    assert isinstance(err, NotFoundError)
    assert (err.kind, err.id) == ("claim", "c-1")


def test_overloaded_is_typed_and_transient() -> None:
    err = error_from_problem(503, problem("overloaded", 503))
    assert isinstance(err, OverloadedError) and err.transient
    # unknown slug on a 5xx gateway status is also transient
    raw = error_from_problem(503, {"type": "x", "status": 503})
    assert isinstance(raw, UnexpectedError) and raw.transient


def test_rate_limited_carries_retry_after() -> None:
    err = error_from_problem(429, problem("rate-limited", 429), retry_after=7.0)
    assert isinstance(err, RateLimitedError) and err.retry_after == 7.0


def test_grpc_error_info_round_trip() -> None:
    info = error_details_pb2.ErrorInfo(reason="head-conflict", domain="mmp.ioka.io")
    info.metadata["expected"] = "3"
    info.metadata["actual"] = "7"
    err = error_from_grpc("ABORTED", "head conflict", info)
    assert isinstance(err, HeadConflictError)
    assert (err.expected, err.actual) == (3, 7)


def test_grpc_truncated_findings_are_marked() -> None:
    info = error_details_pb2.ErrorInfo(reason="policy-rejection", domain="mmp.ioka.io")
    info.metadata["gate_findings"] = (
        '[{"rule_id":"gate.ledger-conflict","severity":"block","message":"m"}]'
    )
    info.metadata["findings_total"] = "40"
    info.metadata["findings_truncated"] = "true"
    err = error_from_grpc("FAILED_PRECONDITION", "policy rejection", info)
    assert isinstance(err, PolicyRejectionError)
    assert err.findings_total == 40 and err.findings_truncated
    assert len(err.findings) == 1


def test_grpc_aborted_without_details_is_head_conflict() -> None:
    err = error_from_grpc("ABORTED", "conflict via intermediary", None)
    assert isinstance(err, HeadConflictError)
    assert (err.expected, err.actual) == (0, 0)


def test_seg_percent_encodes_path_segments() -> None:
    assert seg("phase/1.reveal?x") == "phase%2F1.reveal%3Fx"
    assert seg("support-tickets@1") == "support-tickets%401"


def test_reject_zero_guards_proto3_sentinels() -> None:
    from munarium_client import InvalidInputError

    reject_zero("as_of_seq", None)
    reject_zero("as_of_seq", 5)
    with pytest.raises(InvalidInputError):
        reject_zero("as_of_seq", 0)


def test_yaml_hash_matches_server_definition() -> None:
    import hashlib

    y = "kind: Shape\n"
    assert yaml_hash(y) == hashlib.sha256(y.encode()).hexdigest()
