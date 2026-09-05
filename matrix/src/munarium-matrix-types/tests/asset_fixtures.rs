// SPDX-License-Identifier: Apache-2.0
//! The asset fixture trees are the validator's real test suite.
//!
//! `fixtures/assets/valid/` must validate clean. `fixtures/assets/invalid/`
//! holds one file per fail-closed rule, and **the file name is the finding
//! code it must produce** — so a case that stops firing is a rename away from
//! obvious, not a silent green.
//!
//! The last test closes the loop in the other direction: it scans the
//! validator's own source for every code it can emit and fails if one has no
//! fixture. Adding a rule without a case is therefore a build failure, and the
//! exemption list makes the few unreachable-from-YAML codes explicit rather
//! than merely absent.

use std::path::{Path, PathBuf};

use munarium_matrix_types::{parse_asset, validate::is_error};

fn fixtures(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/assets")
        .join(sub)
}

fn yaml_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .collect();
    v.sort();
    assert!(!v.is_empty(), "no fixtures in {}", dir.display());
    v
}

#[test]
fn every_valid_fixture_parses_and_validates_clean() {
    for path in yaml_files(&fixtures("valid")) {
        let text = std::fs::read_to_string(&path).unwrap();
        let asset =
            parse_asset(&text).unwrap_or_else(|e| panic!("{}: did not parse: {e}", path.display()));
        let errors: Vec<_> = asset.validate().into_iter().filter(is_error).collect();
        assert!(
            errors.is_empty(),
            "{} should be valid, got {errors:#?}",
            path.display()
        );
    }
}

#[test]
fn every_invalid_fixture_produces_the_code_its_filename_names() {
    for path in yaml_files(&fixtures("invalid")) {
        let expected = path.file_stem().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&path).unwrap();

        // Every invalid fixture must reach the VALIDATOR. If serde rejects it
        // first the fixture is testing serde, not the rule its name claims —
        // and the rule would then be untested while the test stayed green.
        let asset = parse_asset(&text).unwrap_or_else(|e| {
            panic!(
                "{} must parse and then fail validation, but serde rejected it                  first: {e}. Fix the fixture so it exercises '{expected}'.",
                path.display()
            )
        });

        let findings = asset.validate();
        let codes: Vec<&str> = findings.iter().map(|f| f.code.as_str()).collect();
        assert!(
            codes.contains(&expected.as_str()),
            "{} must produce '{expected}', produced {codes:?}",
            path.display()
        );
        assert!(
            findings.iter().any(is_error),
            "{} produced '{expected}' but only as an advisory",
            path.display()
        );
    }
}

/// Codes that get no invalid fixture, each with the reason. Anything not
/// listed here needs one.
const NO_FIXTURE: &[(&str, &str)] = &[
    (
        "envelope.kind",
        "parse_asset dispatches ON kind, so a mismatched kind never reaches a typed validator",
    ),
    (
        "authorization.classes-ignored",
        "advisory: classes declared under source_native are inert, not invalid",
    ),
    (
        "mapping.authority-inert",
        "advisory: `authority` under shadow mode is inert, not invalid",
    ),
    (
        "limits.above-inline-seal",
        "advisory: a bigger artifact costs an extra round trip, it is not invalid",
    ),
];

fn codes_emitted_by_the_validator() -> Vec<String> {
    // Scan the validator's own text. `Finding::new(` is followed by the code as
    // the first argument, on the same line or the next one after rustfmt.
    const SRC: &str = include_str!("../src/validate.rs");
    let mut out = Vec::new();
    let mut rest = SRC;
    while let Some(i) = rest.find("Finding::new(") {
        rest = &rest[i + "Finding::new(".len()..];
        let Some(open) = rest.find('"') else { break };
        // Only accept a literal that starts within the argument list, not one
        // several lines away (which would mean the code was a variable).
        if rest[..open].contains(';') {
            continue;
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else { break };
        let code = &after[..close];
        if code.contains('.') && !code.contains(' ') {
            out.push(code.to_string());
        }
        rest = after;
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn every_rule_the_validator_can_emit_has_a_fixture_or_a_stated_reason() {
    let have: Vec<String> = yaml_files(&fixtures("invalid"))
        .iter()
        .map(|p| p.file_stem().unwrap().to_string_lossy().to_string())
        .collect();
    let exempt: Vec<&str> = NO_FIXTURE.iter().map(|(c, _)| *c).collect();

    let emitted = codes_emitted_by_the_validator();
    assert!(
        emitted.len() > 20,
        "the source scan found only {} codes — it has stopped working, \
         which would make this test vacuously green",
        emitted.len()
    );

    let missing: Vec<&String> = emitted
        .iter()
        .filter(|c| !have.contains(c) && !exempt.contains(&c.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "these validator rules have no invalid fixture: {missing:#?}\n\
         add matrix/fixtures/assets/invalid/<code>.yaml, or list the code in \
         NOT_REACHABLE_FROM_YAML with a reason"
    );

    // And the reverse: a fixture naming a code no rule emits is a stale test.
    let stale: Vec<&String> = have.iter().filter(|c| !emitted.contains(c)).collect();
    assert!(
        stale.is_empty(),
        "these fixtures name codes the validator no longer emits: {stale:#?}"
    );
}
