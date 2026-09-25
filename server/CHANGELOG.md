# Munarium Server — release notes

These are source release notes. See the [container publication record](CONTAINER.md#versions-and-verification)
for registry availability, digests and public signing instructions.

## Unreleased

- Add immutable governance profiles for memory versions, opt-in exact string
  comparison, explicit existing-history transitions and retained assessments.
  Governed claim evaluations commit with their findings; original decisions remain
  unchanged. PostgreSQL migration 0036 retains legacy defaults and rejects old
  writers on governed versions.
- **Upgrade action:** upgrade all writers before adopting a profile, transition
  existing memory versions explicitly, review assessments, and rebuild affected
  collections (or all collections in a small deployment). Re-extraction, publication
  pins and fresh sessions may also need updating. Follow the
  [governance-policy upgrade guide](docs/ops/governance-policy-upgrade.md), including
  rollback and retention steps. Installing the migration alone does not adopt
  exact semantics or rebuild existing collections.
- Fail a `verifyDataViews` step when Matrix answers 200 with a body that is not a
  verify response (not JSON, or no integer `failed`). Such a body previously read as
  zero failed questions and marked the data view verified.
- Report crafted datastore artifacts as integrity errors instead of panicking: a
  lexical archive entry whose declared length overflows its offset, and a DiskANN
  header that declares an impossible vector count. DiskANN now sizes its vectors
  from the bytes present rather than the header.
- **Operator-visible:** an occupied REST or gRPC port, or a shutdown-signal handler
  that cannot install, now ends startup with one `startup error:` line and exit
  status 1 instead of a panic with exit status 101. Configuration errors keep exit
  status 2.
- Keep serving after a panic elsewhere poisons a metrics, readiness, cache, shape
  registry or token-cache lock; the in-memory source store and provider rate budget
  refuse with a storage error instead. Rate-budget arithmetic no longer overflows,
  so an enormous token estimate can no longer pass under a tpm limit in release builds.
- Return a storage error instead of storing or returning `null`/`{}` when a turn,
  draft validation or runbook value fails to serialize. A turn whose result cannot be
  rendered ends its stream with an `error` event, not `done` with `{}`.
- Deny `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!` and `unimplemented!` in
  production code across the workspace, with two reasoned exemptions. See
  [panic boundaries](docs/panic-boundaries.md).

## 1.2.1

- Update rustls to 0.23.45 and refresh dependency notices for the patched build.
- Add collection-scoped query orchestration, retained publication governance,
  and original-file authorization over REST and native gRPC. Query callers send
  their question and collection scope; Server selects governing versions,
  applies vocabulary, retrieves passages and composes the narrative.
- Add append-only migration 0034 and revision-checked publication snapshots.
  Withdrawals do not revive superseded originals; a governance change during
  completion rejects the in-flight answer. Applications keep serving file bytes.
- Publish model routes and budgets by exact access clearance, and apply the
  collection's model/processing policy to vocabulary generation. Only activated
  indexes may be registered as published sources.
- Preserve explanatory answers and verified citations for insufficient and
  review results. Request provider-native structured output for answer and
  vocabulary protocols while retaining ordinary completion behavior.
- Keep per-index BM25 statistics separate during cross-file ranking, without
  letting index names or task completion order starve later files of context.
- Support explicit OpenRouter downstream routing through ProviderConfig.
- Give the answer model request-local passage IDs and map checked citations back
  to the caller's original IDs. Keep source paths, hashes and opaque caller IDs
  out of the model's citation namespace. Unknown IDs and altered quotes fail closed.
- Clarify generic answer instructions for keyword topics and partially supported
  requests without adding domain-specific aliases or inferred facts.
- Extend REST/gRPC regressions for forged provenance, access levels, collection
  scopes, user/tenant isolation, expired tokens and invalid model citations.
- Derive the generated client API inventory's version from OpenAPI.
- Align all four Server SDK target constants with 1.2.1 and check them against
  the compatibility record. Live conformance uses those checked constants.

Published from merged commit `c638a8e56fff45cef358ff2f4a5b5ba57957ba59`.
See the [container qualification record](CONTAINER.md#versions-and-verification)
for exact image identities, verification and database-restore rollback requirements.

## 1.2.0

- Add the complete `ServerApiService`: every documented REST operation has a
  named native RPC, including streaming turns and formerly REST-only management
  operations. All four Server SDKs expose the complete surface, generated from
  the same contract and checked for drift. Both transports use the same handlers.
- Add REST `/v1.2` APIs for collection vocabularies, configurable default-on sampled
  generation, explicit refresh, revision-checked editing and enable/disable controls.
  Apply vocabulary expansion in scoped search and session retrieval.
- Add checked narrative answers with server-resolved source references. Applications
  retain and serve original files; source IDs and hashes are not download grants.
- Carry pinned extraction locations through PostgreSQL and Datastore, distinguishing
  PDF pages from DOCX paragraphs and declaring UTF-8 versus Unicode-scalar offsets.
- Preserve the indexed source hash in Datastore after later source changes. Legacy
  artifacts resolve hashes from their pinned chunk rows, never the current source.
- Add migrations 0032/0033 and make Cargo rebuild its embedded migrator when the
  migrations directory changes. Existing indexes remain immutable.

See [configuration, API and upgrade details](docs/guides/collection-vocabularies.md).
The new APIs are available over REST and `ServerApiService` gRPC. Automatic
generation is on by default, including after
upgrade; configure it before loading credentials if existing collection policies
need review. New location metadata requires a new index build.
## 1.1.1

- Apply an allowed session-turn model override to both model-based query
  expansion and answer generation. Previously, expansion always used its
  runbook default, even when the caller selected another provider or tier.
- Reject invalid or disallowed overrides before query expansion can call a
  provider, including when expansion is optional.

Without an override, each task retains its configured model. Retrieval-only
turns still reject nonempty model overrides. Selecting a higher tier now also
applies that tier to query expansion and can increase its cost and latency.
No wire-contract or database migration is required. Rollback to 1.1.0 restores
the previous routing behavior without changing stored data or configuration.

## 1.1.0

- Add the `ollama` provider with native chat completion, batched embeddings and
  named model-health checks over REST and gRPC. Local configurations require an
  explicit endpoint and may omit credentials; authenticated proxies use the
  existing environment/file secret references.
- Support named Ollama configs and explicit family selection with configured
  model tiers. Automatic cloud-provider priority remains unchanged.
- Preserve token budgets, invocation provenance and truncation handling; include
  provider family in embedding-cache identity. Validate Ollama response shapes
  and reject unsupported tool requests and oversized embedding inputs.
- Add a pinned Docker Desktop evaluation stack, model digest checks and live
  REST/gRPC acceptance covering embeddings, retrieval and grounded completion.

The MMP wire major remains 1; no database migration is required. Upgrade from
1.0.0 with the existing PostgreSQL volume and configuration. Existing cloud
providers retain their credential requirements. Ollama weights live in its own
volume and are not distributed with the Munarium image.

Rollback to 1.0 requires moving Ollama-dependent runbooks to a provider that 1.0
supports. Existing ledger data and cloud configurations remain compatible.

Ollama completion is non-streaming with thinking disabled. Index builds continue
to use the existing local embedder; provider-backed index embeddings and tool
execution are not included. The prior release's other accepted limitations remain.

## 1.0.0

The first public release.

**What 1.0 commits to.** The MMP wire contract under the N/N−1 policy, the
`MUNARIUM_*` configuration contract, and additive-only database migrations,
all under semantic versioning. It does not claim every planned capability is
finished. The list below is that gap, stated rather than implied.

### Accepted limitations

- **Direct gRPC TLS is not implemented.** The listener serves plaintext;
  earlier documentation incorrectly advertised `MUNARIUM_GRPC_TLS_CERT/KEY`.
  Terminate TLS at an external proxy. The Helm gateway's default listener is
  HTTP on port 80 and also needs explicit TLS configuration for remote use.

- **PostgreSQL hardening is unfinished.** Slice resolution is not yet pushed
  into SQL. Correction to the original build guidance: queries are runtime-checked
  strings, so compilation needs neither a database nor a sqlx offline cache.
  PostgreSQL integration/conformance tests provide query validation.
- **The AKS Terraform example has never been applied end to end.** It passes
  `terraform fmt -check`, `terraform init -backend=false` and
  `terraform validate` in CI, and the Helm chart has been installed and probed
  on kind. Neither is a claim that a full apply, or a restore drill, has been
  exercised. Backup drills remain.
- **gRPC parity for the platform surface is a follow-up.** The uid contract,
  capability tokens, compartmentalized collections, runbook applications,
  sessions, ingestion and reports are complete over REST. `session.proto` has
  no server-streaming `Turn`, so a gRPC client gets the unary turn and nothing
  in between.
- **No native OpenTelemetry export, and no Grafana dashboard bundle.** The
  Prometheus surface, the structured logs and the interaction records are all
  present and an external platform can integrate them today; OTLP would be an
  adapter over that, not a prerequisite. See `docs/observability.md`.
- **Local OCR does not cover every PDF encoding.** JBIG2- and CCITT-encoded
  pages have no pure-Rust decoder and extract as `empty`. Those encodings are
  common in older court filings. The Azure Document Intelligence escalation
  (`munarium-docintel-az`, off by default) is what handles them; it bills per
  page and sends documents outside the cluster.
- **The Helm chart requires an explicit image repository.** Its default is
  empty, so rendering fails until `image.repository` is supplied. Use
  `iokaio/munarium` for the public Server image or your own source build.
- **Releases are cut outside this repository.** Everything needed to *operate*
  Munarium without Ioka is here — deployment runbooks, clustering, backup and
  restore, troubleshooting, the conformance suites. The publishing workflow
  is not public. This does not prevent checking an image using published
  release-specific Cosign instructions; see the container publication record
  above for which releases have those instructions.

### Known-gaps ledger

`docs/guides/dev-guide.md` §13 carries the working ledger of what is folklore,
missing or half-built, kept current rather than aspirational. It is more
detailed than this file and is the right place to look before filing an issue.
