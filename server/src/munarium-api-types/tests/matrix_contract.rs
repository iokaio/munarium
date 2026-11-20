// SPDX-License-Identifier: Apache-2.0
//! The vendored Munarium Matrix contract, checked from the server tree.
//!
//! `server/contract/matrix/` is a vendored cut of `iokaio/munarium-matrix`'s
//! `contract/`, written by its `contract/publish.py` together with a `contract.lock` (see
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
//! boundary into a Matrix checkout — because a test that reached into the other repository
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
             `py ../munarium-matrix/contract/publish.py --out server/contract/matrix`",
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
/// `contract.lock` is written by Matrix's `contract/publish.py` when the copy is
/// cut: a sha256 per file over the bytes it wrote (UTF-8, LF, no BOM — which
/// is why `server/contract/**` is pinned `eol=lf` in `.gitattributes`) and a
/// digest over the sorted list. This is the rule `publish.py --verify` applies,
/// in the language this tree already tests in, so that a Server checkout with
/// no Matrix checkout beside it still proves its copy is exactly what the publisher
/// cut: every listed file present and unchanged, nothing unlisted, the digest
/// and the contract version agreeing with the lock.
#[test]
fn the_vendored_copy_matches_its_lock() {
    let problems = lock_problems(&contract_dir());
    assert!(
        problems.is_empty(),
        "the vendored copy does not match contract.lock; it was edited in place, so \
         re-cut it: rm -r server/contract/matrix && py ../munarium-matrix/contract/publish.py \
         --out server/contract/matrix\n{}",
        problems.join("\n")
    );
}

/// Every way `dir` differs from its own `contract.lock`: the rule
/// `publish.py --verify` applies. Empty means the copy is what the publisher cut.
fn lock_problems(dir: &Path) -> Vec<String> {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    let mut problems = Vec::new();
    let lock = read_json(&dir.join("contract.lock"));
    let listed: BTreeMap<String, String> = lock["files"]
        .as_object()
        .expect("contract.lock carries a `files` map")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().expect("sha256 hex").to_string()))
        .collect();
    if listed.is_empty() {
        problems.push("contract.lock lists no files".to_string());
    }

    let mut on_disk = BTreeSet::new();
    files_under(dir, dir, &mut on_disk);
    for (rel, expected) in &listed {
        match std::fs::read(dir.join(rel)) {
            Err(e) => problems.push(format!("MISSING {rel}: {e}")),
            Ok(bytes) => {
                if expected != &hex::encode(Sha256::digest(&bytes)) {
                    problems.push(format!("MODIFIED {rel}"));
                }
            }
        }
        on_disk.remove(rel);
    }
    for rel in on_disk {
        problems.push(format!("UNLISTED {rel}"));
    }

    let mut listing = String::new();
    for (rel, h) in &listed {
        listing.push_str(&format!("{h}  {rel}\n"));
    }
    if lock["bundle_digest"].as_str()
        != Some(hex::encode(Sha256::digest(listing.as_bytes())).as_str())
    {
        problems.push("bundle_digest does not match the file list".to_string());
    }
    match std::fs::read_to_string(dir.join("VERSION")) {
        Ok(version) if lock["contract_version"].as_str() == Some(version.trim()) => {}
        _ => problems.push("contract_version disagrees with VERSION".to_string()),
    }
    problems
}

/// A scratch copy of the vendored contract for the negative controls below.
struct ScratchCopy(PathBuf);

impl ScratchCopy {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "munarium-matrix-contract-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        copy_tree(&contract_dir(), &dir);
        ScratchCopy(dir)
    }
}

impl Drop for ScratchCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create scratch dir");
    for entry in std::fs::read_dir(from).expect("read contract dir") {
        let path = entry.expect("dir entry").path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copy contract file");
        }
    }
}

const VECTOR: &str = "examples/evidence-block.count.json";

#[test]
fn an_unmodified_copy_passes_the_lock_rule() {
    let copy = ScratchCopy::new("clean");
    assert_eq!(lock_problems(&copy.0), Vec::<String>::new());
}

#[test]
fn a_tampered_vector_is_refused() {
    let copy = ScratchCopy::new("tampered");
    let path = copy.0.join(VECTOR);
    let mut bytes = std::fs::read(&path).expect("read vector");
    bytes.push(b' ');
    std::fs::write(&path, bytes).expect("write vector");
    assert_eq!(lock_problems(&copy.0), vec![format!("MODIFIED {VECTOR}")]);
}

#[test]
fn a_missing_vector_is_refused() {
    let copy = ScratchCopy::new("missing");
    std::fs::remove_file(copy.0.join(VECTOR)).expect("remove vector");
    let problems = lock_problems(&copy.0);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with(&format!("MISSING {VECTOR}")),
        "{problems:?}"
    );
}

#[test]
fn an_unlisted_file_is_refused() {
    let copy = ScratchCopy::new("unlisted");
    std::fs::write(copy.0.join("examples/extra.json"), b"{}\n").expect("write extra");
    assert_eq!(
        lock_problems(&copy.0),
        vec!["UNLISTED examples/extra.json".to_string()]
    );
}

/// ClearView M1 step 3: the Server compiles against the generated pin of the
/// Matrix publisher's consumer export (`contract/matrix-consumer/`). Until the
/// vendored copy is removed in step 4, the two must name the same bundle, so
/// the pin is proven byte-identical to the v1.4 vendored cut: the lock test above
/// ties every vendored file to this digest.
#[test]
fn the_pinned_export_names_the_vendored_bundle() {
    let lock = read_json(&contract_dir().join("contract.lock"));
    let export = read_json(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../contract/matrix-consumer/matrix-consumer.json"),
    );
    let wire = &export["profile"]["wire_contract"];
    assert_eq!(wire["contract_version"], lock["contract_version"]);
    assert_eq!(wire["bundle_digest"], lock["bundle_digest"]);
    assert_eq!(lock["files"].as_object().map(|f| f.len()), Some(17));
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
         the Matrix contract, and a BOM is how a console redirect silently breaks that",
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
         iokaio/munarium-matrix's contract/validate_examples.py): {unpaired:?}"
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
