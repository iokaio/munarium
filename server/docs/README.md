# munarium-server documentation

Everything under `server/docs/` in one place. Until now these documents were
findable only by `ls` and folklore; this index is the map. If you add a
document, add it here — an unlisted doc is an unread doc.

## Design and record

| Document | What it is |
|---|---|
| [architecture.md](architecture.md) | The architecture of what ships: layers, crate boundaries, the data and retrieval tiers, shapes and runbooks, the deployment shapes, and the design targets the code does not yet reach. Read this before making structural decisions |
| [security-posture.md](security-posture.md) | Why the API-management layer is the security boundary; the uid contract, capability JWTs, and credential posture |

## API reference

| Document | What it is |
|---|---|
| [api/rest.md](api/rest.md) | The human REST guide: auth, uid, idempotency, route map |
| [api/grpc.md](api/grpc.md) | gRPC connections, metadata, plane parity, and honest transport gaps |
| [api/errors.md](api/errors.md) | The problem-slug registry: slug ↔ HTTP status ↔ gRPC code |
| [api/openapi.json](api/openapi.json) | **Generated** (`cargo run -p munarium-server -- openapi`), CI drift-checked — never hand-edit |
| [route-index.py](route-index.py) | **Generator** (2026-09-02) for the developers guide's Appendix F route index from `api/openapi.json`; the server crate's `docs_coverage` tests fail `cargo test` when that appendix, `api/rest.md` or `api/errors.md` fall behind the served routes and slugs |
| [api/grpc-reference.md](api/grpc-reference.md) | **Generated** (`gen-grpc-docs`), CI drift-checked — never hand-edit |

## Guides

| Document | What it is |
|---|---|
| [guides/getting-started.md](guides/getting-started.md) | From the published Docker image to a persistent corpus application: authenticated writes, shapes, runbooks, ingestion, index approval and evidence retrieval, with optional model completion |
| [guides/managing-key-and-secrets.md](guides/managing-key-and-secrets.md) | Supported AI providers, environment and Docker file secrets, provider verification, rotation and revocation, PostgreSQL passwords, API tokens and capability signing keys |
| [guides/datastore.md](guides/datastore.md) | Complete Datastore configuration, Docker storage setup, artifact builds and verification, promotion, serving rollout, fleet gates, testing, rollback and links to the existing references |
| [guides/measuring-performance.md](guides/measuring-performance.md) | Metrics and persisted performance records, repeatable benchmarks, large ingest/index workloads, retrieval and AI query scaling, bottleneck diagnosis and engineering opportunities |
| [guides/dev-guide.md](guides/dev-guide.md) | The developers guide — Parts I and II of the three-part book, with the Preface and Appendices A–F (verified against v0.1.2): I — developing munarium-server itself (setup, test tiers, crate map, recipes, CI); II — building AI-enabled corpus applications on the mesh (chat, research, red-flag review; the measured application patterns; §21A–§21C). |
| [guides/loading-corpora.md](guides/loading-corpora.md) | Getting documents in: filename-as-identity, which sample corpora are public datasets and which are not, the upload-prefix convention, extraction expectations |
| [guides/creating-a-lab.md](guides/creating-a-lab.md) | A tutorial without code examples for designing a corpus laboratory: independent answer keys, baseline and candidate shapes/runbooks, controlled experiments, failure diagnosis, acceptance and continued improvement |
| [guides/retrieval-sizing.md](guides/retrieval-sizing.md) | Sizing the search over those documents (2026-08-30): why the engine defaults are a compatibility floor and not a recommendation, the topK / candidateN / contextCharBudget / maxTokens arithmetic to do before writing a `retrieval:` block, which of history-revolution's knobs are corpus-specific and must not be copied, and why a capable model's refusal is a retrieval measurement rather than a prompt bug |
| [guides/source-stores.md](guides/source-stores.md) | Where document bytes live: Azure Blob / S3 / GCS / file / pg / mem, per-backend setup and credentials |
| [tokenbudgets.md](tokenbudgets.md) | **Token budgets (2026-09-02)**: the eight per-call `max_tokens` ceilings, their built-ins, the `MUNARIUM_MAX_TOKENS_*` environment, runbook precedence, and `GET`/`POST /v1/max-tokens` (whole-object replace, no partial update) with the client calls |
| [guides/document-intelligence.md](guides/document-intelligence.md) | The paid OCR escalation: why it is off by default, and how to turn it on deliberately |
| [guides/evidence-hierarchy.md](guides/evidence-hierarchy.md) | Research profiles, evidence layers and typed assertions (S-3.x, 2026-08-28): how a turn reads documents, a governed Matrix table and pinned ledger facts in declared trust order; what each kind of evidence may be used for; and why a turn naming no profile is byte-identical to the one that ran before |
| [guides/platform-features.md](guides/platform-features.md) | The platform walkthrough: uid, capability tokens, collections, runbook v2, sessions, reports |

## Operations

| Document | What it is |
|---|---|
| [ops/mmctl.md](ops/mmctl.md) | The `mmctl` CLI: runbook apply/list/info/validate, token issuance |
| [ops/index-deletion-runbook.md](ops/index-deletion-runbook.md) | DBA-only physical deletion of a collection's data — partitions **and** the object-store bytes |
| [ops/clustering.md](ops/clustering.md) | Running N instances on one PostgreSQL (2026-08-17): shared-vs-per-instance inventory, pool math, rolling restarts, orphaned-run diagnosis, partition-overflow recovery |
| [ops/deployment-runbook.md](ops/deployment-runbook.md) | Deploying with what ships: gate → build and push the image → install or upgrade the Helm chart (or the example AKS module) → verify the rollout, not the hostname → roll back → backups |
| [ops/troubleshooting.md](ops/troubleshooting.md) | Symptom → check → fix: startup exit codes, error slugs, deployed-environment classics, and the look-at-this-first order |
| [ops/backup-restore.md](ops/backup-restore.md) | What a database restore covers and does not, and the point-in-time restore procedure for a managed PostgreSQL or a CNPG cell — drill it in your own environment before you need it |

Deployment documentation lives with the deployment code:
[../deploy/helm/munarium/README.md](../deploy/helm/munarium/README.md) (the chart) and
[../deploy/terraform/example-aks/README.md](../deploy/terraform/example-aks/README.md) (an illustrative AKS module that consumes it).
