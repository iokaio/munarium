# Munarium Matrix: Technical Evaluation and Enterprise Integration Guide

> **Review basis.** This guide is based on a source-level review of
> the complete `matrix/src` Rust workspace, its contracts, migrations,
> conformance suites, and deployment assets. Executable source, current tests,
> recorded cycles, and the current
> [adapter support matrix](../../adapters/build-matrix.md) are used for claims
> about behavior.

## Full outline

1. Executive technical evaluation
2. System context and reference architecture
3. Shared contract and asset model
4. Adapter architecture and source onboarding
5. Choosing an integration mode
6. Mode A: materialization and change capture
7. Mode B: governed query and sealed evidence
8. Mode C: reconciliation and controlled correction
9. Canonicalization, identity, and semantic consistency
10. Runtime request pipeline
11. APIs, CLI, MCP, and client libraries
12. Security and governance architecture
13. Persistence, durability, and operations
14. Deployment and configuration
15. End-to-end enterprise integration playbook
16. Testing, conformance, and evidence of support
17. Performance, capacity, and cost engineering
18. Failure modes, refusals, and troubleshooting
19. Extending Matrix safely
20. How the implementation evolved
21. Appendices: configuration, assets, APIs, adapters, refusals, source map,
    and production checklists

## Audience, scope, and reading paths

This is an integration guide for teams connecting Munarium Server to structured
systems of record while retaining Server's direct indexing of document corpora.
It addresses both the technical implementation and the governance decision:
which structured data should be copied, queried in place, or compared with
Munarium's canonical memory.

- **Architects and business owners** should read Chapters 1, 2, 5, 15, 16,
  and 17. They describe fit, tradeoffs, maturity, adoption gates, and cost.
- **Integration developers** should read Chapters 3 through 11 and the asset
  examples in Appendix B.
- **Security, data-governance, and platform teams** should read Chapters 8,
  12 through 14, and Appendix G.
- **Contributors** should add Chapters 16, 19, and 20 to that path.

This guide uses four evidence labels deliberately:

| Label | Meaning |
|---|---|
| **Implemented** | A production code path exists and compiles. It is not, by itself, a support claim. |
| **Offline-tested** | Unit, contract, captured-payload, or in-memory tests exercise the path without a live service. |
| **Compose-tested** | Black-box scenarios exercise real local services over the network. |
| **Live-proven** | A recorded, reviewable cycle exercised the actual managed provider or a deployed Matrix. |

## 1. Executive technical evaluation

### 1.1 What Matrix is

Munarium Matrix is the structured-data plane beside Munarium Server's document
evidence plane. Server continues to index files, pages, emails, reports, and
other document corpora directly. Matrix adds bounded access to databases,
warehouses, semantic layers, and governed landing exports, and turns each
structured result into evidence that Server can cite and verify.

Matrix is not a general SQL proxy, an ETL platform, or an autonomous schema
explorer. Its core abstraction is an **immutable, versioned asset** that says in
advance what may be read, under which source identity and authorization class,
with what parameters, result schema, identity rule, limits, and evidence
requirements. Runtime input fills declared parameters or chooses from a closed
semantic vocabulary; it does not widen the contract.

The system offers three complementary integration modes:

- **Mode A — materialize:** project structured rows into deterministic records
  and upload them to Server for ordinary indexing and retrieval.
- **Mode B — query:** run a predeclared query or semantic selection at request
  time and return a sealed `EvidenceBlock` without copying the source first.
- **Mode C — reconcile:** observe structured facts, compare them with a pinned
  Server memory head, report discrepancies, and—only after explicit promotion
  and authority checks—propose controlled corrections.

### 1.2 Architectural strengths

1. **The execution surface is closed.** The compiler accepts read-only,
   allowlisted SQL; parameters are typed and bound separately; semantic views
   expose named measures, dimensions, and equality filters. `SELECT *`,
   subqueries, unapproved tables and columns, nondeterministic functions, and
   model-generated SQL do not enter the ordinary execute path.
2. **Policy is part of result identity.** Authorization class, denied columns,
   row identity, ordering, truncation, schema, and canonicalization version all
   participate in the sealed manifest or hash preimage. Two users who are not
   entitled to the same rows do not accidentally share one evidence identity.
3. **Exact data remains exact.** Decimal values remain text plus declared
   scale; timestamps, UUIDs, arrays, JSON, bytes, and nulls have explicit
   canonical forms. Server also preserves Matrix cell text rather than routing
   financial values through IEEE-754 numbers.
4. **Failure is explicit.** A closed refusal class—`not_covered`,
   `unavailable`, `denied`, `incomplete`, `invalid`, or `exhausted`—is paired
   with a more specific code. Unsupported or unsafe behavior is refused rather
   than approximated.
5. **Write authority is staged and reversible.** Mode C begins in shadow mode.
   Promotion requires current quality gates and a decision id. Rollback appends
   superseding corrections; it does not erase the historical action.
6. **The pure kernel is genuinely separated.** Canonicalization, compilation,
   derivation, planning, result validation, and refusal logic live in
   `munarium-matrix-core`, whose dependency boundary forbids network, runtime,
   web, object-store, and database crates.
7. **Support claims are unusually auditable.** The repository records live
   cycles, checks documentation cycle ids, distinguishes skipped scenarios,
   and documents defects found at real provider boundaries.

### 1.3 Material tradeoffs and limits

- **Contracts are deliberate work.** A developer or data owner must model
  parameters, source reads, result identity, authorization classes, limits,
  and verified questions. This is the cost that buys predictability.
- **Matrix PostgreSQL is mandatory durable infrastructure.** Every role uses
  the registry/journal/queue store; the query role is not designed as a
  stateless sidecar.
- **The current built-in authentication is static-token based.** It uses
  constant-time comparison and tenant/role binding, but enterprise identity
  federation, token rotation, rate limiting, and TLS termination belong at the
  deployment edge today.
- **Not every compiled adapter is live-proven.** Snowflake and dbt have never
  run live. BigQuery Mode B is live-proven, while BigQuery Mode A is not.
- **Replay strength differs by engine.** Most adapters seal the observed
  result; Databricks additionally advertises source time travel. A seal proves
  the evidence bytes and declared provenance, not that every source can replay
  its historical database state indefinitely.
- **PostgreSQL CDC has a real policy caveat.** `pgoutput` publication filters
  and column lists are enforced and inspected, but Matrix cannot prove a
  publication predicate is logically equivalent to an RLS predicate. That
  equivalence remains an operator assertion.
- **This is a 1.0 release, not a mature legacy service.** Production-wiring,
  watermark, route, TLS, payload, and least-privilege defects were found and
  closed during its development, the way a first release should. That history
  still argues for controlled rollout and evidence-based acceptance rather
  than a big-bang cutover.

### 1.4 Workload fit

| Workload | Fit | Reason |
|---|---|---|
| Governed register lookups and bounded aggregates | Strong | Closed parameters, exact result schema, sealed provenance. |
| Combining cited documents with current CRM/ERP/warehouse facts | Strong | Server evidence layers can combine direct document retrieval with `matrix:<view>` sources. |
| Periodic publication of structured entities into search | Strong | Checkpointed Mode A with deterministic paths and replay. |
| Controlled comparison between system-of-record values and memory | Strong, after shadow calibration | Mode C separates observation, findings, promotion, proposal, and rollback. |
| Ad hoc analyst SQL or unrestricted BI exploration | Poor | Free SQL is intentionally outside the product contract. |
| High-volume streaming with subsecond latency | Limited | PostgreSQL CDC is bounded pull through logical-slot SQL, not `START_REPLICATION`; other engines use batch modes. |
| Cross-table arbitrary semantic modeling | Limited | Native `DataView` compilation is intentionally one fact table with declared aggregates. Cube/dbt can provide richer external semantics. |
| Source mutation or transactional integration | Not supported | Source adapters are read-only; canonical writes go through Server's proposal contract. |

### 1.5 Production-readiness decision

Matrix is suitable for a production pilot when the selected adapter/mode has
matching evidence, the source principal passes introspection, verified
questions cover business invariants, evidence sealing is lockstep-compatible
with Server, and the rollout begins in read-only or shadow mode. It should not
be promoted to authoritative correction merely because Mode B queries work.
Mode C promotion requires its own measured identity precision, value
conformance, authority-scope review, decision record, and tested rollback.

## 2. System context and reference architecture

![Munarium Matrix reference architecture: users and applications reach Munarium Server, which reaches Munarium Matrix, which reads structured sources through the core adapters for PostgreSQL, MySQL, SQL Server and landing exports, with a Matrix PostgreSQL holding the registry, journal, queues and checkpoints. The analytics-platform adapters are Munarium Matrix Enterprise and are not in this repository](images/matrix-reference-architecture.svg)

*Figure 1. Matrix is a governed structured-data plane. Document corpora retain
their separate, direct path into Munarium Server.*

The separation in Figure 1 is an architectural boundary, not just a deployment
choice. Matrix does not link to Server crates. The
[server client crate](../../../src/munarium-matrix-server-client/src/lib.rs)
speaks Server's evidence, bulk-upload, memory-head, finding, and proposal HTTP
contracts. In the opposite direction, Server's
[`MatrixProvider`](../../../../server/src/munarium-server/src/evidence_providers.rs)
speaks Matrix's REST contract. The independent build prevents a private Rust
type from silently becoming the integration protocol.

### 2.1 The four planes

| Plane | Responsibility | Typical role |
|---|---|---|
| Control | Validate/apply immutable assets, probe/introspect sources, enqueue jobs, govern promotions, expose journal/reports/admin UI. | `control` |
| Query | Bind and compile a declared request, execute it under an effective source identity, canonicalize and seal evidence, return a block. | `query` |
| Materialization | Read snapshots, watermarks, CDF/CDC, or manifests; render deterministic records; upload and seal; checkpoint last. | `sync` |
| Reconciliation | Turn source rows into typed observations, compare to a pinned memory head, file findings, and conditionally make proposals. | `reconcile` |

One binary can run `all` roles for a laptop. Production manifests split them so
short query deadlines, long sync jobs, privileged control operations, and
reconciliation workloads can scale and fail independently. Route gating is
structural: a role does not merely deny an irrelevant route; it does not mount
it.

### 2.2 Server integration

A Server runbook declares named `dataViews`. A research-profile layer refers to
one as `matrix:<name>`. At apply time Server resolves the name to a Matrix
`QueryContract`, `MetricView`, or `DataView`, pins its version and parameter
schema, and validates the layer. At request time:

1. Server intersects the session's access level and compartments with the
   view's declared authorization.
2. A query contract produces a `structured_query` intent containing the pinned
   contract ref and typed parameters. No question text or SQL is sent.
3. A semantic view requires an intent task to resolve a selection from its
   admitted measures, dimensions, and equality filters. Server sends that
   closed selection, still not SQL.
4. Matrix executes and seals a count or complete table. Server preserves row
   ids, text cells, truncation, and the evidence id for context and citation
   verification.
5. A Matrix 4xx refusal is treated as a governed answer, not as a circuit-
   breaker failure. Network failures, timeouts, and 5xx responses can open the
   provider breaker.

Layers also declare required/optional behavior, controlling/supporting role,
byte and deadline ceilings, and whether a complete structured result must be
preserved. A required Matrix layer that cannot run refuses the research path;
it does not quietly become document search. See Server's
[evidence-hierarchy guide](../../../../server/docs/guides/evidence-hierarchy.md).

### 2.3 Trust boundaries

There are four independent credentials to reason about:

- the end-user/session authorization known to Server;
- the Server-to-Matrix bearer token and `X-Munarium-Uid`;
- the Matrix tenant/role token used on its own API;
- the source `credentialRef`, resolved by Matrix only at call time.

Do not collapse them into a shared superuser secret. The session determines
what the request may ask for, the Matrix token determines which tenant and API
operations are available, and the source credential determines what the engine
will actually expose. Matrix additionally validates the declared authorization
class and denied columns before sealing.

## 3. The shared contract and asset model

![The asset chain and the order it must exist in: a DataSource carrying connection, egress, credential reference and authorization; then a DataView or MetricView as the contract for what may be asked; then a Mapping declaring where results land and under whose authority; then a run producing sealed, journalled evidence. Beneath it the operational sequence: validate, apply, probe, introspect, publish](images/matrix-asset-lifecycle.svg)

*Figure 2. Assets are validated locally and server-side, applied immutably,
verified against the real source, and only then consumed by runtime jobs.*

### 3.1 Versioned envelope

The current shared contract version is `0.1.0`; canonical result encoding is
`canon@1`. Asset YAML is strict (`deny_unknown_fields`) because a misspelled
policy field must not be accepted and ignored. Wire DTOs are additive-tolerant
so a client can survive new response fields. The asset discriminator is parsed
from YAML, not guessed by searching text.

Every asset has `apiVersion`, `kind`, `metadata.name`, and
`metadata.version`. Registry identity is `(tenant, kind, name, version)`.
Applying identical bytes again is idempotent; applying different bytes to the
same identity is a conflict. Corrections therefore create a new version.

### 3.2 Asset types

| Asset | Purpose | Depends on |
|---|---|---|
| `DataSource` | Adapter kind, connection metadata, secret reference, egress, effective role, authorization classes, limits, snapshot/fingerprint/sync/planner policy. | External source and credential reference |
| `QueryContract` | Typed parameters, per-dialect statement or hashed statement file, read allowlist, result schema/order/derivations, policy, limits, evidence behavior, verified questions. | `DataSource` |
| `DataView` | Closed native aggregate model over one declared fact table: measures, dimensions, filters, synonyms, policy, limits, questions. | `DataSource` |
| `MetricView` | Closed semantic-provider model whose definition is fingerprinted and reverified. | Cube/dbt-style `DataSource` |
| `ClaimMapping` | Row/entity identity, aliases, properties, temporal/change semantics, shadow or authoritative mode, authority scopes, run ceilings. | `DataSource` and Server memory/proposal plane |

The operational dependency order is normally source → contract/view or
mapping → probe/introspect → verify/dry run → Server runbook binding or worker
schedule. Asset application alone proves syntax and declared invariants; it
does not prove connectivity, privileges, remote definition stability, or a
business result.

### 3.3 Fail-closed validation

The validator in
[`validate.rs`](../../../src/munarium-matrix-types/src/validate.rs) checks, among
other things:

- API and metadata shape, adapter-specific connection requirements, nonempty
  egress allowlists, known roles/classes, and heuristics that reject literal
  credentials;
- sync-mode support, watermark columns, inclusive/exclusive semantics,
  tie-break requirements, CDC identifiers, projection keys, and entity keys;
- statement presence for required dialects, file hash, named parameter
  declarations, typed allowed-value domains, read tables/columns, result
  schema, row identity, ordering, derivations, and verified questions;
- denial consistency: a denied column cannot reappear in reads, results,
  ordering, or derivation inputs;
- mapping templates restricted to declared entity keys, aliases that do not
  collide, known properties, temporal declarations, confidence, and authority
  scopes;
- semantic measures/dimensions/filters and provider capabilities.

Only three findings are advisory: `limits.above-inline-seal`,
`mapping.authority-inert`, and `authorization.classes-ignored`. All other
findings make the asset invalid.

### 3.4 Configuration versus proof

An immutable source asset is a claim about intended posture, not evidence that
the remote principal has that posture. `probe` tests reachability.
`introspect` obtains the schema and role facts the adapter can prove. `verify`
runs business questions through the same bind/compile/execute/canonicalize/seal
path as production. A healthy integration needs all three.

## 4. Adapter architecture and source onboarding

### 4.1 The adapter seam

[`SourceAdapter`](../../../src/munarium-matrix-adapter/src/lib.rs) is the
provider boundary. Each implementation declares its version and
[`Capabilities`](../../../src/munarium-matrix-adapter/src/capabilities.rs),
then implements the applicable operations: `probe`, `introspect`,
`read_batch`, contract `execute`, definition fingerprinting, semantic execute,
or planner ask. The orchestration layer calls `require_*` capability guards
before using an optional surface. Unsupported behavior becomes a typed refusal.

The seam carries an `EffectiveIdentity`—authorization class, credential
reference, and principal—rather than letting an adapter infer identity from
ambient state. `ReadMode` carries a watermark declaration with the mode;
`Watermark::resolve` is the sole resolution point, so an adapter cannot silently
fall back to `updated_at,id`.

Parameters pass through the binding layer as a deterministically ordered map.
It validates type, decimal scale, and allowed domain without coercion. Values
are sent through engine binding APIs and never interpolated into statement
text.

### 4.2 Adapter families

| Family | Adapters | Primary pattern |
|---|---|---|
| Relational | PostgreSQL, MySQL, SQL Server | SQL contracts; snapshot/watermark; PostgreSQL also `pgoutput` CDC. |
| Warehouses | BigQuery, Snowflake, Databricks | Bounded query jobs; snapshot/watermark or Databricks CDF; provider-specific cancellation and budgets. |
| Semantic | Cube, dbt | Provider APIs for declared measures/dimensions; no table materialization. |
| Landing | Filesystem, Azure Blob | Immutable manifest plus exact CSV/JSONL schema; materialization only. |

### 4.3 Current capability and evidence position

| Adapter | Mode A | Mode B | Strongest evidence at review date | Important limit |
|---|---|---|---|---|
| PostgreSQL | Snapshot, watermark, CDC | SQL | Compose and live; CDC 7/7 compose | Publication filter equivalence to RLS is operator-asserted. |
| Landing | Manifest, snapshot | Refused | Filesystem and Azure Blob, live | No query contracts. |
| Databricks, Snowflake, BigQuery, Cube, dbt | — | — | Not in this repository — Munarium Matrix Enterprise; a core build refuses them by name with `adapter_not_available` | Registered through `adapters::AdapterRegistry`. |
| SQL Server | Snapshot, watermark | SQL | Compose 7/7 | No CDC; certificate mode must be chosen deliberately. |
| MySQL | Snapshot, watermark | SQL | Compose 7/7 | No binlog CDC; cancellation capability is false. |
| BigQuery | Snapshot/watermark implemented | Query live | Live Mode B 7/7 | Mode A remains unrun; no trustworthy snapshot marker yet. |
| Snowflake | Snapshot/watermark implemented | SQL implemented | No live account | Both modes unrun; source-side row limiting capability is false. |
| Cube | Refused | Semantic | Compose 4/4 | Semantic only. |
| dbt | Refused | Semantic implemented | No live deployment | Unrun. |

The full evidence and cycle record is maintained in
[`build-matrix.md`](../../adapters/build-matrix.md). Treat that file, not the
existence of a crate, as the current support statement.

Of the nine, **four are in this repository**: PostgreSQL, MySQL, SQL Server
and Landing. Databricks, BigQuery, Snowflake, Cube and dbt are Munarium Matrix
Enterprise adapters, registered through the same `SourceAdapter` interface;
their rows describe the interface each meets and the evidence recorded when it
was built. A core build refuses an asset naming one of them at execution with
`adapter_not_available`.

### 4.4 Onboarding sequence

1. Select a mode based on data purpose, not merely adapter capability.
2. Create a read-only source principal and engine-native policy. For classed
   access, create or map one effective principal per authorization class.
3. Declare the `DataSource`, including egress hosts, `credentialRef`, role,
   authorization strategy, budgets, and sync/fingerprint policy.
4. Validate locally, apply, probe, and introspect. Resolve every posture or
   schema refusal before writing contracts.
5. Author the contract/view/mapping against the schema visible to the exact
   effective principal—not an administrator's schema.
6. Add verified questions containing both result examples and invariants.
7. Run at limits smaller than production, review manifests and journals, then
   increase ceilings only from measurements.

## 5. Choosing an integration mode

![Mode selection: Mode A materializes a corpus so it is searchable alongside documents; Mode B answers an exact bounded question at request time through a contract; Mode C reconciles a canonical property one system is authorized to correct over effective dates. These are distinct assets with distinct budgets and authority](images/matrix-mode-selection.svg)

*Figure 3. Choose by product semantics: indexed search, current governed
answers, or controlled canonical comparison. A source can use more than one.*

| Decision factor | Mode A | Mode B | Mode C |
|---|---|---|---|
| Primary outcome | Searchable/indexed records | Fresh sealed table/count | Findings and optional proposals |
| Source access time | Scheduled/background | User request critical path | Scheduled/background |
| Freshness | Last successful checkpoint | Source-at-execution | Last reconciliation pass |
| Server dependency | Bulk upload and seal | Evidence seal on every call | Memory head, findings, proposals, seal |
| Source load | Batch scans/incrementals | Per evidence request | Batch observation scans |
| Authority | Copied evidence, not automatic canon authority | Read-only evidence | Shadow by default; scoped authority after promotion |
| Failure recovery | Replay from checkpoint/idempotency key | Retry only safe refusal classes | Idempotent events/proposals; rollback by supersession |

Use **Mode A** when structured content should participate in ordinary document
retrieval, tolerate checkpoint freshness, and be searchable without hitting
the source on every user request. Use **Mode B** for current balances, counts,
registers, and aggregates whose exact result should be included as a complete
citable table. Use **Mode C** only where there is a business decision about
which system may correct which canonical property over which effective dates.

Common combinations are valid. A CRM can materialize account descriptions for
search, expose a bounded current-pipeline contract for request-time evidence,
and reconcile a small authoritative ownership field. These must be distinct
assets with distinct budgets and authority—not one broad credential and query.

## 6. Mode A: materialization and change capture

![Mode A materialization: read by snapshot, watermark or change feed; canonicalize to declared types with exact decimals; chunk each row into a citable unit; upload to a Server collection; and commit the checkpoint only after the upload lands. The acceptance check is that the same batch replayed from one checkpoint produces identical events, after which an unchanged source produces none](images/matrix-mode-a-pipeline.svg)

*Figure 4. Checkpoint advancement is the final durable action. A read or upload
that is not sealed must be replayable.*

### 6.1 Supported read forms

- **Manifest:** immutable CSV/JSONL objects named by a landing manifest, with
  optional SHA-256 integrity. The manifest snapshot id is the checkpoint.
- **Snapshot:** a bounded consistent read. Engines may attach a snapshot marker;
  absence is recorded rather than invented.
- **Watermark:** ordered incremental read using the source-declared column,
  inclusive flag, and optional tie-break. Exclusive reads without a tie-break
  are rejected because equal-watermark rows could be lost.
- **CDF:** Databricks Change Data Feed, including tombstones. A retention gap
  is an incomplete history and triggers an explicitly recorded resnapshot.
- **CDC:** PostgreSQL `pgoutput` logical decoding with inspected publication
  column list and row filter.

### 6.2 The durable sequence

The sync worker in
[`sync.rs`](../../../src/munarium-matrix-workers/src/sync.rs) performs these
steps per authorization class:

1. Load the immutable source version and previous checkpoint.
2. Introspect and compare the schema fingerprint under the configured drift
   policy (`refuse` or an explicit compatibility decision id).
3. Read a bounded batch through the declared mode and effective identity.
4. Validate schema and stable row identity; render row records or deletion
   tombstones to deterministic paths.
5. Bulk upload to the class-specific Server collection.
6. Seal coverage and counts, including excluded rows and truncation.
7. Persist uploaded-document/event idempotency state and **then** advance the
   checkpoint.

The class-specific collection name prevents one RLS/authorization view from
being overwritten by another. Source-native authorization uses a declared or
default source-native class; per-class authorization requires an explicit
class-to-principal mapping. Classification has no permissive fallback.

### 6.3 Identity, replay, and deletion

Rendered paths and idempotency keys include the source and entity identity,
render version, row/event position, and checkpoint context. Replaying the same
batch overwrites the same logical record instead of multiplying records.
Upload failure cannot advance the checkpoint. PostgreSQL CDC peeks changes and
advances its logical slot only when the next call presents a checkpoint proving
the preceding batch was durably handled.

A delete is a tombstone, not a row filled with nulls. Some engines provide only
identity columns on deletion; the record says that explicitly. PostgreSQL
`TRUNCATE` is refused because it cannot be represented as a bounded set of row
tombstones without silently leaving stale materialized records.

### 6.4 PostgreSQL CDC operating obligations

Matrix never creates a replication slot. Slots retain WAL and can fill the
source disk after Matrix is gone; creation is therefore an operator-owned
database change. Matrix derives or accepts declared slot/publication names,
checks that `pgoutput` is used, rejects policy-bypassing `test_decoding`, and
reports retained WAL bytes. A secured table's publication must expose exactly
the projected columns and have a row filter. If a filter uses a non-key column
while another column must be withheld, PostgreSQL may require a suitable unique
index and `REPLICA IDENTITY USING INDEX`.

### 6.5 Acceptance checks

Run the same incremental batch twice from one checkpoint and require identical
events. Then commit the checkpoint and require an unchanged source to produce
no new rows. Test two rows sharing a watermark, a boundary update, deletion,
schema drift, an unavailable Server during upload, and a stale CDC/CDF
position. Verify the Server collection under every authorization class, not
only the administrator's class.

## 7. Mode B: governed query and sealed evidence

![Mode B governed query: a contract fixes the only shape that may be asked; the compiler produces one plan hashed over the parsed AST; parameters bind rather than being interpolated; execution runs under the effective principal at declared limits; and the result is sealed into an evidence block and manifest](images/matrix-mode-b-pipeline.svg)

*Figure 5. A request can select only what an immutable asset already admitted;
successful source execution is still not returned until canonical evidence is
sealed.*

### 7.1 Request contract

A `QueryIntent` carries contract version, kind (`structured_query` or
`semantic`), request identity, a pinned contract/view or semantic selection,
typed parameters, authorization snapshot, limits, deadline, freshness, and seal
requirements. Effective limits are the minimum of source, asset, and request
ceilings. A caller cannot raise a limit declared by the data owner.

For SQL contracts the compiler:

- parses the declared statement and enforces read-only `SELECT` shape;
- checks tables and columns against `reads` and denied-column policy;
- rejects stars, subqueries, unsupported/nondeterministic constructs, and
  undeclared parameters;
- rewrites named placeholders to the target dialect while leaving values out
  of SQL text;
- records compiler and statement identity in provenance.

`DataView` uses the same evidence pipeline but compiles a closed measure,
dimension, and equality-filter selection into a one-table aggregate.
`MetricView` asks Cube/dbt through its provider API. The definition fingerprint
must have a passing verification record and is reread before each execute.

### 7.2 Result discipline

The adapter result is reconciled with the declared schema. Column names, order,
types, nullability, decimal precision/scale, row keys, and ordering must agree.
No implicit decimal rounding is performed. The engine may return an empty
result, but the schema still comes from the contract so an empty table can be
sealed without fabricating columns.

The pure kernel computes:

- a **logical result hash** over canonical schema, identity/policy context, and
  rows; keyed rows are a sorted multiset while position-identified rows retain
  order; and
- an **artifact hash** over the exact rendered evidence artifact.

Keeping the hashes separate prevents an incidental rendering change from
claiming the logical data changed, or vice versa.

### 7.3 Sealing and evidence blocks

The worker renders canonical CSV with LF line endings, quoted non-null cells,
and an unquoted empty field for null, builds an `EvidenceManifest`, and asks
Server to seal it. The manifest records source/adapter/asset/compiler versions,
authorization class, query identity, parameters by safe hash or approved
representation, schema, row-id rule, result hashes, completeness/truncation,
timings, and replay information. If identity is insufficient or sealing fails,
the request refuses; it does not return unciteable source data.

The response is a closed `EvidenceBlock`: currently a count, a complete table,
or a refusal. Server uses Matrix row ids for citation anchors and believes the
sealed manifest's truncation status. Decimal cell strings stay strings end to
end.

### 7.4 Verified questions

`verifiedQuestions` are the contract's executable regression suite. They run
through production binding, compilation, adapter execution, result validation,
derivation, and sealing. A question may check exact rows and business
invariants. The verify API returns HTTP 200 even when a question fails because
the operation succeeded and the asset failed its test; `mxctl verify` converts
that state to exit code 3 for CI.

### 7.5 Planner assistance is not execution

Databricks Genie can be configured as a conversational planner. It proposes a
plan and pin. Evaluation records it without admission; assist can admit SQL
only through the contract/compiler policy. Planner output never bypasses a
declared contract and the planner endpoint itself executes nothing. Treat it as
an authoring aid, not an ad hoc query back door.

## 8. Mode C: reconciliation, authority, and controlled correction

![Mode C reconciliation: observe the source's view of a property, compare it by identity match and value conformance, propose a correction with its authority scope, and promote only inside declared effective dates. Promotion requires measured identity precision, value conformance, an authority-scope review, a decision record and a tested rollback](images/matrix-mode-c-lifecycle.svg)

*Figure 6. Observation and discrepancy reporting are separated from canonical
authority. Promotion is a governed state transition; rollback appends history.*

### 8.1 Observation before opinion

[`observe.rs`](../../../src/munarium-matrix-workers/src/observe.rs) turns a
mapping's bounded source rows into typed `Observation`s. Each non-null mapped
property becomes one observation with source lineage, canonical row key,
subject/scope templates, optional aliases, value, event/valid time, and
confidence. Null is skipped and counted; it is not treated as a fact that a
property equals null. Transaction time comes from the source event/position or
declared valid time, not the worker's local clock.

Aliases are normalized only by documented case/whitespace rules. Conflicting
aliases within a scope produce ambiguity rather than an arbitrary winner.
Observation batches are canonicalized and sealed through the same evidence
machinery as Mode B.

### 8.2 Comparison model

The reconciler pins a Server memory head sequence and its visible facts, then
compares typed values. Possible verdicts include `agree`, `differ`,
`missing_in_ledger`, `missing_in_source`, `backdated_requires_review`, and
`ambiguous`. Decimal comparison is exact. Alias fallback is attempted only
when exact identity finds no fact; a tied or below-threshold candidate remains
ambiguous.

`missing_in_source` is asserted only for a complete read within a standing
declared namespace/alias scope. A partial or truncated read cannot prove
absence. Backdated changes require review rather than silently rewriting a
historical interval.

### 8.3 Shadow, promotion, and authority

Every mapping starts safely in or can be held in **shadow**: discrepancies file
warning findings and canonical memory is untouched. An authoritative mapping
still cannot write unless all of these are true:

1. the immutable mapping version is the promoted version;
2. its latest completed run clears current identity-precision and
   value-conformance thresholds;
3. an operator supplies a decision id (and optional reason) at promotion;
4. the property and effective date fall inside an explicit authority scope;
5. normal source-vs-document authority rules permit the change.

The default is `document_over_source`. Authoritative mode is therefore not a
blanket declaration that a database always wins. Proposals carry mapping ref,
version and lineage, row/property identity, canonical value, event position,
and idempotency key.

Before writing findings or proposals the reconciler performs a dry count and
enforces per-run ceilings. The proposal ledger in Matrix PostgreSQL is separate
from Server canonical memory. Demotion stops future proposals. Rollback asks
Server to append claims restoring prior values with `origin.kind=rollback`;
the promotion and erroneous proposals remain auditable.

### 8.4 Enterprise control design

Use distinct `mgmt` and `rw` credentials for observation and promotion paths.
Require a decision record from a change-management or data-governance system,
review authority scopes property by property, and monitor gate history rather
than one passing run. A production promotion exercise should plant a wrong
value, verify the proposed correction and provenance, apply it in an isolated
tenant, roll it back, and prove both actions remain visible.

## 9. Canonicalization, identity, and semantic consistency

### 9.1 `canon@1`

The canonical value vocabulary is closed: boolean, signed 64-bit integer,
decimal, 64-bit float, string, bytes, date, timestamp with timezone, naive
timestamp, interval, UUID, JSON, array, and null. Null has its own sentinel.
Decimals use exact text and declared scale. Zoned timestamps normalize their
instant; naive timestamps remain explicitly naive. JSON and arrays are encoded
under type-aware rules rather than generic display formatting.

[`CanonicalResult`](../../../src/munarium-matrix-core/src/result.rs) validates
the declared schema before hashing. Stable row identity comes from declared key
columns or explicit position. Keyed results are order-independent; positional
results are not. This distinction matters for aggregates and registers where
engines may return the same rows in different physical orders.

### 9.2 Schema fingerprints and drift

Adapter introspection produces a stable fingerprint over normalized table and
column names, source types, and nullability. Before Mode A work, the current
fingerprint is compared with the pinned one. `refuse` halts on drift. A
`compat:<decision-id>` policy allows an operator to acknowledge a compatible
change while preserving who made that judgment. Do not use compatibility as a
generic bypass; update and reverify dependent assets when the result meaning
changes.

### 9.3 Entity identity

Claim mappings define source keys, entity subject and scope templates, property
columns, optional aliases, temporal meaning, and confidence. Templates are
limited to declared key material so a friendly non-key label cannot become an
unstable canonical identity. The observation row key is computed by the same
canonical rules used for result rows.

### 9.4 Semantic consistency across providers

`DataView` compilation gives Matrix control over the SQL and supports a
deliberately small native semantic vocabulary. `MetricView` delegates semantics
to Cube or dbt, so Matrix pins the remote definition fingerprint and requires
verification. Equal names do not imply equal business definitions across
engines; verified questions must cover aggregation grain, null treatment,
currency/timezone, filter semantics, and access policy. A definition change
invalidates execution until reverified.

## 10. Runtime request pipeline

![The runtime request pipeline and its refusal points: tenant and role, asset resolution, authorization class, the egress allowlist, credential resolution, limits, and sealing — each able to refuse for a named reason. Every refusal carries a class saying whether a retry can help and a code saying what to change](images/matrix-runtime-enforcement.svg)

*Figure 7. Enforcement is layered: asset validation, caller authorization,
adapter capability, source policy, result conformance, budget, and evidence
sealing all have independent refusal points.*

### 10.1 Startup and construction

The server parses all `MUNARIUM_MATRIX_*` configuration before binding ports,
connects/migrates Matrix PostgreSQL, installs the explicit rustls crypto
provider, checks target Server compatibility when configured, constructs the
adapter registry from applied sources, and starts only the listeners and worker
loops for its role. Bad environment is a startup error rather than a latent
first-request failure.

Adapter construction resolves metadata but not secret values. A
`credentialRef` is resolved at call time from `env:NAME`, `file:PATH`, or
`MUNARIUM_MATRIX_SECRET_<NAME>`. Landing `file` sources are confined beneath
`MUNARIUM_MATRIX_FILE_ROOT`; Azure landing uses workload identity. Default-
deny egress requires the source host to appear in its asset allowlist.

### 10.2 Query path

1. Authenticate bearer token in constant time and bind tenant/role.
2. Load the exact immutable asset version and validate intent contract version.
3. Resolve the authorization class/effective principal and effective ceilings.
4. Reserve request budget atomically in PostgreSQL.
5. Bind typed parameters or validate a semantic selection.
6. Compile/authorize; execute with deadline/cancellation behavior advertised by
   the adapter.
7. Validate the returned schema and values, derive declared aggregates, and
   canonicalize.
8. Build and seal the manifest with Server.
9. Journal redacted operation, timing and outcome; return block or refusal.

The REST response includes `Server-Timing` components for total, source, seal,
and residual Matrix time. The same source/seal durations land in journal rows
for later attribution.

### 10.3 Long-running worker path

Control endpoints enqueue sync/reconcile jobs rather than holding a client
connection. PostgreSQL queues use `FOR UPDATE SKIP LOCKED` and leases. Workers
claim, heartbeat, run, journal, and complete/fail jobs. Checkpoints,
observed-event keys, uploaded-document keys, and proposal ids make retries
idempotent. Readiness becomes false while draining so a load balancer stops new
traffic before process termination.

### 10.4 Cancellation and budgets

Capabilities truthfully distinguish adapters that can cancel remote work from
those that can only drop the client future. BigQuery uses its jobs API;
Databricks uses statement cancellation; SQL Server cancels by dropping the
exclusive TDS connection; MySQL declares cancellation false. Budget
reservation is stored transactionally to avoid concurrent check-then-spend
races. A budget or limit refusal is an expected terminal result, not an
internal error.

## 11. APIs, CLI, MCP, and client libraries

### 11.1 REST surface

REST on port 8180 is the broad operational contract. The committed
[OpenAPI document](../../api/openapi.json) is generated from the binary and
checked against the router. Meta routes (`/healthz`, `/readyz`, `/version`,
`/openapi.json`, `/docs`) are available on every role; authenticated routes
depend on role.

| Area | Principal routes | Required role |
|---|---|---|
| Assets | `POST /v1/assets`, `POST /v1/assets/validate`, list/get sources, contracts, metric views, data views, mappings | `control`; `rw` to write |
| Source operations | `probe`, `introspect`, `sync`, `planner/ask` | `control`; write operations require `rw` |
| Query | `execute` and `verify` for contracts, metric views, and data views | `query`; `ro` may execute |
| Reconciliation | run, promotion status, gate history, promote, demote, rollback | `control`; writes require `rw`/governance path |
| Audit | `/v1/journal`, `/healthdata` | `mgmt` for journal |
| Human operations | `/admin/...` | `control`, admin enabled, management login |

Every API failure uses RFC 9457 `problem+json` with a `matrix:` slug. A source
refusal remains typed in the body. Writes are journaled with payload values
redacted; ordinary reads are not journaled. The complete route behavior is in
the [REST guide](../../api/rest.md).

### 11.2 gRPC

The gRPC data plane listens on 50151 when enabled and exposes only
`matrix.v1.MatrixQuery/Execute`. It streams progress states—authenticated,
loading, wiring, budget, executing, sealed—followed by one terminal evidence
block or refusal. A refusal is a protocol message, not a gRPC transport error.
The vendored JSON schemas remain normative; proto drift tests ensure the mirror
does not acquire an incompatible meaning.

Choose gRPC when a service wants progress streaming and a narrowly scoped
execute plane. REST remains necessary for registry, verification, job,
promotion, and reporting operations. At the review date Munarium Server's
`MatrixProvider` uses REST, so enabling gRPC does not remove the REST query
service from a Server deployment.

### 11.3 MCP

`POST /mcp` implements JSON-RPC MCP `initialize`, `ping`, `tools/list`, and
`tools/call`. Tools are generated from applied assets, not from source schema
exploration, and dispatch into the same execute handlers and policy. The MCP
description is therefore deployment-specific and `tools/list` is authoritative.
MCP does not add free SQL; a tool call still names a predeclared contract or
semantic surface. A refusal rides as a tool error without losing its governed
meaning. See [the MCP guide](../../api/mcp.md).

### 11.4 Operator CLIs

The standalone Matrix image contains **`mxctl`**. Important commands are:

```text
mxctl validate -f source.yaml
mxctl apply -f source.yaml
mxctl list datasources --all
mxctl info contracts open-pipeline
mxctl verify open-pipeline
mxctl verify-view pipeline-metrics
mxctl sync crm
mxctl reconcile captable
mxctl mappings status captable
mxctl mappings promote captable --decision CHG-1427 --reason "two-week shadow gate passed"
mxctl mappings demote captable --decision INC-921
mxctl mappings rollback captable --decision INC-921
```

`MUNARIUM_MATRIX_URL` selects Matrix (default `http://localhost:8180`) and
`MUNARIUM_MATRIX_TOKEN` carries its bearer. Local `validate` requires no
service. Exit code 3 means validation findings or failed verified questions;
it distinguishes a broken asset from a broken command.

Munarium Server's existing operator binary also exposes an **`mmctl matrix`**
surface. Use it when operations are centered on Server; use `mxctl` for the
standalone Matrix control plane and local asset authoring. They are separate
binaries and should not be named interchangeably in automation.

### 11.5 Client libraries

The Rust [`MatrixClient`](../../../src/munarium-matrix-client/src/lib.rs)
covers the REST control/query operations. Python, .NET, and Java client trees
mirror the supported operational contract and have offline/live CI tiers. The
separate `munarium-matrix-server-client` is internal to Matrix's connection to
Munarium Server; it is not a replacement for an application Matrix client.

Client compatibility rules:

- send the exact contract version and tolerate additive response fields;
- preserve strings for decimals and counts until the application applies its
  own declared numeric type;
- treat refusal class/code as data and retry only when the class is retryable;
- retain request/evidence ids in telemetry;
- never infer success from HTTP 200 on `verify`; inspect `failed`;
- do not synthesize row ids—use Matrix's ids from the evidence block.

## 12. Security and governance architecture

![Three credentials answering three different questions: the session authorization carried from Munarium Server decides what the request may ask for; the Matrix tenant and role token decides which tenant and which operations; and the source credential reference, resolved only at call time, decides what the engine will actually expose](images/matrix-security-boundaries.svg)

*Figure 8. Authorization is intersected across the caller, immutable asset,
Matrix tenant/role, and source principal. No one layer is treated as sufficient.*

### 12.1 Credentials and secret handling

Assets store references, not values. The validator looks for literal URI
userinfo and common secret shapes. At runtime Matrix resolves `env:NAME`,
`file:PATH`, or normalized `MUNARIUM_MATRIX_SECRET_<NAME>`. Config `Debug`
redacts static tokens and runtime journaling redacts parameters. Deploy source
secrets through a secret manager or workload identity and grant Matrix's role
read access only to the references its source versions require.

Built-in static tokens use `token:tenant:role` entries with `ro`, `rw`, or
`mgmt`. `AUTH_MODE=disabled` grants broad development behavior on
`tenant-default` and is unsuitable outside isolated conformance/local use.
Terminate TLS and add enterprise identity/rate controls at an ingress or
service mesh; the service itself currently does not provide login throttling or
OIDC federation.

### 12.2 Defense in depth

| Enforcement layer | What it prevents |
|---|---|
| Server runbook binding | An undeclared `matrix:` view or unresolved semantic selection entering a turn. |
| Session/view intersection | A view lending clearance to a session or a session widening a view. |
| Matrix tenant and API role | Cross-tenant registry access and read/write/management privilege confusion. |
| Asset validator | Secret literals, undeclared reads, denied-column reintroduction, unsafe sync identity, invalid authority. |
| Compiler/binder | SQL widening, interpolation, type coercion, unrestricted constructs. |
| Adapter capability | Calling a provider surface it does not truthfully support. |
| Source-native policy | Actual RLS, column grants, warehouse role, semantic-provider policy. |
| Result validator/sealer | Returning drifted, malformed, unidentifiable, over-limit, or unciteable data. |

For `source_native` authorization, the engine's policy is decisive and Matrix
records the class. For `per_class`, each class maps to its own effective
principal. Never use one high-privilege credential and depend only on a class
label in the request.

### 12.3 Egress and transport

`egressDefaultDeny` is true by default, and every source declares allowed
hosts. This is a destination control, not a content firewall. Network policy
should independently restrict each role to Matrix PostgreSQL, Server, and its
approved sources. The shipping dependency graph is rustls-only. Provider TLS
settings still matter: do not enable SQL Server `TrustServerCertificate` or a
warehouse's insecure certificate mode except in isolated development.

### 12.4 Evidence and refusal privacy

Server deliberately does not forward Matrix's operator-oriented refusal detail
to the end user because it can name a source, class, or column the user cannot
see. It preserves the specific safe code and replaces the message. Matrix's
`Refusal::hidden` is unable to carry source/detail. Required layers fail closed
without leaking which hidden structured object caused the refusal.

### 12.5 Admin console boundaries

The server-rendered `/admin` UI uses no JavaScript. It is mounted only on the
control role when enabled, uses a management login cookie, validates Host and
Origin on writes, and derives CSRF state from the supplied credential and a
per-process boot secret. An `rw` credential is requested for each mutating
operation. Hardened deployments should disable the route and operate by API if
the console is unnecessary.

Known non-defenses should be designed around: there is no login rate limiter;
the cookie has no application expiry beyond browser/session behavior; TLS and
forwarded-protocol correctness are ingress responsibilities; and a compromised
management principal can inspect registry/journal metadata within its tenant.

### 12.6 Enterprise threat-model checklist

- Can any role reach a source or Server endpoint not in its job description?
- Does every source principal lack DDL/DML, ownership, superuser, and policy-
  bypass capabilities? Does live introspection prove that claim?
- Are authorization classes backed by distinct source enforcement where
  required?
- Can a denied column enter a statement, result, derivative, manifest, log, or
  exception?
- Are provider certificates validated and secret references rotated without
  rewriting assets?
- Can an interrupted read be replayed without data loss or double proposal?
- Is Server/Matrix lockstep verified before evidence ids are minted?
- Are promotion, demotion, and rollback tied to external decision ids and
  separate operator identities?

## 13. Persistence, durability, and operational behavior

### 13.1 Matrix PostgreSQL

All application objects are fully qualified in schema `matrix`, owned by
`matrix_owner`; connections set `search_path=matrix,public`. Migrations are
additive-only and the boundary script rejects table/column drops, renames, and
column retypes. The store uses UUIDv7 identifiers for time-sortable records.

The migration sequence separates concerns:

1. immutable asset registry and checkpoints;
2. redacted journal and atomic budgets;
3. queues, leases, runs, and role operations;
4. promotions, proposals, and rollback metadata;
5. metric-view verification history;
6. native data-view asset kind;
7. source and seal timing columns.

### 13.2 Durable invariants

- Registry bytes are immutable and retrievable verbatim.
- Checkpoints advance after upload/seal, never merely after reading.
- Jobs are leased; a dead worker's job becomes reclaimable.
- Observed events, uploaded documents, and proposals have idempotency records.
- Parameter domains and schema fingerprints are pinned with their asset/run.
- Promotion state and gate history are separate from individual reconcile runs.
- Rollback does not delete proposals or journal entries.
- Budget reservations are atomic across concurrent requests.

### 13.3 Health and reporting

Port 9190 serves `/healthz`, `/readyz`, and `/metrics` on a separate listener.
The REST `/healthdata` endpoint reports registration, not live connectivity;
probing every source on health would create outbound load and could expose
provider failures as a platform restart loop. Use explicit probe operations and
source reports for connectivity.

Reports cover freshness, usage, queue depth/age, budgets, refusals, and recent
activity. The journal includes request/asset identity, outcome, safe refusal,
duration, source time, and seal time, but not raw parameters. Monitor at least:
oldest queued job, lease expirations, checkpoint age, retained PostgreSQL CDC
WAL, refusal counts by safe code, verify failures, promotion gate trend, and
Server seal latency.

### 13.4 Backup, restore, and retention

Back up Matrix PostgreSQL with the same recovery-point discipline as the
Server evidence and memory stores. A database restore can rewind checkpoints
and job/proposal idempotency while a source or Server remains ahead. A recovery
runbook must therefore reconcile three positions: Matrix store, source
CDC/CDF/watermark state, and Server evidence/canonical head. Prefer a controlled
resnapshot when continuity cannot be proven; mark it as such.

Retain immutable assets, promotion decisions, proposal lineage, and audit
journal for the governance retention period. Evidence artifacts themselves are
owned by Server's evidence store; coordinate retention so a journaled evidence
id is not expected to resolve after its configured expiry.

## 14. Deployment and configuration

Production separates roles while sharing one durable registry. Query scales
for latency; sync and reconcile scale for queue throughput. Figure 1 shows the
external topology and Figure 7 shows the role-independent enforcement path.

### 14.1 Local development

From `matrix/`, Docker Compose provides Matrix PostgreSQL, an all-role Matrix
service, and optionally Munarium Server. Profiles add SQL Server, MySQL, Cube,
and other test dependencies. Sealing tests need Server because a Matrix process
that must seal cannot honestly succeed without its peer.

The typical authoring loop is:

```powershell
Set-Location matrix
docker compose up -d postgres
./test.ps1
$env:MUNARIUM_MATRIX_URL = 'http://localhost:8180'
$env:MUNARIUM_MATRIX_TOKEN = '<development rw token>'
mxctl validate -f fixtures/assets/valid/datasource.crm.yaml
```

Use `docker compose down -v` only when intentionally rebuilding fixture state;
PostgreSQL initialization scripts do not rerun on an old volume.

### 14.2 Container image

The multistage build targets musl, uses fat LTO as part of the size constraint,
and ships a distroless, non-root runtime. CI checks the shipping dependency
graph and a 30 MB image ceiling. The runtime must receive writable network
access but does not require a writable application filesystem except configured
secret files/landing roots. Kubernetes manifests set non-root uid 65532,
read-only root filesystem, seccomp, no privilege escalation, and dropped
capabilities.

### 14.3 Kubernetes/Helm

The Helm chart creates one deployment per role: by default control 1, query 2,
sync 1, and reconcile 1 replica. The query service publishes REST and gRPC;
ops ports remain available to probes/metrics. All roles share the database URL,
Server URL/token reference, lockstep target, auth tokens, and source secret
references.

Scale **query** from p95/p99 execute latency and concurrent deadlines. Scale
**sync/reconcile** from queue age and source rate limits; adding workers can
increase source load and cost even when PostgreSQL leasing is correct. Keep
control small and highly available enough for asset/queue operations. Do not
run multiple role deployments with `ROLE=all` in production.

### 14.4 Managed-container pattern (Azure)

Matrix has run on Azure Container Apps with a user-assigned managed identity,
Key Vault secret references, `AcrPull`, and source-specific RBAC (`Storage
Blob Data Reader` for a `store: az` landing source), with the ordinary REST
service and a query/gRPC sibling as separate apps because ingress transport is
configured per app. That is one operational example, not evidence that every
adapter shares Azure semantics. Provider accounts must still be created with
least privilege and their own live cycle.

### 14.5 Upgrade procedure

1. Run offline gates, adapter conformance, OpenAPI/proto/client drift checks,
   and applicable live cycles against the candidate image.
2. Apply additive migrations before or as the first compatible control
   instance starts; retain the previous image.
3. Verify `/version` reports exact Server compatibility. Do not mint new
   evidence ids under a non-exact lockstep verdict.
4. Roll query instances gradually, then workers; watch refusals, seal latency,
   queue leases, and checkpoints.
5. Reverify metric/data views and business questions if compiler, adapter,
   source definition, or canonicalization changes.
6. Roll back the image if needed. Do not roll back the database schema by
   deleting additive columns/tables; old binaries must tolerate them.

## 15. End-to-end enterprise integration playbook

### Phase 0 — classify the business decision

Name the source owner, data steward, consuming workflow, maximum tolerated
staleness, required authorization classes, business invariants, evidence
retention, and whether the source is merely evidence or may correct canon. Pick
Mode A/B/C at this stage. If nobody can name the authority rule, Mode C remains
shadow.

### Phase 1 — prepare least privilege

Create a dedicated source identity per required class or an engine-native
source identity with proven row/column policy. Remove ownership, DDL/DML,
superuser and bypass-policy privileges. Configure TLS, network route, firewall,
warehouse quotas, and secret-manager access. For PostgreSQL CDC, create the
publication/slot explicitly and monitor retained WAL.

### Phase 2 — author the source asset

Declare connection metadata, credential reference, allowed egress, expected
role, authorization strategy, limits, snapshot/fingerprint and sync policy.
Validate locally and in CI. Apply the immutable version, then probe and
introspect using the actual serving revision and effective principal.

### Phase 3 — author the workload asset

For Mode B, start from verified questions and write the smallest query or
semantic vocabulary that answers them. Explicitly declare reads, denied
columns, row identity, result schema, ordering, limits and evidence behavior.
For Mode A, minimize projection and establish checkpoint/deletion semantics.
For Mode C, define stable keys, properties, temporal meaning, alias scope,
confidence and authority windows.

### Phase 4 — prove the real boundary

Run negative tests first: denied row/column, unapproved parameter, excess
budget, schema drift, source timeout, empty result, null versus empty string,
large decimal, and unavailable Server seal. Then run verified questions and
record source/result/seal timings. A mock that accepts any seal payload or
constructs an imagined provider response is insufficient.

### Phase 5 — bind Munarium Server

Configure Server with `MUNARIUM_MATRIX_BASE_URL` and
`MUNARIUM_MATRIX_TOKEN`. Declare a runbook `dataViews` entry that pins Matrix
kind/name/version, typed parameters and access restrictions. Reference it in a
research layer as `matrix:<view>`, choose required/optional and
controlling/supporting behavior, set deadline/byte bounds, and apply the
runbook. Verify the binding endpoint and run a turn containing both a document
source and Matrix view. Confirm citations use Matrix evidence/row ids and
document citations remain on their direct index path.

### Phase 6 — shadow and observe

For Mode A, compare materialized collection counts and sampled records to the
source for every class. For Mode B, monitor verification and refusal rates
before making the layer required. For Mode C, run shadow for a meaningful
business cycle, adjudicate false positives/negatives, and watch gate history.

### Phase 7 — production acceptance

Approve only when rollback/replay is demonstrated, support evidence matches the
exact adapter mode, source owners accept load/cost, security signs the
principal and trust boundaries, operations own alerts/runbooks, and business
owners accept verified questions and authority scopes. Record immutable asset
refs and deployment image digest in the release decision.

## 16. Testing, conformance, and evidence of support

### 16.1 Test tiers

| Tier | Command/path | What it can prove |
|---|---|---|
| Offline | `matrix/test.ps1` | Workspace units, pure kernel, strict assets, captured provider bytes, boundaries, contracts, doc cycle ids, OpenAPI generation. |
| Gates | `matrix/test.ps1 -Gates` | Formatting and clippy in addition to offline behavior. |
| PostgreSQL | `matrix/test.ps1 -Postgres` | Real Matrix store, PostgreSQL adapter, policy and CDC scenarios. |
| Black box | `matrix/test.ps1 -BlackBox` | HTTP, gRPC, MCP, admin, compose engines and Server sealing. |
| Browser | `matrix/test.ps1 -BlackBox -Browser` | Real operator UI login/write flow and screenshots. |
| Live | the env-gated tiers (`MUNARIUM_MATRIX_LIVE_*`, `MUNARIUM_MATRIX_TEST_*`) against a deployed Matrix and real providers | Managed identity, ingress, real provider APIs, deployed roles and actual payloads. |

Skipped provider tiers print **SKIPPED** rather than green. Live tests are kept
out of the ordinary runner because they cost money and need infrastructure an
operator creates and destroys deliberately.

### 16.2 Governance scenarios G1–G7

The conformance program treats governance properties as scenarios, not prose:
contract containment, stable evidence/replay, identity and authorization
behavior, least privilege, denial/non-leakage, and source/Server boundary
behavior are provoked. Review scenario names and bodies when accepting a new
adapter; a count of green tests is meaningless if the mode's decisive behavior
has no scenario. The five-day watermark defect existed precisely because
several adapters had no incremental scenario.

### 16.3 The evidence ladder

A trustworthy adapter claim normally advances through:

1. capability declaration and refusal tests;
2. unit tests for type mapping/binding/error conversion;
3. captured response bytes from the real provider;
4. compose or emulator black-box scenarios where faithful;
5. live least-privilege principal over real ingress;
6. repeat cycle proving idempotency and cleanup;
7. recorded result JSON and updated support matrix.

Captured payloads are valuable because BigQuery's real schema omitted scale and
returned scientific-notation epochs where constructed fixtures did not.
Captured bytes still do not prove IAM, network, billing, cancellation, or live
source policy.

### 16.4 Repository gates

[`boundaries.py`](../../../scripts/boundaries.py) checks the musl shipping graph:
no Server crate dependencies, a pure core, rustls-only TLS graph, and additive
migrations. Contract example validation checks schema drift. Documentation lint
requires every cited eight-character live cycle id to have a committed result
or explicit unrecorded disposition. OpenAPI is generated inside the server
binary and compared byte-for-byte with the committed copy.

## 17. Performance, capacity, and cost engineering

### 17.1 Critical paths

| Mode | Latency/cost equation | Scaling lever |
|---|---|---|
| A | introspection + source batch + render + bulk upload + seal | batch size, cadence, sync workers, projection |
| B | Server routing + Matrix bind/compile + source execute + canonicalize + seal + Server generation | query replicas, source query design, result bounds, seal locality |
| C | source batch + observation + memory-head read + comparison + findings/proposals | cadence, reconcile workers, mapping scope |

Mode B is user-facing, so p95/p99 source and seal latency dominate. `Server-
Timing` and journal `source_ms`/`seal_ms` separate those from Matrix's own work.
Do not optimize Matrix parsing when a warehouse queue or cross-region seal is
the dominant component.

### 17.2 Bounds before scale

Every source and asset should have `maxRows`, `maxBytes`, timeout and budget
ceilings; request limits can only lower them. Prefer a complete bounded table to
an enormous truncated one whose absence semantics are unsafe. For large
registers use Mode A and search, or partition into explicit contracts. Reconcile
per-run ceilings prevent a drifted mapping from filing or proposing an
unbounded number of changes.

Source-side enforcement differs. BigQuery can set maximum bytes billed and
cancel jobs. Databricks uses statement limits/cancellation. Snowflake currently
declares no source-side row limit capability and truncates after retrieval,
which protects the evidence response but not necessarily warehouse scan cost.
MySQL cannot promise remote cancellation. Capacity plans must use the adapter's
truthful capability, not one generic timeout assumption.

### 17.3 Measurement plan

For each verified question and sync/reconcile batch, record source rows scanned,
rows returned/excluded, bytes, provider cost units, source time, seal time,
total time, concurrency, authorization class, asset version, and cold/warm
state. Run at representative data distribution and policy selectivity. Repeat
after index/partition changes. Monitor source workload separately so improved
Matrix latency is not purchased by unacceptable system-of-record contention.

The live runs to date prove functional ingress and specific scenario counts;
they are not a general capacity benchmark. The reviewed code has concurrency and
budget controls, but every enterprise must establish its own SLO and cost curve.

## 18. Failure modes, refusals, and troubleshooting

### 18.1 Refusal model

The class determines broad client behavior; the code determines operator action.

| Class | Meaning | Default client action |
|---|---|---|
| `not_covered` | Adapter/asset intentionally does not support the request. | Do not retry unchanged; choose a supported mode/asset. |
| `unavailable` | Dependency or remote service is unavailable. | Retry with bounded backoff if deadline permits. |
| `denied` | Identity/policy forbids the request. | Do not retry; correct entitlement or contract. |
| `incomplete` | Continuity/completeness cannot be proven. | Do not treat as absence; resnapshot or repair coverage. |
| `invalid` | Asset, intent, schema, value, or contract is invalid. | Fix configuration/code; do not retry unchanged. |
| `exhausted` | Budget, row, byte, time, or resource ceiling is reached. | Retry only after a deliberate limit/budget/cadence decision. |

The core currently marks only unavailable/exhausted as retryable. Preserve that
signal; do not make every 422 into a generic exponential retry storm.

### 18.2 Diagnostic sequence

1. Capture HTTP status/problem slug or evidence refusal class/code, request id,
   asset ref, role, and timestamp. Do not capture secret/parameter values.
2. Check `/version` for exact Server lockstep and `/readyz` for draining/store
   health.
3. Confirm the route is mounted on the process role; a structural 404 is often
   a role/service routing error.
4. Query the redacted journal and relevant freshness/queue/refusal report.
5. Run `mxctl validate`, then source `probe` and `introspect` under the failing
   tenant/class.
6. For Mode B, run `mxctl verify` and inspect source/schema/seal timing. For a
   semantic view, compare the current definition fingerprint.
7. For Mode A, inspect run, lease and checkpoint; compare it with source
   watermark/CDF/CDC position and Server collection seal.
8. For Mode C, inspect pinned head sequence, observation completeness, alias
   ambiguity, gate history, promotion version and authority scope.

### 18.3 Frequent mistakes

| Symptom | Likely cause | Recovery |
|---|---|---|
| Asset validates; watermark query names the wrong/absent column | Old asset/code or missing declaration | Upgrade; declare the real source watermark and tie-break; run advancement scenario. |
| Query succeeds at source but Matrix refuses schema | Remote type/scale or column order differs | Update the contract only if business meaning agrees; otherwise fix source query. |
| Empty result cannot seal | Contract/result schema not available or stale build | Ensure a declared result schema and current empty-result fix. |
| `metric_view_changed` | Cube/dbt definition fingerprint moved | Review definition, rerun verified questions, record new verification. |
| Required runbook layer refuses as unavailable | Server base URL/token, routing role, breaker, or seal peer missing | Check Server `MUNARIUM_MATRIX_BASE_URL/TOKEN`, Matrix query service, and lockstep. |
| Sync repeats full batch | Checkpoint did not advance or worker died before seal | Inspect run/journal; replay safely; upgrade if on pre-watermark-fix build. |
| CDC retained WAL grows | Consumer stalled or checkpoint not committed | Restore worker/Server; inspect slot position; resnapshot only under explicit recovery. |
| `missing_in_source` seems wrong | Read was partial, alias namespace unclear, or mapping too broad | Keep shadow; require complete scope and correct mapping. |
| Promotion is refused | Gates, mapping version, latest run, or authority scope not current | Fix evidence and rerun; never bypass by editing stored state. |
| 404 on a valid endpoint | Request reached the wrong role deployment | Route control/query traffic to the corresponding service. |

## 19. Extending Matrix safely

### 19.1 Preserve the layer boundaries

The workspace is intentionally split:

- `types` owns strict assets and tolerant wire DTOs;
- `core` owns pure deterministic policy/canonical logic;
- `adapter` owns the provider-neutral trait, identity, binding and capabilities;
- provider crates own transport and source type mapping;
- `workers` own orchestration for query/sync/observe/reconcile/semantic;
- `store` owns PostgreSQL durability;
- `server-client` owns Matrix-to-Server HTTP contracts;
- `server` owns REST/gRPC/MCP/admin/runtime/roles;
- `client` and language clients own public consumption;
- `cli` owns operator workflows.

Do not import a Server crate, move I/O into core, or let an adapter seal/journal
directly. The boundary checker makes some violations mechanical; review must
catch semantic leakage.

### 19.2 Adding an adapter

1. Define truthful capabilities first, including unsupported modes,
   cancellation, source-side limits, replay level, dialect/semantic provider,
   and snapshot marker.
2. Extend strict DataSource connection validation and runtime construction.
3. Implement probe/introspection before execution; expose actual effective
   posture where the engine permits it.
4. Reuse typed binding and core result validation. Do not coerce provider data
   to make fixtures pass.
5. Map every unsupported or unsafe provider state to a typed refusal with a
   safe code.
6. Add captured real payloads, negative type cases, exact decimal/timestamp,
   null/empty, policy and cancellation tests.
7. Add a conformance tier that prints skipped when credentials are absent,
   then earn the support row with a least-privilege live cycle.

### 19.3 Contract/API changes

Change the vendored JSON schemas first, update examples, strict asset parsing,
tolerant DTOs, REST/OpenAPI, proto mirror, MCP/client methods, and Server
integration where applicable. Preserve additive compatibility or deliberately
raise the version. Add database changes only through a new additive migration.
Run protocol drift, all language client tests, and a Server/Matrix lockstep
cycle before release.

### 19.4 New worker or authority behavior

Keep pure classification/decision logic below orchestration, dry-count writes
before side effects, add durable idempotency keys, define crash points and
checkpoint order, and make rollback/history behavior explicit. Any new path
that can write Server canon needs a shadow phase, measured gates, scoped
authority, decision id, journal, and superseding rollback.

## 20. How the implementation evolved

The order in which the pieces landed explains several of the seams above, so it
is recorded here by theme. Superseded intermediate behavior is not presented as
current truth anywhere in this guide: every claim was checked against the
implementation as it is.

| Theme | Lasting result |
|---|---|
| Initial product slices | Core/types/adapters/workers/store/server foundation; independent Server contract; evidence-provider integration. |
| Production wiring and routes | Query compiler reached the real execute path; promised routes and spec/router checks became executable. |
| Live Mode B | Source time-travel evidence and the live execute path. |
| Mode C and authority | Observation, missing-in-source discipline, shadow gates, promotion/demotion/rollback, live defect repairs. |
| Data plane expansion | gRPC, MCP and the clients, native DataView, CDC, MySQL and SQL Server behind one adapter seam. |
| Safety and operational truth | Admin hardening, SQL Server certificate handling, an explicit rustls provider. |
| Incremental correctness | Watermark declaration reaches adapters, checkpoints advance, empty results seal, authorization classes and drift measurements close. |

## Appendix A. Configuration and environment-variable reference

The server parses these at startup unless noted. Secret *values* should come
from a secret provider; this table names references and nonsecret controls.

| Variable | Default / requirement | Purpose |
|---|---|---|
| `MUNARIUM_MATRIX_ROLE` | `all` | `control`, `query`, `sync`, `reconcile`, or laptop `all`. |
| `MUNARIUM_MATRIX_HTTP_ADDR` | `0.0.0.0:8180` | REST/MCP/admin listener. |
| `MUNARIUM_MATRIX_OPS_ADDR` | `0.0.0.0:9190` | Metrics/liveness/readiness listener. |
| `MUNARIUM_MATRIX_GRPC_ADDR` | `0.0.0.0:50151`; `disabled` supported | Execute gRPC listener. |
| `MUNARIUM_MATRIX_DATABASE_URL` | Required | Matrix PostgreSQL connection. |
| `MUNARIUM_MATRIX_DB_MAX_CONNS` | `10` | Store pool ceiling per process. |
| `MUNARIUM_MATRIX_AUTH_MODE` | `static` | `static` or development-only `disabled`. |
| `MUNARIUM_MATRIX_STATIC_TOKENS` / `_FILE` | Required in static mode | Comma-separated `token:tenant:ro|rw|mgmt`. |
| `MUNARIUM_MATRIX_SERVER_URL` | Optional by role; required to seal/write Server | Base URL of Munarium Server. |
| `MUNARIUM_MATRIX_SERVER_TOKEN_REF` | Optional reference | Server bearer secret reference. |
| `MUNARIUM_MATRIX_TARGET_SERVER_VERSION` | `1.0.0` | Lockstep target. |
| `MUNARIUM_MATRIX_MAX_CONCURRENCY` | `64` | Process work ceiling. |
| `MUNARIUM_MATRIX_EGRESS_DEFAULT_DENY` | true | Require asset host admission. |
| `MUNARIUM_MATRIX_FILE_ROOT` | None | Root for landing `file` objects. |
| `MUNARIUM_MATRIX_PROMOTION_MIN_IDENTITY_PRECISION` | `0.95` | Current Mode C promotion gate. |
| `MUNARIUM_MATRIX_PROMOTION_MIN_VALUE_CONFORMANCE` | `0.99` | Current Mode C promotion gate. |
| `MUNARIUM_MATRIX_ADMIN` | `enabled` | Set `disabled` to omit admin routes. |
| `MUNARIUM_MATRIX_JOB_LEASE_SECS` | `300` | Worker job lease. |
| `MUNARIUM_MATRIX_LOG` | `info` | Rust tracing filter. |
| `MUNARIUM_MATRIX_LOG_FORMAT` | `plain`; `json` supported | Structured logging. |
| `MUNARIUM_MATRIX_SECRET_<NAME>` | No default | Normalized named source secret. |

Matrix clients use `MUNARIUM_MATRIX_URL` and `MUNARIUM_MATRIX_TOKEN`.
Munarium Server uses `MUNARIUM_MATRIX_BASE_URL` and
`MUNARIUM_MATRIX_TOKEN` to reach Matrix; do not confuse client URL and Server
base URL variables.

## Appendix B. Asset field reference and annotated examples

The repository's executable examples under
[`fixtures/assets`](../../../fixtures/assets) are the canonical starting point.
This shortened example illustrates composition; copy exact fields from the
current schema before applying.

```yaml
apiVersion: munarium.ioka.io/v1
kind: DataSource
metadata: { name: crm, version: 1 }
spec:
  adapter: postgres
  connection:
    host: crm.internal.example.com
    database: crm
    sslmode: verify-full
  credentialRef: matrix-crm
  egress: { allowHosts: [crm.internal.example.com] }
  role:
    mustBe: { readOnly: true, subjectToRowSecurity: true, notOwner: true }
  authorization: { strategy: source_native }
  limits:
    maxRows: 10000
    maxBytes: 8388608
    statementTimeoutMs: 8000
    budgetPerHour: 500
  snapshot: { kind: pg_snapshot, replayLevel: sealed_result }
  schemaFingerprint: { onDrift: refuse }
  sync:
    mode: watermark
    schedule: "*/15 * * * *"
    watermark:
      column: updated_at
      inclusive: false
      tieBreak: id
    deletes: { kind: soft, column: deleted_at }
    entity: { table: opportunities, key: [id] }
    projection: [id, name, stage, amount, region, owner_id, updated_at]
```

```yaml
apiVersion: munarium.ioka.io/v1
kind: QueryContract
metadata: { name: open-pipeline-by-region, version: 3 }
spec:
  source: crm
  parameters:
    as_of: { type: date, required: true }
  statementByDialect:
    postgres:
      inline: >-
        SELECT region, SUM(amount) AS pipeline_amount,
        COUNT(*) AS opportunity_count FROM opportunities
        WHERE stage <> 'Closed Won' AND stage <> 'Closed Lost'
        AND updated_at <= :as_of GROUP BY region ORDER BY region
  reads:
    tables: [opportunities]
    columns: [region, amount, stage, updated_at]
  result:
    columns:
      region: { type: string, key: true }
      pipeline_amount: { type: decimal, scale: 2, unit: USD, additivity: additive }
      opportunity_count: { type: int64, additivity: additive }
    columnOrder: [region, pipeline_amount, opportunity_count]
    orderBy: [region]
    derivations:
      total_pipeline: { op: sum, over: pipeline_amount }
  policy:
    authorization: source_native
    deniedColumns: [owner_email]
  limits: { maxRows: 500, maxBytes: 1048576, timeoutMs: 6000 }
  evidence: { retentionDays: 400, replayLevel: sealed_result }
  verifiedQuestions:
    - question: "What is the open pipeline by region as of 2026-06-30?"
      parameters: { as_of: "2026-06-30" }
      expect:
        rows: 1
        invariants:
          - { op: sum, over: pipeline_amount, equals: "2520000.50" }
```

Field names in the examples are intentionally specific. Do not generalize a
database URL into `connection` if it embeds credentials; do not omit result
identity; do not claim a watermark without testing advancement; and do not put
friendly aliases in entity-key templates unless they are declared stable keys.

## Appendix C. API and client operation matrix

| Operation | REST | gRPC | MCP | `mxctl` | Rust/language clients |
|---|---:|---:|---:|---:|---:|
| Validate/apply assets | Yes | No | No | Yes | Yes |
| List/get registry | Yes | No | No | Yes | Yes |
| Probe/introspect | Yes | No | No | Partial CLI | Yes |
| Execute contract/view | Yes | Yes | Yes | Indirect through verify | Yes |
| Verify questions | Yes | No | Tool-specific execute only | Yes | Yes |
| Enqueue sync/reconcile | Yes | No | No | Yes | Yes |
| Promotion/gate/rollback | Yes | No | No | Yes | Yes |
| Journal/reports | Yes | No | No | Journal/health | Yes |
| Streaming progress | No | Yes | No | No | gRPC-generated client |

## Appendix D. Adapter capability and evidence matrix

The summarized table in §4.3 is intentionally conservative. Before procurement
or production approval, read the live row and limitations in
[`docs/adapters/build-matrix.md`](../../adapters/build-matrix.md), then inspect
the adapter's `capabilities()` implementation. The following are especially
easy to misread:

- capability `query_contracts=true` does not mean live evidence exists;
- a snapshot marker of `None` is honest and weaker than a provider job id;
- `sealed_result` replay proves the result artifact, not source time travel;
- `source_side_limits=false` means post-read truncation may not control scan
  cost;
- `cancellation=false` means a dropped client does not prove remote work
  stopped;
- semantic adapters do not support Mode A even if their APIs expose tables.

## Appendix E. Error and refusal reference

The specific registry and RFC 9457 mapping are documented in
[`docs/errors.md`](../../errors.md). Applications should branch first on
refusal class, log the safe specific code, and display a product-appropriate
message. End-user messages should not repeat operator detail from source errors.

Representative code families include contract/semantic not covered; source or
credential unavailable; policy, role, column or egress denied; schema,
checkpoint, CDC/CDF or completeness gaps; invalid binding/result/type/scale;
and row/byte/time/budget exhaustion. Consult current source rather than coding
an exhaustive enum into a client: refusal **classes** are closed, while specific
codes can grow.

## Appendix F. Source-code map

| Concern | Primary implementation |
|---|---|
| Contracts/assets/DTOs | [`munarium-matrix-types`](../../../src/munarium-matrix-types/src/lib.rs) |
| Canonical values/results/hashes | [`core/value.rs`](../../../src/munarium-matrix-core/src/value.rs), [`core/result.rs`](../../../src/munarium-matrix-core/src/result.rs), [`core/canon.rs`](../../../src/munarium-matrix-core/src/canon.rs) |
| Compiler/derivations/semantics | [`core/compile.rs`](../../../src/munarium-matrix-core/src/compile.rs), [`derivation.rs`](../../../src/munarium-matrix-core/src/derivation.rs), [`semantic.rs`](../../../src/munarium-matrix-core/src/semantic.rs) |
| Adapter seam/binding/capabilities | [`munarium-matrix-adapter`](../../../src/munarium-matrix-adapter/src/lib.rs) |
| Provider adapters | `matrix/src/munarium-matrix-adapter-{postgres,mysql,sqlserver,landing}` in this repository; Databricks, BigQuery, Snowflake, Cube and dbt are Munarium Matrix Enterprise |
| Query/evidence | [`workers/query.rs`](../../../src/munarium-matrix-workers/src/query.rs), [`workers/evidence.rs`](../../../src/munarium-matrix-workers/src/evidence.rs) |
| Materialization | [`workers/sync.rs`](../../../src/munarium-matrix-workers/src/sync.rs) |
| Observation/reconciliation | [`workers/observe.rs`](../../../src/munarium-matrix-workers/src/observe.rs), [`workers/reconcile.rs`](../../../src/munarium-matrix-workers/src/reconcile.rs), [`workers/authority.rs`](../../../src/munarium-matrix-workers/src/authority.rs) |
| Durable store/migrations | [`munarium-matrix-store`](../../../src/munarium-matrix-store/src/lib.rs), [`migrations`](../../../src/munarium-matrix-store/migrations/0001_registry.sql) |
| REST/gRPC/MCP/admin/runtime | [`munarium-matrix-server`](../../../src/munarium-matrix-server/src/main.rs) |
| Matrix-to-Server contract | [`munarium-matrix-server-client`](../../../src/munarium-matrix-server-client/src/lib.rs) |
| Public Rust client/CLI | [`munarium-matrix-client`](../../../src/munarium-matrix-client/src/lib.rs), [`mxctl`](../../../src/munarium-matrix-cli/src/main.rs) |
| Server consumer | [`evidence_providers.rs`](../../../../server/src/munarium-server/src/evidence_providers.rs), [`research.rs`](../../../../server/src/munarium-runbooks/src/research.rs) |
| Boundary and test gates | [`scripts/boundaries.py`](../../../scripts/boundaries.py), [`test.ps1`](../../../test.ps1) |

## Appendix G. Production-readiness checklists

### Source and contract

- [ ] Exact adapter mode is compose- or live-proven for the intended engine.
- [ ] Dedicated source principal passes live posture introspection.
- [ ] RLS/column/semantic policy is tested with allowed and denied principals.
- [ ] Egress host, TLS verification, credential reference and rotation are owned.
- [ ] Projection/read allowlist is minimal; denied columns are absent everywhere.
- [ ] Result schema, decimal scales, nulls, row identity and ordering are explicit.
- [ ] Verified questions cover business totals, negative cases, empty results and drift.
- [ ] Row/byte/time/cost limits are based on measurements.

### Server integration

- [ ] Server `dataViews` pins kind/name/version and typed parameters.
- [ ] Research layer declares required/optional, role, deadline and byte ceiling.
- [ ] Session/view authorization intersection has positive and negative tests.
- [ ] A combined document + Matrix turn returns and verifies both citation kinds.
- [ ] Server and Matrix `/version` lockstep is exact.
- [ ] Circuit-breaker/refusal behavior is observable and does not leak hidden source detail.

### Operations and recovery

- [ ] Role-specific routing, probes, metrics, queue and checkpoint alerts exist.
- [ ] Database backup/restore is coordinated with source position and Server evidence.
- [ ] Interrupted query/sync/reconcile and Server seal outage have been exercised.
- [ ] Image digest and immutable asset refs are in the release record.
- [ ] Live conformance evidence and provider-cost cleanup are recorded.
- [ ] Snowflake/dbt/BigQuery Mode A are not described internally as proven unless new evidence exists.

### Additional Mode C gates

- [ ] Meaningful shadow period completed with adjudicated false positives/negatives.
- [ ] Identity precision and value conformance clear current thresholds over time.
- [ ] Property/date authority scopes and `document_over_source` consequences are approved.
- [ ] Promotion requires a unique external decision id and separate operator role.
- [ ] Proposal idempotency, demotion and rollback-by-supersession are demonstrated.
- [ ] Business/data owners have signed the source-of-truth decision.
