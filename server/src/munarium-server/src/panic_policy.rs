// SPDX-License-Identifier: Apache-2.0
//! Panic-policy completeness gate (P15/R32).
//!
//! Every production crate root in the Server workspace denies the panicking
//! shortcuts — `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!`,
//! `unimplemented!` — outside test code, with one crate-root attribute:
//!
//! ```text
//! #![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used,
//!     clippy::panic, clippy::unreachable, clippy::todo, clippy::unimplemented))]
//! ```
//!
//! Clippy enforces the attribute where it exists; this test makes sure it
//! exists. A new member crate, a new binary, or an attribute deleted in a
//! refactor fails `cargo test` here instead of quietly re-opening the crate
//! to panics. The policy and its exemptions: `docs/panic-boundaries.md`.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    const LINTS: [&str; 6] = [
        "clippy::unwrap_used",
        "clippy::expect_used",
        "clippy::panic",
        "clippy::unreachable",
        "clippy::todo",
        "clippy::unimplemented",
    ];

    fn server_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The quoted paths in the workspace's `members = [ ... ]` list.
    fn members(manifest: &str) -> Vec<String> {
        let start = manifest.find("members = [").expect("a members list");
        let list = &manifest[start..];
        let list = &list[..list.find(']').expect("a closed members list")];
        list.split('"')
            .skip(1)
            .step_by(2)
            .map(String::from)
            .collect()
    }

    /// `path = "..."` values declared under `[[bin]]` tables.
    fn declared_bins(manifest: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut in_bin = false;
        for line in manifest.lines().map(str::trim) {
            if line.starts_with('[') {
                in_bin = line == "[[bin]]";
            } else if in_bin {
                if let Some(value) = line.strip_prefix("path") {
                    let value = value.trim_start().trim_start_matches('=').trim();
                    out.push(value.trim_matches('"').to_string());
                }
            }
        }
        out
    }

    /// Every crate root a member builds: `src/lib.rs`, `src/main.rs`,
    /// `src/bin/*.rs` and any `[[bin]] path`.
    fn crate_roots(member: &Path) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for conventional in ["src/lib.rs", "src/main.rs"] {
            let p = member.join(conventional);
            if p.is_file() {
                roots.push(p);
            }
        }
        if let Ok(entries) = std::fs::read_dir(member.join("src/bin")) {
            for e in entries.flatten() {
                if e.path().extension().is_some_and(|x| x == "rs") {
                    roots.push(e.path());
                }
            }
        }
        for bin in declared_bins(&read(&member.join("Cargo.toml"))) {
            roots.push(member.join(bin));
        }
        roots
    }

    /// True when `source` carries the policy attribute: a crate-level
    /// `cfg_attr(not(test), deny(...))` naming all six lints. Whitespace is
    /// ignored so rustfmt's layout does not matter.
    fn declares_policy(source: &str) -> bool {
        let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
        let Some(start) = compact.find("#![cfg_attr(not(test),deny(") else {
            return false;
        };
        let attr = &compact[start..];
        let attr = &attr[..attr.find(")]").map_or(attr.len(), |end| end + 2)];
        LINTS.iter().all(|lint| attr.contains(lint))
    }

    #[test]
    fn every_production_crate_root_denies_panicking_shortcuts() {
        let server = server_root();
        let mut roots: Vec<PathBuf> = members(&read(&server.join("Cargo.toml")))
            .iter()
            .flat_map(|m| crate_roots(&server.join(m)))
            .map(|p| p.canonicalize().unwrap_or(p))
            .collect();
        roots.sort();
        roots.dedup();
        // 18 library crates, the conformance library, and four binaries
        // (server, mmctl, mmp-conformance, gen-grpc-docs). A smaller count
        // means the discovery broke, not that the policy holds.
        assert!(
            roots.len() >= 23,
            "found only {} crate roots: {roots:#?}",
            roots.len()
        );
        let missing: Vec<&PathBuf> = roots
            .iter()
            .filter(|root| !declares_policy(&read(root)))
            .collect();
        assert!(
            missing.is_empty(),
            "crate roots without the P15 panic policy (see docs/panic-boundaries.md): {missing:#?}"
        );
    }

    #[test]
    fn the_matcher_accepts_the_policy_and_rejects_near_misses() {
        let policy = "#![cfg_attr(\n    not(test),\n    deny(\n        clippy::unwrap_used,\n        \
                      clippy::expect_used,\n        clippy::panic,\n        clippy::unreachable,\n        \
                      clippy::todo,\n        clippy::unimplemented\n    )\n)]\n";
        assert!(declares_policy(policy));
        assert!(declares_policy(&format!("//! doc\n\n{policy}\nmod a;")));
        // A lint missing, the test exemption dropped, or the attribute
        // demoted to an item attribute are all refusals.
        assert!(!declares_policy(
            &policy.replace("clippy::expect_used,", "")
        ));
        assert!(!declares_policy(&policy.replace("not(test)", "test")));
        assert!(!declares_policy(&policy.replace("#![", "#[")));
        assert!(!declares_policy("#![deny(clippy::unwrap_used)]"));
        assert_eq!(
            members("members = [\n    \"src/a\",\n    \"conformance\",\n]\n"),
            ["src/a", "conformance"]
        );
        assert_eq!(
            declared_bins(
                "[package]\nname = \"x\"\n\n[[bin]]\nname = \"x\"\npath = \"src/main.rs\"\n"
            ),
            ["src/main.rs"]
        );
    }
}
