# munarium-server

A cloud-native, containerized Rust **governed-memory service**: an append-only fact ledger with
governance in the write path, hybrid retrieval with a provenance envelope on every answer, declarative
runbooks, and bring-your-own-key model providers. It speaks the **Munarium Memory Protocol (MMP)**
over REST and gRPC; the normative protocol lives under [proto/mmp/v1/](proto/mmp/v1/).

Licensed under the **Apache License 2.0** ([LICENSE](LICENSE), [NOTICE](NOTICE), [TRADEMARK.md](../TRADEMARK.md)).
Munarium Enterprise is a separate proprietary distribution of this software with certified builds,
supported deployment architectures and a support term; it is not open source, and nothing here
grants any right to it ([SUPPORT.md](../SUPPORT.md)).

The authoritative design is [docs/architecture.md](docs/architecture.md).

For Docker Hub images, quick starts, configuration, and source builds, see
[the container guide](CONTAINER.md).

**The core invariants** (enforced by the [conformance suite](conformance/)): the ledger is
append-only with supersession chains (a correction is a new row, never an update); governance is a
property of the command path (blocked claims are recorded `disputed`, never dropped); one
`as_of_seq` pin bounds facts, anchors, promises, counters, and entities together, and digests are
deterministically rebuilt under a pin; every retrieval answer carries a provenance envelope.

## About this repository

Munarium Server begins here, at version 1.0.0. Its design was worked out over an extended period of
private research and development — experiments, measurements, superseded designs, and the
operational records of the environments they ran in — and that history is deliberately not carried
into this repository.

It is omitted because it documents how the design was reached rather than how the software behaves,
and it would give an evaluator, an operator or a contributor nothing they need. What that work
produced is here in full: the implementation, its conformance suite, its API documentation and its
deployment assets. The conformance scenarios are the executable specification, and they are the
record worth reading.

## What is built, and what is not

The capability record, kept honest rather than aspirational. A "Complete" row is backed by the
conformance suite or a named test; a "Partial" row states what remains. **Version 1.0 commits to the
wire contract, the `MUNARIUM_*` configuration contract and additive-only migrations under semantic
versioning — it does not claim every row below is finished**, and the partial rows are this
release's published limitations.

| Capability | Scope | Status |
|---|---|---|
| **Memory kernel** | workspace, MMP protos, `munarium-core` kernel (all six gates incl. chronology), in-memory backend, conformance harness | Complete |
| **PostgreSQL cell** | `munarium-store-pg`, partitioned ledger, `lineage_heads` FOR UPDATE seq allocation, additive migrations, pg conformance + concurrency tests | Built; hardening remains — slice resolution is not yet pushed into SQL, and sqlx offline query data is not committed |
| **Server + container** | REST :8080 + direct gRPC :50051 planes, auth (static tokens, rw/ro), idempotency, problem+json, distroless image (~29 MB) | Complete — black-box conformance passes on both planes, against memory and postgres backends |
| **Shapes + retrieval** | shape registry (schema violations -> disputed claims), content-addressed ingest, in-Postgres hybrid (tsvector + pgvector HNSW, RRF), provenance envelope, versioned immutable indexes | Complete — end-to-end incl. re-index and old-version resolvability |
| **Providers (BYOK)** | Anthropic + OpenAI + OpenRouter, credentialRef seam (env/file = KV paths), rpm/tpm budgets, retry-after honor, embedding cache, invocation provenance events; `GET /v1/providers` free-tier→model disclosure with zero provider calls | Complete — contract tests against recorded fixtures |
| **Runbooks + mmctl** | checkpointed executor (transitions = ledger events), side-by-side build -> verify -> approval-gated cutover -> retireOld, `mmctl apply/run/approve` | Complete — full lifecycle covered |
| **Deploy** | Helm chart, Envoy gateway plane (compose --profile gateway), an illustrative Terraform module for AKS + CNPG ([deploy/terraform/example-aks](deploy/terraform/example-aks/)), distroless image | Partial — the chart installs and probes on kind; the Terraform module validates but has never been applied end to end, and backup drills remain |
| **Identity, interactions and capability tokens** | uid contract on every /v1 call (`X-Munarium-Uid` / `munarium-uid`), per-uid interaction capture, `mgmt`-role static tokens, `POST /v1/access-tokens` minting short-lived HS256 capability JWTs (level + compartments + query/ingest scopes), `munarium-access` crate — see [docs/security-posture.md](docs/security-posture.md) | Complete |
| **Compartmentalized collections** | first-class `collections` (access_level + compartments), `collection_chunks` LIST-partitioned per collection (own HNSW/GIN per partition, pruned queries), advisory-locked runtime partition DDL, multi-collection RRF search, `/v1/collections` (no delete API — [docs/ops/index-deletion-runbook.md](docs/ops/index-deletion-runbook.md)) | Complete — pg integration tests cover isolation, DDL race and one-active-index |
| **Runbook v2: retrieval applications** | `spec.collections` (per-collection access levels + declarative source bindings), `retrieval:` knobs, `models:` per-task defaults + `allowOverrides` policy, optional `completion:`, per-collection executor steps with per-collection approval gates, `GET /v1/runbooks(+/{name})`, `POST /v1/runbooks/validate` (deterministic findings + AI suggestions via BYOK), v1 pipelines untouched | Complete — end-to-end on postgres |
| **Sessions + turns** | multiturn data plane: session pins name@version + snapshots token level/compartments; turns = access-filtered multi-collection retrieval (per-collection ProvenanceEnvelopes) + optional RAG completion through the shared model resolver (`model_override` policy-gated); JWT `query` scope + uid; `POST /v1/sessions/{id}/turns/stream` for the same turn as SSE phase-progress events | Complete |
| **Ingestion + lifecycle** | `POST /v1/ingest(+/batch)` (`ingest` scope; explicit or matcher auto-bind; clearance-checked writes), double-pass soft removal (`remove-request` → `remove-confirm`, 15-min TTL, 410 afterwards, data retained), DBA-only physical deletion runbook | Complete |
| **Reporting + hardening** | `GET /v1/reports/usage\|audit\|cost` (mgmt), token issuance audit + revoke (deny-list enforced when `MUNARIUM_TOKEN_REVOCATION_CHECK=true`), idempotency table-backed in pg mode (restart/replica-safe) | REST complete; gRPC parity for the platform surface is a tracked follow-up |
| **Storage + extraction** | multi-cloud source stores: `SourceStore` seam + `munarium-store-objects` over `object_store` 0.14 (Azure Blob / S3(-compatible) / GCS / local file), local DOCX/PDF extraction (`munarium-extract`, optional `ocr` feature), Azure Document Intelligence escalation (`munarium-docintel-az`, off by default), MinIO `--profile s3` smoke target | Complete |

## Quickstart (dev profile)

Prerequisites: Docker (all-in-one) or Rust ≥1.89 (source builds).

```powershell
cd server
docker compose up --build          # postgres+pgvector, munarium-server
# optional profiles:
docker compose --profile gateway up      # + Envoy gateway plane on :8443
docker compose --profile s3 up -d minio  # + MinIO, the s3 source-store smoke target

# first requests (the full /v1 surface is live — see docs/api/)
curl http://localhost:8080/healthz
curl http://localhost:8080/version
```

> Port clash note: if another local service owns `127.0.0.1:8080`, run the server on an
> alternate port with `MUNARIUM_HTTP_ADDR=127.0.0.1:18080` (or remap the compose port).

From source on Windows (native — never link musl locally; the Linux binary is built in Docker/CI):

```powershell
cd server
cargo test --workspace                       # kernel + conformance, all offline
cargo run -p mmp-conformance -- --in-process # the fixture report
cargo run -p munarium-server                    # skeleton on :8080
```

## API planes

Three planes, one service — each built and documented:

| Plane | Port | Transport | Docs |
|---|---|---|---|
| **HTTP REST** | 8080 (443 via gateway) | JSON, problem+json errors, OpenAPI | [docs/api/rest.md](docs/api/rest.md) |
| **gRPC via gateway** | 443 | HTTP/2, content-type routed by Envoy to the gRPC upstream | [docs/api/grpc.md](docs/api/grpc.md) |
| **gRPC direct TCP** | 50051 | raw tonic listener (plaintext in demo; `MUNARIUM_GRPC_TLS_CERT/KEY` to arm TLS) | [docs/api/grpc.md](docs/api/grpc.md) |

The proto files under [proto/mmp/v1/](proto/mmp/v1/) are normative. Errors:
[docs/api/errors.md](docs/api/errors.md).

**Using the platform features** (uid contract, capability tokens,
compartmentalized collections, runbook applications, sessions, ingestion,
reports): the worked walkthrough is
[docs/guides/platform-features.md](docs/guides/platform-features.md);
loading real corpora into blob storage is
[docs/guides/loading-corpora.md](docs/guides/loading-corpora.md);
the optional OCR escalation is
[docs/guides/document-intelligence.md](docs/guides/document-intelligence.md);
the security rationale is [docs/security-posture.md](docs/security-posture.md).

**Client libraries:** official Rust / Python / .NET / Java clients (both transports,
conformance-proven against this server) live under
[clients/](../clients/).

## Configuration (env vars)

All `MUNARIUM_`-prefixed. The contract is stable; unset-but-required vars fail closed at startup:

| Var | Default | Purpose |
|---|---|---|
| `MUNARIUM_HTTP_ADDR` | `0.0.0.0:8080` | REST + /docs + health |
| `MUNARIUM_GRPC_ADDR` | `0.0.0.0:50051` | direct gRPC listener; literal `disabled` turns it off (ACA fallback) |
| `MUNARIUM_OPS_ADDR` | `0.0.0.0:9090` | ops plane: `/healthz`, `/readyz` (real store probe), `/metrics` (Prometheus text). Never exposed via ingress |
| `MUNARIUM_STORE` | `postgres` | `postgres` \| `memory` |
| `MUNARIUM_DATABASE_URL` | — | Postgres connection string |
| `MUNARIUM_DB_MAX_CONNS` | `10` | sqlx pool size per instance (floor 2). Cluster math: N × this + in-flight runbook locks < postgres max_connections |
| `MUNARIUM_AUTH_MODE` | `static` | `static` \| `disabled` (OIDC is deliberately NOT implemented — the API-management layer owns authn; see [docs/security-posture.md](docs/security-posture.md)) |
| `MUNARIUM_STATIC_TOKENS` | — | `token:tenant:role,...` (or `MUNARIUM_STATIC_TOKEN_FILE`); role ∈ `rw` \| `ro` \| `mgmt` |
| `MUNARIUM_TOKEN_SECRET` | — | HS256 secret (≥ 32 bytes) for capability JWTs (or `MUNARIUM_TOKEN_SECRET_FILE`); unset = token issuance/JWT auth unavailable |
| `MUNARIUM_TOKEN_TTL_SECS` | `3600` | default capability-token TTL (hard cap 24 h) |
| `MUNARIUM_REQUIRE_UID` | `true` | require `X-Munarium-Uid` / `munarium-uid` on every /v1 call (`false` substitutes `anonymous` — dev only) |
| `MUNARIUM_INTERACTION_BODY_MAX` | `32768` | interaction-capture body cap; larger bodies stored as sha256+length |
| `MUNARIUM_TOKEN_REVOCATION_CHECK` | `false` | check the access_tokens deny-list on every JWT verify |
| `MUNARIUM_SOURCE_STORE` | `az` (pg store) / `mem` | where raw document bytes live: `az` \| `pg` \| `mem` \| `s3` \| `gcs` \| `file`. `pg` is the offline fallback the dev profile uses; the cloud + `file` backends ride the `object_store` adapter (munarium-store-objects) |
| `MUNARIUM_AZURE_STORAGE_ACCOUNT` | — | **required** when `MUNARIUM_SOURCE_STORE=az` (fails closed, like `MUNARIUM_DATABASE_URL` under `MUNARIUM_STORE=postgres`) |
| `MUNARIUM_AZURE_BLOB_CONTAINER` | `sources` | blob container holding source documents |
| `MUNARIUM_BLOB_AUTH` | `managed_identity` | `managed_identity` (no secret exists) \| `sas` (off-Azure tooling) |
| `MUNARIUM_AZURE_CLIENT_ID` | unset | user-assigned identity client id; unset = system-assigned |
| `MUNARIUM_BLOB_SAS_REF` | — | required under `MUNARIUM_BLOB_AUTH=sas`: an env-var name, or `file:/path` for a CSI mount |
| `MUNARIUM_AZURE_BLOB_ENDPOINT` | unset | endpoint override (Azurite, sovereign clouds) |
| `MUNARIUM_S3_BUCKET` | — | **required** when `MUNARIUM_SOURCE_STORE=s3` (fails closed) |
| `MUNARIUM_S3_REGION` | `AWS_REGION` / `AWS_DEFAULT_REGION` | SigV4 region; defaults to `us-east-1` only when `MUNARIUM_S3_ENDPOINT` is set (S3-compatibles ignore it) |
| `MUNARIUM_S3_ENDPOINT` | unset | S3-compatible endpoint (MinIO, Cloudflare R2); `http://` additionally allows plaintext for loopback tooling |
| `MUNARIUM_S3_FORCE_PATH_STYLE` | `true` iff endpoint set | bucket-in-path addressing (MinIO needs it; AWS prefers virtual-hosted) |
| `MUNARIUM_S3_ACCESS_KEY_ID` | unset | static-credential mode for off-cloud tooling; unset = the ambient AWS chain (env vars, web identity/IRSA, instance profile) |
| `MUNARIUM_S3_SECRET_KEY_REF` | — | required iff the key id is set: an env-var name, or `file:/path` — never the secret inline |
| `MUNARIUM_GCS_BUCKET` | — | **required** when `MUNARIUM_SOURCE_STORE=gcs` (fails closed) |
| `MUNARIUM_GCS_CREDENTIALS_REF` | unset | env-var name or `file:/path` yielding service-account key JSON; unset = `GOOGLE_APPLICATION_CREDENTIALS` / metadata server |
| `MUNARIUM_FILE_ROOT` | — | **required** when `MUNARIUM_SOURCE_STORE=file` (no silent temp-dir default); directory is created if absent |
| `MUNARIUM_DOCINTEL` | `none` | document-intelligence escalation for unreadable scans: `none` \| `azure`. **Off by default — it is paid and egresses**; see [docs/guides/document-intelligence.md](docs/guides/document-intelligence.md) |
| `MUNARIUM_DOCINTEL_ENDPOINT` | — | **required** when a provider is selected (fails closed) |
| `MUNARIUM_DOCINTEL_AUTH` | `managed_identity` | `managed_identity` (no secret exists) \| `key` |
| `MUNARIUM_DOCINTEL_KEY_REF` | — | required under `key`: env-var name, or `file:/path` |
| `MUNARIUM_DOCINTEL_MODEL` / `_MAX_BYTES` / `_TIMEOUT_SECS` | `prebuilt-read` / 100 MiB / 180 | model and per-document bounds |
| `MUNARIUM_LOG` / `MUNARIUM_LOG_FORMAT` | `info` / `pretty` | tracing filter / `json` for structured JSON lines |
| `MUNARIUM_MAX_CONCURRENCY` | `512` | in-flight request ceiling per plane per instance; at the limit new /v1 and /mmp.v1 requests get 503 `overloaded` + `Retry-After: 1` (health routes never shed) |
| `MUNARIUM_IDEMPOTENCY_TTL_SECS` | `86400` | idempotency-key retention, pruned by the per-instance janitor; `0` keeps records forever |
| `MUNARIUM_REPLICA_COUNT` | `1` | instances sharing this database. `>1` arms cluster-mode validation (refuses per-process stores) and divides provider rate budgets so the CLUSTER honors a configured rpm/tpm ([docs/ops/clustering.md](docs/ops/clustering.md)) |
| `MUNARIUM_REGISTRY_TTL_SECS` | `15` | shape/provider registry staleness bound across instances; `0` = load-once (single-instance only) — also bounds how fast a `POST /v1/max-tokens` replacement reaches the other replicas |
| `MUNARIUM_MAX_TOKENS_TURN_COMPLETION` | `2048` | per-call output ceiling for a session turn's answer (the truncation retry pays one 4x re-ask); a runbook's `completion.maxTokens` overrides it, a tenant's `POST /v1/max-tokens` replaces it — see [docs/tokenbudgets.md](docs/tokenbudgets.md) |
| `MUNARIUM_MAX_TOKENS_QUERY_EXPANSION` | `256` | the `modelQueryExpansion` call (runbook `modelQueryExpansion.maxTokens` overrides) |
| `MUNARIUM_MAX_TOKENS_COMPLETE_DEFAULT` | `1024` | `POST /v1/providers/{name}/complete` when the request omits `max_tokens` |
| `MUNARIUM_MAX_TOKENS_HEALTHAI_PROBE` | `512` | each `/healthai` probe completion |
| `MUNARIUM_MAX_TOKENS_HIERARCHY_CLASSIFIER` | `32` | the evidence hierarchy's question classifier |
| `MUNARIUM_MAX_TOKENS_HIERARCHY_INTENT` | `480` | the evidence hierarchy's semantic-intent task |
| `MUNARIUM_MAX_TOKENS_RUNBOOK_ADVISORY` | `2048` | the runbook validation AI advisory pass |
| `MUNARIUM_MAX_TOKENS_AUTHORING_ASSIST` | `8192` | the guided-authoring assist draft. All eight: unset = built-in; set = must parse and sit in range or the server refuses to start |
| `MUNARIUM_RETRIEVAL_MODE` | `postgres` | which engine serves retrieval: `postgres` \| `mirror` \| `shadow` \| `datastore` ([Datastore guide](docs/guides/datastore.md)). Anything but `postgres` needs the Postgres store and `MUNARIUM_DATASTORE_LOCAL_ROOT`; in `datastore` mode each scope's engine is the rollout selector's (`PUT /v1/retrieval-rollout`). An unknown value logs and falls back to `postgres` |
| `MUNARIUM_DATASTORE_LOCAL_ROOT` | unset | local-disk root for hydrated artifacts (the L1 tier) — required for every mode but `postgres`; `<root>/l2` and `<root>/staging` are the defaults of the two roots below |
| `MUNARIUM_DATASTORE_ARTIFACT_STORE` | unset (= unavailable to capability detection) | set explicitly: `file` \| `az` \| `s3` \| `gcs`; the artifact factory does not implement `pg`. Its own fallback is `file`, but omitting the setting does not enable the artifact-store capability. Cloud clients reuse the cloud source-store configuration with a separate artifact container/bucket |
| `MUNARIUM_DATASTORE_ARTIFACT_CONTAINER` | `indexes` | container / bucket for artifacts on `az` / `s3` / `gcs` |
| `MUNARIUM_DATASTORE_ARTIFACT_PREFIX` | `v1` | key prefix under that container |
| `MUNARIUM_DATASTORE_ARTIFACT_ROOT` | `<LOCAL_ROOT>/l2` | artifact root for the `file` store |
| `MUNARIUM_DATASTORE_STAGING_ROOT` | `<LOCAL_ROOT>/staging` | where a build assembles an artifact before sealing it (a failed build cleans its own directory) |
| `MUNARIUM_DATASTORE_L1_HIGH_WATERMARK` / `_LOW_WATERMARK` | 8 GiB / 6 GiB (bytes) | L1 eviction thresholds; a freshly hydrated artifact is never evicted by its own hydration |
| `MUNARIUM_DATASTORE_L0_OPEN_SHARDS` | `8` | how many shards stay open in memory; raise it when many small collections thrash the default |
| `MUNARIUM_DATASTORE_PIN_HORIZON` | derived | seconds a retired index version stays serving-required so a live session's pin still resolves; unset DERIVES it from the session/runbook TTLs plus the recovery margin. With `MUNARIUM_SESSION_IDLE_TTL_SECS=0` (immortal sessions) nothing derivable covers a pin, and the server says so at boot |
| `MUNARIUM_DATASTORE_ALLOW_SHORT_PIN_HORIZON` | `false` | `true` accepts a horizon below the TTLs in force; otherwise that configuration is refused |
| `MUNARIUM_DATASTORE_RETIRED_RETENTION` | 2× the derived pin horizon (seconds) | retention policy validated against the configured pin horizon; a lower value is refused. An explicit longer pin horizon does not increase this default, so set retention explicitly as needed |
| `MUNARIUM_DATASTORE_ROLLOUT_REFRESH_MS` | `15000` (min 1000) | the readiness warmer's re-read interval for the rollout selector |
| `MUNARIUM_DATASTORE_STARTUP_HYDRATE_TIMEOUT_MS` | `120000` | how long startup hydration may take before `/readyz` reports the unmet scope |
| `MUNARIUM_DATASTORE_RECONCILE_INTERVAL_SECS` | `60` (min 30) | interval for reconciling interrupted sealed-build attempts/publication; also runs at startup |
| `MUNARIUM_DATASTORE_BUILDER` | unset (off) | `enabled` runs the durable build-job loop on this process (`POST /v1/index-build-jobs`); any Postgres-connected process with the staging configuration can be a builder |
| `MUNARIUM_DATASTORE_BUILDER_POLL_MS` | `5000` (min 250) | the builder's queue poll interval |
| `MUNARIUM_DATASTORE_JOB_LEASE_SECS` | `600` (min 30) | a claimed job's lease; a lapsed lease is re-offered until the attempt ceiling |
| `MUNARIUM_DATASTORE_SHADOW_SAMPLE_RATE` | `0` (off) | in `shadow` mode, run one turn in N through the datastore candidate path for comparison |
| `MUNARIUM_DATASTORE_SHADOW_MAX_CONCURRENT` | `2` | shadow comparisons in flight |
| `MUNARIUM_DATASTORE_QUERY_TIMEOUT_MS` | `5000` (min 1) | the shadow query deadline; not a timeout for all serving requests |
| `MUNARIUM_DATASTORE_VECTOR_APPROX_THRESHOLD` | `4096` | DIRECT builds with vectors choose DiskANN at or above this chunk-count threshold when `vector-diskann` is compiled in. `off` forces exact; numeric values clamp to at least 1; invalid values warn and select exact. Existing artifacts and mirror plans are unchanged |
| `MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID` | `local` | the environment scope of node snapshots and plane expectations (a process serves every tenant, so these are per environment, not per tenant) |
| `MUNARIUM_DEPLOYMENT_PLANE` | `rest` | plane label in serving-node snapshots, matched by fleet promotion expectations |
| `MUNARIUM_DEPLOYMENT_REVISION` | `local` | deployment revision label in serving-node snapshots, matched by fleet promotion expectations |
| `MUNARIUM_SESSION_IDLE_TTL_SECS` | `0` (off) | idle-session expiry: open sessions idle longer than this are stamped `expired` by the janitor; further turns answer 409 `session-not-open` |
| `MUNARIUM_INSTANCE_ID` | `HOSTNAME`→`COMPUTERNAME`→random | this instance's identity in logs and interaction rows |
| `MUNARIUM_SHUTDOWN_GRACE_SECS` | `20` | drain window on SIGTERM/SIGINT (/readyz flips to 503 "draining" the moment the signal fires) |
| `MUNARIUM_GRPC_TLS_CERT` / `MUNARIUM_GRPC_TLS_KEY` | unset | TLS for the direct gRPC port |
| `MUNARIUM_SECRET_ANTHROPIC` / `MUNARIUM_SECRET_OPENAI` / `MUNARIUM_SECRET_OPENROUTER` | unset | default BYOK provider keys (inject them from your secret store in a deployed environment) — power the default-provider rule (config name `default`: anthropic → openai → openrouter) and `GET /healthai` (live probe of the nine built-in tier models: haiku/sonnet/fable-5-1, gpt-5.4-mini/gpt-5.4/gpt-5.6-sol, deepseek-v4-flash/glm-5.2/glm-5.3) |

## Workspace layout

Crates live in [src/](src/): `munarium-proto` (generated MMP), `munarium-api-types` (REST DTOs — no server dependency, they ship in the public contract bundle), `munarium-api-conv` (the core↔DTO `Convert` conversions),
`munarium-access` (HS256 capability JWTs + the level/compartment permit check — pure logic),
`munarium-core` (the pure kernel — ledger, gates, composer; storage/retrieval/provider behind
traits), `munarium-store-mem` / `munarium-store-pg`, `munarium-azure-auth` (managed-identity token
acquisition with expiry-skew caching), `munarium-store-objects` (S3 / Azure Blob / GCS /
local-file `SourceStore` over the Apache Arrow `object_store` crate — one adapter, four
backends, ambient credentials), `munarium-extract` (local DOCX/PDF text extraction, OCR behind
a feature — pure Rust only), `munarium-docintel-az` (Azure Document Intelligence behind the
`DocumentIntelligence` trait — the paid OCR escalation), `munarium-retrieval-pg`,
`munarium-shapes`, `munarium-runbooks`, `munarium-providers`, `munarium-server`, `munarium-cli`
(`mmctl`). Plus [proto/](proto/), [conformance/](conformance/), [deploy/](deploy/)
(helm, envoy, an example terraform module), [runbooks/](runbooks/).

Boundary rules (CI-checked): `munarium-core` never depends on sqlx/axum/tonic/reqwest;
`munarium-providers` never on storage crates; rustls everywhere, openssl banned by `deny.toml`.

## Demo vs production posture

| Concern | This demo | Production (architecture.md) |
|---|---|---|
| Tenancy | single DB, `tenant_id` column, `TenantScopedStore` handle | database-per-tenant per CNPG cell |
| Pooling | direct sqlx pool | pgcat transaction pooling, watermark-routed replicas |
| Secrets | env/file in compose; your secret store (Key Vault, CSI) when deployed | Secrets Store CSI + customer vault |
| Cells | one | share-nothing fleet, tenant placement |
| gRPC direct TLS | plaintext (documented) | rustls via `MUNARIUM_GRPC_TLS_*` |

## Conformance

```powershell
cargo run -p mmp-conformance -- --in-process   # in-memory backend
docker compose up -d postgres
cargo run -p mmp-conformance -- --postgres postgres://munarium:munarium-dev@localhost:5433/munarium
# pg integration tests run when MUNARIUM_TEST_DATABASE_URL is set (same URL);
# --http/--grpc run the black-box mode against a live server — the same
# scenarios drive both planes, so the run IS the cross-plane parity check
```

## Deploy

Everything needed to run the server beyond a laptop is under [deploy/](deploy/): the Helm
chart ([deploy/helm/munarium/](deploy/helm/munarium/README.md)) and an illustrative Terraform
module that stands up AKS, CloudNativePG and Envoy Gateway and installs the chart
([deploy/terraform/example-aks/](deploy/terraform/example-aks/README.md)). The operator's
procedure — build, roll, verify, roll back — is
[docs/ops/deployment-runbook.md](docs/ops/deployment-runbook.md). `.\gates.ps1` runs every
gate CI runs, locally, against a compose PostgreSQL. CI: `.github/workflows/server-ci.yml`
(path-scoped to `server/**`).

---

© 2026 Ioka LLC.
