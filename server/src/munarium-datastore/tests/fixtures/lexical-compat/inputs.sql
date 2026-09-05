-- Lexical-compatibility corpus: the inputs.
--
-- better-plan.md section 6.1 names the principal parity risk as token
-- CLASSIFICATION, not stemming. PostgreSQL's default parser recognises distinct
-- URL, host, email, file, version, numeric, alphanumeric and hyphenated-word
-- classes and can emit BOTH a whole compound and its parts. Tantivy's default
-- tokenizer splits on whitespace and punctuation. Every row below is a string
-- where those two behaviours may differ, and where the difference would change
-- what a Munarium corpus can be searched for.
--
-- `provenance` is load-bearing. 'harvested' rows are verbatim strings found by
-- grep in a corpus of the shape `source` names; 'constructed' rows are written by hand to
-- cover a class the harvest could not reach cheaply. A reader deciding whether a
-- parity difference matters needs to know which is which -- a constructed string
-- proves the parser's behaviour but not that Munarium's corpora contain it.
--
-- Regenerate the capture with:
--   docker compose -f server/docker-compose.yml up -d postgres
--   psql < inputs.sql && psql -f capture.sql -tA > pg16-english.jsonl

DROP TABLE IF EXISTS lexcompat_inputs;
CREATE TABLE lexcompat_inputs (
    id         text PRIMARY KEY,
    class      text NOT NULL,
    provenance text NOT NULL CHECK (provenance IN ('harvested', 'constructed')),
    source     text,
    body       text NOT NULL
);

INSERT INTO lexcompat_inputs (id, class, provenance, source, body) VALUES
-- ---------------------------------------------------------------- identifiers
('cve-01',    'cve',            'harvested',   'threat-reports', 'CVE-2025-31337'),
('cve-02',    'cve',            'constructed', NULL, 'Patched in CVE-2024-3094 and CVE-2021-44228.'),
('patent-01', 'patent-serial',  'harvested',   'patent-documents', '07/638,337'),
('patent-02', 'patent-serial',  'harvested',   'patent-documents', '08/225,502'),
('patent-03', 'patent-pubno',   'harvested',   'patent-documents', 'US7654321B2'),
('patent-04', 'patent-serial',  'constructed', NULL, 'Application 10/123,456 was rejected over US7654321B2.'),
-- --------------------------------------------------------------------- network
('ip-01',     'ipv4',           'harvested',   'threat-reports', '198.51.100.101'),
('ip-02',     'ipv4',           'constructed', NULL, 'Beaconing to 198.51.100.140 every 300 seconds.'),
('host-01',   'host',           'constructed', NULL, 'cdn-sync.example.com'),
('defang-01', 'defanged-host',  'harvested',   'threat-reports', 'cloud-sync[.]example'),
('defang-02', 'defanged-host',  'harvested',   'threat-reports', 'grayfrost-leaks[.]example'),
('url-01',    'url',            'harvested',   'knowledge-sources', 'http://schemas.openxmlformats.org/package/2006/content-types'),
('url-02',    'url',            'constructed', NULL, 'See https://docs.example.com/v2/api-reference#auth for details.'),
('email-01',  'email',          'constructed', NULL, 'escalate to support-team@vale-advisors.example.com'),
-- ---------------------------------------------------------------------- hashes
('hash-01',   'sha256',         'harvested',   'threat-reports', '4f1c9ad2e77b3a6058c1de44b0f9a217c3e8d5619b02fa7c4d81e6a390bb57c2'),
('hash-02',   'md5-like',       'constructed', NULL, 'd41d8cd98f00b204e9800998ecf8427e'),
-- --------------------------------------------------------------------- numbers
('money-01',  'currency',       'harvested',   'advisory-records', '$0.00'),
('money-02',  'currency',       'constructed', NULL, 'Consideration of $2,520,000.50 payable at closing.'),
('money-03',  'currency-trap',  'constructed', NULL, 'The register says 900000.50 but the minutes say 900000.5'),
('pct-01',    'percent',        'harvested',   'advisory-records', '0.1%'),
('num-01',    'signed',         'constructed', NULL, 'A variance of -12.75 against +3.5 in the prior quarter.'),
('num-02',    'scientific',     'constructed', NULL, 'Tolerance 1.2e-9 metres.'),
-- -------------------------------------------------------------------- versions
('ver-01',    'semver',         'harvested',   'knowledge-sources', '4.2.1'),
('ver-02',    'semver',         'constructed', NULL, 'Fixed in release v4.2.1 and backported to 3.11.7-rc2.'),
-- ----------------------------------------------------------------------- paths
('path-01',   'unix-path',      'constructed', NULL, '/var/lib/munarium/indexes/manifest.json'),
('path-02',   'win-path',       'constructed', NULL, 'C:\Users\operator\corpus_text\DV-0003.docx'),
('file-01',   'filename',       'constructed', NULL, 'The attachment MINUTES-board-2026-05.md is the controlling record.'),
-- ------------------------------------------------------------ compound / hyphen
('hyph-01',   'hyphenated',     'constructed', NULL, 'A cloud-native multi-tenant retrieval tier.'),
('hyph-02',   'hyphenated-num', 'constructed', NULL, 'Part number ABC-1234-XY revision 2.'),
('hyph-03',   'hyphenated',     'harvested',   'dataroom-documents', 'well-established'),
-- ------------------------------------------------------------------ regulatory
('cfr-01',    'legal-cite',     'constructed', NULL, 'Violations of 21 CFR 211.100 were cited in the warning letter.'),
-- ------------------------------------------------------- stemming / stop words
('stem-01',   'stemming',       'constructed', NULL, 'running runs ran runner runnings'),
('stem-02',   'stemming',       'constructed', NULL, 'organize organizes organizing organization organizational'),
('stop-01',   'stop-only',      'constructed', NULL, 'the and of to a an is are was were'),
('stop-02',   'stop-mixed',     'constructed', NULL, 'What cities did George Washington visit?'),
('empty-01',  'empty',          'constructed', NULL, ''),
('ws-01',     'whitespace',     'constructed', NULL, '   '),
-- --------------------------------------------------------- unicode / case / len
('uni-01',    'unicode-accent', 'constructed', NULL, 'Cafe naive resume versus Café naïve résumé Zürich Ångström'),
('uni-02',    'cjk',            'constructed', NULL, 'Tokyo 東京 supply chain 供給連鎖'),
('uni-03',    'punct-heavy',    'constructed', NULL, '“Quoted,” (parenthetical); em—dash – and ellipsis…'),
('case-01',   'case',           'constructed', NULL, 'PostgreSQL POSTGRESQL postgresql PostGreSQL'),
('long-01',   'token-length',   'constructed', NULL, 'supercalifragilisticexpialidociousandthensomemoretomakeitlongerthanthedefaulttokenlengthlimit'),
-- ------------------------------------------------------------ real query shapes
('query-01',  'query',          'harvested',   'archival-documents', 'What cities did George Washington visit?'),
('query-02',  'query',          'constructed', NULL, 'Boston Tea Party'),
('query-03',  'query',          'constructed', NULL, 'open pipeline by region EMEA'),
('query-04',  'query',          'constructed', NULL, 'CVE-2025-31337 exploitation by an actor alias');
