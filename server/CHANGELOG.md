# Munarium Server — release notes

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

- **PostgreSQL hardening is unfinished.** Slice resolution is not yet pushed
  into SQL, and no sqlx offline query data is committed — a build therefore
  needs a reachable database for the query macros rather than compiling from a
  checked-in query cache.
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
- **The Helm chart's image repository is a placeholder** (`<your registry>/…`).
  A default `helm install` will not pull. Supply your own registry, or build
  the image from this repository.
- **Releases are cut outside this repository.** Everything needed to *operate*
  Munarium without Ioka is here — deployment runbooks, clustering, backup and
  restore, troubleshooting, the conformance suites. Signed images and version
  tags are not: there is no public release workflow in this repository, so a
  signed artifact cannot currently be reproduced from a public tag. Building
  from source is reproducible; verifying an official signed image is not.

### Known-gaps ledger

`docs/guides/dev-guide.md` §13 carries the working ledger of what is folklore,
missing or half-built, kept current rather than aspirational. It is more
detailed than this file and is the right place to look before filing an issue.
