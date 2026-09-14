// SPDX-License-Identifier: Apache-2.0
fn main() {
    // sqlx's embedded migrator must be rebuilt when an additive migration
    // arrives, including builds which reuse a Cargo cache mount.
    println!("cargo:rerun-if-changed=migrations");
}
