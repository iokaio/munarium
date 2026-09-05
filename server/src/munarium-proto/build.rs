// SPDX-License-Identifier: Apache-2.0
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Vendored protoc: no system protoc on Windows dev boxes or CI runners.
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);

    // In the workspace the normative protos live at server/proto and this
    // crate reads them in place. In the PACKAGED crate they cannot: a .crate
    // holds only what is under the crate directory, so a release copies
    // proto/mmp/v1 into ./proto right before `cargo package` (README.md).
    // The in-crate copy wins when present, and is never committed.
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let proto_root = if manifest_dir.join("proto/mmp/v1").is_dir() {
        manifest_dir.join("proto")
    } else {
        manifest_dir.join("../../proto")
    };
    let protos: Vec<PathBuf> = [
        "common",
        "ledger",
        "command",
        "query",
        "retrieval",
        "ingest",
        "runbook",
        "provider",
        "admin",
        "session",
    ]
    .iter()
    .map(|f| proto_root.join("mmp/v1").join(format!("{f}.proto")))
    .collect();

    let descriptor_path = PathBuf::from(std::env::var("OUT_DIR")?).join("mmp_v1_descriptor.bin");

    tonic_build::configure()
        .file_descriptor_set_path(&descriptor_path) // powers tonic-reflection (grpcurl without local protos)
        .compile_protos(&protos, &[proto_root])?;

    for p in &protos {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    Ok(())
}
