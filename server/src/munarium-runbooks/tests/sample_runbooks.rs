// SPDX-License-Identifier: Apache-2.0
//! The shipped sample runbooks under `runbooks/` must parse and validate.
//!
//! They are documentation people copy, so a broken one is worse than none —
//! and the `sources.prefix-mismatch` rule only earns its keep if something
//! actually runs it over the set.

use std::path::PathBuf;
#[test]
fn shipped_sample_runbooks_parse_and_validate() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runbooks");
    let mut fails = Vec::new();
    let mut n = 0;
    for dir in [root.join("applications"), root.join("pipelines")] {
        for e in std::fs::read_dir(&dir).expect("dir") {
            let p = e.expect("e").path();
            if p.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            let y = std::fs::read_to_string(&p).expect("read");
            match munarium_runbooks::parse_runbook(&y) {
                Err(e) => fails.push(format!("{p:?} PARSE {e}")),
                Ok(d) => {
                    let f = munarium_runbooks::validate::validate_runbook(&d);
                    for x in &f {
                        println!(
                            "{:?} [{:?}] {} — {}",
                            p.file_name().unwrap(),
                            x.severity,
                            x.code,
                            x.message
                        );
                    }
                    if !munarium_runbooks::validate::is_valid(&f) {
                        fails.push(format!("{p:?} INVALID"));
                    }
                    n += 1;
                }
            }
        }
    }
    assert!(fails.is_empty(), "{fails:#?}");
    // This tree ships thirteen experiment runbooks and two pipelines; the full
    // exemplar set is additionally pinned by munarium-authoring's embed test.
    assert_eq!(
        n, 15,
        "expected all fifteen shipped sample runbooks, found {n}"
    );
    println!("validated {n} runbooks");
}
