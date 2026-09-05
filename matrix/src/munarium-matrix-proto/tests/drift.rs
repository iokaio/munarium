// SPDX-License-Identifier: Apache-2.0
//! The drift check: every committed contract example survives a round trip
//! through the `matrix.v1` messages unchanged.
//!
//! `matrix/contract/*.schema.json` is the one normative contract. The proto
//! mirrors it, and a mirror drifts silently — a field added to the schema and
//! forgotten here would simply be dropped on the wire. So the committed
//! examples, which the schema check already validates, are the oracle: parse
//! one into the contract type, cross to proto, cross back, and compare both
//! the typed value and its JSON to the original. A field the proto cannot
//! carry fails here, on every push, for $0.

use munarium_matrix_core::Refusal;
use munarium_matrix_proto::v1 as pb;
use munarium_matrix_types::contract::{EvidenceBlock, QueryIntent};
use std::path::PathBuf;

fn example(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contract/examples")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn roundtrip_intent(name: &str) {
    let json = example(name);
    let parsed: QueryIntent = serde_json::from_value(json.clone()).expect("example parses");
    let wire = pb::QueryIntent::from(&parsed);
    let back = QueryIntent::try_from(wire).expect("proto converts back");
    assert_eq!(parsed, back, "{name}: the typed value moved");
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        serde_json::to_value(&back).unwrap(),
        "{name}: the JSON moved"
    );
}

fn roundtrip_block(name: &str) {
    let json = example(name);
    let parsed: EvidenceBlock = serde_json::from_value(json).expect("example parses");
    let wire = pb::EvidenceBlock::try_from(&parsed).expect("contract converts to proto");
    let back = EvidenceBlock::try_from(&wire).expect("proto converts back");
    assert_eq!(parsed, back, "{name}: the typed value moved");
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        serde_json::to_value(&back).unwrap(),
        "{name}: the JSON moved"
    );
}

fn roundtrip_refusal(name: &str) {
    let json = example(name);
    let parsed: Refusal = serde_json::from_value(json).expect("example parses");
    let wire = pb::Refusal::from(&parsed);
    let back = Refusal::try_from(&wire).expect("proto converts back");
    assert_eq!(parsed, back, "{name}: the typed value moved");
}

#[test]
fn the_structured_intent_example_round_trips() {
    roundtrip_intent("query-intent.structured.json");
}

#[test]
fn the_complete_table_example_round_trips() {
    roundtrip_block("evidence-block.complete-table.json");
}

#[test]
fn the_count_example_round_trips() {
    roundtrip_block("evidence-block.count.json");
}

#[test]
fn the_refusal_block_example_round_trips() {
    roundtrip_block("evidence-block.refusal.json");
}

#[test]
fn the_refusal_examples_round_trip() {
    roundtrip_refusal("refusal.policy-denied.json");
    roundtrip_refusal("refusal.hidden-required-layer.json");
}

/// A NULL cell and an empty-string cell are different on the wire, because
/// they are different in the T0 fixture and conflating them files a false
/// discrepancy.
#[test]
fn a_null_cell_survives_the_wire_as_null() {
    let mut json = example("evidence-block.complete-table.json");
    let rows = json["rows"].as_array_mut().expect("rows");
    rows[0]["cells"][0] = serde_json::Value::Null;
    rows[0]["cells"][1] = serde_json::Value::String(String::new());
    let parsed: EvidenceBlock = serde_json::from_value(json).unwrap();
    let wire = pb::EvidenceBlock::try_from(&parsed).unwrap();
    let back = EvidenceBlock::try_from(&wire).unwrap();
    assert_eq!(parsed, back);
    if let EvidenceBlock::CompleteTable { rows, .. } = back {
        assert_eq!(rows[0].cells[0], None);
        assert_eq!(rows[0].cells[1].as_deref(), Some(""));
    } else {
        panic!("not a table");
    }
}

/// An `UNSPECIFIED` enum is a value the caller did not send; it is an error,
/// never a default.
#[test]
fn an_unspecified_kind_is_refused_not_defaulted() {
    let mut wire = pb::QueryIntent::from(
        &serde_json::from_value::<QueryIntent>(example("query-intent.structured.json")).unwrap(),
    );
    wire.kind = 0;
    assert!(QueryIntent::try_from(wire).is_err());
}

/// The descriptor names the one service this plane serves, and nothing else.
#[test]
fn the_descriptor_carries_exactly_one_service() {
    use prost::Message;
    let set = prost_types::FileDescriptorSet::decode(pb::FILE_DESCRIPTOR_SET)
        .expect("descriptor decodes");
    let services: Vec<String> = set
        .file
        .iter()
        .flat_map(|f| {
            f.service
                .iter()
                .map(|s| format!("{}.{}", f.package(), s.name()))
        })
        .collect();
    assert_eq!(services, vec!["matrix.v1.MatrixQuery".to_string()]);
}
