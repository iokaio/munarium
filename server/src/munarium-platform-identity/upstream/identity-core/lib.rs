// SPDX-License-Identifier: Apache-2.0
//! Decision verification package using the same owner-maintained Warden source.
//! No grant store, provider, broker, network client or runtime listener is linked.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[path = "../src/encoding.rs"]
pub mod encoding;
#[path = "../src/policy.rs"]
pub mod policy;
#[path = "../src/principal.rs"]
pub mod principal;

/// Sanitized verification errors; no dependency/provider diagnostics.
pub mod error {
    /// Fail-closed decision verifier results.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Error {
        /// Malformed, unsupported or oversized input.
        InvalidInput,
        /// Signature, scope, time or identity binding failed.
        Identity,
        /// The current authority snapshot is unavailable.
        Unavailable,
    }
    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }
    impl std::error::Error for Error {}
}
