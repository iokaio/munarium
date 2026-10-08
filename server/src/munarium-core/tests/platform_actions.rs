// SPDX-License-Identifier: Apache-2.0
use munarium_core::{platform::canonical_record, platform_actions::*};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[test]
fn independently_consume_all_candidate_records_and_digests() {
    let vectors: Value = serde_json::from_str(include_str!(
        "../../../contract/platform-stage2-v1/vectors.json"
    ))
    .unwrap();
    for (name, record) in vectors["records"].as_object().unwrap() {
        let raw = vectors["canonical"][name].as_str().unwrap();
        assert_eq!(canonical_record(raw.as_bytes()).unwrap(), *record, "{name}");
        validate_action_record(record).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            action_digest(record["type"].as_str().unwrap(), record).unwrap(),
            vectors["digests"][name],
            "{name}"
        );
    }
    verify_action_ack(
        &vectors["records"]["predispatch"],
        &vectors["records"]["ack"],
    )
    .unwrap();
}
#[test]
fn verify_exported_bytes_and_exact_candidate_source() {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contract/platform-stage2-v1");
    let lock: Value =
        serde_json::from_slice(&std::fs::read(directory.join("vendor-lock.json")).unwrap())
            .unwrap();
    assert_eq!(lock["revision"], "2fb118909633264fad23ede2e7c9942aba374312");
    assert_eq!(lock["bundle_sha256"], BUNDLE);
    let mut aggregate = String::new();
    for (name, expected) in lock["files"].as_object().unwrap() {
        let raw = std::fs::read_to_string(directory.join(name))
            .unwrap()
            .replace("\r\n", "\n");
        let actual = format!("{:x}", Sha256::digest(raw.as_bytes()));
        assert_eq!(actual, expected.as_str().unwrap(), "{name}");
        aggregate.push_str(&format!("{name}\0{actual}\n"));
    }
    assert_eq!(
        format!("{:x}", Sha256::digest(aggregate.as_bytes())),
        BUNDLE
    );
    let bundle: Value =
        serde_json::from_slice(&std::fs::read(directory.join("bundle-lock.json")).unwrap())
            .unwrap();
    assert_eq!(bundle["files"], lock["files"]);
    assert_eq!(bundle["bundle_sha256"], BUNDLE);
}
