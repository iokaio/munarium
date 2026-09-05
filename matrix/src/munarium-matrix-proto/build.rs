// SPDX-License-Identifier: Apache-2.0
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Vendored protoc: no system protoc on Windows dev boxes, CI runners or
    // the Alpine builder stage. Same choice the server made.
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);

    let proto_root = PathBuf::from("../../proto");
    let proto = proto_root.join("matrix/v1/matrix.proto");
    let descriptor = PathBuf::from(std::env::var("OUT_DIR")?).join("matrix_v1_descriptor.bin");

    tonic_build::configure()
        // Powers tonic-reflection, so `grpcurl` works without local protos.
        .file_descriptor_set_path(&descriptor)
        .compile_protos(&[&proto], &[&proto_root])?;

    println!("cargo:rerun-if-changed={}", proto.display());
    Ok(())
}
