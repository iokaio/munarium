# Lexical-compatibility corpus

The PostgreSQL analyzer oracle that the Munarium Tantivy tokenizer is measured
against. Token **classification** — not stemming — is the principal parity risk
between the two engines, and the open question is which differences are
accepted. This directory is the evidence for that answer.

## Files

| File | What it is |
|---|---|
| `inputs.sql` | The corpus definition: 46 strings, each tagged with a token class and `harvested` or `constructed` provenance. Committed so the capture is regenerable. |
| `capture.sql` | The query that emits the oracle. Records four views per string — see its header for why each one is needed. |
| `pg16-english.jsonl` | The captured oracle: one JSON object per input. **This is the fixture.** |

## Regenerating

```bash
cd server
docker compose up -d postgres
F=src/munarium-datastore/tests/fixtures/lexical-compat
docker compose exec -T postgres psql -U munarium -d munarium -v ON_ERROR_STOP=1 -q \
  < $F/inputs.sql
docker compose exec -T postgres psql -U munarium -d munarium -v ON_ERROR_STOP=1 -tA \
  < $F/capture.sql \
  > $F/pg16-english.jsonl
```

The `NOTICE: text-search query contains only stop words` lines on stderr are
expected — `stop-01`, `empty-01` and `ws-01` exist precisely to capture that.

## What was captured, and against what

PostgreSQL 16.15 (`pgvector/pgvector:pg16`), `default` parser, `english`
configuration — the same configuration `munarium-retrieval-pg` uses, which
matters: the crate builds its query lexemes by round-tripping
`plainto_tsquery('english', $1)::text`, so the `plainto_tsquery` column is not a
hypothetical surface but the exact string that function parses today.

The oracle is **version- and configuration-bound**. It is valid for pg16 with the
stock English configuration and nothing else. If the deployed server's text
search configuration ever diverges from `english`, or the major version moves,
this file must be recaptured before it is used to judge a tokenizer.

## Provenance is load-bearing

`harvested` rows are verbatim strings found by grep in corpora of the shapes the
sample runbooks describe — `threat-reports` CVEs, IPs and defanged hosts, `patent-documents`
serials and publication numbers, `advisory-records` currency and percentages,
`knowledge-sources` versions and URLs, `dataroom-documents` hyphenated compounds.
`constructed` rows cover a class the harvest could not reach cheaply.

The distinction decides whether a parity difference matters. A difference on a
constructed string proves the parser behaves a certain way; a difference on a
harvested string proves Munarium's own corpora will hit it.

## The finding

In summary: 46 strings produce **17 distinct token classes**, and 6 of them emit
**overlapping tokens** — the whole compound *and* its parts at consecutive
positions. A tokenizer that splits on whitespace and punctuation reproduces
neither the classes nor the positions.
