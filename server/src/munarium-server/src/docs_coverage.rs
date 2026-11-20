// SPDX-License-Identifier: Apache-2.0
//! Documentation coverage gates (2026-09-02).
//!
//! Two API documents are contracts the code can check: `docs/api/rest.md`
//! (the human route map, which claims every route) and `docs/api/errors.md`
//! (the problem-slug registry). A route or slug that ships without a row is
//! exactly the drift the 2026-09-02 audit found — thirteen datastore-plane
//! routes and one slug had gone undocumented for three days while both docs
//! still read as complete — so these tests fail `cargo test` instead. The
//! developers guide's route index (Appendix F) is held to the same rule for
//! paths, which is what keeps the book complete for the API by construction.
//!
//! Matching is by path SHAPE: a documented `/v1/versions/{id}/claims` covers
//! the contract's `/v1/versions/{version_id}/claims`. A parameter segment
//! matches any non-empty segment, so docs may spell it `{x}`, `:x`, `<x>`,
//! `$X` or a concrete example value; literal segments must match exactly.
//! Shorthand like `.../answers` does not count — spell the route.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn docs_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs")
    }

    fn read(rel: &str) -> String {
        let p = docs_root().join(rel);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    /// Every path template in the served OpenAPI document.
    fn contract_paths() -> Vec<String> {
        crate::openapi::doc().paths.paths.keys().cloned().collect()
    }

    fn is_param(seg: &str) -> bool {
        seg.starts_with('{') && seg.ends_with('}')
    }

    /// Characters that end a route token in prose or a Markdown table.
    fn ends_token(c: char) -> bool {
        c.is_whitespace() || "`()|,\"'·?#\\[]*;".contains(c)
    }

    /// Does `text` mention a path of `template`'s shape?
    fn mentions(text: &str, template: &str) -> bool {
        let tsegs: Vec<&str> = template.trim_start_matches('/').split('/').collect();
        for (i, _) in text.match_indices('/') {
            if i > 0 && text[..i].ends_with('/') {
                continue; // the second slash of `//`
            }
            let rest = &text[i + 1..];
            let end = rest.find(ends_token).unwrap_or(rest.len());
            let cand = &rest[..end];
            let csegs: Vec<&str> = cand.split('/').collect();
            if csegs.len() != tsegs.len() {
                continue;
            }
            let ok = tsegs
                .iter()
                .zip(&csegs)
                .all(|(t, c)| if is_param(t) { !c.is_empty() } else { t == c });
            if ok {
                return true;
            }
        }
        false
    }

    fn missing_from(text: &str) -> Vec<String> {
        contract_paths()
            .into_iter()
            .filter(|p| !mentions(text, p))
            .collect()
    }

    #[test]
    fn the_shape_matcher_reads_docs_the_way_people_write_them() {
        let doc = "`POST/GET /v1/collections` · `GET /v1/collections/{id}` and \
                   `GET /v1/versions/{version_id}/claims?limit=5` (see /v1/runbooks/<name>/sessions); \
                   also `PUT .../answers` is shorthand.";
        assert!(mentions(doc, "/v1/collections"));
        assert!(mentions(doc, "/v1/collections/{collection_id}"));
        assert!(mentions(doc, "/v1/versions/{version_id}/claims"));
        assert!(mentions(doc, "/v1/runbooks/{name}/sessions"));
        assert!(!mentions(doc, "/v1/collections/{id}/activate-index"));
        assert!(!mentions(doc, "/v1/authoring/drafts/{draft_id}/answers"));
        // A parameter never matches an empty segment, and literals are exact.
        assert!(!mentions(
            "/v1/versions//claims",
            "/v1/versions/{id}/claims"
        ));
        assert!(!mentions(
            "/v1/version/{id}/claims",
            "/v1/versions/{id}/claims"
        ));
    }

    #[test]
    fn every_openapi_path_is_in_the_rest_route_map() {
        let missing = missing_from(&read("api/rest.md"));
        assert!(
            missing.is_empty(),
            "docs/api/rest.md lacks {} route(s) the server serves — add a row (spell the \
             full path; shorthand does not count):\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    #[test]
    fn every_openapi_path_is_in_the_guides_route_index() {
        let guide = read("guides/dev-guide.md");
        let idx = guide
            .find("### Appendix F: Route index")
            .expect("docs/guides/dev-guide.md must carry 'Appendix F: Route index'");
        let missing = missing_from(&guide[idx..]);
        assert!(
            missing.is_empty(),
            "the developers guide's Appendix F route index lacks {} route(s):\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    /// Every `.md` under a directory, recursively, sorted.
    fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                markdown_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }

    /// The relative-path targets of every `[text](target)` link outside fenced
    /// code. Schemes (`http://`, `https://`, `mailto:`) and anchor-only links
    /// are skipped; a `#fragment` is dropped; `%20` is decoded.
    fn relative_link_targets(text: &str) -> Vec<String> {
        let mut kept = String::with_capacity(text.len());
        let mut in_fence = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if !in_fence {
                kept.push_str(line);
                kept.push('\n');
            }
        }
        let mut out = Vec::new();
        let mut rest = kept.as_str();
        while let Some(i) = rest.find("](") {
            rest = &rest[i + 2..];
            let Some(end) = rest.find(')') else { break };
            let raw = rest[..end]
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>');
            rest = &rest[end + 1..];
            let target = raw.split_whitespace().next().unwrap_or("");
            if target.is_empty()
                || target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
                || target.starts_with('#')
            {
                continue;
            }
            let path = target.split('#').next().unwrap_or("").replace("%20", " ");
            if !path.is_empty() {
                out.push(path);
            }
        }
        out
    }

    /// Every relative link under docs/ names a file or directory that exists.
    ///
    /// A link check found twenty-one broken relative links across the docs
    /// before this test existed — source files that had moved, relative depths
    /// written for a different folder. Every relative link under docs/ must
    /// resolve inside this repository.
    #[test]
    fn every_relative_link_under_docs_resolves() {
        let mut files = Vec::new();
        markdown_files(&docs_root(), &mut files);
        files.sort();
        assert!(
            files.len() > 10,
            "expected the docs tree under {}",
            docs_root().display()
        );
        let mut broken = Vec::new();
        for file in &files {
            let bytes = std::fs::read(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            let text = String::from_utf8_lossy(&bytes);
            let dir = file.parent().expect("a file has a parent");
            for target in relative_link_targets(&text) {
                if !dir.join(&target).exists() {
                    let rel = file.strip_prefix(docs_root()).unwrap_or(file);
                    broken.push(format!("{} -> {target}", rel.display()));
                }
            }
        }
        assert!(
            broken.is_empty(),
            "{} broken relative link(s) under docs/ — fix the link or the file it names:\n  {}",
            broken.len(),
            broken.join("\n  ")
        );
    }

    /// ClearView U11: every documentation link the operator console prints
    /// names a file that exists under docs/, every console page links at
    /// least one, and the links point at this repository's docs tree.
    #[test]
    fn every_console_page_links_documentation_that_exists() {
        assert!(
            crate::dashboard::DOCS_BASE.ends_with("/server/docs/"),
            "{}",
            crate::dashboard::DOCS_BASE
        );
        let mut missing = Vec::new();
        for group in crate::dashboard::NAV {
            for page in group.pages {
                assert!(
                    !page.docs.is_empty(),
                    "{} links no documentation",
                    page.href
                );
                for doc in page.docs {
                    assert!(
                        !doc.starts_with('/') && !doc.contains(".."),
                        "{}: {doc} must be a path under docs/",
                        page.href
                    );
                    if !docs_root().join(doc).is_file() {
                        missing.push(format!("{} -> {doc}", page.href));
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "console documentation links to missing files:\n  {}",
            missing.join("\n  ")
        );
    }

    #[test]
    fn the_link_scanner_reads_links_the_way_people_write_them() {
        let text = "see [a](x.md), [b](../y/z.md#frag), [c](https://x.example/), \
                    [d](<spaced%20name.md>), [e](#only-anchor), [f](mailto:x@y)\n\
                    ```\n[fenced](nope.md)\n```\n[g](dir/)";
        assert_eq!(
            relative_link_targets(text),
            vec!["x.md", "../y/z.md", "spaced name.md", "dir/"]
        );
    }

    /// Every problem slug the server can emit, read from the source that
    /// defines them: the `slug()` match arms in error.rs and every
    /// `CustomError { slug: "…" }` literal across the crate.
    fn slugs_in_source() -> BTreeSet<String> {
        fn is_slug(s: &str) -> bool {
            s.contains('-')
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }
        fn scan(dir: &Path, out: &mut BTreeSet<String>) {
            for entry in std::fs::read_dir(dir).expect("src dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    scan(&path, out);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).expect("read source");
                let patterns: &[&str] = if path.ends_with("error.rs") {
                    &["slug: \"", "=> \""]
                } else {
                    &["slug: \""]
                };
                for pat in patterns {
                    for (i, _) in src.match_indices(pat) {
                        let rest = &src[i + pat.len()..];
                        if let Some(end) = rest.find('"') {
                            let s = &rest[..end];
                            if is_slug(s) {
                                out.insert(s.to_string());
                            }
                        }
                    }
                }
            }
        }
        let mut out = BTreeSet::new();
        scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
        out
    }

    /// The anchors a GitHub rendering of `text` offers: every explicit
    /// `<a id="…">` and every heading's generated id (lowercased, punctuation
    /// other than `-` and `_` dropped, spaces as hyphens).
    pub(crate) fn anchors_in(text: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for (i, _) in text.match_indices("<a id=\"") {
            let rest = &text[i + 7..];
            if let Some(end) = rest.find('"') {
                out.insert(rest[..end].to_string());
            }
        }
        let mut in_fence = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence || !line.starts_with('#') {
                continue;
            }
            let heading = line.trim_start_matches('#').trim();
            let id: String = heading
                .to_lowercase()
                .chars()
                .filter_map(|c| match c {
                    ' ' => Some('-'),
                    c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
                    _ => None,
                })
                .collect();
            out.insert(id);
        }
        out
    }

    #[test]
    fn the_anchor_reader_reads_headings_the_way_github_does() {
        let a = anchors_in(
            "# Title\n## 4. Define a shape and a runbook\n### Common first-run problems\n\
             ```\n## not a heading\n```\n| `x`<a id=\"problem-x\"></a> |",
        );
        for id in [
            "title",
            "4-define-a-shape-and-a-runbook",
            "common-first-run-problems",
            "problem-x",
        ] {
            assert!(a.contains(id), "{id}: {a:?}");
        }
        assert!(!a.contains("not-a-heading"), "{a:?}");
    }

    /// ClearView U11: every problem slug the console can link has its
    /// `#problem-<slug>` anchor in the registry. The console's slug comes
    /// from `error::problem_slug`, whose range is every source slug (the
    /// scan below requires a hyphen, so the one-word kernel slugs are added
    /// from `error::slug`) plus the `overloaded` notice.
    #[test]
    fn every_problem_slug_has_a_registry_anchor() {
        use munarium_core::KernelError;
        let anchors = anchors_in(&read(crate::dashboard::ERRORS_DOC));
        let mut slugs = slugs_in_source();
        for e in [
            KernelError::HeadConflict {
                expected: 0,
                actual: 1,
            },
            KernelError::PolicyRejection { findings: vec![] },
            KernelError::ShapeViolation {
                shape_ref: "s@1".into(),
                detail: String::new(),
            },
            KernelError::IdempotencyMismatch,
            KernelError::NotFound {
                kind: "k",
                id: "i".into(),
            },
            KernelError::InvalidInput(String::new()),
            KernelError::Unauthenticated(String::new()),
            KernelError::Forbidden(String::new()),
            KernelError::RateLimited(String::new()),
            KernelError::Storage(String::new()),
            KernelError::DatastoreUnavailable(String::new()),
            KernelError::Provider(String::new()),
        ] {
            slugs.insert(crate::error::slug(&e).to_string());
            slugs.insert(crate::error::problem_slug(&e).to_string());
        }
        slugs.insert("overloaded".into());
        let missing: Vec<String> = slugs
            .iter()
            .filter(|s| !anchors.contains(&format!("problem-{s}")))
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "docs/api/errors.md lacks the #problem-<slug> anchor for: {missing:?}"
        );
        // The console's link names exactly that anchor.
        let link = crate::dashboard::problem_link("not-found");
        assert!(
            link.contains(&format!(
                "{}{}#problem-not-found\"",
                crate::dashboard::DOCS_BASE,
                crate::dashboard::ERRORS_DOC
            )),
            "{link}"
        );
    }

    /// ClearView U11: every build-job failure code has its
    /// `#job-failure-<code>` anchor, which the build-job page links to.
    #[test]
    fn every_build_job_failure_code_has_a_registry_anchor() {
        use munarium_store_pg::job_failure::JobFailureCode;
        let anchors = anchors_in(&read(crate::dashboard::ERRORS_DOC));
        for code in JobFailureCode::ALL {
            let id = format!("job-failure-{}", code.as_str());
            assert!(anchors.contains(&id), "errors.md lacks #{id}");
            assert!(
                crate::dashboard::job_failure_link(code.as_str()).contains(&format!("#{id}\"")),
                "{id}"
            );
        }
    }

    /// ClearView U11: the empty-deployment landing's three tasks each link a
    /// document that exists, at an anchor it has.
    #[test]
    fn every_first_task_links_a_guide_and_anchor_that_exist() {
        assert_eq!(crate::dashboard::FIRST_TASKS.len(), 3);
        for task in crate::dashboard::FIRST_TASKS {
            let (path, anchor) = match task.doc.split_once('#') {
                Some((p, a)) => (p, Some(a)),
                None => (task.doc, None),
            };
            assert!(
                !path.starts_with('/') && !path.contains(".."),
                "{}: {path} must be a path under docs/",
                task.title
            );
            assert!(
                docs_root().join(path).is_file(),
                "{}: docs/{path} does not exist",
                task.title
            );
            if let Some(anchor) = anchor {
                assert!(
                    anchors_in(&read(path)).contains(anchor),
                    "{}: docs/{path} has no #{anchor}",
                    task.title
                );
            }
        }
    }

    #[test]
    fn every_problem_slug_is_in_the_error_registry() {
        let slugs = slugs_in_source();
        assert!(
            slugs.len() >= 30,
            "the slug scan found only {} slugs; the scanner is broken, not the docs",
            slugs.len()
        );
        let registry = read("api/errors.md");
        let missing: Vec<&String> = slugs
            .iter()
            .filter(|s| !registry.contains(&format!("`{s}`")))
            .collect();
        assert!(
            missing.is_empty(),
            "docs/api/errors.md lacks {} slug(s) the server emits: {missing:?}",
            missing.len()
        );
    }

    /// ClearView C2: the build-job failure codes the builder can record
    /// (`JobFailureCode::ALL`, the typed mapping's whole range) are exactly
    /// the codes the registry's "Build-job failure codes" table lists, each
    /// with its recorded message.
    #[test]
    fn build_job_failure_codes_match_the_error_registry() {
        use munarium_store_pg::job_failure::JobFailureCode;
        let registry = read("api/errors.md");
        let section = registry
            .split(
                "
## Build-job failure codes",
            )
            .nth(1)
            .expect("errors.md lacks the Build-job failure codes section");
        let section = section
            .split(
                "
## ",
            )
            .next()
            .unwrap();
        let rows: Vec<&str> = section.lines().filter(|l| l.starts_with("| `")).collect();
        let listed: BTreeSet<&str> = rows
            .iter()
            .filter_map(|l| l[3..].split('`').next())
            .collect();
        let typed: BTreeSet<&str> = JobFailureCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(listed, typed, "registry and typed codes differ");
        assert_eq!(rows.len(), typed.len(), "a code is listed twice");
        for code in JobFailureCode::ALL {
            let row = rows
                .iter()
                .find(|l| l.starts_with(&format!("| `{}`", code.as_str())))
                .unwrap();
            assert!(
                row.trim_end().ends_with(&format!("| {} |", code.message())),
                "the registry row for {} does not show its message",
                code.as_str()
            );
        }
    }
}
