// SPDX-License-Identifier: Apache-2.0
//! `munarium-matrix-core` — the pure kernel of Munarium Matrix.
//!
//! Everything here is a function of its inputs: typed values and their
//! `canon@1` encoding, evidence identity, the refusal taxonomy, declared
//! derivations, record rendering, and the connector checkpoint contract.
//!
//! **This crate performs no I/O and depends on no runtime.** No `sqlx`, no
//! `reqwest`, no `axum`, no `tokio`, no adapter. That is not tidiness: it is
//! what lets the evidence-identity rules be tested exhaustively on a laptop in
//! milliseconds, and it is enforced in CI by a `cargo tree` grep
//! (`matrix/test.ps1` runs the same check locally).
//!
//! The one thing to read first is [`canon`]: two hashes, computed from
//! different inputs, that must never be conflated.

#![forbid(unsafe_code)]
// A `Refusal` is the payload, not an anomaly: it carries a class, a code, a
// message and structured detail because a caller must be able to act on it.
// That makes it larger than clippy's Err-variant threshold, and boxing it
// everywhere would trade real ergonomics for a lint. The server workspace
// makes the same call for `KernelError`.
#![allow(clippy::result_large_err)]

pub mod canon;
pub mod checkpoint;
pub mod compile;
pub mod derivation;
pub mod planner;
pub mod refusal;
pub mod render;
pub mod result;
pub mod semantic;
pub mod value;

pub use canon::{artifact_hash, logical_result_hash, row_id, CANON_VERSION};
pub use compile::{compile, CompileScope, CompiledStatement, COMPILER_VERSION};
pub use derivation::{ComputedDerivation, Derivation, DerivationOp};
pub use refusal::{Refusal, RefusalClass};
pub use render::{record_path, render_record, RecordDocument, RenderSpec, RENDER_VERSION};
pub use result::{
    Additivity, AuthorizationClass, Column, ResultSchema, Row, RowIdRule, SchemaError, TypedResult,
};
pub use value::{ColumnType, Value};

/// The contract version this build speaks. Kept in lockstep with
/// `matrix/contract/VERSION`; the conformance suite asserts they agree, so a
/// contract bump that forgets the code (or vice versa) fails a test rather
/// than shipping a silent mismatch.
pub const CONTRACT_VERSION: &str = "0.1.0";

/// The result type used throughout Matrix: a typed refusal, never a string.
pub type Result<T> = std::result::Result<T, Refusal>;

#[cfg(test)]
mod contract_tests {
    use super::*;
    use std::path::PathBuf;

    fn contract_dir() -> PathBuf {
        // CARGO_MANIFEST_DIR is matrix/src/munarium-matrix-core
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("contract")
    }

    #[test]
    fn the_code_and_the_contract_agree_on_the_version() {
        let file = std::fs::read_to_string(contract_dir().join("VERSION"))
            .expect("matrix/contract/VERSION must exist");
        assert_eq!(
            file.trim(),
            CONTRACT_VERSION,
            "contract/VERSION and CONTRACT_VERSION drifted"
        );
    }

    /// The canonicalization schema is normative; this test is what stops
    /// `value.rs` and the schema from disagreeing silently.
    #[test]
    fn canon_rules_match_the_contract() {
        let text = std::fs::read_to_string(contract_dir().join("canonicalization.schema.json"))
            .expect("canonicalization schema must exist");
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let rules = &doc["properties"]["rules"]["properties"];

        assert_eq!(doc["properties"]["canon"]["const"], CANON_VERSION);

        // The closed type list must match ColumnType exactly, in order.
        let listed: Vec<String> = rules["logical_types"]["const"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let ours = [
            ColumnType::Bool,
            ColumnType::Int64,
            ColumnType::Decimal,
            ColumnType::Float64,
            ColumnType::String,
            ColumnType::Bytes,
            ColumnType::Date,
            ColumnType::TimestampTz,
            ColumnType::TimestampNaive,
            ColumnType::Interval,
            ColumnType::Uuid,
            ColumnType::Json,
            ColumnType::Array,
        ]
        .map(|t| t.as_str().to_string())
        .to_vec();
        assert_eq!(listed, ours, "contract type list and ColumnType drifted");

        // The separators are load-bearing: a change here changes every hash.
        let seps = &rules["hashing"]["properties"]["separators"]["const"];
        assert_eq!(seps["field"].as_u64().unwrap() as u8, value::FIELD_SEP);
        assert_eq!(seps["row"].as_u64().unwrap() as u8, value::ROW_SEP);
        assert_eq!(seps["section"].as_u64().unwrap() as u8, value::SECTION_SEP);
    }
}
