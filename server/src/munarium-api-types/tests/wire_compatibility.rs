// SPDX-License-Identifier: Apache-2.0
//! Published numeric JSON shapes and the existing direction-specific policy.
use munarium_api_types as dto;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

fn round_trip<T: DeserializeOwned + Serialize>(value: Value) -> Value {
    let bytes = serde_json::to_vec(&value).unwrap();
    let parsed: T = serde_json::from_slice(&bytes).unwrap();
    serde_json::to_value(parsed).unwrap()
}

#[test]
fn wire_integer_json_fields_remain_exact_numeric_tokens() {
    for value in [
        0,
        (1u64 << 53) - 1,
        1u64 << 53,
        (1u64 << 53) + 1,
        i64::MAX as u64,
        u64::MAX,
    ] {
        let head = round_trip::<dto::HeadResponse>(json!({"head_seq": value}));
        assert_eq!(head["head_seq"].as_u64(), Some(value));
        assert!(serde_json::to_string(&head)
            .unwrap()
            .contains(&format!(":{value}")));
        let counter =
            round_trip::<dto::CounterDto>(json!({"key":"fixture", "total":value, "budget":value}));
        assert_eq!(counter["total"].as_u64(), Some(value));
        assert_eq!(counter["budget"].as_u64(), Some(value));
        let context = round_trip::<dto::ComposedContextDto>(
            json!({"sections":[], "text":"", "estimated_tokens":value, "content_hash":"opaque-hash", "as_of_seq":value}),
        );
        assert_eq!(context["estimated_tokens"].as_u64(), Some(value));
        assert_eq!(context["as_of_seq"].as_u64(), Some(value));
        let completion = round_trip::<dto::TurnCompletionDto>(
            json!({"provider":"fixture", "model":"opaque:model", "was_override":false, "text":"", "input_tokens":value, "output_tokens":value}),
        );
        assert_eq!(completion["input_tokens"].as_u64(), Some(value));
        assert_eq!(completion["output_tokens"].as_u64(), Some(value));
        let envelope = round_trip::<dto::ProvenanceEnvelopeDto>(
            json!({"chunk_ids":[], "source_ids":[], "source_paths":[], "source_content_hashes":[], "index_version":"opaque:version", "event_watermark":value}),
        );
        assert_eq!(envelope["event_watermark"].as_u64(), Some(value));
    }
    // Signed DTOs preserve their entire transport range. The report operation
    // separately validates that actual usage is representable and nonnegative.
    for value in [
        i64::MIN,
        -1,
        0,
        (1i64 << 53) - 1,
        1i64 << 53,
        (1i64 << 53) + 1,
        i64::MAX,
    ] {
        let row = round_trip::<dto::BudgetRow>(
            json!({"config":"fixture", "tier":"fast", "day":"2026-01-01", "held_tokens":value, "settled_tokens":value, "reservations":value, "limit":value, "remaining":value}),
        );
        for field in [
            "held_tokens",
            "settled_tokens",
            "reservations",
            "limit",
            "remaining",
        ] {
            assert_eq!(row[field].as_i64(), Some(value));
        }
    }
}

#[test]
fn wire_integer_requests_reject_malformed_fractional_negative_and_overflow() {
    for value in [
        "-1",
        "0.5",
        "1.0",
        "18446744073709551616",
        "\"9007199254740993\"",
        "true",
        "[]",
        "NaN",
        "1e999",
    ] {
        let body = format!(r#"{{"key":"fixture","scope_path":"fixture","count":{value}}}"#);
        assert!(
            serde_json::from_str::<dto::RecordCountsRequest>(&body).is_err(),
            "{value}"
        );
    }
    for value in ["-1", "1.5", "4294967296"] {
        let body = format!(r#"{{"max_tokens":{value}}}"#);
        assert!(
            serde_json::from_str::<dto::CompleteRequest>(&body).is_err(),
            "{value}"
        );
    }
    let max: dto::CompleteRequest = serde_json::from_str(r#"{"max_tokens":4294967295}"#).unwrap();
    assert_eq!(max.max_tokens, Some(u32::MAX));
}

#[test]
fn wire_additions_null_and_omission_keep_existing_policy() {
    let previous = json!({"claim_type":"fact","subject":"fixture","key":"key","value":"value"});
    let mut current = previous.clone();
    current["future_control"] = json!({"allowed":true});
    current["expected_head"] = Value::Null;
    current["evidence"] = Value::Null;
    current["provenance"] = Value::Null;
    let old: dto::ProposeClaimRequest = serde_json::from_value(previous).unwrap();
    let new: dto::ProposeClaimRequest = serde_json::from_value(current).unwrap();
    assert_eq!(
        serde_json::to_value(old).unwrap(),
        serde_json::to_value(new).unwrap()
    );
    let response = round_trip::<dto::HeadResponse>(
        json!({"head_seq":9007199254740993u64,"future_field":{"enabled":true}}),
    );
    assert_eq!(response, json!({"head_seq":9007199254740993u64}));
    for unknown in [json!("future"), Value::Null, json!(255)] {
        assert!(serde_json::from_value::<dto::ClaimStatusDto>(unknown.clone()).is_err());
        assert!(serde_json::from_value::<dto::SeverityDto>(unknown.clone()).is_err());
        assert!(serde_json::from_value::<dto::ProvenanceDto>(unknown).is_err());
    }
}

#[cfg(feature = "proto")]
#[test]
fn wire_typed_protobuf_integer_and_unknown_enum_policy() {
    use munarium_proto::mmp::v1 as pb;
    for value in [
        (1u64 << 53) - 1,
        1u64 << 53,
        (1u64 << 53) + 1,
        i64::MAX as u64,
        u64::MAX,
    ] {
        let claim: dto::ClaimDto = pb::Claim {
            seq: value,
            claim_type: 999,
            status: 999,
            provenance: 999,
            ..Default::default()
        }
        .into();
        assert_eq!(claim.seq, value);
        assert_eq!(claim.claim_type, dto::ClaimTypeDto::Fact);
        assert_eq!(claim.status, dto::ClaimStatusDto::Disputed);
        assert_eq!(claim.provenance, dto::ProvenanceDto::Emergent);
        let back: pb::Claim = claim.into();
        assert_eq!(back.seq, value);
        let finding: dto::GateFindingDto = pb::GateFinding {
            severity: 999,
            ..Default::default()
        }
        .into();
        assert_eq!(finding.severity, dto::SeverityDto::Block);
        let request: dto::ProposeClaimRequest = pb::ProposeClaimRequest {
            expected_head: Some(value),
            ..Default::default()
        }
        .into();
        assert_eq!(request.expected_head, Some(value));
    }
}
