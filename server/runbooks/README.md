# Sample runbooks

Declarative pipelines and retrieval applications you can apply to a running
munarium-server. Three kinds of document live here:

| Directory | Kind | What it is |
|---|---|---|
| [`shapes/`](shapes/) | `Shape` | The fact schema, supersession identity, chunking and indexing a collection is governed by. |
| [`pipelines/`](pipelines/) | `Runbook` (v1) | A single-shape reindex pipeline (`tickets-reindex`) following the same five-step shape as [architecture.md §7](../docs/architecture.md)'s worked example (`cuad-reindex@2`). |
| [`applications/`](applications/) | `Runbook` (v2) | One worked retrieval application per corpus, expressed as server-side collections. |

Everything here parses through `munarium_runbooks::parse_runbook` /
`munarium_shapes::parse_shape` and validates clean (see
[Validation](#validation) for the two deliberate exceptions).

## Applying one

A collection cannot bind to an unpublished shape, so shapes go first:

```bash
mmctl apply -f runbooks/shapes/dataroom-documents.yaml
mmctl apply -f runbooks/applications/due-diligence.yaml
mmctl runbook validate -f runbooks/applications/due-diligence.yaml   # deterministic findings
mmctl run due-diligence --watch
mmctl approve <run-id> <cutover-step-ordinal>                       # cutover is gated
```

Every application runbook runs the same five steps — `resolveSources`,
`buildIndex`, `verify`, `cutover: {approval: required}`,
`retireOld: {keep_versions: 2}` — once per collection. The build is
side-by-side and the approval gate is where a human decides the new index goes
live.

## Authoring your own set (guided)

You do not need these samples as your only teacher to build a well-designed
set. A running server carries the guided authoring surface: the seven
application patterns of dev-guide §19 served as a catalog (each naming which
of these files to start from, and what the pattern is strongest at), a
§16-ordered interview whose answers deterministically
materialize a shape + runbook, set-level validation the per-document
validators cannot do (`set.shape-unresolved`, `set.shape-version-conflict`,
`set.prefix-shadows-restricted`, `set.answer-key-filename`, …), an optional
BYOK AI drafting pass, and a hash-manifested export bundle:

```bash
mmctl author patterns                       # the pattern catalog
mmctl author new my-app --pattern ask-the-corpus
mmctl author answer <draft-id> -f answers.yaml   # re-materializes + validates
mmctl author assist <draft-id>              # optional BYOK refinement
mmctl author export <draft-id> --out out/   # writes shapes/, runbooks/, bundle.json (hash-verified)
git add out/ && git commit                     # git stays the source of truth
mmctl bundle apply -f out/bundle.json       # PROD: verifies hashes, then the same
                                               # kind-sniffed /v1/shapes + /v1/runbooks applies
```

The same loop is available raw over `/v1/authoring/*` (see
docs/api/rest.md); the `/admin/authoring` pages that once mirrored it were
removed 2026-08-27 — the operator console instead shows the applied result
(`/admin/runbooks`, `/admin/shapes/{ref}`). Export refuses while error
findings exist, and `bundle apply` dies on any hash drift between export
and deploy — so what reaches production is exactly the validated set that
left the authoring server.

## The sample set

Each runbook maps one corpus onto server collections, and its header says
which modelling decision it is there to demonstrate. **The corpora themselves
are not shipped in this repository** —
[docs/guides/loading-corpora.md](../docs/guides/loading-corpora.md) says which
are public datasets you can obtain yourself and how to point a runbook at a
corpus of your own.

| Runbook | Shape | Collections | What it demonstrates |
|---|---|---|---|
| [customer-support](applications/customer-support.yaml) | helpdesk-tickets@1 | 2 | One source system split into two collections at different exposure; binding by media type as well as prefix |
| [due-diligence](applications/due-diligence.yaml) | dataroom-documents@1 | 13 | A compartment per functional area — one runbook, many audiences, one index |
| [financial-advisory](applications/financial-advisory.yaml) | advisory-records@1 | 14 | PII as the compartment boundary, including documents needing two compartments at once |
| [history-revolution](applications/history-revolution.yaml) | archival-documents@1 | 5 | Sharding a large corpus by BYTES rather than document count, with stable hash assignment |
| [insurance-claims](applications/insurance-claims.yaml) | claim-files@1 | 5 | Loss type as the collection boundary, with one type escalated behind its own compartment |
| [legal-appeal](applications/legal-appeal.yaml) | case-filings@1 | 1 | When *not* to compartmentalize, and how to say so in a runbook |
| [legal-contracts](applications/legal-contracts.yaml) | commercial-contracts@1 | 2 | A full corpus behind a clearance plus a level-0 smoke slice for cheap end-to-end verification |
| [patent-analysis](applications/patent-analysis.yaml) | patent-documents@1 | 5 | Privilege as a real three-level boundary, up to attorney work product alone at the top |
| [regulatory-compliance](applications/regulatory-compliance.yaml) | regulatory-documents@1 | 2 | Two level-0 collections separated for retrieval reasons rather than governance reasons |
| [support-knowledge](applications/support-knowledge.yaml) | knowledge-sources@1 | 10 | A compartment model derived from source ownership; DOCX/PDF binding by content type |
| [sweep-coverage](applications/sweep-coverage.yaml) | dataroom-documents@1 | 1 (shared) | Sharing one collection handle between two runbooks so the index is built once |
| [sweep-v2](applications/sweep-v2.yaml) | dataroom-documents@1 | 1 (shared) | Two applications over one collection differing only in completion policy |
| [threat-intelligence](applications/threat-intelligence.yaml) | threat-reports@1 | 4 | The vendor feed as the compartment boundary, which is what makes aliasing visible |

## Conventions these samples follow

**Shapes are shared, not copied.** `dataroom-documents@1` governs three
runbooks (due-diligence, sweep-coverage, sweep-v2) because they read one
corpus through different retrieval architectures — which is exactly what these
samples compare.

**Collections are shared when the corpus is.** sweep-coverage and sweep-v2
both declare a collection named `northgate-dataroom` with identical level,
compartments and binding. Collection names are tenant-unique handles, so the
executor resolves the existing collection instead of building a second index.

**A source may bind into several collections.** A file uploaded as
`northgate/03_finance/audited-fy2023.md` matches both the due-diligence
runbook's `northgate/03_finance/` binding and the sweep runbooks' whole-room
`northgate/` binding. Both applications see it; neither copies it.

**Collections are drawn on real governance boundaries.** Where a corpus
has genuinely different sensitivity — employment files, client PII, unfiled
patent drafts, attorney work product, vendor intel under contract — that
becomes an access level and a compartment. Where it does not (public court
filings, LOC archives, the CFR), the collections stay level 0 rather than
inventing a clearance story.

**Each runbook declares where its documents live.** The `spec.sources` block
names the container and the path prefix everything it reads sits under:

```yaml
spec:
  sources:
    container: sources
    prefix: "northgate/"
```

This is checkable, not decorative: `mmctl runbook validate` raises
`sources.prefix-mismatch` (**Error**) when a collection binds a path outside the
declared prefix — a runbook claiming its documents live somewhere its own
bindings can never match. It also warns when a prefix does not end in `/`, since
matching is a literal `starts_with` and `north` would also match
`northgate-archive/`.

**Prefix bindings mirror the corpus tree.** Matching is literal
`starts_with`, not glob, so each collection binds one prefix. Where a
corpus is already foldered (`bugtrail/`, `prior_art/`, `03_finance/`) the
prefix is that folder; where it is flat, the file header states the upload
convention it assumes (`northgate/…`, `vale/…`, `rev/…`, `juliana/…`,
`intel/<vendor>/…`).

**`mediaTypes` only where media discriminates.** Prefix and media type AND
together when both are present, so an unnecessary media constraint is a way to
silently bind nothing. These samples declare it where the corpus genuinely
mixes formats — DOCX policies and PDF SLAs in support-knowledge, the CSV
exports in customer-support and insurance-claims.

**Answer keys are never uploaded.** Where a corpus comes with a graded key,
it appears in no binding here: a key inside the retrieval index is not a
measurement.

**A document's filename is its identity.** Blob path, source identity, and the
string `filenamePrefix` matches are all the same value. So the same bytes staged
under two paths are two independently bindable, rebuildable, retirable sources —
which is what lets `contracts/smoke/` and `contracts/cuad/` hold the same
document without one silently shadowing the other.

**Keys carry no dots.** `subject.key=value` splits at the LAST dot
([`munarium-core/src/ledger.rs`](../src/munarium-core/src/ledger.rs)), so a dotted
key silently steals from the subject — the failure mode is a version-bearing
key blinding the chronology gate until it is dash-encoded. Every shape here enforces dot-free keys at the
command gate, so the mistake lands as a `disputed` claim instead of quiet
corruption. A dotted SUBJECT is fine (`117.126.record_retention` resolves
exactly as intended) — that is why regulatory-documents@1 allows it.

**Completion templates carry the hard-won rules.** They are not decoration:
the enumerable-set rule in financial-advisory is what stops a model
generalising a constancy claim from a sample, and the "a search hit you did
not read is not a citation" rule in history-revolution is what stops it citing
a document search surfaced but the turn never served.

## Validation

```bash
mmctl runbook validate -f runbooks/applications/<name>.yaml
mmctl runbook validate -f runbooks/applications/<name>.yaml --suggest   # + AI review (BYOK)
```

All fifteen runbooks parse and validate with **no Error and no Warn
findings** — enforced by `cargo test -p munarium-runbooks`, which walks this
directory so a broken sample fails CI rather than shipping as documentation
someone copies. Two emit one Info finding each, deliberately:

- `history-revolution` and `regulatory-compliance` →
  `collections.uniform-access`. Both corpora are wholly public (LOC archives;
  FDA letters and the CFR), so their collections share level 0. The finding is
  correct and the uniformity is the honest modelling.
