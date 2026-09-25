// SPDX-License-Identifier: Apache-2.0
//! Synthetic wire fixtures, not claims of running a previous released binary.
use crate::server_api::{ApiRequest, ApiResponse};
use crate::{dto, MunariumError};
use serde_json::json;

const EXACT: [u64; 6] = [
    (1 << 53) - 1,
    1 << 53,
    (1 << 53) + 1,
    i64::MAX as u64,
    i64::MAX as u64 + 1,
    u64::MAX,
];

// Frozen previous-release response model from clients-v1.1.0
// (afd3eb7bfcea253ce7f48f7060c72c23e26920c0), server/src/munarium-api-types/src/lib.rs.
// Source blob c17f221d4777182ed6df0a102c1fcd188a9f2d47 is also used by clients-v1.1.1.
// This uses current serde dependencies; it does not execute an old SDK binary.
#[derive(serde::Serialize, serde::Deserialize)]
struct PreviousCompleteResponse {
    text: String,
    stop_reason: String,
    input_tokens: u64,
    output_tokens: u64,
    provider: String,
    model: String,
    invocation_event_id: Option<String>,
}

#[test]
fn wire_integer_json_request_response_and_error_metadata_are_exact() {
    for n in EXACT {
        let input = ApiRequest::json(&json!({"expected_head": n})).unwrap();
        assert_eq!(
            String::from_utf8(input.body).unwrap(),
            format!("{{\"expected_head\":{n}}}")
        );
        let response = ApiResponse {
            status: 200,
            content_type: "application/json".into(),
            body: format!("{{\"head_seq\":{n},\"future_field\":true}}").into_bytes(),
        };
        assert_eq!(response.json::<dto::HeadResponse>().unwrap().head_seq, n);
        let problem = json!({"type":"https://munarium.ioka.io/problems/head-conflict", "expected":n, "actual":n, "future_field":true});
        assert!(
            matches!(MunariumError::from_problem(409, None, &problem), MunariumError::HeadConflict { expected, actual } if expected == n && actual == n)
        );
    }
    for invalid in [
        "-1",
        "0.5",
        "9007199254740993.0",
        "18446744073709551616",
        "true",
        "\"123\"",
        "null",
    ] {
        let response = ApiResponse {
            status: 200,
            content_type: "application/json".into(),
            body: format!("{{\"head_seq\":{invalid}}}").into_bytes(),
        };
        assert!(
            response.json::<dto::HeadResponse>().is_err(),
            "accepted {invalid}"
        );
    }
}

#[cfg(feature = "grpc")]
#[test]
fn wire_integer_protobuf_and_native_body_preserve_exact_values() {
    use munarium_proto::mmp::v1 as pb;
    use prost::Message;
    use tonic_types::{ErrorDetails, StatusExt};
    for n in EXACT {
        let encoded = pb::GetHeadResponse { head_seq: n }.encode_to_vec();
        // Unknown varint field 127 = 1: an older receiver skips it.
        let mut future = encoded;
        future.extend_from_slice(&[0xf8, 0x07, 0x01]);
        assert_eq!(
            pb::GetHeadResponse::decode(future.as_slice())
                .unwrap()
                .head_seq,
            n
        );
        let body = format!("{{\"head_seq\":{n},\"optional\":null}}").into_bytes();
        let bridge = pb::ServerApiResponse {
            status: 200,
            content_type: "application/json".into(),
            body: body.clone(),
        };
        assert_eq!(
            pb::ServerApiResponse::decode(bridge.encode_to_vec().as_slice())
                .unwrap()
                .body,
            body
        );
        let mut details = ErrorDetails::new();
        details.set_error_info(
            "head-conflict",
            "mmp.ioka.io",
            [
                ("expected".into(), n.to_string()),
                ("actual".into(), n.to_string()),
            ],
        );
        let status = tonic::Status::with_error_details(tonic::Code::Aborted, "conflict", details);
        assert!(
            matches!(crate::error::from_status(status), MunariumError::HeadConflict { expected, actual } if expected == n && actual == n)
        );
    }
}

#[test]
fn wire_previous_completion_shape_and_additive_usage_are_readable() {
    // The 1.1 baseline omits usage evidence; P01 adds metadata alongside
    // the original integer counters. Unknown evidence must not imply known usage.
    let previous = json!({"text":"fictional answer", "stop_reason":"stop", "input_tokens":9007199254740993_u64, "output_tokens":1, "provider":"fixture", "model":"fixture"});
    for usage in [
        None,
        Some(serde_json::Value::Null),
        Some(json!({"quality":"future_quality", "input_tokens":null, "output_tokens":1})),
    ] {
        let mut current = previous.clone();
        if let Some(usage) = usage {
            current["usage"] = usage;
        }
        current["future_optional"] = json!({"opaque_id":"id:with/slash+suffix"});
        let typed: dto::CompleteResponse = serde_json::from_value(current.clone()).unwrap();
        let prior: PreviousCompleteResponse = serde_json::from_value(current.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(prior).unwrap(),
            serde_json::to_value(&typed).unwrap()
        );
        assert_eq!(typed.input_tokens, 9007199254740993);
        assert_eq!(typed.invocation_event_id, None);
        let raw = ApiResponse {
            status: 200,
            content_type: "application/json".into(),
            body: serde_json::to_vec(&current).unwrap(),
        };
        assert_eq!(raw.json::<serde_json::Value>().unwrap(), current);
    }
    for optional in [None, Some(serde_json::Value::Null), Some(json!(0))] {
        let mut value =
            json!({"subject":"fixture", "key":"color", "value":"blue", "claim_type":"fact"});
        if let Some(ref optional) = optional {
            value["expected_head"] = optional.clone();
        }
        let input: dto::ProposeClaimRequest = serde_json::from_value(value).unwrap();
        assert_eq!(input.expected_head, optional.and_then(|v| v.as_u64()));
    }
    for unknown in ["future_status", ""] {
        assert!(serde_json::from_value::<dto::ClaimStatusDto>(json!(unknown)).is_err());
        assert!(serde_json::from_value::<dto::SeverityDto>(json!(unknown)).is_err());
    }
}

#[test]
fn wire_previous_client_completion_model_matches_current_at_integer_limits() {
    for n in EXACT {
        for invocation in [
            None,
            Some(serde_json::Value::Null),
            Some(json!("opaque:id/one")),
        ] {
            let mut payload = json!({"text":"fixture", "stop_reason":"stop", "input_tokens":n, "output_tokens":n, "provider":"fixture", "model":"fixture", "usage":{"quality":"complete"}});
            if let Some(invocation) = invocation {
                payload["invocation_event_id"] = invocation;
            }
            let previous: PreviousCompleteResponse =
                serde_json::from_value(payload.clone()).unwrap();
            let current: dto::CompleteResponse = serde_json::from_value(payload).unwrap();
            assert_eq!(previous.input_tokens, n);
            assert_eq!(previous.output_tokens, n);
            assert_eq!(
                serde_json::to_value(previous).unwrap(),
                serde_json::to_value(current).unwrap()
            );
        }
    }
}
