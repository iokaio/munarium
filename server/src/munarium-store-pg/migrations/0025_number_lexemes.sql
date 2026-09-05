-- Number-form lexemes (2026-08-30, dev-guide §13.5 entry 25; the demo tuning
-- study's class A). The corpus writes `US4436097`; a person writes
-- `4,436,097`. Postgres' parser makes the first one token and the second
-- three, so no lexical setting, the bag-of-words vector leg, or model query
-- expansion (whose prompt forbids numbers) can connect them.
--
-- Per built index version: every letter-prefixed number lexeme the corpus
-- actually holds, keyed by its digit suffix — `4436097 -> us4436097`. Derived
-- from the corpus itself with ts_stat, like index_lexeme_frequency beside it,
-- and INCLUDING singletons: the frequency table's ndoc >= max(2, 1%) floor is
-- exactly why it cannot serve this purpose — an identifier that appears once
-- is the identifier someone asks about.
--
-- A sentinel row (digits = '', lexeme = '') records that the version was
-- scanned, so an index with no such lexemes is not rescanned per query. Rows
-- retire with the version's chunks.
CREATE TABLE IF NOT EXISTS index_number_lexemes (
    tenant_id        TEXT NOT NULL,
    collection_id    TEXT NOT NULL,
    index_version_id TEXT NOT NULL,
    digits           TEXT NOT NULL,
    lexeme           TEXT NOT NULL,
    PRIMARY KEY (tenant_id, collection_id, index_version_id, digits, lexeme)
);
