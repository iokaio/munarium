# munarium-server: Architecture

The architecture of what ships in this repository: the Rust workspace under `server/`, the container it builds, the Helm chart and compose profiles that run it, and the design rules the code is held to. Where a paragraph describes a design target the code does not yet reach, it says so in that paragraph — the two are never blended.

---

## 1. Purpose and Scope

`munarium-server` is a cloud-native, containerized Rust implementation of the Munarium governed-memory service: an append-only fact ledger with supersession chains, governance enforced below the application layer, and reproducible retrieval with a provenance envelope on every answer. It is built to be deployed by an platform into its own Kubernetes environment and operated without Ioka in the loop, and it connects to **the operator's** LLM provider accounts, endpoints and credentials (bring-your-own-key). Anthropic, OpenAI and OpenRouter are the supported provider dialects; OpenAI-compatible endpoints (vLLM, Ollama-served, platform inference gateways) ride the OpenAI implementation with a base-URL override.

Three artifacts make up a release:

1. **The container** — a static Rust binary in a distroless OCI image ([`Dockerfile`](../Dockerfile)).
2. **The deployment code** — the Helm chart ([`deploy/helm/munarium`](../deploy/helm/munarium/README.md)), the compose profiles ([`docker-compose.yml`](../docker-compose.yml)), and an illustrative Terraform module for AKS ([`deploy/terraform/example-aks`](../deploy/terraform/example-aks/README.md)).
3. **The runbook system** — declarative, versioned definitions of data shapes and their indexing pipelines ([`runbooks/`](../runbooks/README.md)), applied to a running server without code changes to the core.

Out of scope for this document: the client libraries (they live in [`clients/`](../../clients/) and speak to the server only through the Munarium Protocol), Munarium Matrix (the structured-evidence plane, its own image in [`matrix/`](../../matrix/); the two trees never share a crate), and a Kubernetes operator (nothing here needs CRD-driven provisioning).

---

## 2. Architecture Overview

The system is four layers plus one cross-cutting gateway. Each layer scales by a different mechanism.

```
                        ┌─────────────────────────────────────┐
                        │        Envoy Gateway / Ingress       │  TLS; gRPC routed by content-type
                        └───────────────┬─────────────────────┘
                                        │
                        ┌───────────────▼─────────────────────┐
   Layer 1 (stateless)  │  munarium-server pods (Rust, axum)  │  N identical replicas
                        │   Command · Query · Retrieval ·     │
                        │   Ingest · Runbook · Provider APIs  │
                        └───────┬───────────────┬─────────────┘
                                │               │
                  ┌─────────────▼──┐    ┌───────▼──────────────┐
   Layer 2        │ sqlx pool per  │    │  Provider Gateway    │  BYOK egress to
                  │ replica        │    │  (Anthropic/OpenAI/  │  the operator's LLM
                  └──────┬─────────┘    │   OpenRouter)        │  endpoints
                         │              └──────────────────────┘
          ┌──────────────▼──────────────────────┐
   Layer 3│  PostgreSQL 16 + pgvector           │  a CloudNativePG cell (the chart)
          │  partitioned, tenant-scoped ledger  │  or any managed service
          └──────────────┬──────────────────────┘
                         │
          ┌──────────────▼──────────────────────┐
   Layer 4│  Object storage via object_store:   │  content-addressed source documents,
          │  Az Blob · S3(-compat) · GCS ·      │  sealed evidence, search artifacts
          │  local file (pg/mem fallbacks)      │  (the datastore tier)
          └─────────────────────────────────────┘
```

**Design tenets.** The model proposes; the mesh disposes — governance is a property of the command path, never an API to call or skip. Reproducibility is the product — every retrieval answer carries a provenance envelope (document ids, content hashes, index version, event watermark), and every index is rebuildable from content-addressed sources. Rebuild, don't migrate — derived stores are regenerable; only the ledger is precious. No proprietary service is required in the path — every managed cloud service is a substitution at a protocol seam, never a dependency.

---

## 3. Rust Workspace

A single Cargo workspace, library-first. The server is a thin shell; the library is the product and the canonical SDK.

```
server/
├── Cargo.toml                 # workspace — 20 crates + conformance
├── src/
│   ├── munarium-proto/           # generated MMP types + tonic service stubs (wire-only)
│   ├── munarium-api-types/       # REST DTOs: the single place JSON casing is decided; no server dependency
│   ├── munarium-api-conv/        # core <-> DTO conversions; keeps api-types server-free
│   ├── munarium-access/          # HS256 capability JWTs + level/compartment permit check
│   ├── munarium-core/            # ledger, supersession, gates, context composition
│   │   └── (no HTTP, no provider calls; storage + retrieval behind traits)
│   ├── munarium-store-mem/       # in-memory StorageBackend (conformance target, dev)
│   ├── munarium-store-pg/        # PostgreSQL StorageBackend (sqlx), partitioning, additive migrations
│   ├── munarium-azure-auth/      # managed-identity tokens (endpoint/IMDS), expiry-skew cached
│   ├── munarium-store-objects/   # SourceStore over object_store: S3 / Az Blob / GCS / local file
│   ├── munarium-extract/         # local DOCX/PDF text extraction; OCR behind the `ocr` feature
│   ├── munarium-docintel-az/     # Azure Document Intelligence: the paid, off-by-default OCR escalation
│   ├── munarium-retrieval-pg/    # in-Postgres hybrid: tsvector lexical + pgvector ANN, RRF fusion
│   ├── munarium-retrieval/       # retrieval coordinator: engine selection, query preparation, facade
│   ├── munarium-datastore/       # immutable, content-verified search artifacts: build, seal, verify, query
│   ├── munarium-shapes/          # shape registry: schema-driven data shapes + validation
│   ├── munarium-runbooks/        # runbook parser + the checkpointed step machine's types
│   ├── munarium-authoring/       # guided shape+runbook authoring: pattern catalog, interview, materialization
│   ├── munarium-providers/       # ModelProvider trait + anthropic / openai / openrouter impls
│   ├── munarium-server/          # axum binary: REST :8080, direct gRPC :50051, ops :9090
│   └── munarium-cli/             # mmctl: apply/run/approve, authoring, bulk upload, datastore ops
├── proto/                     # MMP: protobuf (normative, versioned)
├── conformance/               # black-box suite: in-process, pg, REST/gRPC, platform and cluster tiers
├── contract/                  # inbound Matrix contract (vendored), outbound MMP bundle, datastore artifact contract
├── deploy/
│   ├── envoy/                    # gateway and cluster round-robin configs for the compose profiles
│   ├── helm/munarium/            # the chart: CNPG cell + deployment + three API planes
│   └── terraform/example-aks/    # an illustrative AKS module that consumes the chart
└── runbooks/                  # sample shapes, pipelines, experiment runbooks, provider configs
```

Key crate boundaries:

| Crate | Depends on | Must never depend on |
|---|---|---|
| `munarium-core` | std, serde, thiserror | tokio-postgres/sqlx, axum, reqwest |
| `munarium-store-pg` | munarium-core, sqlx | provider crates |
| `munarium-store-objects` | munarium-core, object_store | sqlx, axum, provider crates |
| `munarium-extract` | munarium-core + pure-Rust parsers (zip/quick-xml/pdf-extract; ocrs behind `ocr`) | network clients, sqlx, axum — no C natives, ever (the musl static link is the enforcement) |
| `munarium-azure-auth` | munarium-core, reqwest | sqlx, axum, storage crates |
| `munarium-docintel-az` | munarium-core (DocumentIntelligence trait), munarium-azure-auth, reqwest | storage crates |
| `munarium-datastore` | munarium-core | axum, tonic, sqlx — independently usable |
| `munarium-providers` | munarium-core (types only), reqwest | storage crates |
| `munarium-server` | everything above | any `matrix/` crate (ground rule 1); scorers, judges, experiment harnesses |

The trait surface in `munarium-core` is the load-bearing element:

```rust
pub trait StorageBackend: Send + Sync {
    async fn append_events(&self, stream: StreamId, expected_head: Seq, events: &[Event]) -> Result<Seq>;
    async fn slice_facts(&self, q: FactQuery, as_of: Option<Seq>) -> Result<FactSlice>;
    async fn head(&self, stream: StreamId) -> Result<Seq>;
    // supersession, lineage, promises, anchors ...
}

pub trait RetrievalBackend: Send + Sync {
    async fn hybrid_search(&self, q: HybridQuery) -> Result<SearchResult>; // always with ProvenanceEnvelope
    async fn index_version(&self) -> Result<IndexVersion>;
}

pub trait ModelProvider: Send + Sync {
    fn id(&self) -> ProviderId;                       // anthropic | openai | openrouter | ...
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse>;
    async fn embed(&self, req: EmbeddingRequest) -> Result<EmbeddingResponse>;
    async fn health(&self) -> Result<ProviderHealth>; // key validity, endpoint reachability
}
```

Toolchain and technology choices:

| Concern | Choice | Rationale |
|---|---|---|
| HTTP | axum + tower | middleware for timeouts, load-shed, limits; hyper-backed |
| DB access | sqlx, runtime-checked query strings (no `query!` macros) | the image builds with no database; conformance against both store backends is the query-drift net |
| Async | tokio | ecosystem default |
| Serialization | serde + prost | MMP is gRPC-first with a JSON/HTTP twin |
| OpenAPI | utoipa, generated from code | `docs/api/openapi.json` is generated and CI drift-checked |
| Errors | thiserror (lib) / typed problem+json (API) | machine-actionable rejections incl. policy citations ([api/errors.md](api/errors.md)) |
| Telemetry | tracing (JSON lines) + hand-rolled Prometheus exposition | vendor-neutral; no OTel export yet (§12) |
| Build | cargo + musl static link | distroless image, < 30 MB |

---

## 4. Data Tier: PostgreSQL

### 4.1 Topology

**PostgreSQL 16+ with pgvector** is the system of record. The Helm chart operates it with **CloudNativePG (CNPG)** — one `Cluster` per release, the pure-OSS posture — and a managed PostgreSQL service (any provider) is a substitution behind the same wire protocol: set `MUNARIUM_DATABASE_URL` and nothing else changes. Neither is a requirement of the other.

*Implemented today:* one database per deployment in which every row is tenant-scoped and every credential is bound to one tenant, served by N identical replicas against one primary. *Design target:* the **cell** as the scaling unit — one CNPG cluster hosting one database per tenant, share-nothing between cells, tenant movement as a database-level operation. The append-only, governance-gated write profile makes a single tenant outgrowing a primary unlikely; the documented escape hatch is distributed SQL behind the Postgres wire seam, gated by a measured improvement rule.

### 4.2 Ledger schema principles

The ledger is append-only with per-stream sequences and optimistic head checks (`append_events` carries `expected_head`; a mismatch is a normal, retryable conflict, and ordering truth is the sequence — no reliance on wall clocks or single-writer files).

```sql
-- illustrative core; real DDL lives in versioned migrations
CREATE TABLE ledger_events (
    tenant_seq   BIGINT GENERATED ALWAYS AS IDENTITY,
    stream_id    UUID        NOT NULL,
    stream_seq   BIGINT      NOT NULL,
    event_type   TEXT        NOT NULL,
    body         JSONB       NOT NULL,
    body_hash    BYTEA       NOT NULL,     -- content address
    shape_ref    TEXT,                     -- schema-driven shape id@version (§6)
    recorded_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (stream_id, stream_seq)
) PARTITION BY RANGE (tenant_seq);
```

Range partitioning by `tenant_seq` keeps indexes small, vacuum local, and archival of cold partitions trivial. Migrations are **additive-only**: new columns nullable/defaulted, new tables, new partitions — never destructive DDL against the ledger. CI greps for it. Derived tables (claims-current, retrieval chunks, embeddings) are regenerable and may be rebuilt rather than migrated.

> **Implemented today: N-instance correctness on one database.** Multiple identical `munarium-server` instances against one PostgreSQL are correct and black-box proven (the `-Cluster` conformance tier: registry convergence within `MUNARIUM_REGISTRY_TTL_SECS`, table-backed idempotency across instances, interleaved seq allocation under the `lineage_heads` FOR UPDATE mutex, and advisory-locked single-executor runbook runs). Instances handle SIGTERM with a readiness-drain window; `ledger_events` partition maintenance runs as an advisory-locked daily sweep in every instance; provider rate budgets divide by `MUNARIUM_REPLICA_COUNT`; every audit row carries its instance id. The operator contract is [ops/clustering.md](ops/clustering.md). **Connection pooling and replica-read routing remain design-only** — each instance pools directly to the primary (`MUNARIUM_DB_MAX_CONNS`). The design is a transaction-pooling proxy with read/write routing, where watermark-satisfied reads (`as_of_seq` ≤ replica replay position) go to replicas and the provenance envelope's event watermark is the read-your-writes proof. The named follow-up for scale-out throughput is moving runbook execution off the request path.

### 4.3 Durability and recovery

CNPG can archive WAL to object storage (the Barman Cloud plugin) for point-in-time recovery per cell — **the chart does not configure a backup target**, so a chart install has a replica and no PITR until you add one; a managed service brings its own continuous backup. Either way the procedure, and what a database restore does and does not cover, is [ops/backup-restore.md](ops/backup-restore.md). Object storage also holds content-addressed source documents, sealed evidence and search artifacts, so **any derived store is rebuildable from ledger + sources** — the production form of "rebuild, don't migrate."

---

## 5. Retrieval and Indexing

### 5.1 In-Postgres hybrid

Lexical search via `tsvector`/`tsquery` with per-shape text configurations; vector ANN via **pgvector** (HNSW). Hybrid ranking (reciprocal rank fusion by default, shape-configurable) executes in `munarium-retrieval-pg`, keeping one engine inside the transaction boundary. Every result carries the provenance envelope: chunk ids, source ids, source **paths** (which documents answered — a bare hash never said), source content hashes, `index_version`, and the ledger event watermark the index reflects.

### 5.2 The datastore tier: immutable search artifacts

`munarium-datastore` builds **immutable, content-verified search artifacts** from the same logical index version — the `artifact@1` contract in [../contract/datastore/README.md](../contract/datastore/README.md) fixes how a manifest is canonicalized and hashed, so an `artifact_id` is a content address that a reader built years later can still verify. A collection or shape is **rolled out** to serve from artifacts (`PUT /v1/retrieval-rollout`, gated on every serving-required version being complete) and rolled back to Postgres with the same route, never gated. Direct builds with vectors choose exact search below `MUNARIUM_DATASTORE_VECTOR_APPROX_THRESHOLD` and DiskANN at or above it when the `vector-diskann` feature is compiled in (on by default in the shipped image); `off` forces exact. The setting does not change existing artifacts or mirror plans. A replica that agreed to serve a scope from the datastore and cannot leaves the traffic pool (`datastore-unavailable`) rather than silently answering from PostgreSQL, so the two engines can never be confused for each other. The `munarium-retrieval` coordinator picks the engine per request; nothing above the trait changes. The [Datastore guide](guides/datastore.md) covers deployment, all settings, validation and rollback; [ops/mmctl.md](ops/mmctl.md) is the command reference.

An external search cluster behind the same `RetrievalBackend` trait remains a possible substitution and is not built.

### 5.3 Index lifecycle

Indexes are **versioned, immutable-once-built artifacts** with a manifest:

```
index_version = hash(shape_id@version, chunker@version, embedder(provider, model, dims), source_set_hash)
```

Building a new index never mutates the serving one. The pipeline (executed by runbooks, §7):

1. **Ingest** — sources land in object storage under their **logical path** (the caller-supplied filename), which is also their identity: collections bind by `filenamePrefix`, so the same bytes staged at two paths are two independently bindable, rebuildable, retirable sources. The content hash is verified before commit and travels with the source as *integrity*, not identity. An `ingest.recorded` event enters the ledger with hash, media type, and shape binding.
2. **Extract & chunk** — binary formats are extracted to text first: DOCX and PDF text layers locally and deterministically, with an OPTIONAL, off-by-default document-intelligence escalation (`munarium-core::docintel::DocumentIntelligence`) for scans local extraction cannot read; the per-shape chunker (deterministic, versioned) then produces chunk records keyed by `(source_id, chunker_version, chunk_ordinal)`.
3. **Embed** — via the tenant's own provider credentials through the Provider Gateway (§9); embedding calls are batched, cached by request hash (invocation provenance), and recorded with provider/model/dimension in the manifest.
4. **Load & verify** — chunks and vectors load into a new index version; verification must pass before the version is eligible.
5. **Cutover** — an atomic pointer flip (`POST /v1/collections/{id}/activate-index`, a compare-and-swap on the active version); the old version is retained per retention policy for reproduction of past answers.

**Re-indexing** is the same pipeline with a different trigger — new chunker version, new embedding model, corrected sources, or an explicit operator runbook — and always builds side-by-side then flips. A re-index never makes previously issued provenance envelopes unverifiable: old index manifests remain resolvable for as long as retention holds.

---

## 6. Schema-Driven Data Shapes

Workloads differ in *what* a fact, a document, and a chunk look like — contract clauses, patent office actions, court filings, regulatory letters. The server absorbs these differences **without code changes to core**. The mechanism is the **shape registry**.

A **shape** is a versioned, declarative bundle:

```yaml
# runbooks/shapes/cuad-contracts@3.yaml
apiVersion: munarium.ioka.io/v1
kind: Shape
metadata:
  name: cuad-contracts
  version: 3
spec:
  fact:
    schema:            # JSON Schema (2020-12) for event bodies bearing this shape_ref
      $ref: ./schemas/cuad-fact.schema.json
    supersession:      # which fields identify a claim lineage
      identity: [contract_id, clause_type]
  document:
    mediaTypes: [application/pdf, text/plain]
    extractor: { name: pdf-text, version: 2 }
  chunking:
    strategy: { name: clause-aware, version: 4, params: { max_tokens: 512, overlap: 64 } }
  indexing:
    lexical: { textConfig: english, fields: [clause_text, clause_type] }
    vector:
      embedding: { providerClass: any, model: preferred-list, dimensions: 1024 }
    hybrid: { fusion: rrf, k: 60 }
  fixtures:            # verification queries + expected-result tolerance bands
    $ref: ./fixtures/cuad-fixtures.yaml
```

**Implemented today** (`munarium-shapes`): `spec.fact.schema` (inline JSON Schema —
`$ref` to a sibling file is not resolved), `spec.fact.supersession.identity`,
`spec.chunking.{strategy, max_chars}` (strategy is a plain version string, e.g.
`para@1`), `spec.indexing.{rrf_k, candidate_n}`, and `spec.evidence` (see
[guides/evidence-hierarchy.md](guides/evidence-hierarchy.md)). The `document:`
block and the structured `chunking.strategy` / `indexing.lexical|vector|hybrid` /
`fixtures:` blocks above are the design target; unknown keys parse but are
ignored, so a copy of this example runs on the built-in defaults. A working
minimal shape is in [guides/platform-features.md](guides/platform-features.md)
§3.1; thirteen worked ones are under [`runbooks/shapes/`](../runbooks/shapes/).

Rules of the registry:

- **Validation at the gate.** Events carrying a `shape_ref` are validated against the shape's JSON Schema in the command path; a violation is a policy rejection with citation, recorded as an event. Governance below the application layer, extended to structure.
- **Additive versioning.** A new shape version may add fields and pipelines; it may not invalidate stored events. Old versions remain resolvable forever (they are part of provenance).
- **Shapes are data, not code.** They deploy through the runbook mechanism (§7), are stored in the ledger (a shape publication is itself an event), and are portable between implementations because they live in the MMP spec, not in any one of them.
- **Domain shapes ship as samples**, not as core: the contracts, patents, case-filings, regulatory and other shapes under `runbooks/shapes/` are the reference shape pack that demonstrates the mechanism.

---

## 7. Runbooks: Declarative, Deployable Operations

A **runbook** is a versioned, declarative definition of an operational pipeline over one shape (v1) or a set of collections (v2) — ingest, index, re-index, verify, archive — executed by the built-in runbook executor (`munarium-runbooks`) with durable, resumable steps recorded in the ledger. No external workflow engine is required; the executor is a checkpointed step machine whose state is (of course) events. An external workflow engine remains the documented substitution seam if a deployment's pipelines outgrow it.

```yaml
# runbooks/pipelines/cuad-reindex@2.yaml
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: cuad-reindex, version: 2 }
spec:
  shape: cuad-contracts@3
  trigger: { manual: true, on: [shape.updated, sources.corrected] }
  steps:
    - resolveSources: { from: ledger, filter: "shape_ref = 'cuad-contracts@*'" }
    - buildIndex:     { sideBySide: true }
    - verify:         { fixtures: shape, tolerance: shape }
    - cutover:        { approval: required }   # human gate; approval is an event
    - retireOld:      { keep_versions: 2 }
  concurrency: { maxParallelEmbedBatches: 8, providerBudget: tenant-default }
  observability: { emitEvents: true, otelSpanPerStep: true }
```

**Implemented today** (`munarium-runbooks`): the five-step vocabulary above, plus
`cutover.approval: required` and `retireOld.keep_versions` (snake_case — the
only step option besides `approval` the parser reads). `trigger:`,
`concurrency:`, `observability:`, and the per-step option maps on
`resolveSources`/`buildIndex`/`verify` are the design target; unknown keys
parse but are ignored. `spec.shape` is the v1 single-shape pipeline; the
shipped platform form is `spec.collections` (v2), with `spec.sources`,
`retrieval:`, `completion:`, `models:` and `researchProfiles:` — see
[guides/platform-features.md](guides/platform-features.md) §3 and the
thirteen samples under [`runbooks/applications/`](../runbooks/applications/).

Deployment of shapes and runbooks is a CLI/API operation, GitOps-friendly:

```
mmctl apply -f runbooks/shapes/cuad-contracts@3.yaml
mmctl apply -f runbooks/pipelines/cuad-reindex@2.yaml
mmctl run cuad-reindex --tenant acme --watch
```

`mmctl apply` validates, records the publication event, and makes the definition active; `run` starts an execution whose every step, retry, and approval is a ledger event — which means runbook executions are themselves reproducible, auditable objects. Wire `mmctl apply` into your own CI so runbook changes flow through review like code; runbooks live only in the database, so rolling an image never applies one.

**Guided authoring.** The server also carries the authoring half of this pipeline, so composing a well-designed shape+runbook set does not require hand-copying the samples: the `munarium-authoring` crate (pure, like `munarium-shapes`/`munarium-runbooks`) serves the seven measured application patterns as a catalog with every committed sample embedded, runs a design interview in the developers guide's decision order, deterministically materializes the documents from the answers, and validates the SET cross-document (`set.*` codes: unresolved shape refs, additive-versioning preflight against published hashes, sensitivity-inverting prefix overlap, answer-key bindings). Drafts live server-side (`authoring_drafts`; `/v1/authoring/*`, rw role, postgres only) with an optional BYOK assist pass that degrades to a note on keyless deployments. The deploy artifact is a hash-manifested JSON bundle: `mmctl author export` writes the reviewed files plus `bundle.json`, git carries them, and `mmctl bundle apply` re-verifies every hash and the manifest before POSTing each file — in shapes-first order — through the same `/v1/shapes` and `/v1/runbooks` routes above. Nothing reaches production that is not byte-identical to the validated export.

---

## 8. Deployment: What Ships

```
deploy/
├── envoy/
│   ├── envoy.yaml             # compose `gateway` profile: one listener on :8443 that routes
│   │                          #   application/grpc to the gRPC port and everything else to REST
│   └── envoy-cluster.yaml     # compose `cluster` profile: the same, round-robin over two instances
├── helm/munarium/             # one release = a CNPG `Cluster` + the server Deployment (N replicas)
│                              #   + ServiceAccount + Services; optional GatewayClass/Gateway/
│                              #   HTTPRoute/GRPCRoute (gateway.enabled) and a LoadBalancer on
│                              #   :50051 (directGrpc.enabled)
└── terraform/example-aks/     # resource group, AKS (system + user pool), a user-assigned identity
                               #   federated to the `munarium` ServiceAccount, a keyless storage
                               #   account, and three helm_releases: the CNPG operator, Envoy
                               #   Gateway, and this chart
```

Principles the deployment code follows:

- **Configuration lives in values, not in the image.** Environment, identities and roles are declared in Helm values (or your overlay) and in Terraform. A new image may require newly declared configuration, so configuration changes roll **with or before** the image they belong to, never after.
- **Updates are image-tag changes.** A new image → `image.tag` bumped → `helm upgrade` performs a rolling deployment behind the real-Postgres readiness probe. Migrations run at startup and are additive-only, so N replicas on two adjacent versions are correct together ([ops/clustering.md](ops/clustering.md)).
- **Rollback is a tag revert.** Because the schema never has to go backward, `helm rollback` (or the previous tag) runs correctly against a newer schema. There is no down-migration and none is needed.
- **BYO-cluster is first-class.** The chart installs on any cluster with the CNPG operator; with `sourceStore.account` empty, document bytes live in Postgres (`MUNARIUM_SOURCE_STORE=pg`) and no cloud account is involved at all; with `workloadIdentity.clientId` empty, no Azure annotation is rendered. The `s3` / `gcs` / `file` backends are set through raw environment overrides until they get first-class values.
- **What the chart does not wire, stated plainly:** `MUNARIUM_TOKEN_SECRET` and the `MUNARIUM_SECRET_*` provider keys ([chart README](../deploy/helm/munarium/README.md) has the workaround), a CNPG backup target (§4.3), and TLS on the gateway listener (provisioned out of band).

Status, stated plainly: the chart's first install was validated on kind (the chart README carries the evidence and what remains unexercised); the example AKS module is authored and `terraform fmt`/`validate`-checked in CI, not yet applied end to end. The operator's procedure is [ops/deployment-runbook.md](ops/deployment-runbook.md).

---

## 9. Provider Gateway: BYOK LLM Connectivity

The gateway is the only component that speaks to LLM APIs, and it speaks with the **tenant's** credentials to the **tenant's** endpoints. Ioka never proxies, meters, or holds model traffic economics; the operator's provider relationship stays theirs.

### 9.1 Supported providers

| Provider | API dialect | Endpoint override | Notes |
|---|---|---|---|
| **Anthropic** | Messages API | yes (default `api.anthropic.com`) | supports platform gateways/proxies via base-URL override |
| **OpenAI** | Chat Completions + Embeddings | yes | override covers Azure OpenAI-style deployments via compatible endpoint |
| **OpenRouter** | OpenAI-compatible | yes | one integration yields broad model routing; per-model allowlist supported |

The `ModelProvider` trait (§3) is the seam; a fourth provider is a new implementation of the trait plus a conformance fixture — no core changes.

### 9.2 Credential handling

Provider configurations are declarative and applied like any other asset (`mmctl apply -f`; samples under [`runbooks/providers/`](../runbooks/providers/)):

```yaml
apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata: { name: primary-anthropic }
spec:
  provider: anthropic
  credentialRef: { env: MUNARIUM_SECRET_ANTHROPIC }   # read from the pod's environment at call time
  budgets:
    rpm: 300
    dailyTokens: { fast: 1000000, capable: 500000 }     # per-tier daily caps, UTC-day window
```

Keys are **never stored in the ledger, in config maps, in the database, or in Terraform state**. `credentialRef` names an environment variable the deployment injects — from a Secrets Store CSI mount, a Kubernetes Secret, or whatever vault the platform uses — and the server reads it at call time; rotation is a redeploy of that secret, invisible to the ledger. `GET /v1/providers` never echoes a credential, only `credential_ok`. The gateway enforces per-config request-rate budgets and per-tier daily token caps (`rate-limited`, `daily-cap-reached`), retries upstream failures before refusing with `provider-error` (carrying an endpoint fingerprint, never key material), and divides rate budgets across replicas by `MUNARIUM_REPLICA_COUNT`. Egress allowlisting is the platform's job (NetworkPolicy), not the server's.

### 9.3 Invocation provenance

Every model call is recorded (request hash, provider, model, endpoint fingerprint, token counts, latency — never the key, and prompt/response bodies only per tenant retention policy). An answer's provenance envelope can therefore name not just its sources but the exact model configuration that touched them. Embedding calls are cached by request hash, which makes re-index runs cheap when only the chunker changed.

---

## 10. API Surface

Versioned, gRPC-first with a JSON/HTTP twin; the protos in `proto/` and the generated `docs/api/openapi.json` are normative, and the human guides are [api/rest.md](api/rest.md), [api/grpc.md](api/grpc.md) and [api/errors.md](api/errors.md). Representative surface:

| API | Operations |
|---|---|
| **Command** | `AppendEvents` (idempotent, optimistic head), `ProposeClaim`, `AcceptClaim`, `SupersedeClaim`, `OpenPromise`, `FulfillPromise`, `LockAnchor` |
| **Query** | `GetHead`, `GetClaim`, `SliceFacts` (current or `as_of_seq`), `GetLineage`, `ComposeContext` (budget-bounded) |
| **Retrieval** | `HybridSearch` → results + ProvenanceEnvelope; collections; index activation |
| **Ingest** | single, batch and bulk-session ingest (verify-then-commit, content-addressed) |
| **Shapes/Runbooks** | apply, validate, run, approve; sessions and turns over a runbook; guided authoring |
| **Providers** | apply config, health, complete, embed (governed; tenant-scoped); token budgets |
| **Evidence** | sealed evidence artifacts, findings, research profiles (the evidence hierarchy) |
| **Admin** | capability-token issuance and revocation; reports; the `/admin` operator console. Tenant lifecycle is declared and answers `UNIMPLEMENTED` — tenancy is provisioned out of band |

Cross-cutting: idempotency keys on all command paths (stateless replicas require it); policy rejections return problem+json with the policy citation and are themselves ledger events; end-user identity is asserted by the API-management layer in front of the server (`X-Munarium-Uid` / capability JWTs — [security-posture.md](security-posture.md)), which is where an platform IdP is integrated.

---

## 11. Container and Runtime

- **Image:** `cargo build --release --target x86_64-unknown-linux-musl`, static binaries (`munarium-server` and `mmctl`) into `gcr.io/distroless/static-debian12:nonroot`. Under 30 MB, cold start in milliseconds, no shell, no OS CVE surface. `build.ps1 -Image` runs the same `docker build`.
- **Supply chain:** `cargo deny` runs in this repository's CI as the license and advisory gate. Signed release images, with SBOM and provenance attestations, are cut by Ioka outside this repository; pin them by digest.
- **Runtime posture:** non-root (uid 65532 in the chart), read-only rootfs, no shell in the image. Config via environment; secrets only via the platform's secret mechanism.
- **Health:** `/healthz` (liveness), `/readyz` (probes the store; reports `draining` once SIGTERM arrives), graceful drain before pod termination.
- **Ports:** 8080 REST + `/docs` + `/openapi.json` + `/admin`; 50051 direct gRPC; 9090 ops (`/healthz`, `/readyz`, `/metrics`) — never exposed through an ingress.
- **Autoscaling** on latency/RPS, PodDisruptionBudgets and topology spread are design targets; the chart sets `replicas`.

---

## 12. Observability and Security

**Observability.** *Implemented today:* the ops plane (:9090) serves `GET /metrics` in Prometheus text format — RED metrics per plane/route/status-class, request-latency and provider-call histograms, DB-pool and audit-writer gauges, runbook step-transition and load-shed counters (hand-rolled exposition; cardinality rules in `munarium-server/src/metrics.rs`: no tenant/uid/instance labels — per-tenant analytics live in the interactions table and the reports API, and the scraper assigns `instance` per target). The ops `/readyz` really probes the store. Built-in server-rendered dashboards live at `/admin` on the REST plane (mgmt auth; inline SVG, zero JS), backed by the `/v1/reports/*` views over the interactions/sessions/runbook tables, which aggregate across every instance sharing the database. Any scrape stack works against `/metrics`; the chart carries `prometheus.io/*` scrape annotations. *Stated plainly:* there is no OTel trace export and no Grafana JSON ships — structured tracing logs (`MUNARIUM_LOG_FORMAT=json`) plus `/metrics` plus the reports API are the observability surface. Per-tenant RED metrics and span-per-runbook-step remain design targets. The full treatment is [observability.md](observability.md).

**Security model.** The API-management layer in front of the server is the security boundary; the server enforces data compartments (levels + compartments on every collection and token) and records the evidence. Tenant isolation today is logical — every row is tenant-scoped and every credential is bound to one tenant; database-per-tenant placement is the design target. Provider keys and the token secret are injected, never stored. All data is encrypted in transit at the ingress and in-cluster where a mesh provides it, and at rest by the storage layer. The ledger's append-only property plus event-recorded policy rejections give a tamper-evident audit trail by construction rather than by add-on. The full treatment, including the residual risks, is [security-posture.md](security-posture.md).

---

## 13. Deployment Profiles

| Profile | Purpose | Shape |
|---|---|---|
| **compose** (default) | evaluation; laptop parity | `docker compose up`: pgvector Postgres + one server; document bytes in Postgres (`MUNARIUM_SOURCE_STORE=pg`) so no object store is needed |
| **compose `gateway` / `cluster` / `s3`** | exercise the gateway plane, two replicas behind Envoy, the S3 backend against MinIO | `docker compose --profile <name> up` |
| **Helm chart** | a real cluster, no cloud account required | one CNPG cell + N replicas; kind or minikube through a managed Kubernetes |
| **example-aks** | a cloud deployment with managed identity and blob storage | the Terraform module — illustrative; read its README before relying on it |

Evaluation must never require the fleet; the compose profile is a contractual deliverable, not a courtesy.

---

## 14. Design Targets Not Yet Built

Named here so they are not mistaken for shipped behaviour:

1. Transaction pooling with read/write routing and replica reads (§4.1); runbook execution off the request path.
2. Database-per-tenant placement in cells (§4.1); tenant lifecycle over the API.
3. A CNPG backup target in the chart (§4.3).
4. The structured `document:` / `chunking` / `indexing` / `fixtures:` shape blocks (§6) and the `trigger:` / `concurrency:` / `observability:` runbook blocks (§7).
5. An external search cluster behind `RetrievalBackend` (§5.2).
6. OTel export, per-tenant RED metrics, span-per-runbook-step (§12); autoscaling and disruption budgets in the chart (§11).
7. OIDC/JWKS verification in the server — today capability tokens are HS256 against one server-held secret, by design ([security-posture.md](security-posture.md)).
