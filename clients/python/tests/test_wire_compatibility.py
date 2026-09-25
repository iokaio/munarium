# SPDX-License-Identifier: Apache-2.0
"""Offline wire regressions; JSON integers never pass through binary64."""

import json

import pytest
from pydantic import ValidationError

from munarium_client import _grpc_common as grpc_wire
from munarium_client import _specs, models
from munarium_client._errors import HeadConflictError, error_from_problem
from munarium_client._proto.mmp.v1 import common_pb2, ledger_pb2
from munarium_client._server_api import ApiRequest, ApiResponse

BOUNDARIES = [0, 2**53 - 1, 2**53, 2**53 + 1, 2**63 - 1, 2**63, 2**64 - 1]
INVALID_UNSIGNED = [-1, 2**64, 1.5, 9007199254740992.0, True, "12", "bad", None]


@pytest.mark.parametrize("value", BOUNDARIES)
def test_head_and_sequence_models_preserve_exact_json_integers(value):
    raw = json.dumps({"head_seq": value, "as_of_seq": value, "facts": []})
    assert _specs.head("fixture").parse(json.loads(raw)) == value
    page = models.FactsPage.model_validate_json(raw)
    assert page.head_seq == page.as_of_seq == value
    assert json.loads(page.model_dump_json())["head_seq"] == value


@pytest.mark.parametrize("value", INVALID_UNSIGNED)
def test_head_rejects_invalid_integer_tokens(value):
    with pytest.raises((ValueError, TypeError)):
        _specs.head("fixture").parse({"head_seq": value})


@pytest.mark.parametrize("value", INVALID_UNSIGNED)
def test_sequence_models_reject_invalid_integer_tokens(value):
    with pytest.raises(ValidationError):
        models.FactsPage(facts=[], head_seq=value, as_of_seq=0)


@pytest.mark.parametrize("value", BOUNDARIES)
def test_protobuf_sequence_survives_serialization_and_sdk_conversion(value):
    message = ledger_pb2.Claim(seq=value)
    # Unknown varint field 127 is skipped by an older protobuf decoder.
    decoded = ledger_pb2.Claim.FromString(message.SerializeToString() + b"\xf8\x07\x01")
    assert grpc_wire.parse_claim(decoded).seq == value


@pytest.mark.parametrize("value", BOUNDARIES)
def test_full_api_and_conflict_metadata_keep_exact_integers(value):
    payload = {"expected_head": value, "optional": None, "future": {"n": value}}
    request = ApiRequest.json(payload)
    response = ApiResponse(200, "application/json", request.proto().body)
    assert response.json() == payload
    error = error_from_problem(
        409,
        {
            "type": "https://munarium.ioka.io/problems/head-conflict",
            "expected": value,
            "actual": value,
        },
    )
    assert isinstance(error, HeadConflictError)
    assert error.expected == error.actual == value


@pytest.mark.parametrize("usage", [None, {"quality": "future_quality", "input_tokens": None}])
def test_previous_and_additive_current_completion_shapes(usage):
    # Synthetic 1.1 shape and additive current/future shapes, not released binaries.
    previous = {
        "text": "fictional answer",
        "stop_reason": "stop",
        "input_tokens": 2**53 + 1,
        "output_tokens": 1,
        "provider": "fixture",
        "model": "fixture",
    }
    current = {**previous, "usage": usage, "future_optional": {"opaque_id": "id:with/slash+suffix"}}
    for payload in (previous, current):
        typed = models.CompleteResult.model_validate(payload)
        assert typed.input_tokens == 2**53 + 1
        assert typed.invocation_event_id is None
        assert ApiResponse(200, "application/json", ApiRequest.json(payload).body).json() == payload


@pytest.mark.parametrize("value", [0, 999])
def test_unknown_governance_enums_are_conservative(value):
    # 0 is proto3's absent/unset tag; 999 represents a newer server enum.
    claim = grpc_wire.parse_claim(ledger_pb2.Claim(status=value, provenance=value))
    finding = grpc_wire.parse_finding(common_pb2.GateFinding(severity=value))
    assert claim.status == "disputed"
    assert claim.provenance == "emergent"
    assert finding.severity == "block"


def test_known_governance_enums_retain_their_meaning():
    claim = grpc_wire.parse_claim(
        ledger_pb2.Claim(
            status=common_pb2.CLAIM_STATUS_ACCEPTED,
            provenance=ledger_pb2.PROVENANCE_WITNESSED,
        )
    )
    assert claim.status == "accepted"
    assert claim.provenance == "witnessed"
    assert (
        grpc_wire.parse_finding(common_pb2.GateFinding(severity=common_pb2.SEVERITY_INFO)).severity
        == "info"
    )


def test_additive_fields_and_null_omission_keep_existing_typed_behavior():
    page = models.FactsPage.model_validate(
        {"facts": [], "head_seq": 2**53 + 1, "as_of_seq": 0, "future": {"n": 2**63 - 1}}
    )
    assert page.model_dump()["future"] == {"n": 2**63 - 1}
    omitted = models.ClaimInput(subject="fixture", key="key", value="value")
    explicit = models.ClaimInput(subject="fixture", key="key", value="value", origin=None)
    assert _specs.claim_body(omitted) == _specs.claim_body(explicit)
    assert "origin" not in _specs.claim_body(explicit)
