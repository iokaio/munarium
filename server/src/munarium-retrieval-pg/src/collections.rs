// SPDX-License-Identifier: Apache-2.0
//! first-class collections — indexes as separate, compartmentalized data
//! collections over one shared, LIST-partitioned chunk store.
//!
//! Physical layout: `collection_chunks PARTITION BY LIST (collection_id)`,
//! one partition per collection created here (advisory-locked runtime DDL).
//! Parent-declared GIN + HNSW indexes cascade per partition, so every
//! collection gets its own ANN graph and a single-collection query prunes to
//! exactly one partition. The partition is also the unit the DBA detaches in
//! the manual deletion runbook — no API deletes index data.

use crate::{chunk_text, local_embed, storage_err, PgRetrieval, LOCAL_EMBEDDER};
use crate::{CHUNKER_VERSION, EMBED_DIMS};
use munarium_core::retrieval::{
    CollectionInfo, CollectionSearchResult, LexicalQueryPlan, MergeWeights, PreparedSearchQuery,
    QueryEmbedder, QueryExpansionRule, SearchHit, SearchParams, SearchResult,
};
use munarium_core::{KernelError, Result};
use pgvector::Vector;
use sha2::Digest as _;
use sqlx::Row;
use std::collections::HashSet;

/// Build the "at least two of these lexemes" tsquery: the OR of every pair
/// ANDed, over the normalized lexemes of a query (as `plainto_tsquery`
/// prints them, quotes included). With fewer than two lexemes there is no
/// pair and the result is None — the leg keeps its single-term behavior.
/// Nineteen lexemes make 171 pairs, which the GIN index evaluates as one
/// query; the pool this excludes is exactly the rows matching a single
/// (usually common) word, which OR-density ranking put last anyway.
pub fn pairs_tsquery(lexemes: &[&str]) -> Option<String> {
    let lexemes: Vec<&str> = lexemes
        .iter()
        .map(|lexeme| lexeme.trim())
        .filter(|lexeme| !lexeme.is_empty())
        .collect();
    if lexemes.len() < 2 {
        return None;
    }
    let mut pairs = Vec::with_capacity(lexemes.len() * (lexemes.len() - 1) / 2);
    for (i, a) in lexemes.iter().enumerate() {
        for b in &lexemes[i + 1..] {
            pairs.push(format!("({a} & {b})"));
        }
    }
    Some(pairs.join(" | "))
}

/// The digit forms a query contributes for number-form normalization
/// (2026-08-30, §13.5 entry 25) — deterministic, vocabulary-free, and
/// deliberately narrow:
///
/// - a COMMA-GROUPED number (`4,436,097`) contributes its joined digits,
///   when the joined form is at least five digits (so `1,234` stays an
///   ordinary number) and it is not the integer part of a decimal
///   (`1,234,567.89` is an amount, not an identifier);
/// - a BARE digit run of five or more (`4436097`) contributes itself, so the
///   corpus-form lookup can run; four digits or fewer is a year or an
///   ordinary count, a decimal is a measurement, and an eight-digit
///   `(19|20)……` is a date;
/// - a LETTER-PREFIXED form (`US4436097`) contributes its digit suffix, so a
///   query in one corpus's convention can reach another's.
///
/// The result is the LOOKUP KEY set; [`PgRetrieval::number_form_lexemes`]
/// turns keys into the corpus-observed forms. Empty means the query has no
/// identifier-shaped number and the caller takes the existing path with no
/// extra round trip.
pub fn number_query_digits(query: &str) -> Vec<String> {
    let bytes = query.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut push = |digits: String| {
        if digits.len() >= 5
            && !(digits.len() == 8 && (digits.starts_with("19") || digits.starts_with("20")))
            && !out.contains(&digits)
        {
            out.push(digits);
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_digit() {
            // A run of digits and group commas. Walk it whole, then decide.
            let start = i;
            let mut joined = String::new();
            let mut grouped = false;
            while i < bytes.len() {
                let c = bytes[i] as char;
                if c.is_ascii_digit() {
                    joined.push(c);
                    i += 1;
                } else if c == ','
                    && i + 3 < bytes.len()
                    && bytes[i + 1].is_ascii_digit()
                    && bytes[i + 2].is_ascii_digit()
                    && bytes[i + 3].is_ascii_digit()
                    && !(i + 4 < bytes.len() && bytes[i + 4].is_ascii_digit())
                {
                    // A comma followed by exactly three digits: a group.
                    grouped = true;
                    i += 1;
                } else {
                    break;
                }
            }
            // The integer part of a decimal is an amount; a run glued to a
            // letter on the left was consumed by the identifier arm below.
            let decimal = i < bytes.len()
                && bytes[i] as char == '.'
                && i + 1 < bytes.len()
                && bytes[i + 1].is_ascii_digit();
            let after_letter = start > 0 && (bytes[start - 1] as char).is_ascii_alphabetic();
            if !decimal && !after_letter && (grouped || joined.len() >= 5) {
                push(joined);
            }
            if decimal {
                // Skip the fraction so its digits are not re-read as a run.
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
        } else if c.is_ascii_alphabetic() {
            // A short letter prefix glued to digits: US4436097, EP1234567.
            let start = i;
            while i < bytes.len() && (bytes[i] as char).is_ascii_alphabetic() {
                i += 1;
            }
            let prefix_len = i - start;
            if prefix_len <= 4 && i < bytes.len() && bytes[i].is_ascii_digit() {
                let mut digits = String::new();
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    digits.push(bytes[i] as char);
                    i += 1;
                }
                // Not part of a longer identifier tail (letters after digits).
                if !(i < bytes.len() && (bytes[i] as char).is_ascii_alphanumeric()) {
                    push(digits);
                }
            }
        } else {
            i += 1;
        }
    }
    out.truncate(8);
    out
}

/// Split `plainto_tsquery(...)::text` output (`'citi' & 'georg' & …`) into
/// its quoted lexemes. `plainto_tsquery` emits only `&`, so the split is
/// total; an empty tsquery prints as an empty string and yields nothing.
pub fn tsquery_lexemes(printed: &str) -> Vec<&str> {
    printed
        .split(" & ")
        .map(str::trim)
        .filter(|lexeme| !lexeme.is_empty())
        .collect()
}

/// One collection's lexical CANDIDATE predicate from the query's quoted
/// lexemes: drop the lexemes that are stop terms in this collection (`stop`
/// holds unquoted lexemes from its frequency statistics), then require at
/// least `minimum_should_match` of the rest (1 = OR, 2 = OR of ANDed pairs).
/// If every lexeme is a stop term the full set is kept — the predicate is
/// never empty. None means "nothing changed": no lexeme dropped and no pair
/// requirement, so the SQL's own OR query stands and the statement is
/// byte-identical to the knob-free one.
pub fn candidate_tsquery(
    lexemes: &[String],
    stop: &HashSet<String>,
    minimum_should_match: usize,
) -> Option<String> {
    let kept: Vec<&str> = lexemes
        .iter()
        .map(String::as_str)
        .filter(|lexeme| !stop.contains(lexeme.trim_matches('\'')))
        .collect();
    let kept: Vec<&str> = if kept.is_empty() {
        lexemes.iter().map(String::as_str).collect()
    } else {
        kept
    };
    if kept.is_empty() {
        return None;
    }
    let dropped = kept.len() < lexemes.len();
    if minimum_should_match >= 2 {
        if let Some(pairs) = pairs_tsquery(&kept) {
            return Some(pairs);
        }
    }
    if dropped {
        return Some(kept.join(" | "));
    }
    None
}

/// Apply the runbook's conditional vocabulary without encoding any corpus or
/// domain terms in the engine. Triggers are case-insensitive whole tokens;
/// additions retain their configured spelling and are de-duplicated.
pub fn expand_query(query: &str, rules: &[QueryExpansionRule]) -> String {
    let query_tokens: HashSet<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut seen = query_tokens.clone();
    let mut additions = Vec::new();

    for rule in rules {
        let applies = rule
            .when_any
            .iter()
            .map(|term| term.trim().to_lowercase())
            .any(|term| query_tokens.contains(&term));
        if !applies {
            continue;
        }
        for term in &rule.add_terms {
            let term = term.trim();
            let folded = term.to_lowercase();
            if !term.is_empty() && seen.insert(folded) {
                additions.push(term.to_string());
            }
        }
    }

    if additions.is_empty() {
        query.to_string()
    } else {
        format!("{} {}", query.trim(), additions.join(" "))
    }
}

/// Blend the original and expanded query vectors, then restore unit length.
/// Expansion terms therefore add semantic recall without erasing the entity
/// and relationship words the caller actually supplied.
pub(crate) fn weighted_query_embedding(original: &str, expanded: &str, weight: f32) -> Vec<f32> {
    if original == expanded || weight >= 1.0 {
        return local_embed(expanded);
    }
    if weight <= 0.0 {
        return local_embed(original);
    }

    let original = local_embed(original);
    let expanded = local_embed(expanded);
    let mut blended: Vec<f32> = original
        .iter()
        .zip(expanded.iter())
        .map(|(left, right)| ((1.0 - weight) * left) + (weight * right))
        .collect();
    let norm = blended
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    if norm > 0.0 {
        for value in &mut blended {
            *value /= norm;
        }
    }
    blended
}

/// Rows per `INSERT … SELECT FROM unnest(...)` during an index build
/// (2026-08-25, entry 21). One statement per chunk was one round trip per
/// chunk — some 400,000 for the Revolution archive — and every one of them
/// also updated the partition's GIN and HNSW indexes individually. 200 rows
/// × 9 columns stays far below Postgres' parameter ceiling while cutting
/// the round trips by two orders of magnitude.
pub(crate) const CHUNK_INSERT_BATCH: usize = 200;

/// Column arrays for one batched chunk insert. Same rows, same constraints,
/// same index maintenance as the per-row statement it replaces — only the
/// number of statements changes.
#[derive(Default)]
pub(crate) struct ChunkBatch {
    chunk_ids: Vec<String>,
    source_ids: Vec<String>,
    source_hashes: Vec<String>,
    ordinals: Vec<i32>,
    texts: Vec<String>,
    embeddings: Vec<Vector>,
}

impl ChunkBatch {
    pub(crate) fn len(&self) -> usize {
        self.chunk_ids.len()
    }

    pub(crate) fn push(
        &mut self,
        chunk_id: String,
        source_id: &str,
        source_hash: &str,
        ordinal: i32,
        text: &str,
        embedding: Vector,
    ) {
        self.chunk_ids.push(chunk_id);
        self.source_ids.push(source_id.to_string());
        self.source_hashes.push(source_hash.to_string());
        self.ordinals.push(ordinal);
        self.texts.push(text.to_string());
        self.embeddings.push(embedding);
    }

    pub(crate) async fn flush(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        tenant_id: &str,
        collection_id: &str,
        index_id: &str,
    ) -> Result<()> {
        if self.chunk_ids.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "INSERT INTO collection_chunks
                (tenant_id, collection_id, index_version_id, chunk_id,
                 source_id, source_hash, ordinal, text, embedding)
             SELECT $1, $2, $3, u.chunk_id, u.source_id, u.source_hash, u.ordinal, u.text, u.embedding
               FROM unnest($4::text[], $5::text[], $6::text[], $7::int4[], $8::text[], $9::vector[])
                    AS u(chunk_id, source_id, source_hash, ordinal, text, embedding)",
        )
        .bind(tenant_id)
        .bind(collection_id)
        .bind(index_id)
        .bind(std::mem::take(&mut self.chunk_ids))
        .bind(std::mem::take(&mut self.source_ids))
        .bind(std::mem::take(&mut self.source_hashes))
        .bind(std::mem::take(&mut self.ordinals))
        .bind(std::mem::take(&mut self.texts))
        .bind(std::mem::take(&mut self.embeddings))
        .execute(&mut **tx)
        .await
        .map_err(storage_err)?;
        Ok(())
    }
}

/// The Snowball English stop list — the same function words Postgres'
/// `english` text-search configuration drops, so a query phrase is built
/// from the words a `ts` vector can actually hold. Engine infrastructure,
/// not corpus vocabulary: nothing here names a domain, an entity, or a
/// question.
#[rustfmt::skip]
const STOPWORDS: &[&str] = &[
    "i", "me", "my", "myself", "we", "our", "ours", "ourselves", "you", "your", "yours",
    "yourself", "yourselves", "he", "him", "his", "himself", "she", "her", "hers", "herself",
    "it", "its", "itself", "they", "them", "their", "theirs", "themselves", "what", "which",
    "who", "whom", "this", "that", "these", "those", "am", "is", "are", "was", "were", "be",
    "been", "being", "have", "has", "had", "having", "do", "does", "did", "doing", "a", "an",
    "the", "and", "but", "if", "or", "because", "as", "until", "while", "of", "at", "by",
    "for", "with", "about", "against", "between", "into", "through", "during", "before",
    "after", "above", "below", "to", "from", "up", "down", "in", "out", "on", "off", "over",
    "under", "again", "further", "then", "once", "here", "there", "when", "where", "why",
    "how", "all", "any", "both", "each", "few", "more", "most", "other", "some", "such", "no",
    "nor", "not", "only", "own", "same", "so", "than", "too", "very", "s", "t", "can", "will",
    "just", "don", "should", "now",
];

fn is_stopword(token: &str) -> bool {
    STOPWORDS.contains(&token)
}

fn word_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The query's own adjacent content-word pairs, in query order and
/// de-duplicated: for "What cities did George Washington visit?" they are
/// `george washington` and `washington visit` ("cities" and "george" are
/// separated by a stop word, so they are not a phrase). A query with no
/// adjacent content words has no phrases and selection falls back to
/// term-density evidence alone.
pub fn query_phrases(query: &str) -> Vec<(String, String)> {
    let tokens = word_tokens(query);
    let mut phrases: Vec<(String, String)> = Vec::new();
    for pair in tokens.windows(2) {
        if is_stopword(&pair[0]) || is_stopword(&pair[1]) {
            continue;
        }
        let phrase = (pair[0].clone(), pair[1].clone());
        if !phrases.contains(&phrase) {
            phrases.push(phrase);
        }
    }
    phrases
}

/// True when any query phrase occurs verbatim (adjacent, case-folded,
/// punctuation-insensitive) in the text.
fn text_has_phrase(text: &str, phrases: &[(String, String)]) -> bool {
    if phrases.is_empty() {
        return false;
    }
    word_tokens(text)
        .windows(2)
        .any(|pair| phrases.iter().any(|(a, b)| pair[0] == *a && pair[1] == *b))
}

/// Rank collection probes by the evidence their ORIGINAL-query pools carry,
/// strongest first:
///
/// 1. **Lexical evidence blended with phrase evidence** — the sum of the
///    pool's three strongest `ts_rank` scores (comparable across collections
///    sharing one shape; summing three prevents one accidental catalog/title
///    match from deciding a wide-corpus route), multiplied by
///    `1 + phrase_boost × phrase_fraction`, where the fraction is the share
///    of the pool whose text contains one of the query's own adjacent
///    content-word pairs verbatim. Term density cannot tell a corpus that
///    *uses* the query's words from one that is *about* them: over a large
///    archival corpus, "What cities did George Washington visit?" ranks
///    nineteenth-century travel narratives
///    above every George Washington letterbook shard on `ts_rank` alone (the
///    narratives say "Washington", "cities" and "visit" constantly — about
///    the city), while "george washington" appeared in 73–87% of the
///    letterbook pools and 11–12% of the narratives'. Proximity is standard
///    IR evidence and carries no vocabulary of its own; when the query has
///    no phrases, or no pool contains one, the multiplier is 1 everywhere and
///    density decides alone. `phrase_boost` 0 disables the phrase signal.
/// 2. **Vector evidence** — the three smallest cosine distances, the
///    fallback when the lexical leg is empty.
///
/// Ties break on collection name so selection is deterministic.
pub fn select_collection_indices(
    results: &[CollectionSearchResult],
    max_collections: usize,
    query: &str,
    phrase_boost: f64,
) -> Vec<usize> {
    let phrases = query_phrases(query);
    let phrase_boost = if phrase_boost.is_finite() {
        phrase_boost.max(0.0)
    } else {
        0.0
    };

    // (blended lexical evidence, vector evidence). The blend is
    // `density × (1 + phrase_boost × phrase_fraction)`: strong phrase
    // evidence multiplies a collection's density (0.85 of a pool carrying
    // "george washington" at boost 3 counts 3.55×), weak phrase evidence
    // barely moves it (0.06 → 1.18×) and density decides — measured on
    // "How did colonial newspapers report the Boston Tea Party?", where the
    // phrase is later coinage, no pool exceeds 0.06 and a lexicographic
    // phrase-first order let that noise outrank the newspaper shards that
    // actually hold the December 1773 reports.
    fn evidence(
        result: &CollectionSearchResult,
        phrases: &[(String, String)],
        phrase_boost: f64,
    ) -> (f64, f64) {
        let hits = &result.result.hits;
        let phrase_fraction = if hits.is_empty() {
            0.0
        } else {
            hits.iter()
                .filter(|hit| text_has_phrase(&hit.text, phrases))
                .count() as f64
                / hits.len() as f64
        };

        let mut lexical: Vec<f64> = hits
            .iter()
            .filter_map(|hit| hit.lexical_score)
            .filter(|score| score.is_finite())
            .collect();
        lexical.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let lexical_evidence = if lexical.is_empty() {
            f64::NEG_INFINITY
        } else {
            lexical.into_iter().take(3).sum::<f64>() * (1.0 + phrase_boost * phrase_fraction)
        };

        let mut vector: Vec<f64> = hits
            .iter()
            .filter_map(|hit| hit.vector_distance)
            .filter(|distance| distance.is_finite())
            .collect();
        vector.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let vector_evidence = if vector.is_empty() {
            f64::NEG_INFINITY
        } else {
            -vector.into_iter().take(3).sum::<f64>()
        };
        (lexical_evidence, vector_evidence)
    }

    let scored: Vec<(f64, f64)> = results
        .iter()
        .map(|result| evidence(result, &phrases, phrase_boost))
        .collect();
    let mut indices: Vec<usize> = results
        .iter()
        .enumerate()
        .filter_map(|(index, result)| (!result.result.hits.is_empty()).then_some(index))
        .collect();
    indices.sort_by(|&a, &b| {
        let (lex_a, vec_a) = scored[a];
        let (lex_b, vec_b) = scored[b];
        lex_b
            .partial_cmp(&lex_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                vec_b
                    .partial_cmp(&vec_a)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(results[a].collection_name.cmp(&results[b].collection_name))
    });
    indices.truncate(max_collections);
    indices
}

fn row_to_info(row: &sqlx::postgres::PgRow) -> CollectionInfo {
    CollectionInfo {
        id: row.get("id"),
        name: row.get("name"),
        shape_ref: row.get("shape_ref"),
        access_level: row.get("access_level"),
        compartments: row.get("compartments"),
        status: row.get("status"),
        description: row.get("description"),
        created_at: row.get("created_at_text"),
    }
}

const INFO_COLUMNS: &str = "id, name, shape_ref, access_level, compartments, status, description, created_at::text AS created_at_text";

/// Partition ident derived from the server-generated collection id
/// (`col-<hex>`). Defense in depth: reject anything outside [a-z0-9-].
pub(crate) fn partition_name(collection_id: &str) -> Result<String> {
    if collection_id.is_empty()
        || !collection_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(KernelError::InvalidInput(format!(
            "bad collection id '{collection_id}'"
        )));
    }
    Ok(format!(
        "collection_chunks_p_{}",
        collection_id.trim_start_matches("col-").replace('-', "_")
    ))
}

impl PgRetrieval {
    /// Create-or-update a collection and its partition. Idempotent and safe
    /// under concurrency: the partition DDL runs inside a transaction holding
    /// a per-tenant advisory lock. An existing collection may update its
    /// access level / compartments / description, never its shape.
    pub async fn ensure_collection(
        &self,
        name: &str,
        shape_ref: &str,
        access_level: i32,
        compartments: &[String],
        description: Option<&str>,
    ) -> Result<CollectionInfo> {
        if name.trim().is_empty() {
            return Err(KernelError::InvalidInput(
                "collection name is required".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1 || ':collection-ddl'))")
            .bind(&self.tenant_id)
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;

        let existing = sqlx::query(&format!(
            "SELECT {INFO_COLUMNS} FROM collections WHERE tenant_id = $1 AND name = $2"
        ))
        .bind(&self.tenant_id)
        .bind(name)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_err)?;

        let id = match &existing {
            Some(row) => {
                let info = row_to_info(row);
                if info.shape_ref != shape_ref {
                    return Err(KernelError::InvalidInput(format!(
                        "collection '{name}' is bound to shape '{}'; a different shape needs a new collection",
                        info.shape_ref
                    )));
                }
                // A collection's access requirement protects the data it
                // holds. When several runbooks share a collection name, a
                // re-declaration may only KEEP or TIGHTEN the requirement,
                // never lower it — otherwise runbook B could re-declare
                // runbook A's level-2 collection as level 0 and leak its
                // contents. Raising is fine (safe); lowering is refused.
                if access_level < info.access_level
                    || !info.compartments.iter().all(|c| compartments.contains(c))
                {
                    return Err(KernelError::InvalidInput(format!(
                        "collection '{name}' already requires access_level {} {:?}; a re-declaration \
                         cannot lower the requirement (use a new collection name for a lower level)",
                        info.access_level, info.compartments
                    )));
                }
                sqlx::query(
                    "UPDATE collections
                        SET access_level = $3, compartments = $4,
                            description = COALESCE($5, description)
                      WHERE tenant_id = $1 AND id = $2",
                )
                .bind(&self.tenant_id)
                .bind(&info.id)
                .bind(access_level)
                .bind(compartments)
                .bind(description)
                .execute(&mut *tx)
                .await
                .map_err(storage_err)?;
                info.id
            }
            None => {
                let id = format!("col-{}", uuid::Uuid::now_v7().simple());
                sqlx::query(
                    "INSERT INTO collections
                        (tenant_id, id, name, shape_ref, access_level, compartments, description)
                     VALUES ($1,$2,$3,$4,$5,$6,$7)",
                )
                .bind(&self.tenant_id)
                .bind(&id)
                .bind(name)
                .bind(shape_ref)
                .bind(access_level)
                .bind(compartments)
                .bind(description)
                .execute(&mut *tx)
                .await
                .map_err(storage_err)?;
                id
            }
        };

        // One partition per collection; parent indexes cascade automatically.
        // DDL identifier is derived from the server-generated id (validated).
        let partition = partition_name(&id)?;
        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS {partition} PARTITION OF collection_chunks FOR VALUES IN ('{id}')"
        ))
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        tx.commit().await.map_err(storage_err)?;

        self.collection_by_id(&id).await
    }

    pub async fn collection_by_id(&self, id: &str) -> Result<CollectionInfo> {
        sqlx::query(&format!(
            "SELECT {INFO_COLUMNS} FROM collections WHERE tenant_id = $1 AND id = $2"
        ))
        .bind(&self.tenant_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?
        .map(|r| row_to_info(&r))
        .ok_or_else(|| KernelError::NotFound {
            kind: "collection",
            id: id.to_string(),
        })
    }

    pub async fn collection_by_name(&self, name: &str) -> Result<CollectionInfo> {
        sqlx::query(&format!(
            "SELECT {INFO_COLUMNS} FROM collections WHERE tenant_id = $1 AND name = $2"
        ))
        .bind(&self.tenant_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)?
        .map(|r| row_to_info(&r))
        .ok_or_else(|| KernelError::NotFound {
            kind: "collection",
            id: name.to_string(),
        })
    }

    pub async fn list_collections(&self) -> Result<Vec<CollectionInfo>> {
        Ok(sqlx::query(&format!(
            "SELECT {INFO_COLUMNS} FROM collections WHERE tenant_id = $1 ORDER BY name"
        ))
        .bind(&self.tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?
        .iter()
        .map(row_to_info)
        .collect())
    }

    /// Bind an already-uploaded source to a collection, by its source_id (the
    /// logical path's identity). Two paths holding identical bytes are two
    /// sources and bind independently.
    /// Bind every source in `source_ids` that exists, in ONE statement.
    ///
    /// The set-based twin of `bind_source` for the runbook sync path, which
    /// binds every source a filename prefix matches on every apply: one
    /// round trip per source was two queries × ~68k sources per
    /// `POST /v1/runbooks` on a deployment, inside one HTTP request. Ids
    /// that name no source are skipped rather than refused — the caller has
    /// just read them from the `sources` table, so an absent one is a
    /// concurrent delete, not a caller mistake. Returns how many bindings
    /// were NEW.
    pub async fn bind_sources(
        &self,
        collection_id: &str,
        source_ids: &[String],
        bound_by_uid: Option<&str>,
    ) -> Result<u64> {
        if source_ids.is_empty() {
            return Ok(0);
        }
        let inserted = sqlx::query(
            "INSERT INTO collection_sources (tenant_id, collection_id, source_id, bound_by_uid)
             SELECT s.tenant_id, $2, s.source_id, $4
               FROM sources s
              WHERE s.tenant_id = $1 AND s.source_id = ANY($3)
             ON CONFLICT (tenant_id, collection_id, source_id) DO NOTHING",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(source_ids)
        .bind(bound_by_uid)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?
        .rows_affected();
        Ok(inserted)
    }

    pub async fn bind_source(
        &self,
        collection_id: &str,
        source_id: &str,
        bound_by_uid: Option<&str>,
    ) -> Result<bool> {
        let exists =
            sqlx::query("SELECT 1 AS one FROM sources WHERE tenant_id = $1 AND source_id = $2")
                .bind(&self.tenant_id)
                .bind(source_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage_err)?
                .is_some();
        if !exists {
            return Err(KernelError::NotFound {
                kind: "source",
                id: source_id.to_string(),
            });
        }
        let inserted = sqlx::query(
            "INSERT INTO collection_sources (tenant_id, collection_id, source_id, bound_by_uid)
             VALUES ($1,$2,$3,$4)
             ON CONFLICT (tenant_id, collection_id, source_id) DO NOTHING",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(source_id)
        .bind(bound_by_uid)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?
        .rows_affected()
            > 0;
        Ok(inserted)
    }

    pub async fn collection_source_count(&self, collection_id: &str) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM collection_sources WHERE tenant_id = $1 AND collection_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_err)
    }

    /// The collection's active index id, if any.
    pub async fn active_collection_index(&self, collection_id: &str) -> Result<Option<String>> {
        sqlx::query_scalar(
            "SELECT id FROM index_versions
              WHERE tenant_id = $1 AND collection_id = $2 AND active",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_err)
    }

    /// Side-by-side build over the collection's bound sources. Identity
    /// includes the collection id, so the same corpus indexed under two
    /// collections yields distinct, independently-cut-over index versions.
    pub async fn build_collection_index(
        &self,
        collection_id: &str,
        max_chars: usize,
        watermark_seq: u64,
        activate: bool,
    ) -> Result<munarium_core::retrieval::IndexVersion> {
        let info = self.collection_by_id(collection_id).await?;
        let sources = sqlx::query(
            "SELECT s.source_id, s.filename, s.content_hash, s.media_type
               FROM collection_sources cs
               JOIN sources s
                 ON s.tenant_id = cs.tenant_id AND s.source_id = cs.source_id
              WHERE cs.tenant_id = $1 AND cs.collection_id = $2
              ORDER BY s.source_id",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        if sources.is_empty() {
            return Err(KernelError::InvalidInput(format!(
                "no sources bound to collection '{}' — ingest or bind sources first",
                info.name
            )));
        }

        let source_hashes: Vec<String> = sources
            .iter()
            .map(|r| r.get::<String, _>("content_hash"))
            .collect();
        // Identity pairs id WITH hash: two sources sharing bytes are distinct
        // indexes, and re-putting one path with new bytes rebuilds.
        let source_set: Vec<String> = sources
            .iter()
            .map(|r| {
                format!(
                    "{}:{}",
                    r.get::<String, _>("source_id"),
                    r.get::<String, _>("content_hash")
                )
            })
            .collect();
        // The extractor version joins the identity: improving DOCX/PDF/OCR
        // extraction changes the TEXT for the same bytes, so it must produce
        // a new index rather than silently serving the old chunks.
        let identity = format!(
            "{collection_id}|{}|{CHUNKER_VERSION}|{LOCAL_EMBEDDER}|{}|{}",
            info.shape_ref,
            self.extractors.version(),
            source_set.join(",")
        );
        let index_id = format!(
            "idx-{}",
            &hex::encode(sha2::Sha256::digest(identity.as_bytes()))[..16]
        );

        let manifest = serde_json::json!({
            "collection_id": collection_id,
            "collection_name": info.name,
            "shape_ref": info.shape_ref,
            "chunker": CHUNKER_VERSION,
            "extractors": self.extractors.version(),
            "embedder": { "provider": "local", "model": LOCAL_EMBEDDER, "dims": EMBED_DIMS },
            "source_set": source_hashes,
            "max_chars": max_chars,
        });

        let exists =
            sqlx::query("SELECT 1 AS one FROM index_versions WHERE tenant_id = $1 AND id = $2")
                .bind(&self.tenant_id)
                .bind(&index_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage_err)?
                .is_some();

        if !exists {
            let mut tx = self.pool.begin().await.map_err(storage_err)?;
            sqlx::query(
                "INSERT INTO index_versions
                    (tenant_id, id, shape_ref, collection_id, manifest, watermark_seq, active)
                 VALUES ($1, $2, $3, $4, $5, $6, false)",
            )
            .bind(&self.tenant_id)
            .bind(&index_id)
            .bind(&info.shape_ref)
            .bind(collection_id)
            .bind(&manifest)
            .bind(watermark_seq as i64)
            .execute(&mut *tx)
            .await
            .map_err(storage_err)?;

            let mut batch = ChunkBatch::default();
            for row in &sources {
                let sid: String = row.get("source_id");
                let path: String = row.get("filename");
                let hash: String = row.get("content_hash");
                let media_type: String = row.get("media_type");
                let key = munarium_core::sources::SourceKey::new(&self.tenant_id, &path, &hash)?;
                let bytes = self.sources.get(&key).await?;
                // DOCX/PDF become text here; anything already text passes
                // through. A failure is recorded per source and the build
                // continues — one bad document never fails the build.
                let extracted = self.extract_source(&media_type, &bytes).await;
                self.record_extraction(&mut *tx, &sid, &extracted).await?;
                let text = extracted.text;
                for (ordinal, chunk) in chunk_text(&text, max_chars).iter().enumerate() {
                    // chunk_id keys on source_id, not the hash: two sources
                    // with identical bytes would otherwise collide here.
                    batch.push(
                        format!("{sid}#{ordinal}"),
                        &sid,
                        &hash,
                        ordinal as i32,
                        chunk,
                        Vector::from(local_embed(chunk)),
                    );
                    if batch.len() >= CHUNK_INSERT_BATCH {
                        batch
                            .flush(&mut tx, &self.tenant_id, collection_id, &index_id)
                            .await?;
                    }
                }
            }
            batch
                .flush(&mut tx, &self.tenant_id, collection_id, &index_id)
                .await?;
            tx.commit().await.map_err(storage_err)?;
            // Corpus-adaptive stop terms read this at query time (entry 21).
            self.record_lexeme_frequency(collection_id, &index_id)
                .await?;
        }

        sqlx::query(
            "UPDATE index_versions SET watermark_seq = $3 WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&self.tenant_id)
        .bind(&index_id)
        .bind(watermark_seq as i64)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;

        // Vacuum + fresh planner statistics for the partition this build just
        // filled (2026-08-25). Nothing did either before: a newly built
        // collection was searched on whatever autovacuum had last seen — none,
        // for a first build — so the planner chose between the GIN bitmap
        // scan and a sequential scan blind, and the GIN index's fastupdate
        // pending list (every row this build inserted, unmerged) was walked
        // sequentially by each of the first queries until autovacuum got to
        // it. VACUUM merges that list; ANALYZE writes the statistics. Neither
        // runs inside a transaction (VACUUM cannot), neither blocks readers,
        // and the partition name is the sanitized identifier `partition_name`
        // derives — maintenance commands take no bind parameters, hence the
        // format.
        let partition = partition_name(collection_id)?;
        sqlx::query(&format!("VACUUM (ANALYZE) {partition}"))
            .execute(&self.pool)
            .await
            .map_err(storage_err)?;

        if activate {
            self.activate_collection_index(collection_id, &index_id)
                .await?;
        }
        self.index_version_by_id(&index_id).await
    }

    /// Atomic per-collection cutover.
    pub async fn activate_collection_index(
        &self,
        collection_id: &str,
        index_id: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(storage_err)?;
        // `deactivated_at` anchors the serving-required horizon (0030): the
        // superseded version stays required for one pin horizon from NOW,
        // not from when it was built.
        sqlx::query(
            "UPDATE index_versions SET active = false, deactivated_at = now()
              WHERE tenant_id = $1 AND collection_id = $2 AND active",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        let updated = sqlx::query(
            "UPDATE index_versions
                SET active = true, activated_at = COALESCE(activated_at, now()),
                    deactivated_at = NULL
              WHERE tenant_id = $1 AND id = $2 AND collection_id = $3",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .bind(collection_id)
        .execute(&mut *tx)
        .await
        .map_err(storage_err)?;
        if updated.rows_affected() == 0 {
            return Err(KernelError::NotFound {
                kind: "index",
                id: index_id.to_string(),
            });
        }
        tx.commit().await.map_err(storage_err)?;
        Ok(())
    }

    /// Deterministic verification over the partitioned store.
    pub async fn verify_collection_index(&self, index_id: &str) -> Result<serde_json::Value> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM collection_chunks WHERE tenant_id = $1 AND index_version_id = $2",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_err)?;
        if count == 0 {
            return Err(KernelError::InvalidInput(format!(
                "index {index_id} has zero chunks"
            )));
        }
        let first: String = sqlx::query_scalar(
            "SELECT text FROM collection_chunks
              WHERE tenant_id = $1 AND index_version_id = $2
              ORDER BY chunk_id LIMIT 1",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_err)?;
        let probe: String = first
            .split_whitespace()
            .take(5)
            .collect::<Vec<_>>()
            .join(" ");
        let hits: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM collection_chunks
              WHERE tenant_id = $1 AND index_version_id = $2
                AND ts @@ plainto_tsquery('english', $3)",
        )
        .bind(&self.tenant_id)
        .bind(index_id)
        .bind(&probe)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(serde_json::json!({ "chunks": count, "self_probe_hits": hits }))
    }

    /// Drop chunk data for inactive collection versions beyond the newest
    /// `keep` — manifests stay resolvable, storage is reclaimed.
    pub async fn retire_old_collection(&self, collection_id: &str, keep: u32) -> Result<u64> {
        // The retiring versions' number-form rows go with their chunks.
        sqlx::query(
            r#"
            DELETE FROM index_number_lexemes
             WHERE tenant_id = $1 AND collection_id = $2 AND index_version_id IN (
                SELECT id FROM index_versions
                 WHERE tenant_id = $1 AND collection_id = $2 AND NOT active
                 ORDER BY built_at DESC OFFSET $3
             )
            "#,
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(keep as i64)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        // The retiring versions' lexeme statistics go with their chunks.
        sqlx::query(
            r#"
            DELETE FROM index_lexeme_frequency
             WHERE tenant_id = $1 AND collection_id = $2 AND index_version_id IN (
                SELECT id FROM index_versions
                 WHERE tenant_id = $1 AND collection_id = $2 AND NOT active
                 ORDER BY built_at DESC OFFSET $3
             )
            "#,
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(keep as i64)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        let retired = sqlx::query(
            r#"
            DELETE FROM collection_chunks
             WHERE tenant_id = $1 AND collection_id = $2 AND index_version_id IN (
                SELECT id FROM index_versions
                 WHERE tenant_id = $1 AND collection_id = $2 AND NOT active
                 ORDER BY built_at DESC OFFSET $3
             )
            "#,
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(keep as i64)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(retired.rows_affected())
    }

    /// Hybrid search over ONE collection's active (or named) index — the
    /// query carries `collection_id = $n`, so partition pruning routes it to
    /// exactly one partition's GIN/HNSW.
    /// Prepare a query once: expand it, and produce the query vector.
    ///
    /// Free of any collection: nothing here needs a shard, which is what makes
    /// it safe to hoist above a fan-out. The per-collection stop-term
    /// subtraction stays in the search path because it reads that collection's
    /// own index statistics.
    pub fn prepare_query(
        query: &str,
        params: &SearchParams,
        embedder: &dyn QueryEmbedder,
    ) -> PreparedSearchQuery {
        let expanded = expand_query(query, &params.query_expansions);
        let weight = params.query_expansion_weight.clamp(0.0, 1.0);
        let embedding = embedder.blend(query, &expanded, weight as f32);
        PreparedSearchQuery {
            lexical: Some(LexicalQueryPlan {
                original: query.to_string(),
                expanded,
                expansion_weight: weight,
                lexemes: params.query_lexemes.clone(),
                demotions: params.content_demotions.clone(),
                minimum_should_match: params.minimum_should_match,
                stop_term_fraction: params.stop_term_fraction,
                policy_version: munarium_core::retrieval::QUERY_POLICY_VERSION,
            }),
            embedding: Some(embedding.into()),
            lexical_candidates: params.candidate_n,
            vector_candidates: params.candidate_n,
            top_k: params.top_k,
            rrf_k: params.rrf_k,
        }
    }

    /// Search one collection, preparing the query inline.
    ///
    /// Kept so existing callers and tests are untouched by the split. A
    /// fan-out should NOT use this -- it re-prepares per collection, which is
    /// the duplicated embedding work the split exists to remove. Use
    /// `prepare_query` once and then `search_collection_prepared`.
    pub async fn search_collection(
        &self,
        collection_id: &str,
        query: &str,
        params: SearchParams,
        index_version: Option<&str>,
    ) -> Result<SearchResult> {
        let prepared = Self::prepare_query(query, &params, &crate::LocalHashEmbedder);
        self.search_collection_prepared(collection_id, &prepared, index_version)
            .await
    }

    /// Search one collection with an ALREADY PREPARED query.
    ///
    /// The expansion, the normalized lexemes and the query VECTOR arrive
    /// computed. Before this split they were derived inside this method, so an
    /// N-collection fan-out embedded the same query N times; worse, shadow mode
    /// would have compared two independently derived plans and been unable to
    /// attribute a difference to the engine. What is still derived here is the
    /// per-collection candidate predicate, because it needs THIS collection's
    /// index statistics -- the plan is shared, the shard-local predicate is not.
    /// Which index version a search of this collection uses, and its
    /// watermark: the named version when the caller pinned one, else the
    /// active pointer.
    ///
    /// Public because the datastore serving path resolves the SAME version
    /// the PostgreSQL path would have — the active pointer stays control-plane
    /// truth in every mode (section 7.3: "PostgreSQL mode does not consult
    /// bindings", and datastore mode consults them only AFTER this).
    pub async fn resolve_index_version(
        &self,
        collection_id: &str,
        index_version: Option<&str>,
    ) -> Result<(String, i64)> {
        match index_version {
            Some(id) => {
                let row = sqlx::query(
                    "SELECT id, watermark_seq FROM index_versions
                      WHERE tenant_id = $1 AND id = $2 AND collection_id = $3",
                )
                .bind(&self.tenant_id)
                .bind(id)
                .bind(collection_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage_err)?;
                row.map(|r| (r.get::<String, _>("id"), r.get::<i64, _>("watermark_seq")))
            }
            None => {
                let row = sqlx::query(
                    "SELECT id, watermark_seq FROM index_versions
                      WHERE tenant_id = $1 AND collection_id = $2 AND active",
                )
                .bind(&self.tenant_id)
                .bind(collection_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage_err)?;
                row.map(|r| (r.get::<String, _>("id"), r.get::<i64, _>("watermark_seq")))
            }
        }
        .ok_or_else(|| KernelError::NotFound {
            kind: "index",
            id: collection_id.to_string(),
        })
    }

    pub async fn search_collection_prepared(
        &self,
        collection_id: &str,
        prepared: &PreparedSearchQuery,
        index_version: Option<&str>,
    ) -> Result<SearchResult> {
        // A prepared query with no lexical plan still has to name the text it
        // came from; an empty plan is the caller asking for the vector leg
        // alone, which the SQL below already handles as an empty tsquery.
        let empty_plan;
        let plan = match prepared.lexical.as_ref() {
            Some(plan) => plan,
            None => {
                empty_plan = LexicalQueryPlan {
                    original: String::new(),
                    expanded: String::new(),
                    expansion_weight: 1.0,
                    lexemes: Vec::new(),
                    demotions: Vec::new(),
                    minimum_should_match: 1,
                    stop_term_fraction: 0.0,
                    policy_version: munarium_core::retrieval::QUERY_POLICY_VERSION,
                };
                &empty_plan
            }
        };
        let query: &str = plan.original.as_str();
        let (index_id, watermark) = self
            .resolve_index_version(collection_id, index_version)
            .await?;

        let expanded_query: &str = plan.expanded.as_str();
        let expansion_weight = plan.expansion_weight;
        let demotions_json = serde_json::to_value(&plan.demotions).map_err(|e| {
            KernelError::InvalidInput(format!("invalid retrieval contentDemotions: {e}"))
        })?;

        // OR-semantics lexical leg (2026-08-24): plainto_tsquery ANDs every
        // word, so a question-shaped query ("Are there change-of-control
        // provisions in the material commercial contracts?") almost never
        // matches ALL terms and the leg comes back empty, leaving ranking to
        // the shallow hash-vector leg alone. Rewriting '&' to '|' ranks by
        // matched-term density instead: ts_rank scores documents higher the
        // more query terms they hold. plainto_tsquery emits only '&', so the
        // rewrite is total; an empty query yields an empty tsquery and
        // matches nothing, as before.
        //
        // Cost controls (2026-08-25, §13.5 entry 20). OR semantics make every
        // lexical query touch every chunk holding ANY query word, and the
        // demotion rule's `strpos(lower(text), …)` detoasted and lowered each
        // of those rows' full text — measured at ~4.5 s per shard under two
        // concurrent turns on a 2-vCPU server. Two levers, both runbook
        // policy: (1) a rule with `match: phrase` tests the already-parsed
        // tsvector (`ts @@ phraseto_tsquery(marker)`) instead of the text,
        // and (2) `params.lexical_prefilter` (the runbook's
        // `minimumShouldMatch`, built by [`lexical_prefilter`]) is a GIN-
        // indexable "at least two query terms" tsquery that excludes the
        // single-term rows before any rank is computed. The rules CTE runs
        // once per query; an empty rule list yields COALESCE(…, 1) and the
        // unweighted rank, byte-identical to the rule-free query it replaces.
        // Every marker and weight is data bound from the runbook; this SQL
        // carries no corpus vocabulary.
        // The per-collection candidate predicate: the caller's normalized
        // lexemes minus this collection's stop terms, under the pair rule.
        // Empty text = the SQL's own OR query (knob-free behavior).
        let candidates = if plan.lexemes.is_empty() {
            String::new()
        } else {
            let stop = if plan.stop_term_fraction > 0.0 {
                self.frequent_lexemes(collection_id, &index_id, plan.stop_term_fraction)
                    .await?
            } else {
                HashSet::new()
            };
            candidate_tsquery(&plan.lexemes, &stop, plan.minimum_should_match).unwrap_or_default()
        };
        let lexical = sqlx::query(
            "WITH rules AS (
                    SELECT (rule->>'lexical_multiplier')::double precision AS multiplier,
                           CASE WHEN rule->>'match' = 'phrase'
                                THEN phraseto_tsquery('english', rule->>'contains') END AS phrase,
                           CASE WHEN rule->>'match' = 'phrase'
                                THEN NULL ELSE lower(rule->>'contains') END AS marker
                      FROM jsonb_array_elements($8::jsonb) AS rule
                 ), q AS (
                    SELECT original, expanded,
                           -- The candidate predicate the GIN index evaluates: the
                           -- two-term pair query when the runbook asks for one,
                           -- else the OR query. One plain `@@`, never an OR with
                           -- a NULL test, so the index can drive the scan.
                           COALESCE(NULLIF($9::text, '')::tsquery, expanded) AS candidates
                      FROM (SELECT replace(plainto_tsquery('english', $4)::text, ' & ', ' | ')::tsquery AS original,
                                   replace(plainto_tsquery('english', $5)::text, ' & ', ' | ')::tsquery AS expanded) base
                 )
                 SELECT c.chunk_id, c.source_id, c.source_hash, c.text,
                        ((CASE
                            WHEN $6::double precision <= 0.0 THEN ts_rank(c.ts, q.original)::double precision
                            WHEN $6::double precision >= 1.0 THEN ts_rank(c.ts, q.expanded)::double precision
                            ELSE ((1.0 - $6::double precision) * ts_rank(c.ts, q.original)::double precision)
                               + ($6::double precision * ts_rank(c.ts, q.expanded)::double precision)
                          END)
                          * COALESCE((
                            SELECT MIN(rules.multiplier)
                              FROM rules
                             WHERE (rules.phrase IS NOT NULL AND c.ts @@ rules.phrase)
                                OR (rules.marker IS NOT NULL AND strpos(lower(c.text), rules.marker) > 0)
                        ), 1.0::double precision))::real AS rank
                   FROM collection_chunks c, q
                  WHERE c.tenant_id = $1 AND c.collection_id = $2 AND c.index_version_id = $3
                    AND c.ts @@ q.candidates
                  ORDER BY rank DESC, c.chunk_id LIMIT $7",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(&index_id)
        .bind(query)
        .bind(expanded_query)
        .bind(expansion_weight)
        .bind(prepared.lexical_candidates)
        .bind(&demotions_json)
        .bind(&candidates)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;

        // The prepared vector, not a recomputed one. `None` is legitimate --
        // a lexical-only query, or a collection built without vectors -- and
        // an adapter must never fabricate a leg, so an absent embedding means
        // the vector leg contributes nothing rather than contributing noise.
        let qvec = prepared
            .embedding
            .as_ref()
            .map(|e| Vector::from(e.as_ref().to_vec()));
        // The vector leg runs in its own transaction so pgvector's per-session
        // search knobs can be set for exactly this query (SET LOCAL semantics
        // via set_config(..., true)). `hnsw.ef_search` defaults to 40, so an
        // ANN scan returned at most 40 rows however large `candidate_n` was;
        // it now follows the candidate pool (pgvector's ceiling is 1000).
        // `hnsw.iterative_scan` (pgvector ≥ 0.8) keeps scanning until the
        // LIMIT is met after the index_version filter — a partition holds the
        // retiring version's chunks beside the active one's, and without it
        // those rows silently consumed the ef_search budget. Older pgvector
        // rejects the unknown name; that SET is savepointed and skipped.
        // No embedding means no vector leg at all -- not a leg bound to NULL.
        // `embedding <=> NULL` is NULL, so an ORDER BY over it returns rows in
        // arbitrary order, which would look like a working vector leg while
        // being noise. Absent is absent.
        let vector = match qvec {
            None => Vec::new(),
            Some(qvec) => {
                let ef_search = prepared.vector_candidates.clamp(40, 1000);
                let mut tx = self.pool.begin().await.map_err(storage_err)?;
                sqlx::query("SELECT set_config('hnsw.ef_search', $1, true)")
                    .bind(ef_search.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(storage_err)?;
                sqlx::query("SAVEPOINT iterative_scan")
                    .execute(&mut *tx)
                    .await
                    .map_err(storage_err)?;
                if sqlx::query("SELECT set_config('hnsw.iterative_scan', 'relaxed_order', true)")
                    .execute(&mut *tx)
                    .await
                    .is_err()
                {
                    sqlx::query("ROLLBACK TO SAVEPOINT iterative_scan")
                        .execute(&mut *tx)
                        .await
                        .map_err(storage_err)?;
                }
                // Preserve the HNSW candidate selection, then rerank that bounded
                // pool with the runbook's content penalties (same rule forms as the
                // lexical leg; an empty rule list adds 0 and keeps the ANN order).
                let vector = sqlx::query(
                    "WITH rules AS (
                        SELECT (rule->>'vector_distance_penalty')::double precision AS penalty,
                               CASE WHEN rule->>'match' = 'phrase'
                                    THEN phraseto_tsquery('english', rule->>'contains') END AS phrase,
                               CASE WHEN rule->>'match' = 'phrase'
                                    THEN NULL ELSE lower(rule->>'contains') END AS marker
                          FROM jsonb_array_elements($6::jsonb) AS rule
                     )
                     SELECT candidates.chunk_id, candidates.source_id, candidates.source_hash, candidates.text,
                            candidates.raw_distance + COALESCE((
                                SELECT MAX(rules.penalty)
                                  FROM rules
                                 WHERE (rules.phrase IS NOT NULL AND candidates.ts @@ rules.phrase)
                                    OR (rules.marker IS NOT NULL
                                        AND strpos(lower(candidates.text), rules.marker) > 0)
                            ), 0.0::double precision) AS distance
                       FROM (
                            SELECT chunk_id, source_id, source_hash, text, ts,
                                   (embedding <=> $4) AS raw_distance
                              FROM collection_chunks
                             WHERE tenant_id = $1 AND collection_id = $2
                               AND index_version_id = $3
                             ORDER BY embedding <=> $4
                             LIMIT $5
                       ) AS candidates
                      ORDER BY distance, candidates.chunk_id",
            )
            .bind(&self.tenant_id)
            .bind(collection_id)
            .bind(&index_id)
            .bind(&qvec)
            .bind(prepared.vector_candidates)
            .bind(&demotions_json)
            .fetch_all(&mut *tx)
            .await
            .map_err(storage_err)?;
                tx.commit().await.map_err(storage_err)?;
                vector
            }
        };

        let mut hits = crate::rrf_fuse(&lexical, &vector, prepared.rrf_k);
        hits.truncate(if prepared.top_k == 0 {
            10
        } else {
            prepared.top_k
        });

        let envelope = self
            .envelope_for(&mut hits, index_id, watermark as u64)
            .await?;
        Ok(SearchResult { hits, envelope })
    }

    /// The normalized lexemes of a query formulation, as `plainto_tsquery`
    /// prints them (quoted, stemmed, stop words removed). One tiny round trip
    /// per formulation per turn, not per collection: the caller computes it
    /// once and hands the list to every `search_collection` through
    /// `SearchParams::query_lexemes`, where the per-collection candidate
    /// predicate is derived (see [`candidate_tsquery`]).
    pub async fn query_lexemes(&self, text: &str) -> Result<Vec<String>> {
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let printed: String = sqlx::query_scalar("SELECT plainto_tsquery('english', $1)::text")
            .bind(text)
            .fetch_one(&self.pool)
            .await
            .map_err(storage_err)?;
        Ok(tsquery_lexemes(&printed)
            .into_iter()
            .map(str::to_string)
            .collect())
    }

    /// The lexemes found in more than `fraction` of an index version's
    /// chunks, from `index_lexeme_frequency`. An index built before that
    /// table existed has no rows; its statistics are computed here once
    /// (the sentinel row marks completion) so the feature needs no rebuild.
    async fn frequent_lexemes(
        &self,
        collection_id: &str,
        index_id: &str,
        fraction: f64,
    ) -> Result<HashSet<String>> {
        // The frequency predicate runs in SQL. This is on the request path,
        // once per collection per query, and the table holds every lexeme in
        // more than ~1% of a version's chunks — tens of thousands of rows for
        // a newspaper collection — of which a stop-term fraction keeps a
        // handful. The sentinel row (empty lexeme, completion marker) is
        // fetched alongside so one round trip answers both questions.
        let fetch = |pool: sqlx::PgPool, tenant: String, col: String, idx: String| async move {
            sqlx::query_scalar::<_, String>(
                "SELECT lexeme FROM index_lexeme_frequency
                  WHERE tenant_id = $1 AND collection_id = $2 AND index_version_id = $3
                    AND (lexeme = ''
                         OR (nchunks > 0 AND ndoc::float8 > $4 * nchunks::float8))",
            )
            .bind(tenant)
            .bind(col)
            .bind(idx)
            .bind(fraction)
            .fetch_all(&pool)
            .await
            .map_err(storage_err)
        };
        let mut rows = fetch(
            self.pool.clone(),
            self.tenant_id.clone(),
            collection_id.to_string(),
            index_id.to_string(),
        )
        .await?;
        if !rows.iter().any(|lexeme| lexeme.is_empty()) {
            self.record_lexeme_frequency(collection_id, index_id)
                .await?;
            rows = fetch(
                self.pool.clone(),
                self.tenant_id.clone(),
                collection_id.to_string(),
                index_id.to_string(),
            )
            .await?;
        }
        Ok(rows.into_iter().filter(|l| !l.is_empty()).collect())
    }

    /// The corpus-observed letter-prefixed forms of a query's digit runs,
    /// over the ACTIVE index of each collection the caller may search
    /// (2026-08-30, §13.5 entry 25). `4436097` comes back as `us4436097`
    /// when — and only when — some permitted collection's corpus writes it
    /// that way; the engine holds no vocabulary.
    ///
    /// Access isolation is the CALLER's collection list: the query is keyed
    /// by tenant and by those collections' ids, so a collection a session
    /// cannot search contributes no forms — asserted by a test, because a
    /// lexeme leak is a smaller cousin of serving the document.
    ///
    /// Lazily populates a version scanned before this table existed (or by
    /// an older build), the same sentinel pattern as `frequent_lexemes`;
    /// `ON CONFLICT DO NOTHING` makes a concurrent double-populate free.
    /// Bounded: at most eight forms come back, alphabetically, so a
    /// pathological corpus cannot balloon the query.
    pub async fn number_form_lexemes(
        &self,
        collections: &[CollectionInfo],
        digits: &[String],
    ) -> Result<Vec<String>> {
        if digits.is_empty() || collections.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = collections.iter().map(|c| c.id.clone()).collect();
        // One round trip answers "which active versions exist" AND "which of
        // them have never been scanned": this runs on every turn, before the
        // search fan-out, and a per-collection EXISTS was 110+ sequential
        // queries on a deployment for a question whose answer is almost
        // always "all scanned".
        let active: Vec<(String, String, bool)> = sqlx::query_as(
            "SELECT v.collection_id, v.id,
                    EXISTS(SELECT 1 FROM index_number_lexemes n
                            WHERE n.tenant_id = v.tenant_id
                              AND n.collection_id = v.collection_id
                              AND n.index_version_id = v.id
                              AND n.digits = '') AS scanned
               FROM index_versions v
              WHERE v.tenant_id = $1 AND v.active AND v.collection_id = ANY($2)",
        )
        .bind(&self.tenant_id)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        for (collection_id, index_id, scanned) in &active {
            if !scanned {
                self.record_number_lexemes(collection_id, index_id).await?;
            }
        }
        let index_ids: Vec<String> = active.into_iter().map(|(_, idx, _)| idx).collect();
        let forms: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT lexeme FROM index_number_lexemes
              WHERE tenant_id = $1 AND index_version_id = ANY($2)
                AND digits = ANY($3) AND lexeme <> ''
              ORDER BY lexeme LIMIT 8",
        )
        .bind(&self.tenant_id)
        .bind(&index_ids)
        .bind(digits)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(forms)
    }

    /// Scan an index version for letter-prefixed number lexemes with
    /// `ts_stat` — every one, singletons included, which is what the
    /// frequency table's `ndoc` floor exists to exclude and this table
    /// exists to keep. Idempotent (the sentinel marks completion; conflicts
    /// are no-ops), so builds and lazy callers can race freely.
    pub async fn record_number_lexemes(&self, collection_id: &str, index_id: &str) -> Result<()> {
        let literal = |value: &str| value.replace('\'', "''");
        let inner = format!(
            "SELECT ts FROM collection_chunks WHERE tenant_id = '{}' AND collection_id = '{}' \
             AND index_version_id = '{}'",
            literal(&self.tenant_id),
            literal(collection_id),
            literal(index_id)
        );
        sqlx::query(
            "WITH inserted AS (
                    INSERT INTO index_number_lexemes
                        (tenant_id, collection_id, index_version_id, digits, lexeme)
                    SELECT $1, $2, $3, regexp_replace(s.word, '[^0-9]', '', 'g'), s.word
                      FROM ts_stat($4::text) AS s
                     WHERE s.word ~ '^[a-z]{1,4}[0-9]{5,}$'
                    ON CONFLICT DO NOTHING
                 )
                 INSERT INTO index_number_lexemes
                     (tenant_id, collection_id, index_version_id, digits, lexeme)
                 VALUES ($1, $2, $3, '', '')
                 ON CONFLICT DO NOTHING",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(index_id)
        .bind(&inner)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(())
    }

    /// Capture an index version's lexeme document frequencies with
    /// `ts_stat` (every lexeme in at least max(2, 1%) of its chunks, plus the
    /// '' sentinel carrying the chunk count). Idempotent: existing rows are
    /// kept. `ts_stat` takes its inner query as text, so the three ids are
    /// embedded as escaped literals — they are internal identifiers, never
    /// caller input, and the literal is a bound parameter, not spliced SQL.
    pub async fn record_lexeme_frequency(&self, collection_id: &str, index_id: &str) -> Result<()> {
        let literal = |value: &str| value.replace('\'', "''");
        let inner = format!(
            "SELECT ts FROM collection_chunks WHERE tenant_id = '{}' AND collection_id = '{}' \
             AND index_version_id = '{}'",
            literal(&self.tenant_id),
            literal(collection_id),
            literal(index_id)
        );
        sqlx::query(
            "WITH total AS (
                    SELECT count(*)::int AS n FROM collection_chunks
                     WHERE tenant_id = $1 AND collection_id = $2 AND index_version_id = $3
                 ), inserted AS (
                    INSERT INTO index_lexeme_frequency
                        (tenant_id, collection_id, index_version_id, lexeme, ndoc, nchunks)
                    SELECT $1, $2, $3, s.word, s.ndoc, total.n
                      FROM ts_stat($4::text) AS s, total
                     WHERE s.ndoc >= GREATEST(2, total.n / 100)
                    ON CONFLICT DO NOTHING
                 )
                 INSERT INTO index_lexeme_frequency
                     (tenant_id, collection_id, index_version_id, lexeme, ndoc, nchunks)
                 SELECT $1, $2, $3, '', 0, total.n FROM total
                 ON CONFLICT DO NOTHING",
        )
        .bind(&self.tenant_id)
        .bind(collection_id)
        .bind(index_id)
        .bind(&inner)
        .execute(&self.pool)
        .await
        .map_err(storage_err)?;
        Ok(())
    }

    /// Multi-collection search: each collection is searched on its own index
    /// (own partition, own envelope); the caller merges with [`merge_hits`],
    /// which re-fuses the pooled candidates globally from their raw leg
    /// scores — per-collection RRF scores are rank-derived and NOT
    /// comparable across collections (see `merge_hits`).
    pub async fn multi_search(
        &self,
        collections: &[CollectionInfo],
        query: &str,
        params: SearchParams,
    ) -> Result<Vec<CollectionSearchResult>> {
        let mut out = Vec::with_capacity(collections.len());
        for info in collections {
            let result = self
                .search_collection(&info.id, query, params.clone(), None)
                .await?;
            out.push(CollectionSearchResult {
                collection_id: info.id.clone(),
                collection_name: info.name.clone(),
                result,
            });
        }
        Ok(out)
    }
}

/// Merge per-collection results into one relevance-ordered hit list of
/// (collection_name, hit), capped at top_k, by GLOBAL reciprocal-rank fusion.
///
/// **HISTORICAL REFERENCE, no longer the serving call site (stage 5,
/// 2026-08-30).** The server merges through
/// `munarium_retrieval::merge_hits_weighted`, which runs the engine-neutral
/// pooled fusion in `munarium-datastore` — this implementation fuses from raw
/// leg scores, which is sound only while one engine produces every score, and
/// the moment engines mix it fuses incommensurable numbers silently. It is
/// kept as the ORACLE for the coordinator's equivalence tests: the swap's gate
/// was "no top-k change in postgres mode", and a bit-identity claim needs the
/// thing it is identical TO. Delete it only together with those tests, once
/// shadow data has stood in for them.
///
/// Per-collection RRF scores are rank-derived and must NOT be compared
/// across collections: every collection's rank-1 scores exactly
/// 1/(rrf_k+1) whether or not the collection is relevant, so a flatten-and-
/// sort merge degenerates into a rank-1 interleave — with `permitted
/// collections >= top_k` the context becomes one arbitrary document per
/// collection and the relevant collection's rank-2 can never surface
/// (found live on the due-diligence demo, 2026-08-24). Instead the raw leg
/// measurements — `ts_rank` (lexical) and cosine distance (vector), which
/// ARE magnitude-comparable across collections sharing one shape and one
/// embedder — re-rank the pooled candidates globally per leg, and the fused
/// score is RRF over those global ranks. Each returned hit's `score` is its
/// global fused score. Hits carrying no raw leg evidence (a legacy producer)
/// sort last, deterministically by chunk_id.
pub fn merge_hits(
    results: &[CollectionSearchResult],
    top_k: usize,
    rrf_k: f64,
) -> Vec<(String, SearchHit)> {
    merge_hits_weighted(results, top_k, rrf_k, &MergeWeights::default())
}

/// [`merge_hits`] with per-leg weights and the optional collection-evidence
/// leg; see [`MergeWeights`]. Ordering among equal fused scores stays
/// chunk-id deterministic.
pub fn merge_hits_weighted(
    results: &[CollectionSearchResult],
    top_k: usize,
    rrf_k: f64,
    weights: &MergeWeights,
) -> Vec<(String, SearchHit)> {
    let mut merged: Vec<(String, SearchHit)> = results
        .iter()
        .flat_map(|r| {
            r.result
                .hits
                .iter()
                .map(|h| (r.collection_name.clone(), h.clone()))
        })
        .collect();

    // Global per-leg orderings over the pooled candidates, one pair of
    // orderings per stratum (deep search vs. original-query probe pools) —
    // see `MergeWeights::probe_collections`.
    let is_probe = |i: usize| weights.probe_collections.contains(&merged[i].0);
    let k = if rrf_k > 0.0 { rrf_k } else { 60.0 };
    let mut fused = vec![0.0f64; merged.len()];
    for probe_stratum in [false, true] {
        let stratum_weight = if probe_stratum {
            weights.probe_weight
        } else {
            1.0
        };
        let mut lexical: Vec<usize> = (0..merged.len())
            .filter(|&i| is_probe(i) == probe_stratum && merged[i].1.lexical_score.is_some())
            .collect();
        lexical.sort_by(|&a, &b| {
            let (sa, sb) = (merged[a].1.lexical_score, merged[b].1.lexical_score);
            sb.partial_cmp(&sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(merged[a].1.chunk_id.cmp(&merged[b].1.chunk_id))
        });
        let mut vector: Vec<usize> = (0..merged.len())
            .filter(|&i| is_probe(i) == probe_stratum && merged[i].1.vector_distance.is_some())
            .collect();
        vector.sort_by(|&a, &b| {
            let (da, db) = (merged[a].1.vector_distance, merged[b].1.vector_distance);
            da.partial_cmp(&db)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(merged[a].1.chunk_id.cmp(&merged[b].1.chunk_id))
        });
        for (rank, &i) in lexical.iter().enumerate() {
            fused[i] += stratum_weight * weights.lexical / (k + rank as f64 + 1.0);
        }
        for (rank, &i) in vector.iter().enumerate() {
            fused[i] += stratum_weight * weights.vector / (k + rank as f64 + 1.0);
        }
    }
    if weights.collection_evidence > 0.0 {
        for (i, (collection, _)) in merged.iter().enumerate() {
            if let Some(&rank) = weights.collection_rank.get(collection) {
                fused[i] += weights.collection_evidence / (k + rank.max(1) as f64);
            }
        }
    }
    for (i, score) in fused.iter().enumerate() {
        merged[i].1.score = *score;
    }

    merged.sort_by(|a, b| {
        b.1.score
            .partial_cmp(&a.1.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.chunk_id.cmp(&b.1.chunk_id))
    });
    merged.truncate(if top_k == 0 { 10 } else { top_k });
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use munarium_core::retrieval::ProvenanceEnvelope;

    fn probe(name: &str, lexical: &[f64], vector: &[f64]) -> CollectionSearchResult {
        probe_with_texts(name, lexical, vector, &[])
    }

    fn probe_with_texts(
        name: &str,
        lexical: &[f64],
        vector: &[f64],
        texts: &[&str],
    ) -> CollectionSearchResult {
        let count = lexical.len().max(vector.len()).max(texts.len());
        let hits = (0..count)
            .map(|index| SearchHit {
                chunk_id: format!("{name}-{index}"),
                source_id: format!("source-{name}-{index}"),
                source_path: format!("{name}/{index}.md"),
                source_content_hash: "hash".into(),
                text: texts.get(index).copied().unwrap_or("probe").into(),
                score: 0.0,
                lexical_rank: lexical.get(index).map(|_| index as u32 + 1),
                vector_rank: vector.get(index).map(|_| index as u32 + 1),
                lexical_score: lexical.get(index).copied(),
                vector_distance: vector.get(index).copied(),
                metadata: None,
            })
            .collect();
        CollectionSearchResult {
            collection_id: format!("id-{name}"),
            collection_name: name.into(),
            result: SearchResult {
                hits,
                envelope: ProvenanceEnvelope {
                    chunk_ids: Vec::new(),
                    source_ids: Vec::new(),
                    source_paths: Vec::new(),
                    source_content_hashes: Vec::new(),
                    index_version: "index".into(),
                    event_watermark: 0,
                    provider_fingerprint: None,
                },
            },
        }
    }

    #[test]
    fn collection_selection_uses_repeated_lexical_evidence_then_vector_fallback() {
        let probes = vec![
            probe("one-title-hit", &[0.7], &[0.2]),
            probe("repeated-evidence", &[0.4, 0.35, 0.3], &[0.6]),
            probe("vector-only", &[], &[0.01, 0.02]),
            probe("empty", &[], &[]),
        ];
        // No adjacent content words in the query: phrase evidence is
        // uniformly zero and lexical density decides, as before.
        let selected = select_collection_indices(&probes, 3, "probe", 3.0);
        let names: Vec<&str> = selected
            .iter()
            .map(|&index| probes[index].collection_name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["repeated-evidence", "one-title-hit", "vector-only"]
        );
    }

    #[test]
    fn query_phrases_are_adjacent_content_word_pairs_only() {
        assert_eq!(
            query_phrases("What cities did George Washington visit?"),
            vec![
                ("george".to_string(), "washington".to_string()),
                ("washington".to_string(), "visit".to_string()),
            ]
        );
        // Every pair straddles a stop word — no phrase, so selection stays
        // on density evidence.
        assert!(query_phrases("Who was the commander at Yorktown?").is_empty());
        assert!(query_phrases("").is_empty());
        // Repeated pairs collapse; order follows the query.
        assert_eq!(
            query_phrases("tea party, the tea party"),
            vec![("tea".to_string(), "party".to_string())]
        );
    }

    /// The measured 2026-08-25 shape: a corpus that USES the query's words
    /// densely (a travel narrative saying "Washington", "cities", "visit"
    /// about the city) out-scores the corpus that is ABOUT the query's
    /// subject on `ts_rank`; the subject's own name as a phrase separates
    /// them. Density still orders collections with equal phrase evidence.
    #[test]
    fn collection_selection_prefers_phrase_evidence_over_term_density() {
        let probes = vec![
            probe_with_texts(
                "travel-narrative",
                &[0.23, 0.21, 0.20],
                &[0.7],
                &[
                    "We reached the city of Washington and visited its cities and places.",
                    "Cities to visit near Washington: Georgetown and Alexandria.",
                    "The tomb of General Washington lies below Mount Vernon.",
                ],
            ),
            probe_with_texts(
                "letterbook-b",
                &[0.17, 0.17, 0.16],
                &[0.9],
                &[
                    "# George Washington Papers, Series 2: Letterbook 17",
                    "George Washington to Henry Knox, October 1789.",
                    "Washington left New York on his tour and lodged at Rye.",
                ],
            ),
            probe_with_texts(
                "letterbook-a",
                &[0.18, 0.17, 0.17],
                &[0.9],
                &[
                    "# George Washington Papers, Series 4: General Correspondence",
                    "George Washington to Israel Putnam, May 21, 1776.",
                    "Orders for the march to the North River.",
                ],
            ),
            probe("no-phrase-dense", &[0.3, 0.3, 0.3], &[0.5]),
        ];
        let selected =
            select_collection_indices(&probes, 3, "What cities did George Washington visit?", 3.0);
        let names: Vec<&str> = selected
            .iter()
            .map(|&index| probes[index].collection_name.as_str())
            .collect();
        // Both letterbooks (phrase in 2 of 3 hits) lead despite the weakest
        // density; density orders them (0.52 vs 0.50). The two phrase-less
        // collections follow in density order, so the narrative — the
        // strongest collection on density alone — is cut at max_collections.
        assert_eq!(
            names,
            vec!["letterbook-a", "letterbook-b", "no-phrase-dense"]
        );
    }

    /// The measured 2026-08-25 merge shape: a collection dense in the
    /// expansion vocabulary wins the raw lexical leg, the bag-of-words vector
    /// leg's rank-1 is a fragment, and the collection the probe ranked first
    /// is starved. Default weights reproduce `merge_hits`; the evidence leg
    /// and a lower vector weight change the order as configured.
    #[test]
    fn weighted_merge_defaults_match_merge_hits_and_evidence_leg_reorders() {
        // Legs that disagree completely, as measured: lexical hits carry no
        // vector rank and the vector leg's best is a fragment.
        let results = vec![
            // Strongest raw lexical scores (dense in expansion words); the
            // probe ranked this collection second.
            probe("narrative", &[0.30, 0.29], &[]),
            // The subject's collection: weaker lexical, evidence rank 1.
            probe("letterbook", &[0.20, 0.19], &[]),
            // A fragment: no lexical match, vector rank 1.
            probe("fragments", &[], &[0.1]),
        ];
        let unweighted = merge_hits(&results, 5, 60.0);
        let default_weighted = merge_hits_weighted(&results, 5, 60.0, &MergeWeights::default());
        assert_eq!(
            unweighted
                .iter()
                .map(|(_, h)| (h.chunk_id.clone(), h.score))
                .collect::<Vec<_>>(),
            default_weighted
                .iter()
                .map(|(_, h)| (h.chunk_id.clone(), h.score))
                .collect::<Vec<_>>()
        );
        // Unweighted: the fragment's vector rank 1 and the narrative's
        // lexical rank 1 both score 1/61 (chunk-id order puts the fragment
        // first); the letterbook's hits trail every narrative hit.
        let order: Vec<&str> = unweighted.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "fragments",
                "narrative",
                "narrative",
                "letterbook",
                "letterbook"
            ]
        );

        let weights = MergeWeights {
            lexical: 1.0,
            vector: 0.3,
            collection_evidence: 2.0,
            collection_rank: [("letterbook".to_string(), 1), ("narrative".to_string(), 2)]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let weighted = merge_hits_weighted(&results, 5, 60.0, &weights);
        let order: Vec<&str> = weighted.iter().map(|(c, _)| c.as_str()).collect();
        // letterbook-0: 1/63 + 2/61 = 0.04866   narrative-0: 1/61 + 2/62 = 0.04865
        // letterbook-1: 1/64 + 2/61 = 0.04841   narrative-1: 1/62 + 2/62 = 0.04839
        // fragments-0:  0.3/61 = 0.00492 (no evidence rank) — the tail.
        assert_eq!(
            order,
            vec![
                "letterbook",
                "narrative",
                "letterbook",
                "narrative",
                "fragments"
            ]
        );
        assert!(weighted.windows(2).all(|w| w[0].1.score >= w[1].1.score));
    }

    /// The Tea Party shape (measured 2026-08-25): the query's phrase is later
    /// coinage, so no pool carries it in more than a few percent of hits —
    /// weak phrase evidence must not override density, and the newspaper
    /// shard that actually holds the reports (densest, phrase-free) must
    /// lead. With boost 0 the phrase signal is off entirely.
    #[test]
    fn weak_phrase_evidence_does_not_override_density() {
        let mut narrative_texts = vec!["The city and its press, described at length."; 19];
        narrative_texts.push("Reprinting what the colonial newspapers said of it.");
        let probes = vec![
            probe_with_texts("narrative", &[0.13, 0.12, 0.11], &[0.8], &narrative_texts),
            probe_with_texts(
                "newspaper",
                &[0.16, 0.15, 0.14],
                &[0.8],
                &["Boston, December 20. The tea was destroyed last Thursday."; 20],
            ),
        ];
        let query = "How did colonial newspapers report the Boston Tea Party?";
        let names = |selected: Vec<usize>| -> Vec<&str> {
            selected
                .iter()
                .map(|&index| probes[index].collection_name.as_str())
                .collect()
        };
        // newspaper 0.45 × 1 = 0.45; narrative 0.36 × (1 + 3 × 0.05) = 0.41
        // — density leads.
        assert_eq!(
            names(select_collection_indices(&probes, 2, query, 3.0)),
            vec!["newspaper", "narrative"]
        );
        assert_eq!(
            names(select_collection_indices(&probes, 2, query, 0.0)),
            vec!["newspaper", "narrative"]
        );
        // A strong phrase signal (every narrative hit carries it) does win at
        // the same densities: 0.36 × 4 = 1.44 > 0.45.
        let strong = vec![
            probe_with_texts(
                "narrative",
                &[0.13, 0.12, 0.11],
                &[0.8],
                &["Reprinting what the colonial newspapers said of it."; 20],
            ),
            probes[1].clone(),
        ];
        let names_strong: Vec<&str> = select_collection_indices(&strong, 2, query, 3.0)
            .iter()
            .map(|&index| strong[index].collection_name.as_str())
            .collect();
        assert_eq!(names_strong, vec!["narrative", "newspaper"]);
    }

    /// Probe pools carry original-query scores (~0.2) where the deep search
    /// carries expanded-query scores (~0.03); compared raw, the probe stratum
    /// wins the lexical leg wholesale. Ranked as its own stratum, a probe
    /// rank-1 counts like a deep rank-1 and the evidence leg arbitrates.
    #[test]
    fn probe_pools_are_ranked_in_their_own_stratum() {
        let results = vec![
            probe("deep-letterbook", &[0.030, 0.028], &[]),
            probe("probe-narrative", &[0.20, 0.19], &[]),
        ];
        // Raw comparison (no strata): the narrative's two hits lead.
        let raw = merge_hits_weighted(&results, 4, 60.0, &MergeWeights::default());
        assert_eq!(
            raw.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(),
            vec![
                "probe-narrative",
                "probe-narrative",
                "deep-letterbook",
                "deep-letterbook"
            ]
        );
        // Strata + evidence (letterbook rank 1, narrative rank 20): the
        // letterbook's rank-1 (1/61 + 1/61) beats the narrative's rank-1
        // (1/61 + 1/80), and so on down.
        let weights = MergeWeights {
            collection_evidence: 1.0,
            collection_rank: [
                ("deep-letterbook".to_string(), 1),
                ("probe-narrative".to_string(), 20),
            ]
            .into_iter()
            .collect(),
            probe_collections: ["probe-narrative".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let strata = merge_hits_weighted(&results, 4, 60.0, &weights);
        assert_eq!(
            strata.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(),
            vec![
                "deep-letterbook",
                "deep-letterbook",
                "probe-narrative",
                "probe-narrative"
            ]
        );
        // probe_weight scales only the probe stratum.
        let half = MergeWeights {
            probe_weight: 0.5,
            ..weights.clone()
        };
        let scaled = merge_hits_weighted(&results, 4, 60.0, &half);
        let narrative_top = scaled
            .iter()
            .find(|(c, _)| c == "probe-narrative")
            .map(|(_, h)| h.score)
            .unwrap();
        let expected = 0.5 / 61.0 + 1.0 / 80.0;
        assert!((narrative_top - expected).abs() < 1e-12);
    }

    #[test]
    fn prefilter_is_the_or_of_anded_lexeme_pairs() {
        let printed = "'citi' & 'georg' & 'washington' & 'visit'";
        let lexemes = tsquery_lexemes(printed);
        assert_eq!(
            lexemes,
            vec!["'citi'", "'georg'", "'washington'", "'visit'"]
        );
        assert_eq!(
            pairs_tsquery(&lexemes).unwrap(),
            "('citi' & 'georg') | ('citi' & 'washington') | ('citi' & 'visit') \
             | ('georg' & 'washington') | ('georg' & 'visit') | ('washington' & 'visit')"
        );
        // Fewer than two lexemes: no pair, no filter — the OR leg as is.
        assert_eq!(pairs_tsquery(&tsquery_lexemes("'yorktown'")), None);
        assert_eq!(pairs_tsquery(&tsquery_lexemes("")), None);
        // Nineteen lexemes → 171 pairs, one query.
        let many: Vec<String> = (0..19).map(|i| format!("'t{i}'")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(pairs_tsquery(&refs).unwrap().matches(" | ").count(), 170);
    }

    #[test]
    fn candidate_predicate_drops_stop_terms_then_applies_the_pair_rule() {
        let lexemes: Vec<String> = ["'citi'", "'georg'", "'washington'", "'visit'"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let none: HashSet<String> = HashSet::new();
        let washington: HashSet<String> = ["washington".to_string()].into_iter().collect();
        let all: HashSet<String> = ["citi", "georg", "washington", "visit"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Nothing dropped, no pair rule: None — the SQL's own OR query stands.
        assert_eq!(candidate_tsquery(&lexemes, &none, 1), None);
        // A stop term dropped at minimumShouldMatch 1: the OR of the rest.
        assert_eq!(
            candidate_tsquery(&lexemes, &washington, 1).as_deref(),
            Some("'citi' | 'georg' | 'visit'")
        );
        // Pair rule over the rest.
        assert_eq!(
            candidate_tsquery(&lexemes, &washington, 2).as_deref(),
            Some("('citi' & 'georg') | ('citi' & 'visit') | ('georg' & 'visit')")
        );
        // Every lexeme a stop term: the full set is kept, never an empty
        // predicate (and with no pair rule that is "nothing changed").
        assert_eq!(candidate_tsquery(&lexemes, &all, 1), None);
        assert!(candidate_tsquery(&lexemes, &all, 2)
            .unwrap()
            .starts_with("('citi' & 'georg')"));
        // One surviving lexeme under the pair rule: no pair possible, so
        // the survivor alone is the predicate.
        let two: Vec<String> = vec!["'washington'".into(), "'tour'".into()];
        assert_eq!(
            candidate_tsquery(&two, &washington, 2).as_deref(),
            Some("'tour'")
        );
    }
}

#[cfg(test)]
mod prepared_query_tests {
    use super::*;
    use munarium_core::retrieval::QueryEmbedder;

    fn params_with(expansions: Vec<QueryExpansionRule>, weight: f64) -> SearchParams {
        SearchParams {
            query_expansions: expansions,
            query_expansion_weight: weight,
            ..SearchParams::default()
        }
    }

    fn rule(when: &str, add: &[&str]) -> QueryExpansionRule {
        QueryExpansionRule {
            when_any: vec![when.to_string()],
            add_terms: add.iter().map(|t| t.to_string()).collect(),
        }
    }

    /// The whole justification for hoisting preparation out of the search path
    /// is that it does not CHANGE the query. If the prepared vector differed
    /// from the one the old inline path produced, every measured retrieval
    /// result on every corpus would silently move.
    #[test]
    fn the_prepared_vector_is_bit_identical_to_the_inline_one() {
        let cases: Vec<(&str, Vec<QueryExpansionRule>, f64)> = vec![
            ("Boston Tea Party", vec![], 1.0),
            (
                "Boston Tea Party",
                vec![rule("tea", &["cargo", "harbour"])],
                1.0,
            ),
            (
                "Boston Tea Party",
                vec![rule("tea", &["cargo", "harbour"])],
                0.0,
            ),
            (
                "Boston Tea Party",
                vec![rule("tea", &["cargo", "harbour"])],
                0.5,
            ),
            (
                "open pipeline by region EMEA",
                vec![rule("region", &["emea"])],
                0.25,
            ),
            ("", vec![], 1.0),
        ];
        for (query, expansions, weight) in cases {
            let params = params_with(expansions, weight);
            // Exactly what the old code did inline, inside search_collection:
            let expanded = expand_query(query, &params.query_expansions);
            let w = params.query_expansion_weight.clamp(0.0, 1.0);
            let inline = weighted_query_embedding(query, &expanded, w as f32);

            let prepared = PgRetrieval::prepare_query(query, &params, &crate::LocalHashEmbedder);
            let got = prepared.embedding.expect("an embedder always produces one");

            assert_eq!(
                got.as_ref(),
                inline.as_slice(),
                "prepared vector differs for {query:?} at weight {weight}"
            );
        }
    }

    /// Expansion is the other half the search path used to derive. A drift here
    /// changes the tsquery text Postgres parses, not merely the ranking.
    #[test]
    fn the_prepared_expansion_matches_the_inline_one() {
        let params = params_with(vec![rule("tea", &["cargo", "harbour"])], 1.0);
        let prepared =
            PgRetrieval::prepare_query("Boston Tea Party", &params, &crate::LocalHashEmbedder);
        let plan = prepared.lexical.expect("a lexical plan");
        assert_eq!(plan.original, "Boston Tea Party");
        assert_eq!(
            plan.expanded,
            expand_query("Boston Tea Party", &params.query_expansions)
        );
        assert_ne!(plan.original, plan.expanded, "the rule should have fired");
    }

    /// The weight was clamped inside the search path. The clamp has to survive
    /// the move, or an out-of-range runbook value quietly changes meaning.
    #[test]
    fn the_expansion_weight_is_clamped_when_prepared() {
        for (given, want) in [(-1.0, 0.0), (0.5, 0.5), (2.0, 1.0)] {
            let params = params_with(vec![rule("tea", &["cargo"])], given);
            let prepared = PgRetrieval::prepare_query("tea", &params, &crate::LocalHashEmbedder);
            assert_eq!(prepared.lexical.unwrap().expansion_weight, want);
        }
    }

    /// Embedding twice and mixing is NOT the same as embedding once, so the
    /// early exits are load-bearing: a "simplification" that always blends
    /// would be a silent behaviour change.
    #[test]
    fn the_blend_early_exits_are_preserved() {
        let e = crate::LocalHashEmbedder;
        assert_eq!(e.blend("a", "a", 0.5), e.embed("a"), "identical texts");
        assert_eq!(
            e.blend("a", "b", 1.0),
            e.embed("b"),
            "weight 1 = expanded only"
        );
        assert_eq!(
            e.blend("a", "b", 0.0),
            e.embed("a"),
            "weight 0 = original only"
        );
        assert_ne!(
            e.blend("a", "b", 0.5),
            e.embed("b"),
            "a real blend is not the expanded embedding"
        );
    }

    /// The bit-identity test above compares the prepared vector to the inline
    /// formula, which shares the blend function -- so on its own it could pass
    /// while `prepare_query` quietly dropped the weight or the expansion. This
    /// pins that they actually REACH the embedder: three weights over the same
    /// query must produce three different vectors, and the endpoints must be
    /// exactly the single-text embeddings.
    #[test]
    fn the_weight_and_expansion_reach_the_embedder() {
        let e = crate::LocalHashEmbedder;
        let expansions = vec![rule("tea", &["cargo", "harbour"])];
        let at = |w: f64| {
            PgRetrieval::prepare_query("Boston Tea Party", &params_with(expansions.clone(), w), &e)
                .embedding
                .expect("an embedding")
        };
        let (low, mid, high) = (at(0.0), at(0.5), at(1.0));
        assert_ne!(low, mid, "weight 0 and 0.5 must differ");
        assert_ne!(mid, high, "weight 0.5 and 1 must differ");
        assert_ne!(low, high, "weight 0 and 1 must differ");

        assert_eq!(
            low.as_ref(),
            e.embed("Boston Tea Party").as_slice(),
            "weight 0 is the ORIGINAL query's vector"
        );
        assert_eq!(
            high.as_ref(),
            e.embed(&expand_query("Boston Tea Party", &expansions))
                .as_slice(),
            "weight 1 is the EXPANDED query's vector"
        );
    }

    /// The fingerprint is what the provenance envelope records as embedder
    /// identity, so it must keep naming the same thing.
    #[test]
    fn the_embedder_fingerprint_is_unchanged() {
        let e = crate::LocalHashEmbedder;
        assert_eq!(e.fingerprint(), "local/local-hash@1/256");
        assert_eq!(e.dimensions(), EMBED_DIMS);
    }
}

#[cfg(test)]
mod number_form_tests {
    use super::number_query_digits;

    #[test]
    fn a_grouped_number_contributes_its_joined_digits() {
        assert_eq!(
            number_query_digits("US patent 4,436,097 issued"),
            vec!["4436097"]
        );
    }

    #[test]
    fn a_bare_long_run_contributes_itself_with_leading_zeros_intact() {
        assert_eq!(
            number_query_digits("serial 0056401 please"),
            vec!["0056401"]
        );
    }

    #[test]
    fn a_letter_prefixed_form_contributes_its_suffix() {
        assert_eq!(number_query_digits("what is US4436097?"), vec!["4436097"]);
    }

    #[test]
    fn decimals_dates_years_and_short_numbers_stay_ordinary() {
        // An amount's integer part is not an identifier.
        assert!(number_query_digits("revenue was $1,234,567.89 that year").is_empty());
        // Years, ISO dates, 8-digit date forms, four-and-under runs.
        assert!(number_query_digits("between 2025 and 2026").is_empty());
        assert!(number_query_digits("as of 2026-06-30").is_empty());
        assert!(number_query_digits("on 20260630 exactly").is_empty());
        assert!(number_query_digits("1,234 items in 45 boxes").is_empty());
        // A long alphanumeric tail is a token of its own, not a number form.
        assert!(number_query_digits("code US4436097x rev").is_empty());
    }

    #[test]
    fn expansion_is_bounded_and_deduplicated() {
        let q = "4,436,097 4,436,097 11111 22222 33333 44444 55555 66666 77777 88888 99999";
        let got = number_query_digits(q);
        assert_eq!(got.len(), 8, "{got:?}");
        assert_eq!(got[0], "4436097");
    }

    #[test]
    fn a_nonnumeric_query_short_circuits_to_nothing() {
        assert!(number_query_digits("What did Washington write to Congress?").is_empty());
    }
}
