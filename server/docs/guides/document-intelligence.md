# Document intelligence: the escalation path

Some documents cannot be read locally. A scanned court filing has no text
layer; a fax-derived TIFF is pixels; a PDF whose page images are JBIG2-encoded
has no pure-Rust decoder at all. Those land in the index as `extraction_status
= 'empty'` — present, findable, and contributing nothing.

A **document intelligence** provider is the escalation for exactly those
documents: a hosted or on-premises analyzer that reads what local extraction
could not.

## It is off by default. That is deliberate.

`MUNARIUM_DOCINTEL` defaults to `none`, and `none` is a *complete* configuration,
not a degraded one. Four reasons, in the order they matter:

**1. It costs money per page.** Local extraction is free and runs on every
source in every build. A hosted analyzer bills per page — roughly $1.50 per
1,000 pages for Azure's `prebuilt-read`. A default that silently enables it is
a default that hands somebody a bill they did not choose. Cheap is not free,
and "cheap per page" times "a 66,000-document corpus" is a real number.

**2. It sends your documents to a third party.** Every call ships the document
outside the cluster. For a data room under NDA, a patient record, or anything
with a residency obligation, that is a decision a human makes once, in writing
— not something a framework default makes on their behalf. The rest of this
system is built so no proprietary service is required in the path; this is the
one place that would quietly stop being true.

**3. It is the only non-deterministic step.** Everything else in the index
pipeline is reproducible from bytes: the chunker is versioned, the embedder is
a hash function, extraction is a parser. A hosted model can change its output
under a fixed input when the vendor ships an update. That is why providers
report a `provider_fingerprint` pinning model and API version — and why this
capability is opt-in rather than ambient.

**4. Nothing should require it.** Every test, `docker compose up`, and CI run
with no provider configured. If the default were "on", the offline path would
rot the first week nobody exercised it.

**Turn it on where the corpus genuinely includes scanned filings** — a
collection of a few hundred court filings, for instance, is largely PDFs
without a text layer. That is the opt-in working as intended: an environment that needs it
says so in its own deployment configuration (the chart's `docIntel.provider` /
`docIntel.endpoint`, or the example AKS module's `doc_intel_endpoint` /
`doc_intel_id`).

## What it costs to leave on

Billing is per page analyzed, with no standing charge — an idle resource
costs nothing. The escalation only fires for documents local
extraction could not read, so the bill tracks *scans*, not corpus size. A
corpus of clean markdown costs nothing no matter how large.

Bound it anyway:

| Var | Default | Purpose |
|---|---|---|
| `MUNARIUM_DOCINTEL_MAX_BYTES` | `104857600` | refuse oversized documents locally instead of paying latency for a service-side rejection |
| `MUNARIUM_DOCINTEL_TIMEOUT_SECS` | `180` | wall-clock ceiling per document, polling included |

## One resource can serve several environments

A Document Intelligence resource holds no per-environment state — a request
carries its own document and the answer comes straight back — so there is
nothing to isolate, and several environments can share one: one endpoint to
grant, one quota to watch, and one line on the bill.

Disable local auth on the resource so the only credential is a managed
identity, and grant each environment's identity the `Cognitive Services User`
role (the example AKS module does exactly this when `doc_intel_id` is set).
No key then exists to rotate or leak.

## How the escalation actually runs

```
build index
  └─ for each source
       ├─ local extraction (free, deterministic, always)
       │    ok      → index the text, method = docx | pdf-text | text
       │    empty   ↓
       │    failed  ↓
       └─ document intelligence (only if configured AND supports the type)
            text    → index it, method = ocr, fingerprint recorded
            empty   → keep the local result; the service read it and found nothing
            error   → keep the local result; log it and CONTINUE
```

Three properties worth stating plainly:

- **The provider is never asked about documents local extraction handled.**
  Markdown, DOCX and text-layer PDFs never reach it, so they never cost
  anything. The trait's `supports()` is honoured too — declining a media type
  means the caller skips you rather than paying for a guaranteed empty answer.
- **A provider outage degrades the index; it never fails the build.** The local
  result stands and the run continues, mirroring the batch-ingest rule that one
  bad file never fails the batch.
- **OCR'd text is marked as such.** `extraction_method = 'ocr'` on the source
  row, because an OCR'd document and a real text layer are not equivalent
  evidence, and anything reasoning over citations deserves to know which it got.

## Adding another provider

The seam is [`munarium_core::docintel::DocumentIntelligence`](../../src/munarium-core/src/docintel.rs).
Nothing above it knows which provider is configured. To add AWS Textract,
Google Document AI, or an on-premises engine:

**1. New crate `munarium-docintel-<name>`**, depending only on `munarium-core` and an
HTTP client. Use `munarium-docintel-az` as the shape: hand-rolled
REST over the workspace's rustls `reqwest`, no vendor SDK. That is not
stylistic — the image is a static musl binary on distroless, so an SDK that
binds native libraries cannot link, and every added crate must clear
`cargo deny` on licenses, advisories, and bans.

```rust
#[async_trait]
impl DocumentIntelligence for MyProvider {
    fn supports(&self, media_type: &str) -> bool { /* decline honestly */ }
    fn id(&self) -> &'static str { "my-provider" }
    async fn analyze(&self, media_type: &str, bytes: &[u8]) -> Result<AnalyzedDocument> { … }
}
```

**2. Meet the four obligations** documented on the trait: decline media types
you cannot handle; return `AnalyzedDocument::empty()` rather than an error when
the service genuinely found nothing (those lead to different operator actions);
bound yourself with a page cap and a timeout; and put no credential in
`provider_fingerprint`, which lands in stored metadata.

**3. Add a config arm.** One match arm in `doc_intel_from_env()`
([config.rs](../../src/munarium-server/src/config.rs)) and one in
`build_doc_intel()` ([state.rs](../../src/munarium-server/src/state.rs)).
Follow the existing shape: fail closed on missing required settings, and
resolve any credential through the `CredentialRef` seam (an env-var name, or
`file:/path` for a mounted secret) rather than taking the secret inline.

**4. On-premises engines need no network exception.** The trait says nothing
about HTTP. A provider wrapping a local service on `http://ocr.internal:8080`,
or an in-process engine, implements the same interface — and for an air-gapped
deployment that is the *point*: the escalation stays available while reason
(2) above stops applying.

**5. Environments opt in individually.** Adding a provider to the codebase
changes no environment's behavior. `MUNARIUM_DOCINTEL` stays `none` until a
deployment sets it, which is the property that makes shipping more providers
safe.

## Configuration reference

| Var | Default | Notes |
|---|---|---|
| `MUNARIUM_DOCINTEL` | `none` | `none` \| `azure`. Other providers extend this. |
| `MUNARIUM_DOCINTEL_ENDPOINT` | — | **required** when a provider is selected (fails closed) |
| `MUNARIUM_DOCINTEL_AUTH` | `managed_identity` | `managed_identity` (no secret exists) \| `key` |
| `MUNARIUM_DOCINTEL_KEY_REF` | — | required under `key`: an env-var name, or `file:/path` |
| `MUNARIUM_DOCINTEL_MODEL` | `prebuilt-read` | OCR + layout; the cheapest model that does this job |
| `MUNARIUM_DOCINTEL_MAX_BYTES` | `104857600` | refuse oversized documents before the call |
| `MUNARIUM_DOCINTEL_TIMEOUT_SECS` | `180` | per-document wall clock |

Enabling a provider without its endpoint is a **startup error**, not a warning.
A silently-disabled escalation looks exactly like "this corpus has no text",
which is the failure mode hardest to notice.

## Isolated PDF extraction

Local PDF extraction (`pdf-text@1`) runs in the server process with no time,
memory or output bound. `MUNARIUM_PDF_EXTRACTION=isolated` opts into running the
same parser in a child process instead (design
[R3/B4](../design/airk-analysis.md#r3)). The default, `inline`, is exactly v1.4.

- The server re-executes its own binary with a private role argument and an
  empty environment. No shell and no extra file is involved, so the distroless
  image needs nothing new.
- Memory and CPU are enforced by the operating system on 64-bit Linux, the
  only production platform: the child applies `setrlimit` to itself (address
  space, CPU time, no core dumps; through the pinned `rlimit` crate) before
  it reads the document, and the identity records `os=rlimit`. Windows and
  other builds are for development and testing only. They apply no OS memory
  or CPU limit, and the identity records `os=none`; the wall-clock deadline,
  output cap, input-size check and admission cap below still apply. The
  extraction crate contains no `unsafe` code on any platform.
- The parent enforces the wall-clock deadline and the output cap, and kills and
  reaps the child on timeout, on overflow and when the build is cancelled. A
  queued direct build also kills it when its job is cancelled or loses its
  lease, through the worker's stop signal (ClearView R6); the in-process
  `inline` parser cannot be stopped that way and runs to its end.
- At most `MUNARIUM_PDF_EXTRACTION_CONCURRENCY` children run per process, in
  arrival order. A document that waits longer than
  `MUNARIUM_PDF_EXTRACTION_QUEUE_SECS` is not parsed.

| Var | Default (provisional) | Notes |
|---|---|---|
| `MUNARIUM_PDF_EXTRACTION` | `inline` | `inline` \| `isolated`; anything else is a startup error |
| `MUNARIUM_PDF_EXTRACTION_MEMORY_MIB` | `512` | child memory limit: address space on Linux, including the mapped binary; not enforced by development builds on other platforms beyond refusing a larger input |
| `MUNARIUM_PDF_EXTRACTION_CPU_SECS` | `10` | child CPU time (user and system) on Linux; not enforced on other platforms |
| `MUNARIUM_PDF_EXTRACTION_TIMEOUT_SECS` | `30` | wall clock from spawn to exit |
| `MUNARIUM_PDF_EXTRACTION_MAX_OUTPUT_MIB` | `64` | largest extracted text accepted |
| `MUNARIUM_PDF_EXTRACTION_CONCURRENCY` | `2` | children per process (1–64) |
| `MUNARIUM_PDF_EXTRACTION_QUEUE_SECS` | `120` | admission wait before a document is deferred |
| `MUNARIUM_PDF_EXTRACTION_ESCALATE_OVER_BUDGET` | `false` | `true` \| `false`; whether an over-budget document may escalate to document intelligence (see below) |
| `MUNARIUM_PDF_EXTRACTION_DEFERRED` | `skip` | `skip` \| `fail`; what a build does with a deferred document (see below) |

The bounds and both policies are read only under `isolated`; an invalid value
is a startup error. They are provisional and not frozen
production limits; the [measurements](../design/pdf-isolation-measurements.md)
record what they were chosen from and what remains to qualify.

**Identity.** The profile's extractor identity is
`pdf-text-isolated@1{os=…;mem_bytes=…;cpu_s=…;wall_ms=…;out_bytes=…}`, so the
extractor-set identity becomes
`extract@1[docx@1,pdf-text-isolated@1{…}]`. Successful text is byte-identical
to `pdf-text@1`; which documents succeed is what differs. Enabling the profile
does not switch any index: a rebuild produces a new index that goes through the
normal verify and activate path, and the v1.4 identity stays available to
rebuilds with `inline`. Concurrency and the queue wait are scheduling settings
and are not part of the identity.

**Extraction status.** Under the profile, `sources.extraction_status` and the
build log distinguish the outcomes `inline` reports as `failed`:

| Status | Class in the log | Meaning | Escalates to document intelligence? |
|---|---|---|---|
| `failed` | `parser_failed` | the parser rejected or crashed on the document | yes, under the existing rules |
| `over_budget` | `over_budget` (with `input`, `memory`, `cpu`, `wall_clock` or `output`) | the document exceeded a bound | **no** by default; yes, under the existing rules, with `MUNARIUM_PDF_EXTRACTION_ESCALATE_OVER_BUDGET=true` |
| `deferred` | `deferred` | no slot within the queue wait; never parsed | **no** |
| `cancelled` | `cancelled` | the build stopped first; the child was killed | **no** |
| `failed` | `isolation_failed` | the child could not be started or confined | **no** |

The escalation rules themselves are unchanged: a provider must be configured,
must accept the media type, and its own `MUNARIUM_DOCINTEL_MAX_BYTES` and
timeout apply. v1.4 escalates any PDF local extraction could not read, so a
parser failure keeps escalating exactly as a v1.4 `failed` result would. An
over-budget document does not escalate unless
`MUNARIUM_PDF_EXTRACTION_ESCALATE_OVER_BUDGET=true`: a document too large or
too slow for the local budget is the one most likely to be expensive to
analyze. When a configured provider would otherwise have been called, the build
logs the refusal at warn level with the bound (`budget=` and the limit), never
the document's text. Work that was never attempted locally does not escalate
under either setting: paying for a document because the local queue was busy,
or because the deployment cannot start its child, would turn congestion or
misconfiguration into cost. Under `inline` none of these classes occur, so the
setting changes nothing there.

**Deferred documents.** With `MUNARIUM_PDF_EXTRACTION_DEFERRED=skip` (the
default) a build continues without a deferred document, records it `deferred`
on its source and logs at warn level how many documents it skipped. A rebuild
picks them up: a version that skipped deferred documents lists them in its
manifest (`deferred_sources`), so a rebuild of the same inputs builds a new
version instead of reusing it, and a queued direct build's version id already
covers each source's outcome. With `fail`, the first deferred document fails
the build and nothing is committed. The error is `rate-limited` (HTTP 429, gRPC
`RESOURCE_EXHAUSTED`), a capacity refusal to retry when extraction load allows;
it names no document. A queued build job records it without an `error_code`,
because no [build-job failure code](../api/errors.md#build-job-failure-codes)
describes local extraction capacity. Neither policy is part of the identity.
