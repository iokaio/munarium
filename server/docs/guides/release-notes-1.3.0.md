# Munarium Server 1.3.0 release notes

Released **26 September 2026**. Munarium Server 1.3.0 strengthens the evidence
behind token and monetary accounting, makes command and runbook recovery safer,
adds explicit governance and source-retention controls, and hardens provider,
storage and wire boundaries. It also expands validation and library support.

These notes cover **every merged PR between the 1.2.1 and 1.3.0 image sources**:
42 PRs, **#29 through #70**. The PR descriptions were checked against the tagged
commit history and the resulting API and operator documentation. The
[complete comparison](https://github.com/iokaio/munarium/compare/v1.2.1...v1.3.0)
runs from `c638a8e56fff45cef358ff2f4a5b5ba57957ba59` to
`eaa04ac6da25cb332b674c6535013a19b87fa0e7`. PR #41 is included: its merge commit
`6b39a36` restores automatic CI, although its commit subject omits the PR number.

Collection queries, publication governance, checked narrative answers and
original-reference authorization already shipped in 1.2.1. They remain available;
the sections below describe the subsequent improvements rather than counting
those earlier features again.

## Release identity and compatibility

| Item | Version or identity |
|---|---|
| Server and `mmctl` | 1.3.0, Git tag `v1.3.0` |
| Previous image | 1.2.1, Git tag `v1.2.1` |
| Docker image | `iokaio/munarium:1.3.0`, Linux AMD64 and ARM64 |
| Immutable OCI index | `sha256:55078aa474c214dd68d84bfe92d7c6ce2d696fad0f36cc5dd270b160d8c1ec4f` |
| Wire contract | MMP major 1; existing `/v1` and `/v1.2` paths retained |
| Named API operations | 135, up from 121; 14 additions and no removals |
| New database migrations | 0035–0040, applied after the 1.2.1 schema |
| Native Server build toolchain | Rust 1.98.0, pinned by the repository |
| Supported embedded datastore minimum | Rust 1.92 |

The [published GitHub release](https://github.com/iokaio/munarium/releases/tag/v1.3.0)
records architecture digests, signing instructions, SBOM/provenance and image
qualification. The release promoted the tested index to `1.3.0`, `1.3` and
`latest`; use the version or digest for reproducible deployments.

SDK publication is separate from image publication. The tagged image source
prepared Server SDK versions 1.2.0 in [#70]; the subsequent [#72] aligns the Rust,
Python, .NET and Java Server SDK source packages to **1.3.0**. Their declared
Server range is **1.3/1.2**. The Rust wire crates `munarium-proto` and
`munarium-api-types` are already versioned 1.3.0 in the tagged source and must be
published before the Rust SDK. Source versions do not establish registry
availability. Published SDK 1.1.1 retains its previously qualified 1.2/1.1 range;
Matrix source remains 1.0.0 and its client packages remain 1.1.1. See the
[client compatibility record](../../../clients/compatibility.json).

## Token evidence, reconciliation and admission

### Missing usage no longer becomes free work

The completion gateway distinguishes complete observed usage, partial counts,
estimates and unknown usage. Complete counts settle to their checked sum,
including a genuine reported zero. Missing or malformed usage retains at least
the reservation and any larger known subtotal. A reservation of 10 tokens with
no usage evidence remains charged at 10; a known partial count of 15 charges 15.
Overflow and PostgreSQL integer conversions are checked. ([#44], [#47])

Original reservations and usage quality survive settlement and process reopen.
Migration 0035 records that evidence atomically in PostgreSQL; memory storage
implements the same behavior. Historical rows and older writers remain explicitly
unknown rather than acquiring invented usage evidence. Defaulted extension
methods preserve existing custom provider and budget-store implementations.

### Late evidence has an auditable correction path

Versioned estimates account for the effective request, including system text,
tools, structured-output schema and output ceiling. Management callers can inspect
evidence, append corrections, read adjustment history and report daily usage
quality. Corrections retain immutable before/after evidence, revision checks and
idempotent replay. They adjust the **original accounting day**, rather than moving
old debt into today's allowance. Migration 0038 stores estimator and evidence
revisions and append-only adjustments. ([#64])

New accounting fields use decimal integer strings where required to preserve
64-bit precision. Unknown unrecorded coverage remains visible. Estimates are
heuristics, not guaranteed upper bounds or provider invoices; corrections do not
rewrite historical observations.

### Shared caps cover each HTTP attempt

Provider configurations can opt into `spec.budgets.dailyTotalTokens`, a shared
UTC-day allowance for gateway completion and embedding HTTP attempts. Explicit
model selections participate without requiring an artificial access tier.
Transport retries require another reservation; a retry is refused before HTTP
when insufficient capacity remains. Cache hits do not create embedding spend.
Cancellation or a failed submission cannot refund work that may have executed.
An unused tier hold is released when the new cap refuses the first attempt.
Memory and PostgreSQL enforce the same policy, including cross-instance races.
([#65])

Legacy tier limits remain supported. Their rows overlap config-wide accounting
and must not be summed as a provider bill. Upgrade all gateway writers before
enabling the optional policy; older binaries ignore it. The dispatch inventory
and completion-event numbering also make initial, truncation and corrective
attempts distinguishable. ([#48]) See [token budgets](../tokenbudgets.md).

## Provider diagnostics and completion behavior

Opt-in `MUNARIUM_MANAGED_PROVIDER_DIAGNOSTICS=true` restricts paid diagnostics and
named provider health to management callers. Managed `/healthai` selects from the
authenticated tenant's applied configurations, requires `dailyTotalTokens`, and
does not fall back to uncapped environment defaults. The exact resolved config
is checked again at dispatch. A separate free diagnostics operation exposes only
an explicitly configured credential alias and source kind. Ordinary provider
listing stays free and omits aliases; errors retain safe status/category without
raw upstream bodies, credential references or endpoint URLs. ([#66])

A provider/model structured-output policy can explicitly refuse unsupported or
unknown native capability **before** admission or dispatch. Omitting the policy
preserves existing behavior; plain completion still works. ([#64])

Claude configurations gain per-model effort/thinking controls. Effective settings
participate in request identity, preserve structured-output schemas, and omit
unsupported temperature fields for the supported model-specific request shapes.
Unsupported configurations fail before dispatch. These controls do not increase
the configured output ceiling. ([#69])

A session completion receives at most one fourfold enlargement when the provider
explicitly reports token exhaustion. Empty text alone no longer triggers another
larger request. Refusals, malformed responses, unsupported continuation and a
second exhaustion fail rather than returning an incomplete answer. Checked retry
arithmetic prevents overflow; accounting retains dispatched usage even when a
later attempt is denied. Verification retains the latest stop reason.

Four bundled applications update their evidence prompts and increment their
runbook versions: `financial-advisory` 6, `history-revolution` 3,
`sweep-coverage` 2 and `threat-intelligence` 3. The example Anthropic provider pins
high effort. Existing sessions retain their runbook version; use fresh sessions
after applying an updated runbook. These changes are not a claim of better model
quality without workload-specific evaluation. See
[provider configuration](managing-key-and-secrets.md) and [model evidence](../model-evidence.md).

## Optional monetary accounting

PostgreSQL deployments can record immutable tenant tariffs, physical gateway
attempts, observations and later reconciliation. Every recorded submission has
its own attempt identity and price pin; retries share an invocation but remain
separate liabilities. Failure to commit a pre-send record prevents dispatch.
Late observations use the original tariff rather than today's price. Missing
prices, missing usage and unresolved attempts remain explicit. ([#60])

Management-only REST and native gRPC APIs expose tariffs, observations and
currency-specific reports. Calculations use checked integer micro-units; currencies
are never added together. Reports reject windows exceeding 10,000 attempts
instead of silently truncating. Migration 0037 retains the price and observation
history. The existing `/v1/reports/cost` keeps its session-completion-token meaning.

This is scoped accounting for recorded gateway HTTP attempts. It does not fetch
prices, impose a monetary admission cap, or claim to reproduce an entire tenant
invoice. Legacy/bypass traffic, document intelligence, infrastructure and other
unrecorded work require separate accounting. See
[monetary accounting](monetary-accounting.md) for exact coverage and reconciliation.

## Durable recovery and runtime supervision

### Runbook history commits with its checkpoint

A crash could previously leave a step marked done without its required ledger
transition, causing re-entry to skip the missing history. Checkpoint, transition
and lineage-head update now commit in one PostgreSQL transaction. Approval
preconditions are checked under the execution lock, preventing a stale concurrent
approval from launching another executor. ([#54])

Existing gaps are not reconstructed. Runs without a version remain checkpoint-only;
the change does not add automatic resume or exactly-once external effects. Drain
older writers before relying on the guarantee.

### Guarded commands retain ambiguous outcomes

PostgreSQL tenants may explicitly activate `guarded-v1`. A durable claim precedes
execution and binds tenant, key, operation, target, request hash and transport.
Concurrent calls and application restart replay a completed response or return
`command-unresolved`; they cannot blindly execute an ambiguous command again.
Seven keyed REST commands and eight typed command RPCs share this protocol.
Management operations expose activation state and receipt metadata. ([#67])

Activation is **one-way**, legacy mode remains the default, and unresolved claims
do not expire. Completed claims retain the existing receipt TTL. The unresolved
error is HTTP 409 / gRPC `FAILED_PRECONDITION`, not a retryable head conflict.
An unresolved command may have applied zero, some or all effects. Mutation and
receipt are not one transaction; investigate authoritative state before authorizing
a new key. The protocol does not cover unkeyed writes, provider calls or general
runbook execution. See [command recovery](../ops/command-recovery.md).

### Failure cannot leave a partially serving process unnoticed

An unexpected REST, gRPC or operations serving-task exit drains sibling planes
and exits unsuccessfully. Critical task failures affect readiness; bounded drain
and actual listener closure have regression coverage. This closes the post-startup
supervision gap identified during panic hardening. ([#64])

Process-death tests now exercise ledger commits, receipt boundaries, runbook
checkpoints, approvals and artifact publication with uninterrupted controls.
Artifact fixtures verify lease exclusion, manifest-last publication and preservation
of previous serving content across same-node and replacement-node recovery.
These are application-process tests with PostgreSQL running, not database-crash
or power-loss guarantees. ([#52], [#53])

## Explicit governance, authority and source retention

Governance profiles are immutable and content-addressed. Memory versions identify
their comparison policy, with legacy behavior retained by default and exact
string comparison available explicitly. Existing histories transition through
child versions with retained assessments; original claim values and accepted or
disputed decisions do not change. Governed evaluations and findings commit with
claims, unsupported future profiles are rejected, and migration 0036 prevents
older writers from writing governed versions. ([#55])

Assessments cover current accepted facts at a recorded head, not a replay of all
history. Ancestor lineages remain live: pause and redirect those writers during
cutover, carry forward chronology settings, review findings, rebuild affected
collections and revisit publication pins and sessions. See the
[governance upgrade procedure](../ops/governance-policy-upgrade.md).

Direct service calls now bind asserted uid to the verified subject. Model evidence
uses explicit data envelopes that preserve citation and collection/index identity,
including shared chunk IDs. Single-pass substitution and complete-envelope budgets
keep formatting from changing those boundaries. Adversarial transport tests observe
provider calls and protected state. A checked retention inventory documents retained
schema and artifact copies. ([#56])

Source retention adds permanent tenant-scoped denial of a stable logical path,
holds, and optional retryable cleanup of PostgreSQL original bytes. Denial applies
to replacement content at that identity and is checked across source access,
warm/cold/retired retrieval pins, vocabulary, rebuild/export/backfill, checked
answers, publication authorization and public session fetch/reuse. Collections
bound to a denied source become conservatively unavailable; unaffected collections
remain usable. ([#68])

Cleanup is durably pending before deletion; byte deletion and completion commit
atomically. Holds and shared collection ownership keep cleanup pending, failed
transactions retry, and restart rediscovers work. There is no undeny operation.
A hold protects against this cleaner, not normal replacement of readable bytes.
Completed cleanup means **only PostgreSQL original bytes** are gone: derived
chunks, artifacts, audit history, previously delivered copies, backups and external
stores retain their own policies. Drain in-flight work for strict denial cutover
and replay authoritative journals before serving restored databases. See
[source retention](../ops/source-retention.md) and the
[retention inventory](../ops/retention-inventory.md).

## Storage, retrieval, wire and library hardening

- **JSON persistence:** default and arbitrary-precision feature sets are qualified
  across DTOs, PostgreSQL, authoring and sealed artifacts, including cross-feature
  reads and independent old-data fixtures. Literal serde_json marker keys no longer
  become numbers, and JSON numbers no longer become marker objects in generated
  YAML. Stored formats remain compatible. ([#46])
- **Deterministic storage tests:** injectable clocks and ID generators retain
  existing constructors while testing midnight rollover, backwards clock movement,
  original-day settlement, pins, disputed replay and stale-age boundaries.
  Separate benchmarks distinguish governance evaluation from snapshot and
  growing-history write costs. ([#49])
- **Retrieval observations:** per-leg requested, fetched and accepted/truncated
  candidates, overfetch/refill information, and available work/exhaustion counts
  make sparse retrieval easier to diagnose. Flat search reports full scans;
  DiskANN reports distance work and effective search-list size; PostgreSQL reports
  counts and timing. Unknown work remains unknown. This adds measurement and
  independent-oracle fixtures, not a ranking change or universal recall guarantee.
  ([#51])
- **Wire correctness:** checked integer conversions and conservative decoding
  prevent wraparound, overflow, lossy SDK values and unknown governance statuses
  being treated as accepted. Existing v1 JSON number formats and protobuf tags
  remain; transport precision and storage limits are documented separately.
  Boundary tests cover values around 2^53, signed/unsigned limits, replay, pins,
  errors, unknown fields/enums and null versus omission. ([#57])
- **Linux permissions:** sealed artifacts are exercised as a nonroot user with a
  read-only root filesystem, separate writable scratch, and no network or
  capabilities. Fixtures cover missing/read-only scratch, denied ancestor traversal,
  corrupt manifests and close/reopen. Immutable artifacts still require writable
  Tantivy scratch; deployment guidance now makes that distinction explicit. ([#58])
- **Panic and serialization boundaries:** malformed lexical/DiskANN artifacts return
  integrity errors, rate-budget and token-lifetime arithmetic is checked, and
  poisoned locks either recover safely or return storage errors. Serialization
  failures no longer silently persist or return `null`/`{}`; an unrenderable turn
  ends its stream with an error. A malformed successful Matrix verify response
  fails `verifyDataViews` instead of falsely approving a view. ([#61], [#62])
- **Startup and embedder compatibility:** listener/signal setup failures exit 1
  with a `startup error:` message; configuration failures remain exit 2.
  `munarium_datastore::Error` is now `#[non_exhaustive]`, so external exhaustive
  matches need a wildcard arm. The datastore is supported as pinned source with a
  declared public surface, Rust 1.92 minimum, four feature sets, isolated consumers
  and artifact exchange checks. It is not a newly published registry crate.
  Production panic shortcuts are denied with documented exceptions; the policy
  checker parses actual crate attributes. ([#61], [#62])

References: [wire compatibility](../wire-compatibility.md),
[embedded support](../embedded-support.md), [panic boundaries](../panic-boundaries.md),
[datastore operations](datastore.md) and [governance baseline](../governance-baseline.md).

## New management API operations

All 14 additions have native `ServerApiService` operations and generated surfaces
for the four Server SDKs. Existing typed service interfaces are not a substitute
for the complete API client. Consult the [REST reference](../api/rest.md),
[native gRPC reference](../api/grpc-reference.md) and
[operation inventory](../../../clients/server-api.json) for schemas and authority.

| Area | Added REST operations | PR |
|---|---|---|
| Token evidence | `GET /v1/budgets/{id}/evidence`; `GET` and `POST /v1/budgets/{id}/adjustments`; `GET /v1/reports/budget-usage` | [#64] |
| Monetary accounting | `GET` and `POST /v1/monetary/prices`; `POST /v1/monetary/observations`; `GET /v1/reports/money` | [#60] |
| Safe provider diagnostics | `GET /v1/providers/{name}/diagnostics` | [#66] |
| Command recovery | `GET` and `POST /v1/command-recovery`; `GET /v1/command-recovery/receipt` | [#67] |
| Source retention | `GET` and `POST /v1/source-retention` | [#68] |

## Validation, packaging and contributor improvements

Validation runners record selected checks as passed, failed or `not_run`, return
distinct success/failure/incomplete statuses, bind receipts to source identity,
and track owned-resource cleanup. Missing prerequisites, source changes,
interruption or cleanup failure cannot silently become success. Matrix adopts
the same receipt contract for its selected profiles and disposable fixtures.
([#46], [#64])

Frozen evaluation manifests, preregistration proofs, immutable raw outcomes and
grading receipts make measured claims reproducible. The recorded deterministic
local campaign did **not** demonstrate the required usefulness improvement:
Munarium and conventional retrieval tied. Its local latency checks passed, but
neither that result nor the scripted provider tests establish live-model quality
or release-wide service-level guarantees. ([#59])

Local and hosted Server gates share a command catalog with dependencies and
prerequisites; negative controls protect boundary, migration and coverage checks.
Automatic suites remain enabled on standard runners. The temporary manual-only
policy in [#40] was reversed by [#41]; [#63] preserves hosted coverage while
removing command duplication. Publication remains separately dispatched.

Manual client publishing supports rehearsals, package-family selection,
registry preflight, skipping already published versions, dependency-ordered Rust
publication and release tags. Rust packaging includes the protocol files and
README correctly, and crates.io uses trusted publishing. Registry installation
guidance now distinguishes available packages from source versions. ([#31]–[#39])

Release preparation aligns Cargo, Docker defaults, wire dependencies, OpenAPI and
the API inventory. A new CI consistency check distinguishes source versions from
published-image installation defaults. Build and lint use locked dependencies;
dirty-image detection includes untracked Server build inputs and excludes unrelated
client edits. Recovery-test handoffs wait for actual advisory-lock release without
weakening live-owner exclusion. ([#70])

The period also adds reviewed architecture/history diagrams, implementation
guidance, safer PR freshness/merge procedures and a community invitation.
These improve documentation and contribution workflows without adding Server
runtime behavior. ([#29], [#30], [#42], [#43], [#45], [#50])

## Upgrade from 1.2.1

| Migration | Purpose |
|---|---|
| 0035 | Original budget reservations and durable usage evidence |
| 0036 | Governance profiles, revisions and transition protections |
| 0037 | Monetary tariffs, attempts and observations |
| 0038 | Budget corrections, estimator revisions and evidence history |
| 0039 | Durable command-recovery activation and claims |
| 0040 | Source denial, holds, cleanup journal and write guards |

1. Back up PostgreSQL, original source bytes, derived artifacts and configuration
   together. Include policy, publication pins and accounting/recovery/retention
   evidence. Rehearse the upgrade on an isolated copy.
2. Drain and upgrade every relevant reader, writer and worker before depending on
   the new policies. Additive tables do not make mixed-version policy enforcement
   safe: older binaries ignore controls or bypass claims and denials.
3. Adopt governance profiles explicitly, review transition assessments, redirect
   ancestor writers and rebuild affected collections. Installing 0036 alone does
   not change historical comparison semantics.
4. Enable shared token caps, managed diagnostics and structured-output policies
   deliberately. Populate monetary tariffs only when their scope and rates are
   understood. Paid diagnostics and automatic vocabulary generation can use tokens.
5. Activate `guarded-v1` only after writer rollout. Treat `command-unresolved` as
   an investigation, not a reason to automatically submit a new key.
6. Export every page of the source-retention journal and preserve authoritative
   holds/denials independently for restore reconciliation. PostgreSQL original
   cleanup does not erase every derived or external copy.
7. Apply revised runbooks and start fresh sessions to evaluate them. Update Rust
   embedder matches for the non-exhaustive datastore error enum. Verify readiness,
   authenticated REST/gRPC operations and the specific policies being adopted.

**Rollback requires more than an older image.** The 1.2.1 migrator does not know
0035–0040. Do not delete migration history. A pre-upgrade backup may be needed for
a legacy rollback, but a stale backup can lose subsequent claims, denials,
revocations or accounting evidence. After activation or external effects, keep
restored systems isolated until authoritative records are reconciled; prefer a
compatible roll-forward fix. Follow the [upgrade guide](server-1.3.md),
[deployment procedure](../ops/deployment-runbook.md) and
[backup/restore procedure](../ops/backup-restore.md).

## Qualification and remaining limits

The published release records successful source CI and exact-image checks for
both architectures: image/security audits, startup/authentication/PostgreSQL/CLI
behavior, Ollama completion and embeddings over REST/gRPC, retrieval and restart
persistence. Model and persistence checks also ran on Docker Hub pulls of the
exact manifests. Upgrade/restore rehearsal covered pre-activation rollback and
activated governance, completed/unresolved commands, and source holds/denials
restored into 1.3.0. See the
[release qualification record](https://github.com/iokaio/munarium/releases/tag/v1.3.0).

AMD64 was tested natively; ARM64 used emulation. That evidence does not qualify
physical ARM hardware, arbitrary production restores, database crashes, power
loss, exactly-once remote effects, all-copy erasure or broader paid-model quality.
Historical PR test results remain tied to their own sources and environments;
this document does not turn omitted or unsuccessful checks into release evidence.

## Complete PR review ledger

Each PR in the tagged comparison appears once below. The order is by PR number;
merge order differs for some concurrent work. [#71] records subsequent image
publication documentation and [#72] prepares SDK 1.3.0 plus these notes; neither
changes the already built `v1.3.0` image.

| PR | Improvement and release significance |
|---|---|
| [#29] | Records signed 1.2.1 image identities and tested upgrade/rollback; establishes the previous-release publication baseline. |
| [#30] | Aligns architecture, API, SDK, operations and developer documentation with 1.2.1; repairs the generated route table. |
| [#31] | Adds manual registry publication and build-only rehearsals for Server clients, Matrix clients and Rust wire crates. |
| [#32] | Fixes the API-types crate's registry dependency on its matching protocol crate so publication resolves correctly. |
| [#33] | Packages the required copied protocol files with wire crates and supplies the Rust client's repository metadata. |
| [#34] | Checks release-tag existence by API exit status, avoiding a missing-tag JSON response being mistaken for a tag. |
| [#35] | Uses crates.io trusted publishing and short-lived OIDC credentials for established Rust packages. |
| [#36] | Includes the Rust client README in its crate so the registry can render installation and usage guidance. |
| [#37] | Aligns seven client sources at 1.1.1, adds the all-packages family, skips already published versions and tags only publishing families. |
| [#38] | Distinguishes source versions from verified registry versions and documents registry and checkout installation paths. |
| [#39] | Links published Server client packages from Server and container documentation. |
| [#40] | Standardizes hosted runners and publishes contributor guidance; its manual-only full-suite policy is superseded by #41. |
| [#41] | Restores automatic Server, Matrix, client and cross-service suites while retaining focused local checks and standard runners. |
| [#42] | Requires current-base review, overlap checks, valid merge methods and revalidation after conflict resolution. |
| [#43] | Adds the public community invitation to the repository README. |
| [#44] | Preserves observed/partial/unknown provider usage during settlement and prevents missing counts from releasing liability. |
| [#45] | Publishes indexed engineering guidance and fixes the budget sweep test's interference with concurrent PostgreSQL fixtures. |
| [#46] | Adds truthful source-bound validation receipts and cross-feature JSON persistence coverage; fixes marker-key/number conversion and null environment handling. |
| [#47] | Persists original reservations and usage quality atomically in memory/PostgreSQL, with additive migration 0035. |
| [#48] | Inventories dispatch accounting, numbers completion attempts correctly and checks retry-ceiling overflow before dispatch. |
| [#49] | Adds injectable clocks/IDs, deterministic budget/ledger tests and separated governance/snapshot/write benchmarks. |
| [#50] | Publishes reviewed PR history with editable diagrams and clearly separated historical results and open work. |
| [#51] | Exposes retrieval candidate/work observations and adds sparse/filter/pin fixtures with independent exact and ANN comparisons. |
| [#52] | Tests actual application-process death around PostgreSQL ledger commits and fresh-reader recovery. |
| [#53] | Extends process-death coverage to commands, runbook approvals/checkpoints and artifact publication; documents ambiguous outcomes. |
| [#54] | Commits runbook checkpoints and required ledger transitions atomically and prevents stale approval re-entry. |
| [#55] | Adds immutable governance profiles, explicit comparison transitions and retained assessments with migration 0036. |
| [#56] | Binds direct-call identity, frames model evidence safely and adds authority/retention inventories and adversarial tests. |
| [#57] | Preserves exact integers and conservative unknown-value handling across storage, wire protocols and four SDKs. |
| [#58] | Qualifies Linux nonroot/read-only artifact serving with writable scratch and documented filesystem permissions. |
| [#59] | Adds frozen evaluation inputs, immutable outcomes and source/environment checks; retains the rejected usefulness claim and limited latency evidence. |
| [#60] | Adds scoped monetary tariffs, per-attempt liability, reconciliation and currency reports with migration 0037. |
| [#61] | Defines embedded datastore support and minimum Rust; fixes panic, malformed verification, serialization and startup boundaries. |
| [#62] | Validates DiskANN dimensions before allocation and prevents comments or unrelated attributes from satisfying panic-policy checks. |
| [#63] | Shares portable Server gate commands between local and hosted execution while checking that CI coverage is preserved. |
| [#64] | Adds late token reconciliation and request estimation, structured-output refusal, runtime supervision and Matrix validation receipts; migration 0038. |
| [#65] | Enforces opt-in shared daily token capacity before each completion/embedding HTTP attempt, including retries. |
| [#66] | Adds management-only capped diagnostics, explicit safe credential aliases and sanitized provider failures. |
| [#67] | Adds durable guarded command claims, management inspection and one-way activation with migration 0039. |
| [#68] | Enforces stable-path source denial and holds across reads/rebuilds/sessions, plus retryable PostgreSQL original cleanup; migration 0040. |
| [#69] | Adds Claude effort/thinking configuration, exhaustion-only bounded retries, incomplete-answer rejection and versioned prompt updates. |
| [#70] | Prepares Server 1.3.0 metadata/docs, adds release-version drift checks, fixes build-context dirty detection and stabilizes recovery-test lock handoffs. |

[#29]: https://github.com/iokaio/munarium/pull/29
[#30]: https://github.com/iokaio/munarium/pull/30
[#31]: https://github.com/iokaio/munarium/pull/31
[#32]: https://github.com/iokaio/munarium/pull/32
[#33]: https://github.com/iokaio/munarium/pull/33
[#34]: https://github.com/iokaio/munarium/pull/34
[#35]: https://github.com/iokaio/munarium/pull/35
[#36]: https://github.com/iokaio/munarium/pull/36
[#37]: https://github.com/iokaio/munarium/pull/37
[#38]: https://github.com/iokaio/munarium/pull/38
[#39]: https://github.com/iokaio/munarium/pull/39
[#40]: https://github.com/iokaio/munarium/pull/40
[#41]: https://github.com/iokaio/munarium/pull/41
[#42]: https://github.com/iokaio/munarium/pull/42
[#43]: https://github.com/iokaio/munarium/pull/43
[#44]: https://github.com/iokaio/munarium/pull/44
[#45]: https://github.com/iokaio/munarium/pull/45
[#46]: https://github.com/iokaio/munarium/pull/46
[#47]: https://github.com/iokaio/munarium/pull/47
[#48]: https://github.com/iokaio/munarium/pull/48
[#49]: https://github.com/iokaio/munarium/pull/49
[#50]: https://github.com/iokaio/munarium/pull/50
[#51]: https://github.com/iokaio/munarium/pull/51
[#52]: https://github.com/iokaio/munarium/pull/52
[#53]: https://github.com/iokaio/munarium/pull/53
[#54]: https://github.com/iokaio/munarium/pull/54
[#55]: https://github.com/iokaio/munarium/pull/55
[#56]: https://github.com/iokaio/munarium/pull/56
[#57]: https://github.com/iokaio/munarium/pull/57
[#58]: https://github.com/iokaio/munarium/pull/58
[#59]: https://github.com/iokaio/munarium/pull/59
[#60]: https://github.com/iokaio/munarium/pull/60
[#61]: https://github.com/iokaio/munarium/pull/61
[#62]: https://github.com/iokaio/munarium/pull/62
[#63]: https://github.com/iokaio/munarium/pull/63
[#64]: https://github.com/iokaio/munarium/pull/64
[#65]: https://github.com/iokaio/munarium/pull/65
[#66]: https://github.com/iokaio/munarium/pull/66
[#67]: https://github.com/iokaio/munarium/pull/67
[#68]: https://github.com/iokaio/munarium/pull/68
[#69]: https://github.com/iokaio/munarium/pull/69
[#70]: https://github.com/iokaio/munarium/pull/70
[#71]: https://github.com/iokaio/munarium/pull/71
[#72]: https://github.com/iokaio/munarium/pull/72
