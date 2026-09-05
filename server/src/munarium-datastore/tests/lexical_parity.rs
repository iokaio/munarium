// SPDX-License-Identifier: Apache-2.0
//! Measure the Tantivy tokenizer against the PostgreSQL oracle.
//!
//! `tests/fixtures/lexical-compat/pg16-english.jsonl` (beside this test) records
//! what PostgreSQL's `english` configuration does to 46 strings, most of them
//! harvested from sample corpora. The recorded parity measurement predicted
//! where Tantivy would diverge. This test replaces those predictions with
//! measurements.
//!
//! **It asserts parity for the classes the tokenizer implements** (the stage 5
//! analyzer work: numbers, identifiers, hyphenation - 39 of 46 rows) and only
//! MEASURES the accepted-differences set (urls, hosts, emails, filesystem
//! paths - see lexical-parity.md). Before the classifying tokenizer existed,
//! asserting anything would have been red on day one; now that the classes are
//! claimed, a divergence in one is a regression, and the printed table remains
//! the answer to decision gate 2:
//! `cargo test -p munarium-datastore --test lexical_parity -- --nocapture`
//!
//! Two properties ARE asserted, because they would be defects rather than
//! differences: an input PostgreSQL reduces to nothing must not produce terms
//! here either, and the tokenizer must be deterministic.

#![cfg(feature = "lexical-tantivy")]

use std::collections::BTreeMap;
use std::path::PathBuf;

use munarium_datastore::lexical::{build, TantivyLexicalIndex};
use munarium_datastore::records::ChunkRecord;

fn oracle_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/lexical-compat/pg16-english.jsonl")
        .canonicalize()
        .expect("the lexical oracle should exist; it is committed beside this test")
}

struct Row {
    id: String,
    class: String,
    provenance: String,
    body: String,
    /// The lexemes PostgreSQL indexed, in the order the tsvector printed them.
    pg_lexemes: Vec<String>,
}

fn load() -> Vec<Row> {
    let text = std::fs::read_to_string(oracle_path()).unwrap();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            // to_tsvector prints `'lex':pos 'lex2':pos` -- take the quoted
            // lexemes, which is what a token stream is comparable to.
            let ts = v["tsvector"].as_str().unwrap_or_default();
            let mut lexemes = Vec::new();
            let mut rest = ts;
            while let Some(start) = rest.find('\'') {
                let after = &rest[start + 1..];
                let Some(end) = after.find('\'') else { break };
                lexemes.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            Row {
                id: v["id"].as_str().unwrap().to_string(),
                class: v["class"].as_str().unwrap().to_string(),
                provenance: v["provenance"].as_str().unwrap().to_string(),
                body: v["body"].as_str().unwrap().to_string(),
                pg_lexemes: lexemes,
            }
        })
        .collect()
}

fn analyzer() -> TantivyLexicalIndex {
    // One document is enough: the tokenizer is a property of the schema, not of
    // the corpus.
    let bytes = build(&[ChunkRecord {
        chunk_id: "probe".into(),
        source_id: "s".into(),
        source_path: "p".into(),
        node_id: None,
        ordinal: 0,
        text: "probe".into(),
        text_sha256: "0".repeat(64),
    }])
    .unwrap();
    TantivyLexicalIndex::open(&bytes).unwrap()
}

#[test]
fn measure_and_report_divergence_from_the_postgresql_oracle() {
    let rows = load();
    assert_eq!(rows.len(), 46, "the committed oracle holds 46 strings");
    let ix = analyzer();

    // A tsvector is a SORTED SET of lexemes; a token stream is a positional
    // SEQUENCE. Comparing them directly measures the representation rather than
    // the behaviour -- it reports "running runs ran" as a divergence purely
    // because one side deduplicates. Compare as sets, which is the question
    // actually being asked: does the same VOCABULARY reach the index?
    fn normalize(v: &[String]) -> Vec<String> {
        let mut out: Vec<String> = v.to_vec();
        out.sort();
        out.dedup();
        out
    }

    let mut agree = 0usize;
    let mut differ: Vec<(&Row, Vec<String>)> = Vec::new();
    for r in &rows {
        let ours = ix.analyze(&r.body).unwrap();
        if normalize(&ours) == normalize(&r.pg_lexemes) {
            agree += 1;
        } else {
            differ.push((r, ours));
        }
    }

    println!("\n=== Munarium (Tantivy en_stem) vs PostgreSQL english ===");
    println!("agree: {agree}/{}   differ: {}", rows.len(), differ.len());

    let mut by_class: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for r in &rows {
        by_class.entry(&r.class).or_default().0 += 1;
    }
    for (r, _) in &differ {
        by_class.entry(&r.class).or_default().1 += 1;
    }
    // The classes the stage 5 analyzer work IMPLEMENTED are asserted, not
    // merely printed: for these, a divergence stopped being a measurement and
    // became a regression the moment the tokenizer claimed them. The classes
    // NOT in this list are the accepted-differences set recorded in
    // lexical-parity.md (urls, hosts, emails, filesystem paths), which stay
    // measured-and-printed only.
    const IMPLEMENTED: &[&str] = &[
        "case",
        "cjk",
        "currency",
        "currency-trap",
        "cve",
        "defanged-host",
        "empty",
        "hyphenated",
        "hyphenated-num",
        "ipv4",
        "legal-cite",
        "md5-like",
        "patent-pubno",
        "patent-serial",
        "percent",
        "punct-heavy",
        "query",
        "scientific",
        "semver",
        "sha256",
        "signed",
        "stemming",
        "stop-mixed",
        "stop-only",
        "token-length",
        "unicode-accent",
        "whitespace",
    ];
    for (r, ours) in &differ {
        assert!(
            !IMPLEMENTED.contains(&r.class.as_str()),
            "class {:?} is implemented and must agree with the oracle; {} diverged: pg {:?} vs ours {:?}",
            r.class,
            r.id,
            normalize(&r.pg_lexemes),
            normalize(ours),
        );
    }

    println!("\n-- by token class (total / differing) --");
    for (class, (total, diff)) in &by_class {
        let flag = if *diff > 0 { "DIFFERS" } else { "agrees " };
        println!("  {flag}  {class:18} {diff}/{total}");
    }

    println!("\n-- divergences (harvested rows first: those our corpora really hit) --");
    let mut ordered: Vec<&(&Row, Vec<String>)> = differ.iter().collect();
    ordered.sort_by_key(|(r, _)| (r.provenance != "harvested", r.id.clone()));
    for (r, ours) in ordered {
        println!("  [{}] {} {:?}", r.provenance, r.id, r.body);
        println!("      pg : {:?}", normalize(&r.pg_lexemes));
        println!("      ours: {:?}", normalize(ours));
    }
    println!();

    // The tokenizer must produce SOMETHING for every input without panicking;
    // a class it cannot handle would show up here rather than at index time.
    for r in &rows {
        let _ = ix.analyze(&r.body).unwrap();
    }
}

/// A defect rather than a difference: if PostgreSQL reduces an input to no
/// lexemes at all (empty, whitespace, stop words only), producing terms here
/// would make an empty query match documents.
#[test]
fn inputs_postgresql_reduces_to_nothing_produce_no_terms_here_either() {
    let ix = analyzer();
    for r in load().iter().filter(|r| r.pg_lexemes.is_empty()) {
        let ours = ix.analyze(&r.body).unwrap();
        assert!(
            ours.is_empty(),
            "{} {:?}: PostgreSQL indexes nothing but we produced {ours:?}",
            r.id,
            r.body
        );
    }
}

/// Also a defect rather than a difference: a tokenizer that is not a function
/// of its input cannot be compared to anything, and would make an artifact's
/// contents depend on when it was built.
#[test]
fn the_tokenizer_is_deterministic() {
    let ix = analyzer();
    for r in load() {
        let a = ix.analyze(&r.body).unwrap();
        let b = ix.analyze(&r.body).unwrap();
        assert_eq!(a, b, "{}", r.id);
    }
}

/// The embedded stop list must equal the committed fixture.
///
/// The analyzer embeds PostgreSQL's `english.stop` so it has no filesystem
/// dependency at runtime; the fixture is the provenance record. Embedding a
/// copy without a drift check is how a list quietly stops matching the engine
/// it was taken from.
#[test]
fn the_embedded_stop_list_matches_the_committed_fixture() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/lexical-compat/pg16-english.stop")
        .canonicalize()
        .expect("the stop-list fixture is committed beside the oracle");
    let fixture: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    assert_eq!(fixture.len(), 127, "pg16 ships 127 English stop words");

    let ix = analyzer();
    // Every fixture word must be dropped by the analyzer, and the analyzer must
    // not drop a word the fixture does not name -- checked in both directions,
    // since a list that is merely a SUPERSET would silently discard content.
    for w in &fixture {
        assert!(
            ix.analyze(w).unwrap().is_empty(),
            "{w:?} is in PostgreSQL's stop list but survived our analyzer"
        );
    }
    for w in ["washington", "congress", "philadelphia", "tea", "emea"] {
        assert!(
            !ix.analyze(w).unwrap().is_empty(),
            "{w:?} is not a stop word and must survive"
        );
    }
}
