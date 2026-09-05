// SPDX-License-Identifier: Apache-2.0
//! The vendored Munarium Matrix contract, checked from the server tree.
//!
//! `server/contract/matrix/` is a vendored cut of `matrix/contract/`, written by
//! `matrix/contract/publish.py` together with a `contract.lock` (see
//! `server/contract/README.md`). Ground rule 1 forbids a crate dependency
//! between the two trees, so the boundary is a copy plus checks — a drift
//! check where the sibling tree exists (`publish.py --check`, both trees' CI),
//! the lock check below where it does not, and this file for meaning.
//!
//! **Meaning** is the older half. It answers a different question than a
//! byte comparison: the two copies can be identical and still both be wrong.
//! So this asserts that every vendored schema is a valid JSON Schema, that
//! every vendored example validates against the schema it claims, and that no
//! example has been added without a pairing. The lock test is the newer half:
//! in a standalone Server checkout there is no `../matrix` to diff against,
//! and the lock is the whole proof that the copy is what the publisher cut.
//!
//! It deliberately reads only from `server/contract/` — never across the tree
//! boundary into `matrix/` — because a test that reached into the other tree
//! would pass in this checkout and fail in the Docker build context, and would
//! be a dependency edge in everything but name.
//!
//! A second half remains open: deserializing each example through the real
//! DTO, so the examples prove the Rust types and not just the schemas.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use jsonschema::Resource;
use serde_json::Value;

/// example file name -> the schema it must validate against.
///
/// Mirrors `PAIRS` in the contract's own `validate_examples.py`. The
/// duplication is intentional and is itself tested: `every_example_is_paired`
/// fails if an example is added without a pairing, so the two lists cannot
/// silently diverge in the direction that matters (an unchecked example). Both
/// trees validating the same pairs is the point — the Python script runs in
/// `matrix-ci`, this test in `server-ci`.
const PAIRS: &[(&str, &str)] = &[
    ("query-intent.structured.json", "query-intent.schema.json"),
    (
        "evidence-manifest.table.json",
        "evidence-manifest.schema.json",
    ),
    (
        "evidence-block.complete-table.json",
        "evidence-block.schema.json",
    ),
    ("evidence-block.count.json", "evidence-block.schema.json"),
    ("evidence-block.refusal.json", "evidence-block.schema.json"),
    ("refusal.policy-denied.json", "refusal.schema.json"),
    ("refusal.hidden-required-layer.json", "refusal.schema.json"),
    ("observation-batch.json", "observation-batch.schema.json"),
];

fn contract_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is server/src/munarium-api-types.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contract/matrix")
        .canonicalize()
        .expect(
            "server/contract/matrix must exist — cut it with \
             `py matrix/contract/publish.py --out server/contract/matrix`",
        )
}

/// Every file under `dir` except the lock itself, as posix paths relative to
/// `dir`; `__pycache__` (a developer running the contract's gate in place) is
/// not contract content.
fn files_under(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).expect("read contract dir") {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if path.is_dir() {
            if name != "__pycache__" {
                files_under(root, &path, out);
            }
        } else if name != "contract.lock" {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel);
        }
    }
}

/// The vendored copy verifies against its own lock, with no sibling tree.
///
/// `contract.lock` is written by `matrix/contract/publish.py` when the copy is
/// cut: a sha256 per file over the bytes it wrote (UTF-8, LF, no BOM — which
/// is why `server/contract/**` is pinned `eol=lf` in `.gitattributes`) and a
/// digest over the sorted list. This is the rule `publish.py --verify` applies,
/// in the language this tree already tests in, so that a Server checkout with
/// no `matrix/` beside it still proves its copy is exactly what the publisher
/// cut: every listed file present and unchanged, nothing unlisted, the digest
/// and the contract version agreeing with the lock.
#[test]
fn the_vendored_copy_matches_its_lock() {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    let dir = contract_dir();
    let lock = read_json(&dir.join("contract.lock"));
    let listed: BTreeMap<String, String> = lock["files"]
        .as_object()
        .expect("contract.lock carries a `files` map")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().expect("sha256 hex").to_string()))
        .collect();
    assert!(!listed.is_empty(), "contract.lock lists no files");

    let mut on_disk = BTreeSet::new();
    files_under(&dir, &dir, &mut on_disk);
    for (rel, expected) in &listed {
        let bytes = std::fs::read(dir.join(rel))
            .unwrap_or_else(|e| panic!("{rel} is listed in contract.lock but missing: {e}"));
        let actual = hex::encode(Sha256::digest(&bytes));
        assert_eq!(
            expected, &actual,
            "{rel} differs from contract.lock — the vendored copy was edited in place; \
             re-cut it: rm -r server/contract/matrix && py matrix/contract/publish.py \
             --out server/contract/matrix"
        );
        on_disk.remove(rel);
    }
    assert!(
        on_disk.is_empty(),
        "files under server/contract/matrix that contract.lock does not list: {on_disk:?}"
    );

    let mut listing = String::new();
    for (rel, h) in &listed {
        listing.push_str(&format!("{h}  {rel}\n"));
    }
    assert_eq!(
        lock["bundle_digest"].as_str().expect("bundle_digest"),
        hex::encode(Sha256::digest(listing.as_bytes())),
        "contract.lock's bundle_digest does not match its own file list"
    );
    let version = std::fs::read_to_string(dir.join("VERSION")).expect("VERSION");
    assert_eq!(
        lock["contract_version"].as_str().expect("contract_version"),
        version.trim(),
        "contract.lock's contract_version disagrees with VERSION"
    );
}

/// Read as bytes and decode UTF-8 explicitly, exactly as the contract README
/// requires. `read_to_string` would accept a BOM as content and produce a
/// parse error three layers away from the cause.
fn read_json(path: &Path) -> Value {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let text =
        String::from_utf8(bytes).unwrap_or_else(|e| panic!("{} is not UTF-8: {e}", path.display()));
    assert!(
        !text.starts_with('\u{feff}'),
        "{} begins with a UTF-8 BOM; the vendored copy must be byte-identical to \
         matrix/contract, and a BOM is how a console redirect silently breaks that",
        path.display()
    );
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()))
}

fn schema_files() -> Vec<(String, Value)> {
    let dir = contract_dir();
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("read contract dir") {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name.ends_with(".schema.json") {
            out.push((name, read_json(&path)));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!out.is_empty(), "no schemas found in {}", dir.display());
    out
}

/// Build a validator with every sibling schema registered under BOTH its bare
/// file name and its `$id`.
///
/// This is the load-bearing part. `evidence-block.schema.json` refers to
/// `refusal.schema.json` by relative file name, and because the schema
/// declares an `$id` of `https://munarium.ioka.io/...`, that relative
/// reference resolves to an **absolute https URI**. Without the registration
/// below, the validator would try to fetch it over the network — a test that
/// passes only on a machine with internet access, and one that would validate
/// against whatever that URL served rather than against the bytes in this
/// repository. Registering both forms keeps resolution entirely local.
fn validator_for(schema: &Value, all: &[(String, Value)]) -> jsonschema::Validator {
    let mut resources: Vec<(String, Resource)> = Vec::new();
    for (name, doc) in all {
        resources.push((
            name.clone(),
            Resource::from_contents(doc.clone()).expect("resource from schema"),
        ));
        if let Some(id) = doc.get("$id").and_then(Value::as_str) {
            resources.push((
                id.to_string(),
                Resource::from_contents(doc.clone()).expect("resource from schema"),
            ));
        }
    }
    jsonschema::options()
        .with_resources(resources.into_iter())
        .build(schema)
        .expect("schema compiles")
}

#[test]
fn every_schema_is_a_valid_json_schema() {
    let all = schema_files();
    for (name, doc) in &all {
        // Compiling IS the validity check: a malformed schema fails to build.
        let _ = validator_for(doc, &all);
        assert_eq!(
            doc.get("$schema").and_then(Value::as_str),
            Some("https://json-schema.org/draft/2020-12/schema"),
            "{name} must declare draft 2020-12 explicitly; the contract's compatibility \
             rule is written against that dialect"
        );
    }
}

#[test]
fn every_example_validates_against_its_schema() {
    let all = schema_files();
    let dir = contract_dir();
    for (example, schema_name) in PAIRS {
        let schema = all
            .iter()
            .find(|(n, _)| n == schema_name)
            .unwrap_or_else(|| panic!("{schema_name} is missing from the vendored contract"));
        let validator = validator_for(&schema.1, &all);
        let instance = read_json(&dir.join("examples").join(example));
        let errors: Vec<String> = validator
            .iter_errors(&instance)
            .map(|e| format!("  at /{}: {e}", e.instance_path))
            .collect();
        assert!(
            errors.is_empty(),
            "{example} does not validate against {schema_name}:\n{}",
            errors.join("\n")
        );
    }
}

#[test]
fn every_example_is_paired() {
    let dir = contract_dir().join("examples");
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .expect("read examples dir")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .filter(|n| n.ends_with(".json"))
        .collect();
    let paired: BTreeSet<String> = PAIRS.iter().map(|(e, _)| (*e).to_string()).collect();

    let unpaired: Vec<&String> = on_disk.difference(&paired).collect();
    assert!(
        unpaired.is_empty(),
        "examples with no schema pairing (add them to PAIRS here and in \
         matrix/contract/validate_examples.py): {unpaired:?}"
    );
    let missing: Vec<&String> = paired.difference(&on_disk).collect();
    assert!(
        missing.is_empty(),
        "PAIRS names examples that do not exist: {missing:?}"
    );
}

#[test]
fn the_contract_declares_a_semver_version() {
    let raw = std::fs::read(contract_dir().join("VERSION")).expect("read VERSION");
    let version = String::from_utf8(raw).expect("VERSION is UTF-8");
    let version = version.trim();
    let parts: Vec<&str> = version.split('.').collect();
    assert_eq!(
        parts.len(),
        3,
        "VERSION must be bare semver with no `v` prefix, got {version:?}"
    );
    for p in parts {
        assert!(
            !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()),
            "VERSION component {p:?} is not numeric in {version:?}"
        );
    }
}
