# Munarium Server — release notes

These are source release notes. See the [container publication record](CONTAINER.md#versions-and-verification)
for registry availability, digests and public signing instructions.

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
