# Loading corpora into blob storage

Every sample runbook under [`runbooks/applications/`](../../runbooks/applications/)
binds a corpus by path prefix. **No corpus ships in this repository.** Some
sample runbooks are built around public datasets you can obtain yourself;
others assume synthetic material you supply. Either way, this page is how
documents get from wherever they are into the server's object store, and how
to point a runbook at a corpus of your own.

## Where the bytes actually land

`MUNARIUM_SOURCE_STORE` picks the backend: `az` (Azure Blob), `s3` (AWS or
any S3-compatible — MinIO, R2), `gcs`, `file` (a local directory), `pg`
(bytea in Postgres — the offline/CI fallback and the compose default), or
`mem`. The cloud and filesystem backends are one adapter over the Apache Arrow
[`object_store`](https://docs.rs/object_store) crate (`munarium-store-objects`);
credentials come from each cloud's ambient chain (managed identity, IAM
role/IRSA, `GOOGLE_APPLICATION_CREDENTIALS`) unless a static override is
configured. See the README's env-var table for the per-backend variables.
Whatever the backend, the row records `storage_backend` and a credential-free
`blob_uri`, so `GET /v1/sources/{id}` always tells you where the bytes went.
Per-backend setup — MinIO for local dev, the ambient-credential posture for
each cloud, the Azure specifics — is worked through in
[source-stores.md](source-stores.md).

## The one rule that governs everything here

**A document's filename IS its identity and its blob path.** Upload
`northgate/03_finance/audited-fy2023.md` and the bytes land at
`sources/<tenant>/northgate/03_finance/audited-fy2023.md`, where the
`due-diligence` runbook's `filenamePrefix: "northgate/03_finance/"` binding
finds it. Matching is a literal `starts_with` — no globs, no path parsing, no
normalization — so the string you upload under is the string the runbook must
declare.

Three consequences worth internalizing before you load anything:

| You upload | What happens |
|---|---|
| same path, same bytes | idempotent replay (`existed: true`) |
| same path, new bytes | an **update** in place; a rebuild is now owed |
| different path, same bytes | two **separate** sources, bound and retired independently |

That last row is deliberate. Staging the same policy document under both
`contracts/smoke/` and `contracts/cuad/` gives you two sources, because each
must be bindable to its own collection on its own schedule.

## Uploading

```bash
# One file, auto-bound by every runbook matcher the token can reach.
curl -X POST localhost:8080/v1/ingest "${H[@]}" -d '{
  "filename": "northgate/03_finance/audited-fy2023.md",
  "media_type": "text/markdown",
  "content_base64": "'"$(base64 -w0 audited-fy2023.md)"'"
}'

# Where did it go?
curl localhost:8080/v1/sources/src-1a2b3c4d5e6f7890 "${H[@]}"
# -> filename, content_hash, storage_backend, blob_uri, extraction_status
```

Batches take up to 500 files. Both ingest routes accept bodies up to 256 MiB
(base64 is 4/3, so budget ~190 MB of document bytes per request).

## Bulk upload sessions — the recommended path for anything over a few hundred files

For whole corpora, open a **bulk upload session** instead of hand-rolling
batches. The session takes a manifest up front, tells you exactly what still
owes bytes, and makes every retry safe:

```bash
mmctl bulk upload --dir sources/corpus_text --prefix rev/ --label history-core
# hashes every file, opens the session, streams <=500-file chunks,
# finalizes, and prints the completion report.
mmctl bulk status <bulk-id> --needed     # progress + remaining work list
mmctl bulk upload --dir ... --prefix rev/ --resume <bulk-id>   # after any failure
```

Under the hood: `POST /v1/ingest/bulk` (manifest: filename + sha256 +
bytes_len + media_type per document) → the server diffs the manifest against
`sources` and answers with `needed` — same path + same bytes means nothing
owed, so a re-run over an already-loaded corpus uploads zero bytes. Chunks go
to `POST /v1/ingest/bulk/{id}/chunk` (same limits and storage/binding path as
batch ingest; each file's bytes verified against the manifest sha256, with a
mismatch failing that file only), and `POST /v1/ingest/bulk/{id}/complete`
re-verifies every entry against `sources` before declaring the load done —
`incomplete` names what is missing or hash-drifted. Every step is
per-document idempotent: kill the loader anywhere, run it again with
`--resume`, and only the un-landed files move. Sessions expire after 7 days.

For corpus-specific path mapping — hash-bucketed shards, year-parsed
newspaper prefixes, excluding an answer key — write a thin layout driver over
the same session protocol (open with a manifest, stream chunks, complete).
`mmctl bulk upload --prefix` covers the flat case, where the directory tree
already is the path layout.

**Answer keys are never uploaded.** A corpus may come with a graded key. It appears in no runbook binding here, and it should
appear in none of yours: a key inside the retrieval index is not a
measurement.

## Pointing a runbook at a corpus of your own

Three things in a runbook decide where it looks: `spec.sources.prefix`, each
collection's `sources.filenamePrefix`, and optionally `mediaTypes`. Upload
under those prefixes and the bindings find the documents; upload anywhere else
and they bind to nothing. To run a sample runbook over your own material,
either lay your files out under the prefixes it declares, or copy the runbook
and change the prefixes to match your layout — `mmctl runbook validate`
raises `sources.prefix-mismatch` when a collection binding falls outside the
declared root, and warns when a prefix does not end in `/`. Keep the answer
key out of the upload set, and keep filenames stable: the filename is the
identity.

## Per-runbook corpora

| Runbook | Prefix | What to put there |
|---|---|---|
| due-diligence | `northgate/<area>/` | An M&A data room, foldered by functional area. Supply your own; the area folders are what the compartment model binds to |
| sweep-coverage, sweep-v2 | `northgate/` | The same data room, read whole. They share the `northgate-dataroom` collection with due-diligence, so there is no second upload |
| financial-advisory | `vale/<area>/` | A wealth-advisory record foldered by area, with client PII separable from the rest |
| threat-intelligence | `intel/<vendor>/` | Vendor threat reports, one folder per feed |
| insurance-claims | `claims/` | Claim files, each a folder of numbered documents, across several loss types |
| support-knowledge | `support/` | Knowledge and ticket material from several source systems, one prefix per system |
| customer-support | `tickets/` | One document per ticket. Derivable from any public helpdesk ticket dataset |
| history-revolution | `rev/<group>/` | **Public** — the Library of Congress American Revolution digital collections and the Chronicling America API |
| legal-contracts | `contracts/` | **Public** — The Atticus Project's CUAD dataset (CC-BY-4.0). Leave the annotations out |
| legal-appeal | `juliana/<docket>/` | **Public** — a case file foldered by docket; the Sabin Center's climate case chart is one source |
| regulatory-compliance | `fda/` (`letters/` and `cfr/` beneath it) | **Public** — FDA warning letters and the Title 21 CFR sections they cite, via the eCFR |
| patent-analysis | `patents/` (`prior_art/`, `decoys/`, `targets/`, `notices/`, `assessments/`) | **Mixed** — prior art and decoys are public patents (USPTO Open Data Portal, free API key); target drafts, office actions and assessments are yours to supply |

The public rows are reproducible today, though a download made now may differ
from one made last month.

## Start small

Loading everything is rarely what you want.

- A **slice** — a dozen documents chosen to hit every collection binding —
  proves a runbook's binding contract end to end in seconds, on
  `MUNARIUM_SOURCE_STORE=pg` with no object store at all. Do this first,
  always.
- A **realistic set** in the tens of megabytes gives you real retrieval
  behaviour at trivial cost; this is where to tune `retrieval:`
  ([retrieval-sizing.md](retrieval-sizing.md)).
- The **full corpus** — an archival collection or a claims archive, at
  hundreds of megabytes and up — is what makes object storage *necessary*
  rather than merely correct; it was never going to work as Postgres `BYTEA`.
  At public list prices object storage at these volumes is cents per month;
  the compute is unchanged.

## Binary formats

DOCX and PDF are extracted to text at **index time**, not ingest time — the raw
bytes stay canonical in blob, so improving an extractor is a rebuild rather than
a re-upload. `GET /v1/sources/{id}` reports which path a document took:

| `extraction_method` | Meaning |
|---|---|
| `text` | already text (markdown, plain, JSON…) |
| `docx` | unzipped `word/document.xml` |
| `pdf-text` | the PDF's embedded text layer |
| `ocr` | recognized from page images (feature `ocr`) |

`extraction_status` is `ok`, `empty`, or `failed`. **`empty` is the one to
watch**: it means the document contributed zero chunks. A scanned PDF with no
text layer reads `empty`, and without OCR enabled it is in the index in name
only.

### The honest PDF limit

Local OCR covers JPEG and Flate-encoded scans. **JBIG2 and CCITT pages have no
pure-Rust decoder** and read as `empty` locally — and those encodings are
common in older court filings, which is exactly what legal-appeal reads.

That is what the [document-intelligence
escalation](document-intelligence.md) exists for: a hosted analyzer handles
those encodings natively. It is **off by default** because it bills per page
and sends documents outside the cluster — read that guide before turning it
on.

Still worth measuring before committing to a corpus like legal-appeal: sample
a dozen of its PDFs and see how many carry a real text layer. That decides
whether the corpus costs pennies or a few dollars to index, not whether it
works.

Extraction never fails a build: a bad document is recorded per-source and the
remaining documents still index, mirroring the batch-ingest rule that one bad
file never fails the batch.
