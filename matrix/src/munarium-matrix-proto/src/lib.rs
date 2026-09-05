// SPDX-License-Identifier: Apache-2.0
//! `matrix.v1` — the gRPC data plane's wire types and service stubs.
//!
//! Generated from `matrix/proto/matrix/v1/matrix.proto`. The JSON schemas in
//! `matrix/contract/` are the one normative contract; these messages mirror
//! them field for field and [`convert`] carries values across. The proof that
//! the mirror is faithful is `tests/drift.rs`, which round-trips every
//! committed contract example through proto and back.

#![forbid(unsafe_code)]

pub mod convert;

#[allow(clippy::all)]
pub mod v1 {
    tonic::include_proto!("matrix.v1");

    /// The encoded file descriptor set, for tonic-reflection.
    pub const FILE_DESCRIPTOR_SET: &[u8] =
        tonic::include_file_descriptor_set!("matrix_v1_descriptor");
}

pub use v1::*;
