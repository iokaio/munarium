-- Lexical-compatibility corpus: the capture.
--
-- Emits one JSON object per input row. This is the oracle the Munarium Tantivy
-- tokenizer is measured against (see the README beside this file), and the
-- reason it records four different views of the same string:
--
--   parse    ts_parse('default', body) -- the raw parser output IN ORDER, with
--            the numeric token id and its alias/description. This is the token
--            CLASSIFICATION layer, and it is the half a whitespace tokenizer
--            cannot reproduce: it is where PostgreSQL decides that
--            `cdn-sync.example.com` is a `host`, that `07/638,337` is a `file`,
--            and where a hyphenated word is emitted as the whole compound AND
--            its parts.
--
--   debug    ts_debug('english', body) -- which dictionary each token was routed
--            to and what lexemes came back. An empty `lexemes` array means the
--            token was STOPPED, which is different from not being recognised;
--            NULL dictionary means no dictionary accepted it at all. Sharing the
--            name "snowball english" with Tantivy proves nothing on its own,
--            which is exactly why this column is captured rather than assumed.
--
--   tsvector to_tsvector('english', body) -- the indexed form, WITH POSITIONS.
--            Positions are not decoration: the current phrase and substring
--            demotions depend on them, so a tokenizer that gets the lexemes
--            right and the positions wrong is still a parity failure.
--
--   tsquery  plainto_tsquery / phraseto_tsquery -- the QUERY side of the same
--            analyzer. munarium-retrieval-pg round-trips plainto_tsquery(...)
--            ::text to obtain query lexemes, so this is not a hypothetical
--            surface; it is the exact string that function parses today.
--
-- Output is JSONL. Run with -tA so psql emits raw unaligned rows:
--   psql -f capture.sql -tA > pg16-english.jsonl

SELECT jsonb_build_object(
    'id',         i.id,
    'class',      i.class,
    'provenance', i.provenance,
    'source',     i.source,
    'body',       i.body,
    'tsvector',   to_tsvector('english', i.body)::text,
    'plainto_tsquery',  plainto_tsquery('english', i.body)::text,
    'phraseto_tsquery', phraseto_tsquery('english', i.body)::text,
    'parse', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
                   'ord',    p.ord,
                   'tokid',  p.tokid,
                   'alias',  t.alias,
                   'descr',  t.description,
                   'token',  p.token)
               ORDER BY p.ord)
        FROM (
            SELECT row_number() OVER () AS ord, tokid, token
            FROM ts_parse('default', i.body)
        ) p
        JOIN ts_token_type('default') t ON t.tokid = p.tokid
    ), '[]'::jsonb),
    'debug', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
                   'ord',        d.ord,
                   'alias',      d.alias,
                   'descr',      d.description,
                   'token',      d.token,
                   'dictionary', d.dictionary::text,
                   'lexemes',    d.lexemes)
               ORDER BY d.ord)
        FROM (
            SELECT row_number() OVER () AS ord, *
            FROM ts_debug('english', i.body)
        ) d
    ), '[]'::jsonb)
)
FROM lexcompat_inputs i
ORDER BY i.id;
