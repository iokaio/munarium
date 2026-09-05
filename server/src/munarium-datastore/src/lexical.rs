// SPDX-License-Identifier: Apache-2.0
//! The Tantivy lexical adapter.
//!
//! Uses Tantivy through its **public crate API** only — no `columnar`, no
//! `sstable`, no private internals (§6.1). A fork is a later, evidence-based
//! decision; this is the seam that makes one unnecessary for now.
//!
//! ## Why the query is a plan, not a string
//!
//! `LexicalPlan` arrives already expanded, normalized and demoted. No backend
//! re-derives any of that from a string, so PostgreSQL and Tantivy answer the
//! *same* question and a difference between them is attributable to the engine
//! — which is the only thing shadow mode can measure. The plan type is defined
//! HERE rather than imported, because this crate must not depend on
//! `munarium-core`; the coordinator translates between the two.
//!
//! ## Positions, from day one
//!
//! The schema indexes with positions even though nothing here needs them yet.
//! The current phrase and substring demotions read positions, and an artifact
//! built without them could not serve those rules without a rebuild — a
//! decision that would be discovered at cutover rather than at build.
//!
//! ## v1 storage: one archived component
//!
//! Tantivy writes many files. They are archived into a single `lexical/` blob
//! so the manifest lists one component with one hash, and open materializes
//! them into a scratch directory. That trades memory for simplicity, which is
//! the right trade while whole-artifact hydration is the serving path anyway;
//! the `Directory`-backed range-read path is explicitly a stage 10 experiment.

use std::io::Write as _;
use std::path::Path;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, PhraseQuery, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, STORED, STRING,
};
use tantivy::tokenizer::{LowerCaser, RemoveLongFilter, StopWordFilter, TextAnalyzer};
use tantivy::{Index, IndexReader, ReloadPolicy, TantivyDocument, Term};

use crate::records::ChunkRecord;
use crate::vector::Candidate;
use crate::Error;

/// The tokenizer this adapter registers and the analyzer contract it claims.
///
/// Recorded in the `BuildSpec`, so it is part of the LOGICAL identity: changing
/// it produces a new logical version that must be activated like any corpus
/// change (the §5.1 V1 decision). Naming it here keeps the claim and the code
/// in one place.
pub const TOKENIZER_ID: &str = "munarium_en";

/// The analyzer CONTRACT version, recorded in every `BuildSpec`'s
/// `lexical_analysis.contract_version` — part of the LOGICAL identity.
///
/// 1 was `SimpleTokenizer` + the pg16 stop list + Snowball. 2 replaced the
/// tokenizer with the classifying [`crate::tokenizer::MunariumTokenizer`]
/// (stage 5 analyzer-parity work): numbers, signed numbers, dotted chains,
/// scientific notation, digit/slash serials and hyphenated compounds now
/// tokenize as the PostgreSQL oracle records. An index built under 2 answers
/// number-shaped queries differently from one built under 1, which is exactly
/// why the version is in the hashed identity.
pub const ANALYZER_CONTRACT_VERSION: u32 = 2;

/// The Tantivy revision this adapter is built against.
///
/// Recorded in the `ArtifactBuildPlan`, so it is part of the PHYSICAL identity:
/// an engine upgrade produces a new `artifact_id` for the same logical version,
/// which is the property the logical/physical split exists to provide. Read from
/// the linked library rather than written by hand, so a dependency bump cannot
/// leave a stale revision stamped on every artifact it produces.
pub fn engine_revision() -> &'static str {
    tantivy::version_string()
}

/// SHA-256 over the stop-term list, as the `BuildSpec` carries it.
///
/// The spec references the list by name AND by hash, because a reference alone
/// would let the list change under a fixed logical id. Computed from the
/// embedded array rather than from the fixture file, so it describes what the
/// analyzer actually applied.
pub fn stop_terms_sha256() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for w in PG_ENGLISH_STOP_WORDS {
        h.update(w.as_bytes());
        h.update(b"\n");
    }
    hex::encode(h.finalize())
}

/// PostgreSQL 16's `english.stop`, verbatim.
///
/// Extracted from a running pg16 with
/// `cat "$(pg_config --sharedir)/tsearch_data/english.stop"` and committed as
/// `tests/fixtures/lexical-compat/pg16-english.stop`; this array is that
/// file. Embedded rather than read at runtime so the analyzer has no filesystem
/// dependency, and checked against the committed fixture by a test, so the two
/// cannot drift.
pub(crate) use crate::stopwords::PG_ENGLISH_STOP_WORDS;

/// Build the Munarium English analyzer.
///
/// Tantivy's stock `en_stem` lowercases and stems but does **not** remove stop
/// words, which the stage 0 oracle caught immediately: PostgreSQL reduces "the
/// and of to a an is are was were" to nothing, while `en_stem` produced ten
/// terms. That is a defect rather than a difference — an all-stop-word query
/// would match documents, and every stop word would generate candidates.
///
/// The pipeline mirrors PostgreSQL's `english` configuration in the order that
/// matters: split, bound token length, lowercase, drop stop words, stem. What
/// it deliberately does NOT reproduce is PostgreSQL's token CLASSIFICATION —
/// URLs, hosts, file paths, versions and hyphenated compounds emitted as whole
/// tokens and parts. Those differences are measured in
/// `tests/lexical_parity.rs` and recorded rather than silently accepted.
fn munarium_en() -> TextAnalyzer {
    TextAnalyzer::builder(crate::tokenizer::MunariumTokenizer)
        // 255 is Tantivy's own convention and comfortably above PostgreSQL's
        // 2047-byte lexeme limit for anything a real corpus produces; a token
        // longer than this is a hash or an encoding accident, not a word.
        .filter(RemoveLongFilter::limit(255))
        .filter(LowerCaser)
        // PostgreSQL's OWN stop list, not Tantivy's. They are not the same:
        // Tantivy's built-in English list omits words PostgreSQL drops -- "what"
        // and "did" among them -- so a query like "What cities did George
        // Washington visit?" would carry two high-frequency terms here that the
        // reference engine discards. The list is the 127-word Snowball file
        // shipped with pg16, extracted from the running server rather than
        // typed from memory, and committed beside the oracle it belongs to.
        .filter(StopWordFilter::remove(
            PG_ENGLISH_STOP_WORDS.iter().map(|w| w.to_string()),
        ))
        .filter(crate::tokenizer::WordOnlyStemmer)
        .build()
}

/// Register the analyzer on an index. Must be called on BOTH build and open:
/// the schema stores the tokenizer's NAME, never its definition, so an index
/// opened without registering it fails at query time rather than at open.
fn register_tokenizer(index: &Index) {
    index.tokenizers().register(TOKENIZER_ID, munarium_en());
}

/// A term in a plan, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTerm {
    pub text: String,
    /// `true` when an expansion rule or a model added it rather than the user
    /// typing it. Carried so diagnostics can say WHY a hit matched.
    pub expanded: bool,
}

impl PlanTerm {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            expanded: false,
        }
    }
    pub fn expanded(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            expanded: true,
        }
    }
}

/// A structured lexical query.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LexicalPlan {
    pub terms: Vec<PlanTerm>,
    /// Phrase groups, each requiring adjacency — which is why the schema
    /// carries positions.
    pub phrases: Vec<Vec<PlanTerm>>,
    /// Multiplier applied to a hit whose text contains the marker. Below 1.0
    /// demotes. Applied AFTER scoring, on the bounded candidate pool.
    pub demotions: Vec<Demotion>,
    /// 1 = any term makes a candidate; 2 = at least two must match.
    pub minimum_should_match: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Demotion {
    pub contains: String,
    pub multiplier: f32,
}

/// Produces lexical candidates. Separate from `VectorIndex` so either adapter
/// can be replaced without touching the other, and so neither can hide fusion.
pub trait LexicalIndex: Send + Sync {
    fn lexical_candidates(&self, plan: &LexicalPlan, limit: usize)
        -> Result<Vec<Candidate>, Error>;
    fn num_docs(&self) -> u64;
}

fn schema() -> (Schema, Field, Field) {
    let mut b = Schema::builder();
    // Positions are indexed unconditionally -- see the module header.
    let text_options = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_ID)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let chunk_id = b.add_text_field("chunk_id", STRING | STORED);
    let text = b.add_text_field("text", text_options);
    let schema = b.build();
    (schema, chunk_id, text)
}

/// A simple archive: repeated (name length, name, byte length, bytes).
///
/// Deliberately not tar or zip: a format with no compression, no metadata and
/// no symlinks has no decompression bomb and no path-traversal surface, and
/// this one is read from an untrusted store.
fn archive(dir: &Path) -> Result<Vec<u8>, Error> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        if !e.file_type()?.is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        // Tantivy's lock files are process state, not index content: present
        // or absent depending on what the builder was doing, and including
        // them would make two byte-identical builds produce different
        // archives and therefore different `artifact_id`s. Skipped here, at
        // the one place names are read, rather than by round-tripping the
        // whole archive through a second scratch directory afterwards.
        if name.ends_with(".lock") {
            continue;
        }
        entries.push((name, std::fs::read(e.path())?));
    }
    // Sorted so the archive is a function of the directory CONTENTS rather than
    // of readdir order. That is all sorting can buy here: Tantivy names each
    // segment with a fresh UUID, so two builds of identical records produce
    // differently-named files and therefore different archive bytes. Measured,
    // not assumed -- see `two_builds_do_not_produce_identical_bytes`.
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (name, bytes) in entries {
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

fn unarchive(bytes: &[u8], dir: &Path) -> Result<(), Error> {
    let bad = |w: &str| Error::Integrity(format!("lexical archive: {w}"));
    if bytes.len() < 4 {
        return Err(bad("shorter than its header"));
    }
    let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let mut pos = 4;
    for i in 0..count {
        if pos + 4 > bytes.len() {
            return Err(bad(&format!("truncated before entry {i}")));
        }
        let nlen = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + nlen > bytes.len() {
            return Err(bad(&format!("entry {i} name runs past the end")));
        }
        let name = std::str::from_utf8(&bytes[pos..pos + nlen])
            .map_err(|_| bad(&format!("entry {i} name is not UTF-8")))?
            .to_string();
        pos += nlen;
        // A name is a FILE name, never a path: the archive has no directories,
        // so anything path-shaped is either corruption or an attempt to escape.
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
            || name.contains(':')
        {
            return Err(bad(&format!("entry {i} has a path-shaped name {name:?}")));
        }
        if pos + 8 > bytes.len() {
            return Err(bad(&format!("truncated before entry {i} length")));
        }
        let blen = u64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap()) as usize;
        pos += 8;
        if pos + blen > bytes.len() {
            return Err(bad(&format!("entry {i} body runs past the end")));
        }
        let mut f = std::fs::File::create(dir.join(&name))?;
        f.write_all(&bytes[pos..pos + blen])?;
        pos += blen;
    }
    Ok(())
}

/// Build a Tantivy index over the records and return its archived bytes.
pub fn build(records: &[ChunkRecord]) -> Result<Vec<u8>, Error> {
    let scratch = tempfile::tempdir()?;
    let (schema, f_chunk, f_text) = schema();
    let index = Index::create_in_dir(scratch.path(), schema)
        .map_err(|e| Error::Invalid(format!("tantivy index create: {e}")))?;
    register_tokenizer(&index);
    let mut writer = index
        .writer(50_000_000)
        .map_err(|e| Error::Invalid(format!("tantivy writer: {e}")))?;
    // The ONLY merge in this build is the explicit force-merge below. With
    // the default LogMergePolicy, background merge threads race it: they can
    // consume segments between `searchable_segment_ids()` and `merge(...)`,
    // and the merge then names ids the SegmentManager no longer has —
    // "segments that were merged could not be found", found LIVE on the
    // 2026-08-31 dev cutover by the newspaper batches (large OCR corpora on
    // a replica that was also hydrating and serving; every smaller corpus
    // and every local build had too few segments to trigger a background
    // merge at all).
    writer.set_merge_policy(Box::new(tantivy::merge_policy::NoMergePolicy));
    for r in records {
        let mut doc = TantivyDocument::default();
        doc.add_text(f_chunk, &r.chunk_id);
        doc.add_text(f_text, &r.text);
        writer
            .add_document(doc)
            .map_err(|e| Error::Invalid(format!("tantivy add: {e}")))?;
    }
    writer
        .commit()
        .map_err(|e| Error::Invalid(format!("tantivy commit: {e}")))?;

    // Force-merge to ONE segment before sealing. The writer indexes on
    // multiple threads and each thread flushes its own segments, so even a
    // small corpus commits several — a thread-count-dependent number, which
    // means variable open cost and archive shape for identical content. An
    // immutable artifact is built once and read many times; paying the merge
    // at build is the right side of that trade. (ParadeDB's block-storage
    // write-up records segment proliferation as their first operational
    // problem in production — theirs from update-heavy workloads, ours would
    // have been from build parallelism; one segment closes both doors for an
    // artifact that never takes an update at all.)
    let segments = index
        .searchable_segment_ids()
        .map_err(|e| Error::Invalid(format!("tantivy segments: {e}")))?;
    if segments.len() > 1 {
        writer
            .merge(&segments)
            .wait()
            .map_err(|e| Error::Invalid(format!("tantivy merge: {e}")))?;
    }
    writer
        .wait_merging_threads()
        .map_err(|e| Error::Invalid(format!("tantivy merge join: {e}")))?;
    // `archive` leaves the lock files out: they are machine state, not index
    // content.
    archive(scratch.path())
}

/// An opened Tantivy index.
///
/// Holds the scratch directory alive: Tantivy mmaps its files, so the directory
/// must outlive every reader.
pub struct TantivyLexicalIndex {
    _scratch: tempfile::TempDir,
    index: Index,
    reader: IndexReader,
    f_chunk: Field,
    f_text: Field,
}

impl std::fmt::Debug for TantivyLexicalIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TantivyLexicalIndex")
            .field("docs", &self.num_docs())
            .finish_non_exhaustive()
    }
}

impl TantivyLexicalIndex {
    pub fn open(archive_bytes: &[u8]) -> Result<Self, Error> {
        let scratch = tempfile::tempdir()?;
        unarchive(archive_bytes, scratch.path())?;
        let index = Index::open_in_dir(scratch.path())
            .map_err(|e| Error::Integrity(format!("tantivy open: {e}")))?;
        register_tokenizer(&index);
        let schema = index.schema();
        let f_chunk = schema
            .get_field("chunk_id")
            .map_err(|_| Error::Integrity("lexical index has no chunk_id field".into()))?;
        let f_text = schema
            .get_field("text")
            .map_err(|_| Error::Integrity("lexical index has no text field".into()))?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|e| Error::Integrity(format!("tantivy reader: {e}")))?;
        Ok(Self {
            _scratch: scratch,
            index,
            reader,
            f_chunk,
            f_text,
        })
    }

    /// Analyze one string through the SAME tokenizer the index uses.
    ///
    /// Exposed because it is how the stage 0 lexical oracle gets measured: the
    /// question "what does Munarium do to this string" has to be answerable
    /// without reindexing a corpus to find out.
    /// How many segments the index holds. One, for anything `build` sealed —
    /// the property the build-time force-merge exists to provide.
    pub fn segment_count(&self) -> usize {
        self.reader.searcher().segment_readers().len()
    }

    pub fn analyze(&self, text: &str) -> Result<Vec<String>, Error> {
        let mut manager = self
            .index
            .tokenizer_for_field(self.f_text)
            .map_err(|e| Error::Invalid(format!("tokenizer: {e}")))?;
        let mut stream = manager.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            out.push(stream.token().text.clone());
        }
        Ok(out)
    }
}

impl LexicalIndex for TantivyLexicalIndex {
    fn lexical_candidates(
        &self,
        plan: &LexicalPlan,
        limit: usize,
    ) -> Result<Vec<Candidate>, Error> {
        if plan.terms.is_empty() && plan.phrases.is_empty() {
            // A stop-only or empty query matches nothing. Returning an empty
            // list rather than an error mirrors what an empty tsquery does, so
            // a caller does not have to special-case it.
            return Ok(Vec::new());
        }
        let term_query = |t: &PlanTerm| -> Box<dyn Query> {
            Box::new(TermQuery::new(
                Term::from_field_text(self.f_text, &t.text),
                IndexRecordOption::WithFreqs,
            ))
        };

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        if plan.minimum_should_match > 1 && plan.terms.len() >= 2 {
            // Tantivy 0.22's BooleanQuery has no minimum-should-match, so this
            // is emulated as the OR of every ANDed PAIR -- which is not a
            // workaround but the same construction PostgreSQL uses
            // (`pairs_tsquery`). Mirroring it keeps the two engines answering
            // the same question, which is the whole point of the parity work.
            //
            // Only "at least two" is expressible this way, and only two is
            // what the runbook grammar offers; a higher value would need
            // n-choose-k clauses and is refused rather than silently treated
            // as two.
            if plan.minimum_should_match > 2 {
                return Err(Error::Unsupported(format!(
                    "minimumShouldMatch {} is not supported; PostgreSQL and this adapter both express only 'at least two'",
                    plan.minimum_should_match
                )));
            }
            for (i, a) in plan.terms.iter().enumerate() {
                for b in &plan.terms[i + 1..] {
                    clauses.push((
                        Occur::Should,
                        Box::new(BooleanQuery::intersection(vec![
                            term_query(a),
                            term_query(b),
                        ])),
                    ));
                }
            }
        } else {
            for t in &plan.terms {
                clauses.push((Occur::Should, term_query(t)));
            }
        }

        for phrase in &plan.phrases {
            if phrase.len() < 2 {
                continue;
            }
            let terms: Vec<Term> = phrase
                .iter()
                .map(|t| Term::from_field_text(self.f_text, &t.text))
                .collect();
            clauses.push((Occur::Should, Box::new(PhraseQuery::new(terms))));
        }
        let query = BooleanQuery::new(clauses);

        let searcher = self.reader.searcher();
        // Exactly `limit` from the engine. The over-fetch that demotion needs
        // — a demoted hit falling out of the top-k, one just below the cut
        // rising into it — is the CALLER's (`OpenShard::lexical_candidates`),
        // which widens `limit` when the plan carries demotions and truncates
        // after applying them. Over-fetching here and truncating before
        // returning, as this once did, made demotion a no-op on exactly the
        // boundary it exists to move.
        let found = searcher
            .search(&query, &TopDocs::with_limit(limit.max(1)))
            .map_err(|e| Error::Invalid(format!("tantivy search: {e}")))?;

        let mut out = Vec::with_capacity(found.len());
        for (score, addr) in found {
            let doc: TantivyDocument = searcher
                .doc(addr)
                .map_err(|e| Error::Integrity(format!("tantivy doc fetch: {e}")))?;
            use tantivy::schema::Value as _;
            let Some(chunk_id) = doc
                .get_first(self.f_chunk)
                .and_then(|v| v.as_str())
                .map(str::to_string)
            else {
                return Err(Error::Integrity(
                    "a lexical hit has no stored chunk_id, so it cannot be cited".into(),
                ));
            };
            out.push(Candidate { chunk_id, score });
        }

        // Deterministic order: score descending, then chunk id. Tantivy's own
        // ordering among equal scores is not specified, so without this a
        // golden test could not exist.
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        out.truncate(limit);
        Ok(out)
    }

    fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }
}

/// Apply content demotions to a candidate list.
///
/// Separate from the search because the marker is tested against the RECORD
/// text, which the lexical index does not store: keeping the text out of the
/// index halves the artifact and keeps one copy of the truth.
pub fn apply_demotions(
    candidates: &mut [Candidate],
    plan: &LexicalPlan,
    text_of: &dyn Fn(&str) -> Option<String>,
) {
    if plan.demotions.is_empty() {
        return;
    }
    // The markers are lowercased once, not once per candidate per rule: this
    // runs on the request path over the widened candidate pool.
    let markers: Vec<(String, f32)> = plan
        .demotions
        .iter()
        .map(|d| (d.contains.to_lowercase(), d.multiplier))
        .collect();
    for c in candidates.iter_mut() {
        let Some(text) = text_of(&c.chunk_id) else {
            continue;
        };
        let lower = text.to_lowercase();
        // The STRONGEST demotion wins (the minimum multiplier), matching the
        // PostgreSQL path's `MIN(multiplier)` -- so two rules cannot compound
        // into a penalty neither one expressed.
        let mut factor = 1.0f32;
        for (marker, multiplier) in &markers {
            if lower.contains(marker.as_str()) {
                factor = factor.min(*multiplier);
            }
        }
        c.score *= factor;
    }
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn rec(id: &str, text: &str) -> ChunkRecord {
        ChunkRecord {
            chunk_id: id.into(),
            source_id: "s".into(),
            source_path: "p.md".into(),
            node_id: None,
            ordinal: 0,
            text: text.into(),
            text_sha256: "0".repeat(64),
        }
    }

    fn corpus() -> Vec<ChunkRecord> {
        vec![
            rec("a", "the continental congress met in philadelphia"),
            rec("b", "washington wrote to congress about supplying the army"),
            rec(
                "c",
                "colonial newspapers reported the destruction of the tea",
            ),
        ]
    }

    fn open() -> TantivyLexicalIndex {
        TantivyLexicalIndex::open(&build(&corpus()).unwrap()).unwrap()
    }

    /// A sealed artifact holds ONE segment, however many the multi-threaded
    /// writer flushed on the way. A corpus large enough to defeat any single
    /// flush proves the merge ran rather than the build getting lucky.
    #[test]
    fn a_sealed_index_holds_exactly_one_segment() {
        let many: Vec<ChunkRecord> = (0..500)
            .map(|i| {
                rec(
                    &format!("chunk-{i:04}"),
                    &format!("document number {i} discusses supply and congress at length"),
                )
            })
            .collect();
        let ix = TantivyLexicalIndex::open(&build(&many).unwrap()).unwrap();
        assert_eq!(ix.segment_count(), 1);
        assert_eq!(ix.num_docs(), 500);
    }

    /// The 2026-08-31 live-cutover defect: a corpus big enough that the
    /// writer flushes MANY segments and the default merge policy would fire
    /// background merges — which then raced the explicit force-merge and
    /// consumed its listed segment ids ("segments that were merged could not
    /// be found in the SegmentManager", every newspaper batch). With
    /// NoMergePolicy on the writer the race is structurally gone: the
    /// force-merge is the only merge, and it must still deliver one segment.
    #[test]
    fn a_large_multi_segment_build_still_seals_to_one_segment() {
        let page = "advertisement and shipping intelligence from the harbour,                     prices current, letters to the printer, acts of the assembly,                     and sundry notices of vendue "
            .repeat(20);
        let many: Vec<ChunkRecord> = (0..4_000)
            .map(|i| rec(&format!("news-{i:05}"), &format!("page {i}: {page}")))
            .collect();
        let ix = TantivyLexicalIndex::open(&build(&many).unwrap()).unwrap();
        assert_eq!(ix.segment_count(), 1);
        assert_eq!(ix.num_docs(), 4_000);
    }

    #[test]
    fn builds_opens_and_finds_by_term() {
        let ix = open();
        assert_eq!(ix.num_docs(), 3);
        let plan = LexicalPlan {
            terms: vec![PlanTerm::user("congress")],
            ..Default::default()
        };
        let hits = ix.lexical_candidates(&plan, 10).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.chunk_id.as_str()).collect();
        assert!(ids.contains(&"a") && ids.contains(&"b"), "{ids:?}");
        assert!(!ids.contains(&"c"));
    }

    #[test]
    fn stemming_matches_an_inflected_form() {
        let ix = open();
        // "supplying" is indexed; "supply" must find it through the stemmer.
        let plan = LexicalPlan {
            terms: vec![PlanTerm::user("suppli")],
            ..Default::default()
        };
        assert!(!ix.lexical_candidates(&plan, 10).unwrap().is_empty());
    }

    #[test]
    fn an_empty_plan_matches_nothing_without_erroring() {
        let ix = open();
        assert!(ix
            .lexical_candidates(&LexicalPlan::default(), 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn minimum_should_match_requires_two_terms() {
        let ix = open();
        let both = LexicalPlan {
            terms: vec![PlanTerm::user("congress"), PlanTerm::user("philadelphia")],
            minimum_should_match: 2,
            ..Default::default()
        };
        let hits = ix.lexical_candidates(&both, 10).unwrap();
        assert_eq!(hits.len(), 1, "only 'a' holds both terms");
        assert_eq!(hits[0].chunk_id, "a");
    }

    #[test]
    fn a_phrase_requires_adjacency_which_needs_positions() {
        let ix = open();
        let adjacent = LexicalPlan {
            phrases: vec![vec![
                PlanTerm::user("continent"),
                PlanTerm::user("congress"),
            ]],
            ..Default::default()
        };
        assert_eq!(ix.lexical_candidates(&adjacent, 10).unwrap().len(), 1);

        let not_adjacent = LexicalPlan {
            phrases: vec![vec![
                PlanTerm::user("congress"),
                PlanTerm::user("continent"),
            ]],
            ..Default::default()
        };
        assert!(ix.lexical_candidates(&not_adjacent, 10).unwrap().is_empty());
    }

    #[test]
    fn demotion_reorders_and_the_strongest_rule_wins() {
        let mut cands = vec![
            Candidate {
                chunk_id: "a".into(),
                score: 10.0,
            },
            Candidate {
                chunk_id: "b".into(),
                score: 9.0,
            },
        ];
        let texts: HashMap<&str, &str> = [("a", "a metadata listing"), ("b", "real content")]
            .into_iter()
            .collect();
        let plan = LexicalPlan {
            demotions: vec![
                Demotion {
                    contains: "metadata".into(),
                    multiplier: 0.5,
                },
                Demotion {
                    contains: "listing".into(),
                    multiplier: 0.1,
                },
            ],
            ..Default::default()
        };
        apply_demotions(&mut cands, &plan, &|id| {
            texts.get(id).map(|s| s.to_string())
        });
        assert_eq!(cands[0].chunk_id, "b", "the demoted hit falls below");
        // 10.0 * 0.1, not 10.0 * 0.5 * 0.1 -- the strongest wins, rules do not
        // compound into a penalty neither expressed.
        assert!((cands[1].score - 1.0).abs() < 1e-5, "{}", cands[1].score);
    }

    /// **Tantivy output is not byte-deterministic**, and this pins the fact
    /// rather than hoping otherwise.
    ///
    /// Each build names its segments with a fresh UUID, so two builds of
    /// identical records produce differently-named files and different archive
    /// bytes. The datastore design anticipated this: builds whose
    /// upstream engines emit different bytes produce different artifact ids
    /// even under the same plan, and warns against promising byte-for-byte
    /// determinism unless proven. It is now measured rather than hedged.
    ///
    /// The consequence is load-bearing: once a lexical component is in a
    /// manifest, a rebuild does NOT converge on the same artifact_id, so
    /// section 7.1 step 7's catalog convergence rule (adopt the existing row on
    /// primary-key conflict) is the mechanism that keeps a rebuild from
    /// duplicating an artifact -- not a nicety. What still converges is the
    /// LOGICAL id, which is what a session pin and an audit depend on.
    #[test]
    fn two_builds_do_not_produce_identical_bytes() {
        let a = build(&corpus()).unwrap();
        let b = build(&corpus()).unwrap();
        assert_ne!(
            a, b,
            "if Tantivy ever becomes byte-deterministic this test should fail, and the convergence story in decisions.md gets simpler -- check before 'fixing' it"
        );
        // Both still open and answer identically: non-determinism is in the
        // file NAMES and layout, not in what the index knows.
        let qa = TantivyLexicalIndex::open(&a).unwrap();
        let qb = TantivyLexicalIndex::open(&b).unwrap();
        let plan = LexicalPlan {
            terms: vec![PlanTerm::user("congress")],
            ..Default::default()
        };
        let ha: Vec<String> = qa
            .lexical_candidates(&plan, 10)
            .unwrap()
            .into_iter()
            .map(|c| c.chunk_id)
            .collect();
        let hb: Vec<String> = qb
            .lexical_candidates(&plan, 10)
            .unwrap()
            .into_iter()
            .map(|c| c.chunk_id)
            .collect();
        assert_eq!(
            ha, hb,
            "two builds must ANSWER identically even if their bytes differ"
        );
    }

    #[test]
    fn a_path_shaped_archive_entry_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut bad = Vec::new();
        bad.extend_from_slice(&1u32.to_le_bytes());
        let name = b"../escape";
        bad.extend_from_slice(&(name.len() as u32).to_le_bytes());
        bad.extend_from_slice(name);
        bad.extend_from_slice(&0u64.to_le_bytes());
        let err = unarchive(&bad, dir.path()).unwrap_err();
        assert!(err.to_string().contains("path-shaped"), "{err}");
    }

    #[test]
    fn a_truncated_archive_is_refused() {
        let bytes = build(&corpus()).unwrap();
        assert!(TantivyLexicalIndex::open(&bytes[..bytes.len() / 2]).is_err());
    }
}
