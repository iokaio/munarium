// SPDX-License-Identifier: Apache-2.0
//! The `munarium-matrix` entry point.
//!
//! Deliberately thin. Everything the service does is in the library beside this
//! file, so that an out-of-tree build — Munarium Matrix Enterprise, or anyone
//! implementing the public `SourceAdapter` interface — can compose its own
//! binary from the same code rather than forking it.

#[tokio::main]
async fn main() {
    munarium_matrix_server::run().await;
}
