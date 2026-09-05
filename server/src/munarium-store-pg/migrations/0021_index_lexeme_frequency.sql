-- Corpus-adaptive stop terms (2026-08-25, dev-guide §13.5 entry 21).
--
-- Per built index version, the lexemes that occur in a meaningful share of
-- the collection's chunks (ndoc >= max(2, 1% of nchunks)), captured with
-- `ts_stat` at the end of a build (and lazily, once, for an index built
-- before this table existed). At query time the runbook's
-- `retrieval.stopTermFraction` drops any query lexeme found in more than
-- that fraction of a collection's chunks from the lexical CANDIDATE
-- predicate — the word still counts toward the rank, it just no longer
-- makes every chunk holding it a candidate ("washington" in a Washington
-- letterbook shard matches nearly every row). Standard IR (dynamic
-- stopwords), derived from the corpus itself; the engine holds no
-- vocabulary.
--
-- A sentinel row with lexeme = '' records that statistics were computed for
-- the version (its ndoc is 0; nchunks is the version's chunk count), so an
-- index with no frequent lexemes is not recomputed on every query. Rows are
-- removed with the version's chunks by retireOld.
CREATE TABLE IF NOT EXISTS index_lexeme_frequency (
    tenant_id        TEXT    NOT NULL,
    collection_id    TEXT    NOT NULL,
    index_version_id TEXT    NOT NULL,
    lexeme           TEXT    NOT NULL,
    ndoc             INTEGER NOT NULL,
    nchunks          INTEGER NOT NULL,
    PRIMARY KEY (tenant_id, collection_id, index_version_id, lexeme)
);
