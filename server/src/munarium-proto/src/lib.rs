// SPDX-License-Identifier: Apache-2.0
//! Generated MMP v1 types and tonic service stubs.
//!
//! The proto files under `server/proto/mmp/v1/` are the normative wire spec.
//! These types are wire-only: axum handlers and tonic service impls convert to
//! `munarium-api-types` / `munarium-core` domain types at the boundary and never let
//! prost types travel further in.

pub mod mmp {
    // Generated code: tonic's service stubs return Result<_, tonic::Status>
    // (~176 bytes), which clippy 1.98's result_large_err flags. The shape is
    // tonic's contract, not ours to change — suppress for the generated
    // module only.
    #[allow(clippy::result_large_err)]
    pub mod v1 {
        tonic::include_proto!("mmp.v1");
    }
}

/// Descriptor set for tonic-reflection, so `grpcurl` works without local protos.
pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("mmp_v1_descriptor");
