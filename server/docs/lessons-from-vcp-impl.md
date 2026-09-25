# Munarium Server implementation plan from the VCP lessons

**Status:** implementation roadmap. The first P01 accounting correction merged in [PR #44](https://github.com/iokaio/munarium/pull/44); P02–P04 merged in [PR #46](https://github.com/iokaio/munarium/pull/46). Durable P01 evidence ([PR #47](https://github.com/iokaio/munarium/pull/47)), the P05 dispatch/retry slice ([PR #48](https://github.com/iokaio/munarium/pull/48)), and the P06 deterministic baseline are implemented below. Their broader follow-ups remain proposed.

**Reviewed:** 2026-09-24.

**Code baseline:** `fe1f216f710b334ee91c2e1d8d894019593bd087`, Server 1.2.1. Completion status updated against `08f9033952fbb65030ae0bbaceca423403c91e04` after PR #44. Unless explicitly updated below, source findings describe the original baseline.

**Input:** [lessons-from-vcp.md](lessons-from-vcp.md), including its R01–R34 recommendation identifiers.

P02–P04 are merged in PR #46: truthful validation results, persistence round-trip tests, and verified documentation corrections. P01 now retains durable reservation and usage evidence; late reconciliation and reporting remain separate follow-ups. P05 dispatch diagnostics and the P06 governance baseline are implemented; use their observations and measure retrieval before optimizing either path. Changes to historical interpretation, recovery guarantees, retention, and the public protocol require explicit compatibility designs before implementation.

This plan develops the recommendations against the current Munarium source. It does not treat downstream measurements as Server benchmarks or assume that every proposed safeguard is absent. The analysis is newly written for this repository; implementation references below point to this repository, without requiring another project's code or private operational records.

No plan can guarantee the absence of regressions. The acceptance standard here is narrower and testable: preserve existing contracts, demonstrate the intended change with meaningful regression tests, qualify upgrades and rollback where persisted state changes, and keep any unavailable evidence visibly incomplete.

## Contents

1. [Evidence and scope](#1-evidence-and-scope)
2. [Compatibility rules and decisions](#2-compatibility-rules-and-decisions)
3. [Delivery sequence](#3-delivery-sequence)
4. [Provider usage, admission, and optional pricing](#4-provider-usage-admission-and-optional-pricing)
5. [Validation receipts and documentation](#5-validation-receipts-and-documentation)
6. [Persistence and JSON feature compatibility](#6-persistence-and-json-feature-compatibility)
7. [Deterministic governance and value comparison](#7-deterministic-governance-and-value-comparison)
8. [Retrieval performance and restricted filesystems](#8-retrieval-performance-and-restricted-filesystems)
9. [Crash recovery and command receipts](#9-crash-recovery-and-command-receipts)
10. [Authority, evidence, and retention](#10-authority-evidence-and-retention)
11. [Wire compatibility](#11-wire-compatibility)
12. [Evaluation and performance evidence](#12-evaluation-and-performance-evidence)
13. [Library support and engineering policy](#13-library-support-and-engineering-policy)
14. [Regression matrix and validation commands](#14-regression-matrix-and-validation-commands)
15. [Recommendation disposition and completion](#15-recommendation-disposition-and-completion)

## 1. Evidence and scope

The review inspected the source, test harnesses, manifests, migrations, and documentation at the baseline above. The companion recommendations are written for this public repository and use repository-local evidence. External activity counts and benchmark figures are not required to justify the Server work below; downstream observations remain prompts for investigation until reproduced on the relevant Server path.

Evidence labels used throughout:

- **Confirmed source behavior:** visible in the inspected implementation; not necessarily a reproduced runtime failure.
- **Existing control:** a mechanism already present that changes must preserve and extend.
- **Investigation:** a risk or transfer hypothesis requiring a focused reproduction.
- **Proposal:** a design to review and implement, not an existing capability or completed test.

### 1.1 Findings that change the implementation order

| Area | Baseline finding | Implementation consequence |
|---|---|---|
| Hosted completion usage | Anthropic and OpenAI-compatible parsers use `as_u64().unwrap_or(0)`; successful completions settle their summed counts | R01 is a concrete correction candidate. Missing usage must remain distinguishable from observed zero |
| Ollama usage | Its completion parser requires input/output counts | Preserve the existing parser contract unless a separately tested compatibility change relaxes it |
| Daily admission | PostgreSQL advisory locking and a memory-store mutex already serialize reservations | Extend the existing budget abstraction; do not add a competing counter |
| Accounting coverage | Daily caps depend on the resolved tier; embeddings and health probes follow different paths | Inventory all paths before claiming a tenant-wide spending cap |
| Cost reports | `op_cost` aggregates stored session completions | Preserve that report's scope; an all-invocation cost report requires additional evidence storage |
| Provider parameters | Structured response formats are emitted; the specific `parallel_tool_calls` issue described downstream is not present here | Add capability handling for parameters actually sent, with compatibility defaults |
| Test outcomes | Requested PostgreSQL setup can fail correctly, but database-dependent tests can return early without a database; optional gates can still end with a broad success banner | Fix evidence reporting without falsely describing every PostgreSQL tier as silently skipped |
| JSON persistence | The downstream serializer issue is not reproduced in this Server | Write actual persistence tests first; do not install a replacement JSON decoder on suspicion |
| Governance equality | `values_equivalent` folds whitespace and case | Preserve that policy for existing histories; introduce exact comparison only with a pinned policy identity |
| Retrieval | Current adapters already contain eligibility filters and bounded-search controls | Characterize sparse scopes and intermediate work before introducing another refill layer |
| Datastore faults | Mirror build phases and injected-failure tests already exist | Reuse those phases when adding actual child-process kill/restart coverage |
| Command receipts | REST/gRPC command wrappers store replay records after the command; insertion failure is logged | Define recovery guarantees before declaring crash-safe command/receipt atomicity |
| Authority | `AccessCtx` is not deserializable, and production JWT construction verifies claims | Audit entry paths and preserve this boundary; do not introduce a redundant authority type without a demonstrated need |
| Answer prompts | Checked answers already label passages as data and validate passage identifiers/quotes | Extend adversarial tests; prompt labels alone do not authorize or prevent effects |
| Removal | Runbook removal is a soft-removal workflow; it is not a general source-erasure API | Define retention modes before changing cleanup or promising erasure |

### 1.2 Scope boundaries

The implementation target is Server and its shared libraries. Client changes are included only where Server contract evolution requires them. Matrix harness adoption is a follow-up after the Server design is qualified, not a reason to change Matrix product behavior. Operational release provenance, paid qualification budgets, and access to external environments are coordination requirements, not code or deployments to perform in this repository.

Do not bundle an unrelated dependency upgrade, new database backend, generic actor framework, automatic routing subsystem, or rewrite of `munarium-server` into these changes. Extract a small shared module or crate only when a concrete dependency boundary or a second consumer justifies it.

## 2. Compatibility rules and decisions

### 2.1 Invariants every work package must preserve

1. **Authority:** tenant scoping, capability attenuation, access levels, compartments, uid attribution, and current authorization apply on both transports. A historical pin does not revive revoked access.
2. **History:** accepted/disputed status, corrections, supersession, provenance, and point-in-time reads remain explainable under the policy that produced them. Blocked claims remain recorded as disputed.
3. **Concurrency:** append gating remains tied to the observed head, with the existing re-gate/retry behavior; admission remains atomic within its declared tenant/config/tier/day scope.
4. **Durability:** no existing migration is edited. An acknowledged write must remain readable under the declared persistence contract. Cleanup and reconciliation cannot discard the only evidence needed for recovery.
5. **Contracts:** preserve existing REST JSON field types, protobuf field numbers, error mappings, runbook defaults, shape versions, and N/N−1 client behavior. New fields are not automatically compatible with every decoder or Rust struct constructor.
6. **Retrieval lifecycle:** retain verified/approved promotion, active-generation consistency, old-version resolution, session pins, and source authorization during candidate selection and response construction.
7. **Boundaries:** keep core/access free of transport/database dependencies, providers free of storage dependencies, and datastore independently usable without a dependency on core or Server.
8. **Validation:** retain automatic build/test suites, dependency/license checks, contract drift checks, runner configuration, and review requirements. A waived or unavailable check remains distinct from success.

These rules are anchored in [contributor guidance](../../CONTRIBUTING.md), [Server architecture](architecture.md), [the security posture](security-posture.md), and [the conformance scenarios](../conformance/SCENARIOS.md).

### 2.2 Decisions and safe defaults

Discovery and tests can proceed before these decisions; dependent behavior changes cannot.

| Decision | Recommended starting position | Work that depends on it |
|---|---|---|
| D1: What is capped when no tier resolves? | Keep existing routing/cap behavior while explicitly reporting the scope. Design an opt-in config-wide cap or explicit missing-tier policy | Broader R15 enforcement; never silently assign a tier to an explicit model |
| D2: Is money a Server reporting feature? | Defer prices until token evidence and invocation coverage are trustworthy | R16 schema, price maintenance, report API |
| D3: Which fault guarantees are supported? | P08 scope: kill/restart the application while PostgreSQL stays running. Database crashes and power loss need separate qualification; stronger recovery contracts remain open | R13 acceptance and required CI coverage |
| D4: What does deletion mean? | Preserve current soft-removal and audit retention; design each stronger mode explicitly | R21 cleanup, tombstones, restore behavior |
| D5: Are embedded crates supported public Rust APIs? | Preserve existing constructors and minimize source breakage while documenting the decision | R31 MSRV/support tier; API shape for R01/R07/R08 |
| D6: What makes governance useful for a target workload? | Freeze task population, minimum useful effect, cost/latency constraints, and mandatory authorization checks before the final evaluation | R25 quality claims and any paid campaign |
| D7: Who may trigger paid diagnostics and view credential aliases? | Keep diagnostics free of credential references; review a separately permissioned operator surface | R28 and health-probe admission policy |
| D8: How is comparison policy selected and pinned? | Immutable profiles; legacy default; existing histories transition explicitly to a child revision | R08 writes, replay, exports, and mixed-version operation |

Record decisions with stable identifiers in the existing engineering record. If a release accepts incomplete evidence, use a numbered known-gap entry with the missing gate, risk, owner decision, and follow-up. Do not mark the gate passed.

## 3. Delivery sequence

Each row is a coherent implementation slice; it may require more than one PR when persistence or contract compatibility warrants separation. Estimates are relative complexity, not promised calendar time.

| Slice | Deliverable | Dependencies | Relative scope | Exit evidence |
|---|---|---|---|---|
| P01 | Provider usage certainty and conservative settlement; first correction merged in PR #44, durable evidence implemented; reconciliation pending | Baseline parser/budget tests | Medium | Explicit zero accepted; absent/partial usage never settles as observed zero; memory/PG parity |
| P02 | Merged in PR #46: Check outcomes, receipts, checker controls | None; parallel with P01 | Medium | Pass/fail/missing/interrupted fixtures and correct exit precedence |
| P03 | Merged in PR #46: JSON feature/persistence characterization | None; parallel with P01 | Small–medium | Default and feature-enabled round trips through actual persistence paths |
| P04 | Merged in PR #46: Verified documentation corrections | Current-source recheck | Small | Current references corrected; historical examples preserved; documentation gates |
| P05 | Dispatch inventory and retry diagnostics implemented; broader admission and diagnostics pending | P01; D1/D7 for policy changes | Medium–large | Every dispatch has an explicit accounting policy; concurrent/retry/cancellation tests |
| P06 | Injectable clocks/IDs and separated governance baseline implemented | Existing conformance; P02 receipts | Medium | Existing constructors unchanged; reproducible traces and separated timings |
| P07 | Characterization merged in PR #51; fixes require demonstrated gaps | P06 baseline where relevant | Medium | Exact-oracle comparisons, authorization parity, bounded-work evidence |
| P08 | Implemented and locally qualified for the D3 application-process scope; atomic runbook checkpoints and retained legacy gaps, §9.1 | D3; P02; existing mirror fault hooks | Large | Named barriers, process termination, reopened-state assertions, reviewed recovery contracts |
| P09 | Durable profiles, explicit transitions and Server evaluation locally qualified, §7.3 | D5/D8; P06; contract design | Medium–large | Historical replay unchanged; exact-policy cross-backend/transport tests |
| P10 | Authority/evidence audit, model-only envelopes and checked retention inventory implemented; qualification below in §10.4 | D4 for stronger retention changes | Medium | Access-path matrix, effect-denial tests, declared derived-content treatment |
| P11 | Integer/unknown-field protocol characterization | Existing contract publisher/client suites | Medium | N/N−1 fixtures; exact integer tests; no unversioned field-type change |
| P12 | Restricted-filesystem qualification | Existing datastore build/reopen fixtures | Medium | Supported Linux permissions documented; separate Windows results |
| P13 | Frozen evaluation and latency reporting | P02/P06/P07; D6 | Medium–large | Offline pilot, validated graders, immutable manifests/results; calibrated timing reports |
| P14 | Optional monetary accounting | P05/P11; D2 | Medium–large | Unknown-price/usage semantics, immutable prices, checked arithmetic, coverage-qualified reports |
| P15 | Library support and lint tightening | D5; measured audit | Medium | Isolated consumer builds/MSRV if adopted; targeted production failure handling |
| P16 | Shared gate definitions and policy follow-ups | P02 stabilized; maintainer-owned workflow changes | Medium | Same required coverage before/after, automatic CI retained, checker self-tests |

P02–P06 diagnostics and baseline slices are merged; broader P05 admission and diagnostic access still await D1/D7. P07 retrieval instrumentation and characterization merged in PR #51. P08 ledger characterization merged in PR #52 and broader characterization in PR #53; atomic runbook recovery and local qualification complete the supported D3 scope (§9.1). Keep the outstanding P01 late reconciliation, estimator revisions, and usage-quality reporting separate. Characterization establishes whether retrieval and recovery fixes are needed. A failing authorization, persistence, or compatibility reproduction discovered in any slice takes priority over optimization. Money and embedded support are conditional product work, not prerequisites for fixing shared-code defects.

For each PR, record affected invariants, a behavioral example, files changed, focused checks, unavailable evidence, and rollback constraints. Keep one behavior and its tests/documentation together. Avoid a large preliminary refactor merely to make later changes aesthetically uniform.

## 4. Provider usage, admission, and optional pricing

### 4.1 P01: preserve usage certainty through settlement — R01

**Change locations:** [provider types](../src/munarium-core/src/provider.rs), [hosted adapters](../src/munarium-providers/src/lib.rs), [Ollama adapter](../src/munarium-providers/src/ollama.rs), [completion gateway](../src/munarium-server/src/providers_api.rs), [budget contract](../src/munarium-core/src/budget.rs), [memory budget](../src/munarium-store-mem/src/budget.rs), and [PostgreSQL budget](../src/munarium-store-pg/src/budget.rs).

At the original baseline, `CompletionResponse` contained two mandatory `u64` counts, hosted parsers collapsed missing or malformed fields into zero, and `complete_with_schema` settled successful responses using the sum. PR #44 corrected that gateway behavior while preserving the legacy response shape.

**Merged first slice:** `UsageEvidence` and `DetailedCompletionResponse` now carry independently optional counts through defaulted detailed provider methods. Complete observed usage settles its checked sum, including explicit zero. Incomplete or unverified usage charges `max(original_reservation, known_subtotal)`; no missing-component estimator was added. Hosted adapters share one decode/request path, Ollama retains its strict parser, and memory/PostgreSQL budget arithmetic is checked. Scripted gateway tests cover both stores and ordinary/structured completion. See [current invocation provenance](architecture.md#93-invocation-provenance).

**Durable evidence slice:** memory and PostgreSQL preserve original reserved units, accounted units, independently optional usage counts and source quality in one settlement. The reservation ID identifies the evidence. Migration 0035 leaves historical original units and usage unknown; it does not relabel historical zeros. Legacy `settle` and custom stores remain source compatible through defaulted methods. Full observation is derived from provider-reported source and both counts; partial/missing/malformed/unverified evidence stays distinct. Duplicate settlement, release and sweep cannot replace settled evidence.

**Still proposed:** an effective-request estimator with revision identity, late reconciliation, broader attempt/admission coverage, and usage-quality reporting. The existing wire counts, metrics, and reports retain their numeric projections. The design and full acceptance matrix below include these follow-ups; PR #44 does not complete all of them.

**Internal representation and extensions.** The first slice added provider-neutral usage evidence with independently optional input/output counts and a source classification. Add an estimator revision when improved estimates are introduced. Preserve raw observed categories needed for future accounting without conflating overlapping categories. The conceptual shape is:

```rust
// Internal evidence shape; not a replacement wire DTO.
struct UsageEvidence {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    source: UsageSource,
}

enum UsageSource {
    ProviderReported,
    LegacyUnverified,
    Missing,
    Malformed,
}
```

Completeness is derived from the known components and provider semantics. Do not let an independently stored `complete: true` contradict missing fields. Keep measured counts separate from the amount conservatively charged to a cap. An accounting record needs both concepts, even when they happen to have the same value.

**Compatibility design.** Do not change existing wire counts to `null`, strings, or omitted fields in a patch. Adding a field to a public Rust struct also breaks downstream struct literals. If those Rust interfaces must remain source compatible, add a detailed response wrapper and a defaulted trait method, leaving existing `complete`/`complete_structured` signatures available. Built-in adapters override the detailed method using one shared decode path; the legacy method projects that same result. Avoid recursive default delegation or a second paid request. A legacy custom provider that cannot attest completeness must produce explicitly unverified evidence, not an invented observation. If D5 instead permits coordinated internal API changes, update all workspace constructors and fixtures together and document the downstream impact.

**Settlement rules:**

| Evidence | Budget treatment | Reporting |
|---|---|---|
| Both counts observed and valid, including two explicit zeros | Settle the checked total | Observed usage |
| One count observed | Charge at least the original reservation and at least the known subtotal; add a documented estimate for missing components where available | Partial observation plus estimated accounting amount |
| Usage absent/malformed but answer otherwise usable | Retain a conservative estimate | Unknown/malformed usage, estimated charge; answer success remains separate |
| Proven failure before dispatch | Release according to the existing no-work-started contract | No provider work submitted |
| Failure/cancellation after dispatch may have happened | Keep reserved capacity or a more conservative amount | Unresolved/estimated outcome; do not assume zero cost |
| Late usage after an estimate has settled | Append/reconcile evidence under an idempotent correction policy | Preserve prior estimate and reconciliation history |

For incomplete usage, use `max(original_reservation, known_subtotal + estimate_of_missing_components)` where the estimate is available and arithmetic is checked. This is conservative relative to available evidence, not a mathematical upper bound on unknown provider work. Simply retaining a smaller reservation would undercount an already observed component larger than it.

The hosted-adapter correction must not silently relax Ollama's existing rejection of missing/malformed counts. Preserve that strict behavior in P01; accepting such an Ollama response would be a separately tested contract change.

The current prompt-bytes/4 estimate omits the separately supplied system prompt and structured-request overhead. Introduce a named estimator over the **effective request**: system, prompt, tools/schema, provider framing, and the normalized output ceiling. Record its revision. Ensure tiny requests and `max_tokens = 0` normalization agree with adapter behavior. A heuristic token estimate cannot honestly be advertised as a strict monetary ceiling.

**Persistence evolution.** [The existing token-budget migration](../src/munarium-store-pg/migrations/0029_token_budgets.sql) stores mutable units and lifecycle state, not the full original-estimate/observed distinction. Add a new migration after the current migration head; do not edit that migration. Retain original reserved units, accounted units, usage quality, and attempt/evidence identity. Backfill existing rows as legacy evidence of unknown quality. Never label historical zeros or settled estimates as observed merely because they are present in the table.

Keep the first correction small: parser evidence, gateway settlement, focused tests. Introduce durable quality/reporting fields in a following compatible slice if necessary. During a rolling upgrade, old binaries can still settle zeros and omit evidence, so the new guarantee begins only after all accounting writers are upgraded or writes are routed exclusively to upgraded workers. New readers must tolerate legacy rows. A rollback to old writers restores the old limitation; describe that honestly rather than calling the data fix rollback-safe by default.

The existing `settle` operation is intentionally a no-op after a reservation leaves `held`. Late reconciliation therefore needs a new compare-and-swap evidence revision or immutable adjustment operation; merely calling `settle` again will not correct a stale estimate. Test concurrent/replayed/out-of-order evidence, settle-versus-sweep races, and actual usage greater than the estimate. Any excess is accounted debt that reduces future admission; do not erase it to force the cap ledger back under its limit. Preserve the original day and do not resurrect a released reservation through an unrelated observation.

**Acceptance tests:** extend [provider contract fixtures](../src/munarium-providers/tests/contract.rs) and [Ollama fixtures](../src/munarium-providers/tests/ollama_contract.rs). Cover absent object, absent one field, null, string, negative, overflow, explicit zero, reasoning truncation, refusal, error-in-200, empty choices, and tool-only output. Exercise the gateway with a real memory budget store, then PostgreSQL parity. Assert the stored accounted amount and quality, returned answer, metrics, and provenance. Verify duplicate settlement, settlement failure, stale sweep, late correction, and integer conversion bounds. A parser-only test is insufficient.

### 4.2 P05: make the admission coverage explicit — R15 and R14

**Implemented slice:** [dispatch inventory](tokenbudgets.md#dispatch-accounting-inventory), sequential session completion ordinals, checked truncation retry ceiling, and scripted retry/cancellation tests. New cap policies, physical-attempt accounting and diagnostic audience changes remain subject to D1/D7.

**Change locations:** `providers_api.rs::complete_with_schema`, `models.rs`, `sessions_api.rs`, `sessions_model_tests.rs`, `runbooks_api.rs`, `authoring_api.rs`, `evidence_hierarchy.rs`, `answers_api.rs`, `vocabulary_api.rs`, and the existing budget-store implementations.

| Dispatch path | Current relevant behavior | Required plan |
|---|---|---|
| Direct completion, REST and native gRPC | Shared gateway; daily reservation when a capped tier resolves | Preserve transport parity and explicit model behavior |
| Structured completion | Same gateway, provider-specific schema parameters | Carry the same usage/attempt evidence |
| Session answer, expansion, corrections, hierarchy tasks | Shared helper paths, separate calls | Record each call and its parent operation; verify every helper's budget scope |
| Runbook advisory, authoring, checked answers, vocabulary | Shared completion gateway | Include failure and cancellation paths; do not equate a final result with all helper work |
| Explicit model or tenant-default fallback without tier | May have rate checks but no daily tier cap | Document and test; D1 decides any new policy |
| API embeddings | Rate budget and cache; no daily token reservation; invocation counts currently zero | Add embedding usage evidence; account cache misses and hits distinctly |
| Index construction | Uses a local embedder independently of the provider embedding API | Do not assign hosted-provider charges to this path |
| `/healthai` | Authenticated any-role diagnostics; directly calls providers and deliberately bypasses the normal budgets | Specify a probe policy and cap; do not silently treat it as a management-only route or as readiness |
| Provider transport retry | 429/5xx retry inside one gateway invocation | Track physical attempts and ambiguous liability, not only the final response |

Preserve `PgBudgetStore::reserve` advisory locking and commit-before-grant, the memory-store mutex, and the same UTC-day expression in enforcement and reports. Test with independent connections/store instances; a single shared Rust mutex does not prove cross-replica admission.

Do not immediately route every path through a new service. First produce a table in tests/code documentation linking each dispatch to its current admission and recording policy. Then extract shared preparation/settlement logic around the existing gateway, keeping provider modules storage-independent. Caller-supplied metadata may identify a requested model; it must not supply trusted scope, reservation identity, or permission to spend.

Distinguish a logical operation, a model invocation, and a transport attempt. The current `send_with_retry_impl` can submit more than once while returning only final usage. Add stable parent/attempt identifiers and a pre-dispatch observation hook or equivalent accounting seam. Decide whether admission reserves an aggregate retry allowance or each attempt separately; both must retain unresolved exposure for an attempt that might have executed. Do not label a later successful response as evidence that an earlier 5xx attempt cost nothing. Preserve existing retry limits until the replacement policy is qualified.

For the session's truncation retry, retain the existing bounded behavior while making the second call visible: initial ceiling, retry ceiling, reason, parent attempt, cap result, and final outcome. Test that there is at most one application-level enlargement and that transport retries do not obscure the actual attempt count. A 4× increase must use checked arithmetic and a validated maximum. Budget denial after the first response must not erase the first attempt's usage.

There are two concrete diagnostics to fix in `sessions_api.rs`: the initial and truncation-retry progress events both currently emit `attempt: 0`, and the retry ceiling uses unchecked multiplication. Verification can make additional bounded corrective calls after the truncation retry; represent these as separate reasons (`initial`, `truncation`, `verification`) in one monotonically identified attempt sequence. Retain final finish reason as well as the original attempts' outcomes. Empty text alone does not establish budget exhaustion. The current SSE path streams progress around buffered completion, not provider tokens; cancellation tests must cover that worker/gateway lifecycle rather than assume a token-streaming adapter exists.

Keep `truncated_by_budget`, `request_cap_reached`, provider rate limiting, timeout, malformed output, and authorization denial distinguishable. A diagnostic label does not turn a failed answer into a pass. Multi-step context may include a server-computed remaining allowance only when such an allowance actually exists. The current runbook executor is primarily an index lifecycle machine, not a general agent loop; do not invent a request-limit scheduler just to display a remaining count. Treat displayed capacity as a snapshot, not a reservation under concurrency.

For embeddings, expose reported usage if supplied by the provider, and record estimated/unknown usage otherwise. A cache hit incurs no new provider submission; reusing an embedding must not copy its original billed usage into a new spend record. Keep rate-limiter/cache behavior unchanged until separately reviewed.

**Acceptance:** two-replica cap races, tenant/config/tier isolation, uncapped paths, cancellation before/after send, 5xx then success, repeated 429, settlement failure, stale reservations, UTC rollover with late settlement against the original reservation day, restart, cache hits/misses, and arithmetic limits. Scripted providers must observe how many physical requests arrived. No paid service is needed for these tests.

### 4.3 Provider capability and diagnostic hygiene — R06 and R28

The current hosted builder does not emit the downstream `parallel_tool_calls` parameter. It does emit structured-output parameters (`response_format` or `output_config`); Ollama has its own `format` behavior. The existing model map is not a full capability catalog.

Add a narrowly scoped per-config/model capability override for parameters the Server actually supports. Keep a compatibility mode reproducing current qualified structured requests. For a newly configured capability with unknown support, choose an explicitly documented prompt-based fallback with the **same response/schema/provenance validation**, or a pre-send unsupported-capability error. Do not silently discard a required schema or retry a rejected request in a more permissive mode at additional cost. Preserve provider-specific token fields, OpenRouter routing/no-fallback settings, temperature opt-in, and request-scoped schema isolation. Add captured-request tests for supported, unsupported, and unspecified capability states.

`GET /v1/providers` already discloses configuration source and credential validity without returning the credential reference. Extend diagnostics only after checking the route's actual audience. An optional operator-defined alias plus source kind (`env`, `file`, `none`) is sufficient; environment-variable names, filesystem paths, key suffixes, and hashes of secret material are not safe substitutes. Audit provider error details and logs as well as success DTOs, because resolution failures and upstream response excerpts can carry metadata. Use synthetic sentinel values to test redaction on REST/gRPC and logs. Do not inspect real credentials for this work.

### 4.4 P14: optional money, after evidence coverage — R16

[`reports_api.rs::op_cost`](../src/munarium-server/src/reports_api.rs) aggregates `session_turns.completion`. Its current turn counts and provider/model totals must retain their meaning. Broadening that same aggregation to all helper calls would change the metric even if the route and field names stayed unchanged.

If D2 adopts monetary reporting, add durable invocation/attempt accounting and either an explicit new report scope or separate additive fields. Track observed token usage, conservatively accounted units, unresolved attempts, and missing price/usage separately. Keep current token-only behavior available without price configuration.

Use immutable, dated price snapshots keyed by provider, effective endpoint/route, model, currency, and all applicable categories. Input/output/cache/reasoning/context rates need explicit semantics; reasoning tokens may be included within output and must not be charged twice. A missing or expired rate is unknown, not zero. A local endpoint is not automatically a zero-cost provider. Never sum different currencies into one scalar.

Evaluate price validity at the invocation's submission time and pin that snapshot. Expiry after submission does not invalidate historically covered work or prevent later usage reconciliation against its recorded tariff. A request submitted outside the validity window remains unpriced until explicit evidence establishes an applicable rate.

Compute integer micro-unit amounts using checked wide intermediates and a specified ceiling rule. Persist the price snapshot identity used for each calculation; do not recalculate old invoices with today's catalog. Corrections append a new accounting observation. Return coverage counts so a known subtotal cannot be mistaken for the entire tenant bill.

Acceptance requires explicit-zero prices, unknown categories, expired prices, mixed currencies, overflow, fractional-rate rounding, overlapping token categories, duplicated observations, late reconciliation, and legacy records. Use fictional tariffs. This work does not require live price lookups or a paid qualification run.

## 5. Validation receipts and documentation

### 5.1 P02: execution outcome is a contract — R02 and R24

**Change locations:** [test.ps1](../test.ps1), [gates.ps1](../gates.ps1), database test prerequisites, and a proposed small helper/catalog under `server/tools/`. Keep existing command-line switches working.

The offline tier intentionally does not exercise a database. Several integration tests return early when the test database URL is unset; that is not PostgreSQL coverage even if Cargo reports successful test functions. Conversely, an explicitly requested PostgreSQL tier already fails when setup fails. Preserve this distinction. `gates.ps1` also allows a missing optional `cargo-deny` command and later prints a broad success banner; scope that summary to the checks that actually ran.

Define a selected profile as a nonempty set of stable required step IDs. Each step records `passed`, `failed`, or `not_run`, plus a reason code and prerequisite details. Coverage outside the selected profile is `not_requested`, or equivalent metadata, rather than an incomplete required step. A successful offline run must say that its offline requirements passed and that database coverage was not requested.

Use exit precedence: failure → 1; otherwise any required step unavailable/incomplete → 3; otherwise all selected requirements passed → 0. Preserve a native interruption code in the receipt even if the wrapper uses this summary code. Invalid invocation, malformed output, a configured-but-unreachable database, and unexpected process termination are not prerequisite absences to excuse as success.

A versioned JSON receipt should include:

- Run ID, selected profile and required steps, start/end timestamps, tool identities, and duration.
- Source commit plus pre/post hashes of relevant tracked contents and explicitly selected safe untracked inputs. HEAD and a dirty flag alone do not identify tested bytes.
- Step IDs, prerequisite resolution, sanitized executable/argument arrays, exit code, outcome, and reason.
- Completion marker, source-change status, and references to sanitized logs where useful.
- Separate waiver/known-gap references; they never overwrite a step's actual result.

Hash only the intended input manifest; do not scan ignored secrets, arbitrary scratch, or another person's untracked data. Exclude receipts/build outputs from their own input hash. Command receipts must redact test database credentials and secret-bearing parameters and must not dump environment variables. Use existing ignored `server/scratch/` for raw local records, with a separate reviewed evidence export when durable public results are needed.

Write an initial incomplete receipt, then atomically update completed steps, and write the terminal marker last. `finally` is useful for cleanup but cannot run after forced termination. Readers must reject an incomplete receipt even if the last completed step passed. Prefer structured process arguments over shell-expression strings; inspect every native exit code before later parsing commands can overwrite it.

Track process/container/database ownership by this run. The current harness has fixed resources and logic for stopping matching listeners; replace broad executable-name/port cleanup with recorded child handles or run-specific identities before reusing it for fault testing. Restore original environment values in `finally` rather than deleting caller variables unconditionally. A failing test must not stop an unrelated development server.

**Checker tests:** use fake executables and tiny fixtures for a successful run, a native nonzero exit, PowerShell exception, missing tool, absent database, configured database failure, failed command followed by successful parsing, interruption, stale receipt, input mutation, duplicate/missing step IDs, empty result sets, malformed JSON, and cleanup ownership. Include known-good and known-bad semantic result bodies: HTTP 200 with `failed: 1` must not pass a verifier. The checker's assertions should concern outcomes and coverage identities, not a fixed incidental number of repository files.

### 5.2 P16: one definition of required checks — R23

The source currently duplicates gate commands and boundary rules between PowerShell and [Server CI](../../.github/workflows/server-ci.yml). Differences include the local/CI `cargo-deny` feature selection, Matrix publisher self-test coverage, and recursive versus top-level retrieval-boundary scanning.

Introduce a declarative catalog of stable step IDs with commands, working directories, prerequisites, features, dependencies, and platform adapters. Keep platform-specific database/process orchestration explicit. A shared catalog must not force Linux CI to imitate a local Windows process manager or remove independent CI jobs.

First make local execution consume it and test it. Then compare the old CI step inventory with the generated/consumed inventory by **identity, features, dependency closure, and required status**. Only after equivalence is demonstrated should a maintainer-owned PR make CI consume it. Preserve automatic triggers, path coverage, default/all-features builds, warnings-as-errors, dependency/license/security checks, contract publishers, infrastructure validation, runner assignments, and release permissions. Do not achieve equivalence by weakening the old inventory.

Boundary checks must recursively inspect the intended source tree and fail when `cargo tree` fails; an empty dependency result is not proof of a clean graph. Add fixtures that intentionally violate each boundary and that fail dependency resolution. Matrix can adopt the receipt format later without depending on Server runtime crates or adopting Server-specific step semantics.

### 5.3 P04: fix verified current documentation, preserve history — R03

| Source recommendation | Recheck | Proposed change |
|---|---|---|
| `-Enterprise` tier | Current instructions name it, but `test.ps1` exposes `-Platform` | Correct current command examples in the relevant READMEs/guidance. Consider an alias separately if callers need it |
| Matrix's Server version | Its current-release text names an older Server version | Correct the current-release field; examine a runnable example's pinned version separately rather than bulk replacing it |
| Provider inventory | Architecture has a three-provider table and outdated extension wording | Add the actual fourth provider and describe its configured HTTP endpoint accurately |
| Container architecture | Architecture describes only x86_64 | Align image support with release/build definitions; distinguish image build support from native runtime qualification |
| Crate count | Developer guide already distinguishes the current workspace from a historical transcript | Preserve the dated transcript; generate or validate only an explicitly current inventory |
| Streaming parity | Historical gap text predates the native `ServerApiService` path | Add a dated resolution with the distinction between that API and the older typed session service |

Reuse [client compatibility validation](../../clients/check_compatibility.py) for its existing package/version checks. If current-doc metadata is added, use an allowlisted set of current-version fields and independent component versions. Do not flag every semver string or historical measured example.

Clarify Ollama wording: it is an HTTP provider transport to the operator-configured endpoint. It may be used locally, but does not make Server an embedded inference runtime or prove an installation is offline.

For R04/R05/R27/R33, propose maintainer-owned policy changes separately: required limitations, behavior-oriented PR boundaries, a review-size trigger with a split/rationale option, continued automatic CI, and a waiver record that retains the original gate outcome. Keep `AGENTS.md` and `CLAUDE.md` synchronized and check byte identity. Do not edit protected templates or contributor policy as an incidental part of a runtime fix.

Make the testing principle explicit in that follow-up: fix the root cause, preserve meaningful assertions, and check the answer's semantics as well as transport success. Never weaken a test or relabel missing coverage merely to obtain a green result.

## 6. Persistence and JSON feature compatibility

### 6.1 P03: reproduce actual storage paths first — R09

**Change locations:** [core types](../src/munarium-core/src/types.rs), [storage types](../src/munarium-core/src/storage.rs), [API types](../src/munarium-api-types/src/lib.rs), [PostgreSQL implementation](../src/munarium-store-pg/src/lib.rs), [datastore model](../src/munarium-datastore/src/model.rs), [canonical serialization](../src/munarium-datastore/src/canonical.rs), and [datastore round-trip tests](../src/munarium-datastore/tests/round_trip.rs).

There is no demonstrated upstream corruption in the reviewed evidence. Create small fictional fixtures using the real typed entry points and persistence formats before proposing a decoder or library change.

The test matrix must distinguish these surfaces:

| Surface | Actual contract to exercise | Equality/rejection rule |
|---|---|---|
| Ledger evidence and version metadata | DTO → validated input → PostgreSQL JSONB → new store instance → read | Supported JSON semantic values preserved; object key order/whitespace need not survive JSONB |
| Runbook results and persisted session/answer evidence | Actual result writer and reader, including stored wrapper enums | Supported values and discriminator meaning preserved through reopen |
| REST DTO and native gRPC API bytes | Decode/encode the actual transport DTO and envelope | Type and precision policy preserved; no lossy intermediary conversion |
| Datastore manifest/build spec | Build → canonicalize → seal → reopen → verify | Existing typed parameters and digest rules preserved; unsupported values rejected |
| Datastore records | Source record/chunk → artifact → reopen/query | Text and string metadata preserved as promised, with citation identity intact |

Datastore `BuildSpec`/`ArtifactManifest` are strongly typed, parameter values are restricted, and canonical serialization rejects floats. Chunk metadata is a string map. Therefore a test demanding arbitrary exact decimal JSON in every manifest would contradict the current format. Use arbitrary JSON fixtures only where the API actually accepts it; test explicit rejection at typed boundaries.

Include a literal `$serde_json::private::Number` key alone, nested, and alongside ordinary keys, with both numeric-looking and nonnumeric string values. Cover supported integer boundaries, arrays/objects, null, large/deep inputs within limits, and decimals under a written precision policy. Malformed or unsupported values must fail before a successful write acknowledgment. Where precision cannot be promised, prefer a clearly declared range or exact string type over silent rounding.

Run separate default-feature and feature-enabled test invocations that force `serde_json/arbitrary_precision` through a qualification-only feature or isolated consumer fixture. Capture `cargo tree -e features` for each selected graph; Cargo feature unification can otherwise make both invocations test the same decoder. Test an artifact written in each supported configuration reopening in the other when cross-configuration compatibility is promised. Use independent processes/new connections so the test cannot pass by reading cached values.

Keep serialization/digest goldens for formats that promise byte stability, and semantic comparisons for JSONB. Do not mechanically normalize stored JSON, rewrite existing artifacts, or change hashes to make a fixture pass. If a defect reproduces, narrow it to the exact serializer/enum/feature path, implement the smallest fix, and add an upgrade fixture containing pre-fix supported data. A dependency update, custom parser, new format revision, or write-time rejection each needs an explicit compatibility rationale.

**Exit:** every intended path either preserves its supported inputs or rejects unsupported ones before acknowledgment in both configurations. A downstream example that cannot be reproduced remains an investigation result, not a Server defect marked fixed.

## 7. Deterministic governance and value comparison

### 7.1 P06: inject only the nondeterminism actually owned — R07

**Implemented baseline:** see [deterministic governance baseline](governance-baseline.md) for constructor compatibility, clock rules, replay tests, measured stages and the local release observations. This adds measurement seams; it does not optimize or change the gates.

**Change locations:** [memory store](../src/munarium-store-mem/src/lib.rs), its budget store, and the existing core/store conformance fixtures.

`MemStore` generates IDs for versions, claims, anchors, and promises. `MemBudgetStore` also owns UTC day/time and reservation IDs. Other abstractions already receive timestamps from callers; do not add a second clock to them.

Add explicit dependency constructors while retaining `new()` and `Default()` with production clocks/IDs. Keep injected traits or closures `Send + Sync` where the stores require them. A small clock/ID interface should not pull a runtime, database, or provider dependency into core. Preserve the existing default UUID format and uniqueness semantics.

Use a fake clock to exercise just-before/after UTC midnight, expiry boundaries, stale-reservation age, and settlement after rollover. Separate wall-clock calendar behavior from elapsed durations where appropriate. Test backward clock changes against the documented rule rather than silently assuming monotonic UTC. Deterministic ID generation is for controlled fixtures, not an alternative production uniqueness strategy.

Use fixed IDs/times/order to reproduce a failing trace. Stable digests additionally require deterministic ordering and canonical serialization; clock injection alone does not make concurrently scheduled writes replay-identical. Keep existing ID-bearing tests and the cross-backend conformance suite as controls.

### 7.2 P06: profile the gate, snapshot, and adapter separately — R10

**Change locations:** [gates](../src/munarium-core/src/gates.rs), `storage.rs::load_snapshot`, memory-store `State::head_of`/`lineage_claims`, and [the shared append service](../src/munarium-server/src/service.rs).

Construct deterministic synthetic corpora at increasing sizes with named ratios of subjects, anchors, corrections, disputes, and lineage depth. Run at least two distinct workloads: one candidate evaluated against an already constructed snapshot, and N writes into a growing history. Add snapshot loading/resolution and serialization as separate measured stages. Record release profile, compiler/dependency identity, corpus hash, hardware, repetition count, allocations or memory where available, and concurrency.

The memory store has scan/clone work around lineage state; the PostgreSQL Server uses a different persistence path. Neither a slow downstream replay nor a fast in-memory gate proves Server startup behavior. Publish the separated result before deciding between an index, cached snapshot, compact projection, or no change.

If profiling justifies a lookup index, prefer immutable per-snapshot indexes for `(subject, key)` or the existing scope key, built once and reused, with deterministic iteration. If snapshot reconstruction dominates, explore an incremental projection keyed by tenant/version/head and policy identity. Such a projection is derived state: invalidate it on revision/policy changes, verify against canonical replay, and rebuild it on mismatch. Do not introduce a persistent projection format in the benchmark PR.

`service::append_events` already pins the observed head and re-snapshots/re-gates on conflicts. Preserve that behavior. A cached gate result cannot survive a changed head just because its candidate is unchanged. Differential tests should compare findings, ordering where contracted, disputed claims, correction behavior, and pin-visible digests against the existing implementation on generated histories.

### 7.3 P09: durable governance profiles and exact values — R08

D8 is decided: versioned governance profiles persist alongside claim history;
existing keys may adopt new semantics through an explicit child-version transition.
Rebuilding collections is acceptable and documented as an upgrade operation.

Schema 1 provides `legacy-text-v1` and `exact-string-v1` bindings. The immutable
profile has a content-derived revision; claims/anchors reference their original
memory version, PostgreSQL stamps the revision in their projections, and new
events preserve original values and profile identity. Missing legacy metadata is
the named legacy policy; unknown explicit semantics fail closed. The old pure-core
entrypoints keep legacy behavior and public struct constructors remain compatible.

Both transports use the same profile-aware append service. Governed evaluations
record the observed head, revision, actual shape content hashes, chronology input,
and findings in the same commit as the claims. A shape validation cache is keyed
by content identity, so distinct definitions cannot share a stale outcome.
Profiles are version configuration, not caller-selected labels on individual claims.

A changed child profile requires the parent's revision, expected head and reason.
Creation assesses current accepted facts against peer canon and anchors while
holding the store's write lock. The profile receipt includes this assessment;
original statuses and findings remain untouched. This is explicitly a comparison
assessment, not a full historical replay. Descendants inherit profiles. Existing
live-lineage behavior remains: operators stop ancestor writers during migration
review, and an assessment never claims coverage beyond its recorded head.

Migration 0036 is additive. Its guards prevent old writers from silently adding
legacy-folded decisions to governed versions and protect immutable policy metadata.
The existing findings API exposes companion profile/evaluation records on both
transports, with a server-owned `governance.` namespace. Exports must include those
records and version metadata; a bare legacy claim DTO is not a complete export.
Current authorization remains independent of historical policy.

The [release upgrade guide](ops/governance-policy-upgrade.md) covers reader/writer
rollout, profile adoption, transitions, assessment review, re-extraction, rebuilding
collections, publication pins, fresh sessions, retention and rollback. Retrieval
profiles remain separate. Future policy dimensions require explicit supported
schema/algorithm versions and new assessment kinds rather than silent reinterpretation.

**Local validation:** 165 core/store/shape tests passed (one ignored benchmark
not executed), two memory/PostgreSQL policy integration tests and six documentation
tests passed. REST and native gRPC each passed nine live scenarios, including the
new policy-write scenario. Relevant all-target/all-feature Clippy with warnings
denied, formatting, license, compatibility and root-link checks passed. The
private-material scan reports 350 pre-existing findings in ignored scratch files,
none outside scratch. The final compatibility pass also passed with unsupported future-profile read
checks and efficient transition assessment. A separate 0035-to-0036 migration test
preserved legacy claim values/statuses and anchors and allowed legacy writes.
No remote CI or production upgrade is claimed.

## 8. Retrieval performance and restricted filesystems

### 8.1 P07: sparse eligibility without changing retrieval meaning — R11

**Change locations:** [PostgreSQL collection search](../src/munarium-retrieval-pg/src/collections.rs), [retrieval executor](../src/munarium-retrieval/src/executor.rs), [shard](../src/munarium-datastore/src/shard.rs), [multi-collection merge](../src/munarium-retrieval/src/merge.rs), and the retrieval serving adapter.

The PostgreSQL path already constrains tenant, collection, and index before `LIMIT`, and uses vector-search settings intended to reduce filtering starvation. Datastore shards already bind a scope/version; lexical/vector legs accept bounded requests, and lexical demotion already overfetches before truncation. These are controls to characterize and preserve. There is no newly reproduced whole-scope scan or starvation defect in this review.

First add instrumentation to the existing per-leg latency record: requested candidates, candidates actually visited where the engine exposes that count, accepted/rejected counts, rejection reason, refill count, work limit, and exhaustion. If an engine does not expose visited work, label that counter unavailable; returned candidates are not a proxy for work performed. Avoid tenant, query text, or source IDs as unbounded metric labels.

Build fixtures where only a small authorized/current subset is eligible, including a relevant candidate late in an independently scored collection. Vary access levels/compartments, removals, retired generations, explicit old-version pins, demotion, and concurrent changes. Compare exact search against an independent eligible-set oracle. For ANN, record recall against exact eligible search and the configured work bound; do not promise full k-result recall under every bounded ANN search.

Start authorization fixtures at the currently supported collection boundary. Finer record-level eligibility inside a shard is a conditional new seam, not an existing per-record ACL promise. Keep those fixture categories separate so the tests do not assert a capability the product never declared.

If a demonstrated gap requires finer eligibility inside a shard, add an engine-neutral eligibility snapshot or predicate/bitmap abstraction keyed by tenant, scope, generation, and relevant policy revision. Its representation must not make datastore depend on Server authorization types. Push supported filters into collection where possible; otherwise use bounded refill with deduplication, stable tie-breaking, and explicit stop reasons. A continuation must retain the same pinned generation; it must not silently mix revisions across batches.

Recheck current authorization at the serving boundary according to the existing access/revocation contract. A cache key must include the facts that define eligibility, and invalidation must cover revocation/removal. A stale authorization snapshot cannot be justified by an old content pin. When too few eligible records exist or bounded search exhausts its allowance, report the reason through existing diagnostics or a compatible extension; do not pad results with forbidden content.

Keep multi-collection RRF behavior and per-index BM25 scales intact. Reuse the existing late-file/starvation regression in `merge.rs`. Preserve datastore mode's no-silent-PostgreSQL-fallback behavior; a rollout comparison is not permission to change the serving engine after an error.

**Rollout:** add metrics first, run shadow comparisons on fictional/test data, then enable the measured optimization through the existing retrieval selector or a narrowly scoped configuration. Compare result identities, eligibility, provenance, p95, and resource work. Roll back on any authorization mismatch, incorrect pin, or unexplained quality loss, even if latency improves. Retain the previous artifact format and engine option until the new path qualifies.

#### P07 characterization slice

The first slice adds internal candidate/work diagnostics alongside datastore
`PhaseLatency`, and opt-in debug events under `munarium_retrieval::work` for
PostgreSQL and datastore collection searches. It adds no diagnostic endpoint or
new metric labels. Records contain counts, engine/leg categories and timings;
no tenant, query, collection, source, or chunk identities are emitted by these
events. D7 still governs any wider diagnostic access.

`requested` is the caller's candidate count; `candidate_limit` includes lexical
demotion overfetch; `fetched` is the returned engine pool; `accepted` and
`rejected` describe adapter truncation only (`rank_cutoff`). They do not measure
engine-internal rejection or authorization decisions. Refill remains zero.
Flat vectors report a full scan and its corpus-size work bound, including the
existing scan at a zero result limit. DiskANN reports its engine's distance
computations and effective search-list size. That list size is not a hard work
bound or a unique-node count. Tantivy/SQL visited work and engine exhaustion
remain unavailable. A short result alone does not prove exhaustion. PostgreSQL
reports adapter counts and configured `ef_search` in debug events; its existing
shadow reference timing remains aggregate and has no attached work record.

The focused fixtures are:

- [Independent exact and ANN characterization](../src/munarium-datastore/tests/retrieval_characterization.rs):
  independently computed f64 cosine ordering, sparse preselected generations,
  late nearest candidates, zero queries, zero/exact/over-limit candidate counts,
  reopen, and measured identity recall@10 at two ANN search-list sizes. These
  engine fixtures do not implement or claim record-level ACLs.
- [Artifact round trips](../src/munarium-datastore/tests/round_trip.rs):
  demotion promotes a late candidate inside the bounded overfetch pool; a
  narrower pool honestly misses it. Concurrent replacement-generation building
  leaves an already opened pin unchanged, including records absent from the new
  snapshot. This is immutable artifact behavior, not a removal-denial contract.
- [PostgreSQL collection characterization](../src/munarium-retrieval-pg/tests/collections_integration.rs):
  32 current vectors beside 1,024 retired-generation vectors, an analytic exact
  ordering oracle, explicit historical pins, and ANN recall with a verified
  HNSW plan. Exact planner controls are test-only. Scores and generation identity
  are checked even when bounded ANN misses the oracle's top ten.
- [Serving authorization characterization](../src/munarium-server/src/sessions_model_tests.rs):
  one eligible collection out of 24, independent level/compartment expectations,
  tenant isolation, old/current generation provenance, and a second connection's
  policy change followed by the next serving-boundary eligibility check. Removed
  collections remain ineligible. This does not establish an atomic authorization
  snapshot across an in-flight policy change.
- [Mirror execution](../src/munarium-retrieval/tests/mirror_integration.rs)
  checks that candidate diagnostics reach the execution latency record. Existing
  merge late-file/starvation and serving no-fallback regressions remain in place.

Run the focused characterization with an isolated PostgreSQL test URL:

```powershell
cargo test --locked --offline -p munarium-datastore --features vector-diskann --test retrieval_characterization --test round_trip -- --nocapture
cargo test --locked --offline -p munarium-retrieval-pg --test collections_integration -- --nocapture
cargo test --locked --offline -p munarium-retrieval --lib --test mirror_integration
cargo test --locked --offline -p munarium-server sparse_collection_eligibility -- --nocapture
```

The PostgreSQL fixtures report `NOT RUN` when their database is unavailable;
the test runner's success in that case is not database evidence. Synthetic recall
is characterization, not a corpus-wide quality guarantee or calibrated latency
SLO. The slice preserves ranking, generation selection, existing overfetch/search
settings, artifact format, and engine routing. No retrieval fix or new eligibility
seam follows merely from the plan; a separately demonstrated gap must scope one.
Rollback removes instrumentation/tests without data migration. P08 remains the
next slice, with D3 defining application-process recovery guarantees separately
from database and power-loss recovery. P01 late reconciliation must preserve
history/original day, handle concurrent/replayed evidence idempotently, and let
excess usage reduce future admission; it remains separate from estimator and
usage-quality reporting follow-ups.

### 8.2 P12: qualify supported path permissions — R12

[`lexical.rs`](../src/munarium-datastore/src/lexical.rs) creates temporary directories during both build and open, and retains scratch ownership for the mmap lifetime. This matters to a read-only artifact deployment even though the sealed artifact itself is immutable.

Start with [the standalone round-trip fixture](../src/munarium-datastore/tests/round_trip.rs). On Linux, test the supported non-root identity with a read-only artifact mount, a distinct writable scratch/work directory, necessary ancestor traversal, and a read-only container root. Build/seal outside the read-only serving phase, then open/query/reopen under serving permissions. Test missing scratch, denied traversal, corrupt manifests, and cleanup of only test-owned files.

Run a separate Windows AppContainer fixture with explicitly documented token/ACL/ancestor-handle conditions if that support tier is adopted. A Linux container test does not qualify AppContainer, and AppContainer denial is not proof of a Linux defect. An unavailable OS environment is `not_run` under P02, not a pass inferred from ordinary Windows execution.

Only if the reproduction requires it, add optional scratch-root/build/open options preserving existing constructors and lifetimes. Wire configuration through Server's composition root with validated paths. Do not solve a path failure by broadening host filesystem access, moving data to an uncontrolled global directory, disabling the sandbox, or allowing cleanup outside the chosen root. Document the required artifact, scratch, and ancestor permissions alongside the datastore guide.

## 9. Crash recovery and command receipts

### 9.1 P08: define what a restart proves — R13

**Change locations:** `PgStore::append_claims`, [REST command wrapper](../src/munarium-server/src/rest.rs), [gRPC command wrapper](../src/munarium-server/src/grpc.rs), [state/receipt persistence](../src/munarium-server/src/state.rs), [runbook executor](../src/munarium-server/src/runbooks_api.rs), [mirror lifecycle](../src/munarium-retrieval/src/mirror.rs), datastore jobs/builds, and the conformance runner.

Existing mirror `BuildPhase`/`FaultHook` hooks and [mirror integration tests](../src/munarium-retrieval/tests/mirror_integration.rs) already cover injected errors and reconciliation. The new capability is deterministic **process death and reopened-state verification**, reusing those phase names where possible.

Characterize existing guarantees before strengthening them:

| Boundary | Current behavior to characterize | Required recovery assertion |
|---|---|---|
| Ledger transaction | Append is transactional and head-checked | No partial batch; committed acknowledged claims survive; pins and supersession remain coherent |
| Command versus replay receipt | `with_idempotency` executes before `idem_store`; insertion is separate and can warn on failure | A durable receipt replays its result; the gap without one is explicitly characterized, not assumed impossible |
| Runbook step versus transition event | `set_step` now commits checkpoint and required transition together | All-or-neither persistence; legacy missing events remain unknown |
| Runbook lock and resume | Advisory connection releases on death; execution resumes explicitly from a non-done step | No implied automatic resume; approval gates survive and cannot be bypassed |
| Artifact publication | Files, manifest, catalog, and binding have separate phases and reconciliation | Never serve a partial/unverified active generation; interrupted work resolves or remains clearly unavailable |
| External provider submission | Remote work is not in the database transaction | Ambiguous submission retains liability and cannot be silently interpreted as an unsubmitted call |

Add a qualification-only barrier observer and small child fixture. The parent starts a child on a run-owned database/tenant and loopback ports, waits for a precise marker at a named phase, terminates only that child, verifies it exited, restarts, and reads state through supported interfaces plus a narrowly scoped independent oracle. Bound every wait and retain the failing seed/phase. A thrown Rust error or cooperative task cancellation does not substitute for process termination.

Start with ledger before-commit, after-commit, and before-reply barriers. Then cover after-command/before-receipt, receipt persisted/before reply, runbook effect/checkpoint boundaries, and mirror claim/export/seal/catalog/upload/manifest/binding phases. Include a no-crash control proving the barrier harness itself can complete. Fault controls must be absent from production artifacts and must not be reachable through an HTTP endpoint or ordinary configuration. Account for `--all-features`: do not accidentally package a qualification fixture into a production image merely because a broad feature build succeeds.

Assert uniqueness and monotonic ordering under the actual sequence contract, not gapless allocation. Check final ledger/head state with an independent replay oracle, complete batches, acknowledged data, stable pinned reads, disputed/corrected claims, and original durable receipt responses. Reuse [cluster scenarios](../conformance/src/cluster.rs) for fixed-seed concurrent appends and interleavings across two instances.

PostgreSQL remains running during an application-process kill. This does not qualify database restart, volume exhaustion, torn writes, host power loss, or backup restore. Define those as separate fault classes with their own environments and evidence. Memory mode is not required to survive process restart unless a new persistence contract is deliberately added.

#### First P08 ledger fixture (merged in PR #52)

The [child-process fixture](../src/munarium-store-pg/src/crash_recovery.rs)
exercises the actual `PgStore::append_claims` transaction at `before_commit`
and `after_commit`, then `before_reply` in the fixture caller. The latter is a
local acknowledgement boundary, not an HTTP/gRPC transport qualification.
Each phase runs once with release/completion and once with forced child death.
A fresh process reconnects, checks whole batches and previously acknowledged
claims, verifies corrections/disputed rows and the original pin, compares every
claim to its ledger event and allocation head, and appends using the recovered
head. PostgreSQL runs continuously. Sequence assertions require unique ordered
rows, not gapless global identity allocation.

The fixture and hooks are guarded by `cfg(test)`, with no Cargo feature, endpoint,
or shipping configuration. Even `--all-features` library/server builds omit them.
Markers use a unique run directory; waits have 30-second bounds. The parent
owns child handles and retains markers on failure. Offline controls check marker
timeout and early exit as distinct failures and successful barrier release.
Use a disposable database: unique tenant rows remain for inspection until that
database is removed. No broad database or process cleanup is performed.

From `server/`, with `MUNARIUM_TEST_DATABASE_URL` set to that disposable database:

```powershell
cargo test --locked --offline -p munarium-store-pg --lib crash_recovery -- --nocapture
```

Without the URL, the ledger test reports `UNAVAILABLE`; a green Cargo result
then covers only offline controls. The child entry is intentionally ignored and
launched by its parent. Existing database-enabled workspace CI runs the parent
without a workflow change. This slice does not change recovery semantics or
claim completion of P08. The broader fixtures below extend command/receipt,
runbook and publication coverage. Two-pool seeded append qualification is added below; stronger command/effect
recovery protocols remain separate work.
Any demonstrated defect needs an explicitly reviewed recovery contract before
a stronger guarantee is implemented. Database crashes, power loss and backup
restore remain separate qualifications under D3.

#### Broader P08 process-crash characterization

The [shared process harness](../tests/support/process_crash.rs) starts separate
setup, writer, observer and recovery processes. It waits for an atomically
published named marker, observes the still-live writer from another process,
then either releases the no-crash control or terminates only the owned child.
Every child and marker wait has a 45-second bound. Each run owns a unique tenant
and local directory; failure retains the directory and prints its identity.
Successful runs remove only their own local files. Use a disposable database:
fixture tenants remain until that database is removed. PostgreSQL stays running.

| Fixture | Cases and checks | Characterized limit |
|---|---|---|
| [Command receipts](../src/munarium-server/src/crash_recovery.rs) | Eight REST/typed-gRPC scenarios across command-completed and receipt-persisted barriers, each with a no-crash control; real authenticated loopback calls, original response replay, request/plane mismatch, tenant scoping and independent version-row counts | Killing after the command but before its receipt leaves a committed version with no receipt. Retrying creates a second version. Killing after the receipt preserves the original encoded response and retry creates no extra version. No exactly-once claim follows. |
| [Runbook checkpoints](../src/munarium-server/src/runbook_crash_tests.rs) | 44 scenarios: effect, uncommitted event/checkpoint and committed boundaries for buildIndex, verify, cutover and retireOld; approval gate and approval-to-running boundaries; live advisory-lock exclusion, release on death, explicit re-entry | New checkpoints and required events commit together. The old split-write gap remains a legacy-data fixture. A cutover effect can already be active while the checkpoint remains running. No automatic resume or invented transition repair. |
| [Artifact publication](../src/munarium-retrieval/tests/process_recovery.rs) | 28 scenarios across all seven existing BuildPhase markers, with controls and same-node/replacement-node recovery; second-process lease exclusion, manifest-last visibility, catalog/binding checks, opened artifact contents and the unchanged previous serving artifact | Same-node sealed publication resumes only while its lease is fresh and staging exists. A replacement node leaves a live owner alone, then abandons its sealed attempt after lease expiry. Before-catalog running attempts expire; expiration is not evidence of publication or staging cleanup. |

Lease expiry is advanced with tenant/version-scoped test SQL **after** the writer
has exited; the test does not wait on wall-clock lease expiry or change production
lease duration. Artifact fixtures use local disk, not a cloud object store. Both
old serving and any new staged binding must reference verified, openable content;
mirror recovery never promotes the new generation to serving. Orphaned/unbound
content is not treated as serving success. Reconciliation is checked twice to
establish a stable second pass.

Runbook tests invoke the existing private executor explicitly after restart to
characterize re-entry. They add no public resume endpoint or scheduler. The
fixture is a v1 shape-scoped runbook; v2 collection ordering and external effects
are not qualified by it. Command fixtures cover the REST command router and
typed CommandService, not the entire ServerApiService surface, ingress stack,
receipt TTL sweep, or simultaneous duplicate command races. Server barriers are
`cfg(test)` only; artifact barriers use the existing programmatic FaultHook from
an integration-test executable. No fault endpoint, Cargo feature, or production
environment switch is introduced, including under `--all-features`.

From `server/`, with an isolated `MUNARIUM_TEST_DATABASE_URL`:

```powershell
cargo test --locked --offline -p munarium-server process_recovery -- --nocapture
cargo test --locked --offline -p munarium-retrieval --test process_recovery -- --nocapture
```

These parents participate in existing database-enabled workspace CI. Without a
database they print `UNAVAILABLE`, which is not recovery evidence. Child entries
are intentionally ignored by the ordinary runner and invoked only by parents.

The command gap matches the published post-completion retry contract. The
runbook checkpoint/history gap discovered in PR #53 is addressed by the atomic
implementation below. P01 and D1/D7 remain separate.

#### Runbook recovery contract and implementation (2026-09-24)

**Decision:** commit a step checkpoint and its required ledger transition in one
PostgreSQL transaction. Missing historical events are not reconstructed. The
implementation uses `PgStore::append_claims_uncommitted` to retain the existing
append validation and lineage lock, writes the guarded checkpoint in that same
transaction, and commits once. No schema migration or backfill is introduced.

The transaction covers the tenant/run/ordinal checkpoint state, its effective
detail, and, when the run names a version, the corresponding claim, ledger event
and lineage-head update. Preserve the existing transition payload and the current
`detail = COALESCE(new_detail, detail)` checkpoint semantics; a transition with no
new detail must not erase previously persisted step results. Runs without a
version retain checkpoint-only behavior: absence of a ledger transition is
expected there, not evidence of corruption.

The PostgreSQL transaction-aware append boundary preserves ordinary
claim validation, sequence allocation and lineage serialization. Ledger SQL stays
in the PostgreSQL store; core remains SQLx-free. The guarded update checks tenant,
run, ordinal, step identity and version association before commit. A mismatch
rolls back the entire transaction, including any uncommitted event and head update.
Lock order is run advisory lock, lineage head, checkpoint row. Approval validates
its state while holding the run lock, so a stale retry cannot launch another
executor. Transition-success metrics are emitted only after commit.

| Interruption | Required durable result and recovery |
|---|---|
| Before the checkpoint transaction commits, including after its UPDATE or ledger INSERT | Neither new checkpoint nor new transition survives. Retain the previous checkpoint and history. |
| After commit, before acknowledgement or executor continuation | Both survive. Reopen reads the committed checkpoint; a done step remains skipped. A lost response is not permission to replay its effect. |
| After a step effect, before the checkpoint transaction | The effect may exist while the checkpoint is still running. Atomic checkpoint/history writes do not close this separate window. |
| During approval | Approval checkpoint and its required transition commit together or neither does. An uncommitted approval cannot authorize cutover. A committed approval can be used only through the existing authorized continuation path. |

**Effect recovery stays explicit.** Reopening a process does not advance a run or
start a scheduler. The existing advisory lock must still exclude another executor
and release on process death. Build, verify, cutover and retirement retain their
individual re-entry rules; this decision does not make their effects exactly once.
Before any stronger resume guarantee, qualify each step's persisted outputs and
preconditions, including a cutover already active while its checkpoint is running.
Do not infer successful completion from death, lock release, a missing event, or
the mere existence of an artifact. Ambiguous effects require reconciliation or
operator investigation rather than blind replay. Approval and artifact-verification
requirements continue to apply on every supported re-entry path.

**Existing gaps stay historical gaps.** Preserve old checkpoints, details and all
existing append-only events. Never backdate or synthesize the missing original
transition, replay a done step to produce an event, or reset its checkpoint to make
history appear complete. A legacy done checkpoint remains the executor's progress
record; it does not prove that the corresponding historical event was recorded.
A current checkpoint also cannot reconstruct every earlier state or approval.
Treat completeness for pre-contract history as unknown unless independently
established; absence of a version means ledger history was not required at all.
Any later diagnostic or operator reconciliation record must identify itself as a
present observation, distinguish observations from inference, and leave the original
gap visible. Such tooling and its wire representation are separate work.

**Compatibility and rollout:** retain current state names, JSON detail behavior,
REST/gRPC responses and explicit continuation semantics. No backfill or schema
rewrite is needed merely to share the transaction. Old runs may receive future
atomic transitions without certifying their earlier history. The guarantee begins
only after every writer of checkpoints and approvals uses the shared boundary;
drain older writers before advertising it. Mixed-version operation and rollback
to an older writer can reopen the gap and therefore suspend the guarantee, even
if old readers remain compatible. Do not delete events during rollback. If the
implementation needs protocol metadata, design an additive migration and its
reader compatibility before adding it.

**Qualification:** the child-process tests include controls and kills after
uncommitted event/head writes, after the checkpoint UPDATE and after commit.
Independently reopen checkpoint/detail, claim/event and lineage head; require
all-or-neither persistence, valid sequence ordering and unchanged acknowledged
history. Retain effect-before-checkpoint cases for buildIndex, verify, cutover and
retireOld, and approval cases before/after commit. Exercise append failure rollback,
missing/wrong-tenant steps, concurrent executor/approval attempts, repeated approval,
no-version runs, detail preservation, and old done checkpoints with absent events.
Legacy gaps must remain unchanged after repeated reopen/re-entry. Check supported
old-reader and mixed-writer behavior and document rollback limits. Qualification
continues to mean application-process death with PostgreSQL running; database
crashes, power loss, backup restore, v2 ordering and external effects need their own
evidence. The two additional server regressions cover identity/error rollback, JSON detail
preservation, no-version runs, unchanged DTO decoding, legacy gaps after repeated
re-entry, simulated old-writer behavior, and competing approvals across independent
pools. The PostgreSQL `recovery_two_pools_seeded_batches` test uses fixed seeds 8
and 24301, racing head-checked batches and checking acknowledged claims against
ledger events after reconnecting. Existing cluster conformance qualifies separate
server processes. An old binary is not executed by the simulated old-writer test.

The supported P08 scope is application-process recovery for the ledger,
REST/typed-gRPC receipts, v1 runbook lifecycle and local artifact publication.
Exactly-once command/effect execution, automatic resume, database/host failure,
v2 crash ordering, cloud artifact stores and remote provider submissions are
separate qualification or contract work; none is claimed by these fixtures.

#### P08 local qualification record

The D3 application-process scope is implemented and locally qualified on Windows
with a disposable PostgreSQL/pgvector 16 container continuously running. The
container and live server processes were removed after validation. No remote CI,
merge, release, deployment or database/power-loss qualification is implied.

Commands below run from `server/`, with `MUNARIUM_TEST_DATABASE_URL` targeting the
isolated database where applicable:

| Check | Result |
|---|---|
| `cargo test --locked --offline -p munarium-server process_recovery -- --nocapture` | 4 parents/regressions passed: 44 runbook crash/control scenarios, 8 receipt scenarios, legacy/rollback and competing-approval regressions |
| `cargo test --locked --offline -p munarium-store-pg` | 55 passed; ledger parent covers 6 crash/control scenarios, including its explicitly invoked child; seeded two-pool batches passed |
| `cargo test --locked --offline -p munarium-retrieval --test process_recovery -- --nocapture` | Artifact parent passed all 28 same-node/replacement-node scenarios; ignored child is invoked by the parent |
| `cargo test --locked --offline -p munarium-server -- --skip process_recovery` | 212 passed, 6 ignored in this invocation; includes the 6 documentation-coverage tests |
| Existing `runbooks_api::json_persistence_tests::json_feature_pg_runbook_result_reopens`, invoked explicitly with `--ignored` in the compiled server test executable | 1 passed; unchanged JSON persistence fixture remains compatible |
| `cargo clippy --locked --offline -p munarium-store-pg -p munarium-server --all-targets --all-features -- -D warnings` | Passed |
| `cargo build --locked --offline -p munarium-server -p mmp-conformance`; existing `Invoke-ValidationLiveTier` helpers for PostgreSQL blackbox/platform/cluster | Passed: 8 REST + 8 gRPC + 10 platform + 5 cluster scenarios; owned servers cleaned up |
| `cargo fmt --all --check`; repository license, client compatibility, root-link and whitespace checks | Passed |
| `scripts/private_material_scan.py` | Failed on 350 pre-existing local scratch findings; none outside scratch. Not waived or reported as passed |

The first competing-approval test run exposed missing source setup in its new
fixture; after adding fictional source content, the complete server recovery run
passed. No production behavior was weakened to satisfy the test. The unchanged
ignored child entries are exercised by their parents; other unselected ignored
suites are not claimed as executed. These local results supplement automatic CI.

### 9.2 Strengthen command recovery only where the contract supports it

The current [REST retry documentation](api/rest.md) describes post-completion receipt behavior. A fault test exposing that window is characterization, not permission to silently impose a new contract across every command.

For a stronger database-only command guarantee, design a durable operation identity and states such as pending/completed/unknown, with tenant/operation/hash binding, concurrent claim ownership, and a recoverable lease/fence if needed. Persist the ledger mutation and its replay result in the same PostgreSQL transaction where feasible. This requires an explicit transaction-aware store boundary; a handler-level mutex would not solve cross-replica recovery. Keep core free of SQLx.

For workflows spanning a provider, object store, and database, persist enough intent before submission to recognize ambiguity. Resume/reconcile rather than blindly resubmit. Provider idempotency can help only where its actual contract supports it; the local receipt alone cannot provide exactly-once remote execution. Specify what a client receives for an in-progress or unknown operation and how an authorized retry becomes a new attempt.

Retain existing request-hash mismatch behavior, tenant isolation, REST/gRPC plane distinctions, stored-response encoding, and TTL semantics. Do not expire the only receipt for an unresolved operation solely because a successful-response replay TTL elapsed. Migrate old completed receipts readably; do not reinterpret them as durable pre-dispatch evidence. Old writers that bypass a new in-flight claim protocol must be drained or excluded before the stronger guarantee is advertised.

For runbooks, §9.1 implements the atomic checkpoint/transition contract. Preserve legacy history gaps rather than reconstructing unobserved events.
Test build-index re-entry, verification, approval, cutover, and retirement
individually. Do not “recover” a step by marking it done merely because its process
died. Keep explicit resume semantics until a separately designed scheduler is adopted.

For datastore publication, reuse existing reconciliation and jobs rather than add a second recovery daemon. Verify orphan handling, manifest-last publication, binding validation, and previous-generation availability after crashes. Preserve old artifacts until pins and retention permit cleanup.

**Release/rollback gate:** test old-data/new-reader, new-data/new-reader, supported old-reader behavior, interruption during migration/backfill, and mixed-version writers. Prefer reader-first additive migrations. If an older binary cannot safely operate after the new protocol is enabled, specify a roll-forward or compatible-reader rollback; never rely on dropping tables or rewriting the ledger.

## 10. Authority, evidence, and retention

### 10.1 P10: audit authority construction, preserve working controls — R19

**Implemented:** the [authority audit](authority-audit.md) records each constructor,
transport, static role, session snapshot and retained worker context. Production
JWT conversion was already centralized after verification. `Principal::access_ctx`
now also binds the uid for direct service callers that omit capture middleware;
public context constructors and deserializable claims remain compatible. REST,
typed-service and native gRPC regressions check denials and adjacent valid calls;
query/original-reference tests observe the provider and evidence tests observe
durable registration. Existing session snapshot and transcript semantics are
documented explicitly, without claiming continuous reauthorization.

[`AccessCtx`](../src/munarium-access/src/lib.rs) already lacks `Deserialize`; `state::authenticate_principal` verifies JWT claims before conversion. Public Rust fields, `From<AccessClaims>`, and `unrestricted` are trusted-code API questions, not evidence of a remotely exploitable bypass.

Inventory every production constructor and caller: REST, typed gRPC, native `ServerApiService`, static rw/ro/mgmt roles, disabled development auth, sessions, background work, and in-process tests. Record where signature, expiry, uid, revocation, scope, level, compartments, and tenant are established and rechecked. Include background tasks that retain a context beyond the originating request.

Where the audit identifies an accidental construction risk, introduce an additive verified-claims wrapper or centralized constructor used by production entry points. Preserve the deserializable JWT claims representation. Do not make fields private or remove constructors in a supposedly source-compatible library release without D5 and a migration path. Avoid compile-fail tests for a hypothetical type that the real handlers do not use.

Extend current transport tests with forged/expired claims, wrong uid, missing scope, cross-tenant IDs, revoked tokens, insufficient level/compartments, and static-role behavior. Reuse [v1.2 transport tests](../src/munarium-server/src/v12_tests.rs), including original-reference access and revoked governed-query cases. A rejection must occur before provider submission or another protected effect; assert that the scripted provider/effect observer saw no call.

### 10.2 Model evidence remains data — R20

**Implemented:** [model evidence](model-evidence.md) describes shared JSON framing
for checked answers, session passages and hierarchy/composer context. The framing
names the source role, available historical identity and citation identifier and
declares that evidence grants no execution or approval authority. Public source
IDs, DTOs and brief text remain unchanged. Complete-envelope budget handling and
adversarial fixtures qualify encoding and protected effects, not general model
instruction obedience.

[`answers_api.rs`](../src/munarium-server/src/answers_api.rs) already labels passages as data, assigns request-local passage IDs, and validates returned references/quotes. Preserve that implementation and expand the same discipline to session/composer/hierarchy prompt construction where absent.

Use a model-only evidence envelope that makes source role, historical pin, citation identifier, and lack of execution/approval authority explicit. Keep external source IDs and existing wire DTOs stable unless there is a separate contract need. Escape/render content consistently so a passage cannot impersonate another envelope field merely through formatting.

Use adversarial fictional passages and scripted completions requesting a mutation, access elevation, changed pin, or approval bypass. Validate that existing publication/approval/mutation boundaries still deny unauthorized action even if the completion contains it. Do not create a general tool executor just to demonstrate this property. Unknown citation IDs, altered quotes, wrong collection/version, and revoked source access must remain rejected.

Keep provenance/citation correctness separate from answer quality and instruction obedience. A valid quote can contain malicious instructions; a passing fixture is evidence for its tested boundary, not general prompt-injection immunity.

### 10.3 Retention contracts before cleanup changes — R21

**Implemented first slice:** the [retention inventory](ops/retention-inventory.md)
declares schema and artifact surfaces under all five modes, including content
inside JSON, archives and caches. Its checker compares migration tables/columns
and datastore component kinds and requires policies for declared non-SQL artifacts.
Negative controls exercise missing and malformed declarations. Retrieval tests
characterize replacement/rebuild, warm and reopened caches, inactive PostgreSQL
chunk retirement, immutable historical citations and shared-source rebuilding.
Existing evidence hold and claim-once tests remain in force. No new source-erasure
API, cleanup journal or restore-denial guarantee is introduced.

The current [platform guide](guides/platform-features.md) describes runbook soft removal that retains underlying history/data. [Immutable datastore records](../src/munarium-datastore/src/records.rs) preserve historical citation text. [Physical deletion](ops/index-deletion-runbook.md) is an explicit operator procedure with retained surfaces. These are deliberate contracts, not accidental omissions to eliminate wholesale.

Create a retention inventory keyed by schema/artifact kind and removal mode. For each item declare origin links, ownership, current read behavior, physical cleanup, hold/exception rules, restore handling, and tests. Include sources, collection chunks, embeddings, manifests/archives, L0/L1 caches, vocabulary generations, checked answers, session turns, interactions, ledger/evidence, exports, and backups. A table-count check alone cannot discover content stored inside a JSON column or object archive.

| Mode | Intended guarantee | Design requirement |
|---|---|---|
| Retrieval exclusion | Current eligible search no longer returns the item | Eligibility revision/invalidation; retained history may still exist |
| Access revocation | The caller can no longer read the affected content | Current authority checks across search, cached answers, originals, and historical pins |
| Logical removal | Supported APIs expose a removed/unavailable state | Durable denial/tombstone plus explicit treatment of existing sessions and references |
| Physical erasure | Declared content-bearing surfaces are removed by a deadline | Cleanup journal, shared-source ownership rules, hold handling, exports/backups policy, and restore closure |
| Protected retention | Certain audit/accounting evidence must remain | Explicit reason, minimized payload, access policy, and duration |

Do not promise the last two modes as current general-purpose source APIs. The first slice should document the inventory and extend exclusion/rebuild tests. A new erasure API is a separate contract/security design.

For a future erasure mode, commit retrieval/read denial and durable pending-cleanup work before asynchronous physical deletion. Every retry/restart must rediscover unfinished work; cleanup needs stable identities, idempotent operations, and shared-content reference protection. After restoring an older backup, reapply required denial state before serving, or explicitly state that the restore cannot yet satisfy the erasure contract. An expired pin should return a documented unavailable/removed error when the mode requires it, never resurrect content by rebuilding from retained sources.

Do not reverse the existing evidence purge ordering casually: it deletes bytes before marking the row purged so a failed delete remains retryable. Marking first without durable pending-cleanup state would lose that retry. Preserve legal holds and qualify hold-versus-purge races. Reuse existing claim-once and hold tests in [PostgreSQL integration coverage](../src/munarium-store-pg/tests/pg_integration.rs).

Test exclusion/removal through warm cache, cold open, active and retired generation, pinned session, background rebuild, restart, restored state, shared-source use by another collection, hold placement, and partial cleanup failure. Record exactly which modes and surfaces each fixture proves. A registry check should fail when a new content-bearing table/artifact lacks a declared policy, while semantic fixtures prove that the declared policy actually works.

### 10.4 P10 local qualification and remaining boundaries

Validation uses fictional fixtures, a loopback scripted provider and a disposable
PostgreSQL database. The source changes require no database migration, wire DTO
revision or constructor removal. Model context formatting changes; public brief
text/hash behavior is retained. A code rollback restores the former prompt format
and direct-service uid assumption without rewriting persisted data. The inventory
and its checks neither delete data nor establish an erasure deadline.

Commands run from `server/` unless described otherwise. The PostgreSQL commands
use `MUNARIUM_TEST_DATABASE_URL` pointing to the owned disposable database.

| Check | Result |
|---|---|
| `cargo test --locked --offline -p munarium-server authority_tests -- --nocapture` | 3 passed; REST/native bridge effect denial, direct typed service uid binding and static/development controls |
| `cargo test --locked --offline -p munarium-core -- --quiet` | 78 passed |
| `cargo test --locked --offline -p munarium-server -- --skip process_recovery --quiet` | 222 passed, including 6 documentation checks and REST/native gRPC adversarial/provider-observer scenarios; 6 existing ignored tests and 4 process-recovery tests not run. The separate registry integration test passed and executed its 15 Python controls |
| `cargo test --locked --offline -p munarium-store-pg --test pg_integration` | 27 passed, including hold placement/lifting and claim-once retention tests |
| `cargo test --locked --offline -p munarium-retrieval --test mirror_integration` | 24 passed, including current-generation exclusion, warm/cold historical retention and shared-source rebuild |
| `cargo clippy --locked --offline -p munarium-core -p munarium-retrieval -p munarium-server --all-targets --all-features -- -D warnings` | Passed |
| `cargo fmt --all -- --check`; root `git diff --check` | Passed |
| Root: `python server/tools/check_retention_inventory.py`; `python -m unittest discover -s server/tools -p test_retention_inventory.py -v` | Registry passed for 55 tables, 525 columns and 24 artifact families; 15 controls passed |
| Root: `python check_license.py`; `python clients/check_compatibility.py`; `python scripts/docs_linkcheck.py`; `python -m unittest discover -s scripts -p 'test_*.py' -q` | Passed; 7 existing checker/grader tests passed |
| Root: `python scripts/private_material_scan.py` | Failed on 262 pre-existing scratch findings; none outside scratch. Not waived or reported as passed |

Python commands used the installed Python 3.13 executable because the local `py`
launcher was unavailable; `RETENTION_PYTHON` selected it for the Rust bridge.
The initial new adversarial governance fixture sent a response-shaped body and
received request-validation failure before the role check. Using a valid request
shape established the intended authorization refusal on both transports; the
final full Server run passed. The disposable PostgreSQL container and its owned
volume were removed after validation. No live provider, release or deployment
was performed.

The registry and semantic fixtures qualify only their declared surfaces and
modes. Full application restart/backup restore after removal, concurrent
hold-versus-byte-delete, partial object-delete failure, and continuous session
reauthorization remain unqualified here. D4 still governs any stronger source
erasure or restore-denial contract. Existing automatic CI remains unchanged.

## 11. Wire compatibility

### 11.1 P11: exact integers and supported ranges — R17

Inventory integer fields across [API types](../src/munarium-api-types/src/lib.rs), [typed MMP sources](../proto/mmp/v1/), [native Server API bridge](../proto/mmp/v1/server_api.proto), PostgreSQL columns/conversions, and all SDKs. Include sequence/head/pin, token counts, reservation amounts, watermarks, and reporting totals.

Existing safeguards matter: `ServerApiService` transports JSON as UTF-8 bytes rather than a floating-point JSON structure, and the Python client already tests `9007199254740993`. Typed protobuf uses integer fields. These do not prove that every browser JSON consumer or PostgreSQL conversion is safe.

Treat two questions separately: can the transport preserve a value, and is the value within the Server/store contract? Several PostgreSQL paths cast unsigned Rust values to signed database integers. Replace unchecked casts at validated boundaries with checked conversion where needed, preserving existing error mapping or adding a documented error through the contract process. A `u64` DTO does not make all unsigned values persistable in `BIGINT`.

Test `2^53 - 1`, `2^53`, `2^53 + 1`, the signed-64 boundary, and each field's actual supported maximum. Include negative, fractional, malformed, and out-of-range requests. Use synthetic transport fixtures and an isolated test-only high-sequence fixture; do not perform enormous numbers of writes or add a production sequence-setting API.

Exercise write/read/replay, pagination/pins, error metadata, and both transports in the client matrix. Preserve existing v1 numeric JSON fields and protobuf tags. If exact browser transport needs a new representation, propose an additive exact field or a new version, define precedence/conflict handling when both are supplied, and test old/new client-server combinations. Decimal strings are an option, not an automatic patch to every numeric field.

### 11.2 Unknown-field policy by direction — R18

Write a policy table covering requests, responses, enums, extension maps, SSE events, persisted documents, and native bridge payloads. Keep current permissive behavior until an endpoint-specific compatibility review justifies tightening it. Python response models and .NET decoding already tolerate unknown fields; SSE has unknown-event/stage tests. Reuse those controls.

For new authority-sensitive request controls, validate recognized values and reject invalid combinations rather than trusting an unknown field. A global `deny_unknown_fields` would change newer-client/older-server behavior and can break extension points. For responses, preserve forward-tolerant decoding while failing closed on unknown values that would otherwise confer permission or success. Persisted formats may require stricter version handling than wire responses; do not give them one universal policy.

Extend N/N−1 fixtures with added fields, missing optional fields, null versus omission, new enum values, opaque IDs, and unknown SSE stages/events. Include completion usage metadata from P01 and any budget/report additions. A new enum variant can break generated exhaustive clients even when JSON remains syntactically valid.

Change normative DTO/proto sources and generators, then run the documented publisher/re-vendoring and SDK regeneration workflow. Never hand-edit generated clients or locked contract copies to satisfy a test. Required publisher/drift checks remain automatic. Record the source and generated artifact versions in the compatibility evidence.

## 12. Evaluation and performance evidence

### 12.1 P13: frozen inputs and independently tested grading — R24 and R26

Reuse [the public example grader](../../docs/lab/example/grade.py), [its tests](../../scripts/test_lab_example.py), and [the performance guide](guides/measuring-performance.md). Positive and negative grader controls already exist; extend them rather than create a second scoring implementation.

Add malformed/missing evidence, duplicate/extra/missing case IDs, empty results, unauthorized evidence, opaque valid IDs, unknown usage, and transport-success/semantic-failure cases. Each control should assert the intended reason, not merely a nonzero exit. Use a scripted provider for a complete zero-cost producer → persisted answer/evidence → grader → receipt run before any live campaign.

Define separate versioned records for the frozen manifest, raw outcomes, and grading result. A manifest includes source/build identities, corpus/query hashes, independent history/task IDs, split assignment, expected evidence, retrieval/runbook settings, model/settings identity where relevant, grader identity, thresholds, failure treatment, and resource limits. Freeze and commit it before the qualifying run. If criteria change, create a new manifest revision and new campaign.

Retain every planned row, including failed, abstained, interrupted, and unexecuted outcomes. A corrected grader creates a new grading record referencing the unchanged raw results; it cannot replace the original score in place. Distinguish a diagnostic rerun from preregistered qualification. Store public, sanitized results under a proposed `server/conformance/results/` index, with schemas and immutable identifiers; do not turn ignored scratch paths into publication evidence.

Extend documentation checks with an explicit measured-claim/result marker and validate referenced result identity, schema, source, and completion. Do not heuristically classify every number in a document as a benchmark. Historical records without evidence can remain explicitly historical/unrecorded, rather than gaining fabricated receipts. Test the validator with missing, malformed, duplicate, and mismatched result IDs. This extends the intent of the existing Matrix result-link discipline without claiming that its current checker validates every statistical claim.

### 12.2 Governance evaluation as a separate workstream — R25

Use independent fictional histories that contain stable facts, corrections, contradictions, disputed claims, removals, access changes, and point-in-time questions. Keep current authorization in force for historical queries. The unit of statistical independence is a history/task, not repeated execution of the same question on several backends.

Compare no memory, maintained versioned notes, a competent conventional retrieval baseline, and Munarium. Give each the same events, timestamps, access facts, model/context allowance, and resource budget. Define how each baseline updates its notes/index and count that maintenance work. Add a same-pipeline governance ablation so retrieval/prompt differences do not masquerade as governance benefit. All arms must still enforce mandatory authorization; an ablation cannot be used to expose forbidden evidence to a model.

Separate deterministic evidence eligibility/provenance from model answer quality. Score supported correctness, stale/conflicting answers, abstention, unauthorized disclosure, and citation validity separately. Record update/storage work, latency, known usage/cost, and unknown liabilities. Publish paired differences and uncertainty across independent cases; a non-significant difference is not proof of equivalence.

Run an offline pilot to assess fixture difficulty, grader behavior, variance, and practical sample needs. Then freeze the held-out set, minimum useful effect, mandatory correctness criteria, and resource thresholds under D6. Preserve a measured rejection or inconclusive result as such. Paid execution needs its own cap and available environment; neither is implied by this plan. Shared-code correctness fixes do not wait for this research result.

### 12.3 Latency budgets after calibration — R22

Reuse [the existing retrieval benchmark](../src/munarium-retrieval/tests/benchmark_baseline.rs). It compares PostgreSQL/datastore over synthetic data and includes cold/warm behavior; it is not yet an HTTP capacity benchmark or proof of a release-wide SLO.

Select three product paths with frozen workload definitions: retrieval p95 at a declared corpus/eligible fraction, ledger append under declared concurrency/history depth, and cold startup/readiness at declared database/artifact sizes. Measure stage times, correctness, goodput, memory, and candidate work where applicable. A faster failed request does not improve latency.

Start with non-blocking result production using the P02/P13 identity/receipt model. Calibrate build profile, hardware class, concurrency, cache state, sample size, and variability. Only then choose product-relevant absolute limits plus a sustained relative regression rule, with predeclared rerun/confirmation rules and a reviewed baseline update policy. Never keep rerunning until a noisy result passes or replace the baseline without retaining the old record.

Retain raw samples or an auditable summary sufficient to check percentile computation. Distinguish measurement-harness failures from performance failures and missing environments. Keep required functional CI unchanged while adding measurement; promoting a calibrated performance check to required status is an explicit maintainer decision.

## 13. Library support and engineering policy

### 13.1 P15: choose the embedded support contract — R31

Core, datastore, and memory-store manifests inherit workspace metadata and do not declare an MSRV. Datastore already emphasizes independent usability and has optional engine/artifact features. A repository compiler pin is not evidence of the minimum supported compiler.

If D5 adopts a supported embedded tier, document the selected crates, features, public API/versioning promise, required public fixtures/contracts, and dependency boundaries. Create an isolated consumer fixture outside the Server workspace with its own dependency resolution; a build inside the original workspace cannot demonstrate standalone selection. Test minimal/default/selected engine features, serializer feature unification, and the agreed dependency closure on the pinned compiler and an empirically selected MSRV. Set `rust-version` only after those builds establish it.

Preserve `new()`/`Default()` and legacy gate/provider seams introduced in earlier slices. Registry publication, a lower compiler requirement, and support for Windows AppContainer are separate decisions. If these crates remain internal, document that consumers pin source and that wire compatibility does not imply stable Rust APIs. Shared-code Server defects still need correction regardless of that policy.

### 13.2 Incremental panic-boundary audit — R32

Inventory production `unwrap`/`expect` sites by category: untrusted input, database/I/O, lock poisoning, date arithmetic, guarded invariants, and constant initialization. Existing chronology/date/regex handling and store lock/serialization paths need different remedies.

Introduce scoped non-test lint enforcement crate by crate after fixing fallible boundaries. Return typed errors where failure is possible; use explicit matches for guarded options; document narrowly justified constant invariants. Consider `expect_used` separately so replacing `unwrap` with `expect` cannot masquerade as safer error handling. Do not return empty/default values to hide storage failure or corrupt evidence. Keep chronology and storage behavior goldens and measure any hot-path impact.

### 13.3 Process follow-ups — R04, R05, R27, R29, R30, R33, R34

Retain the existing human review, DCO, protected-file, and no-autonomous-merge boundaries. A size threshold can trigger review/splitting rationale; raw line count is not evidence of quality. Generated artifacts still require validation even when excluded from the review-size calculation.

Record release input identities and intentional compiler/build overrides in the release-owning workflow after its own audit. Do not import operational configuration or change signing/release behavior here. Locked/offline builds alone do not establish hermetic or byte-reproducible releases.

Before scheduling a qualification milestone, list its environments, owner, availability check, cost authority, and expiry constraints. A build under emulation is not native runtime qualification. A validation-only infrastructure example is not a completed deployment/backup drill. Size any separately authorized paid campaign to include unresolved liability, without treating that planning number as permission to spend.

Use the existing [numbered gaps record](guides/dev-guide.md) for missing evidence and owner waivers. Preserve the original failed/unrun outcomes and attach a revisit condition. Milestone closure, qualification success, elapsed time, human effort, and tool cost are different facts and should be recorded separately.

## 14. Regression matrix and validation commands

### 14.1 Required dimensions by behavior

| Work | Focused offline coverage | Stateful/transport coverage before release | Upgrade/rollback evidence |
|---|---|---|---|
| Usage/admission | Parser/captured request/estimator/overflow and legacy provider fixtures | Real memory/PG budget stores, two replicas, cancellations/retries, REST/gRPC | Legacy rows unknown; old writers identified; late reconciliation idempotent |
| Check runner | Fake process/prerequisite/receipt fixtures | Test-owned process/resource cleanup; current CI inventory equivalence | Existing switches and exit consumers qualified |
| JSON | Typed DTO/canonical format and feature-graph fixtures | Real PG reopen, artifact seal/open, both transport paths | Cross-feature supported data and old artifact goldens |
| Equality | Legacy and exact policy goldens; generated histories | Memory/PG and REST/gRPC, corrections/anchors/pins | Reader-first, mixed-policy rules, incompatible old writers blocked |
| Retrieval | Exact eligible-set oracle, sparse scopes, stable ordering | PG/datastore, multiple collections, concurrent access changes | Existing generation/pins readable; selector rollback |
| Recovery | Barrier/harness controls, feature-off checks | Child kill/reopen, cluster races, runbook approval, artifact publication | Durable operation state readable across supported versions |
| Authority/evidence | Verification and adversarial output fixtures | Both transports, uid/scope/revocation, originals/publication | Existing static roles/development mode preserved |
| Retention | Inventory validation and cleanup state-machine tests | Holds/shared sources, caches, rebuild, restart/restore | Old backups cannot silently bypass the declared denial contract |
| Wire | Integer/unknown field/enum/null/omission fixtures | SDK N/N−1 and REST/typed gRPC/native bridge | Normative publisher and generated artifact drift checks |
| Evaluation/performance | Good/bad graders, offline full pipeline | Frozen workload, repeatability, failure accounting | Original results immutable; baseline changes explicitly reviewed |

Every behavioral fix should have a regression that fails for the relevant pre-fix behavior, plus a control preserving an adjacent valid behavior. Include exact-limit and limit-plus-one cases for affected request-size and resource limits. Do not turn unsupported or unavailable coverage into a green result by reducing a test's meaning.

### 14.2 Commands that exist at the reviewed revision

From the repository root, for this documentation change:

```powershell
py check_license.py
py clients/check_compatibility.py
py scripts/docs_linkcheck.py
py scripts/private_material_scan.py
cargo test --locked --offline --manifest-path server/Cargo.toml -p munarium-server docs_coverage
git diff --check
```

If the `py` launcher is unavailable, use a working installed Python directly. `--offline` uses the local Cargo cache; missing dependencies/toolchains are unavailable coverage, not permission to claim a passed build. The root link checker covers root documentation; Server's `docs_coverage` tests check the Server documentation links/routes/errors.

For later behavioral slices, select affected commands from the existing ladder in `server/`:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p munarium-core -p munarium-store-mem -p munarium-providers
cargo test -p munarium-api-types -p munarium-api-conv
cargo test -p munarium-datastore --test round_trip
cargo test -p munarium-store-pg --test pg_integration
cargo test -p munarium-retrieval --test mirror_integration
.\test.ps1
.\test.ps1 -Postgres -BlackBox -Platform -Cluster
```

The database-dependent commands require a configured, isolated test database and the appropriate prerequisites. Early-return functions without that database do not establish PostgreSQL coverage. Inspect the current harness's fixed resources and cleanup behavior before running the broad ladder; P02 replaces that ownership risk. Do not run the broad ladder against an unrelated local stack.

For grader/checker changes, run `py -m unittest discover -s scripts -p "test_*.py"` from root, plus the new focused harness tests once implemented. For contract/SDK work, run the current publisher/re-vendoring, generation/drift, and per-language checks documented in contributor/client guidance. For migrations, qualify a fresh database and an upgrade of the preceding supported schema with realistic fictional data. Preserve checksum validation; never reset a database to conceal an upgrade defect.

The detailed provider methods exist after PR #44. A `-Fault` tier, result-schema validator, or isolated consumer runner does not exist merely because this plan names it. Add exact commands to the relevant documentation when those slices land. No live provider, deployment, infrastructure, or paid test is required to validate this plan.

### 14.3 Acceptance and rollback checklist for each implementation slice

Before merging, the reviewer should be able to identify the baseline failure/investigation, intended behavior, affected contracts, independent regression evidence, and any unavailable tier. Source identity must match the final tested content. Regenerate derived artifacts through their source of truth and inspect their changes, not just the source patch.

Before enabling a new behavior, qualify the relevant old/new data and binary combinations. Document migration/backfill duration, lock/transaction implications, mixed-version writers, feature activation, and recovery after interruption. Keep new readers tolerant of legacy data without pretending that legacy evidence has higher quality than it does.

Rollback must be described at the level of state and semantics. Switching an engine/configuration can be reversible; appending exact-policy events, changing receipt protocols, or physically erasing data may not be. Preserve data, audit, reservations, and pins, and use a compatible reader or roll-forward fix when an old binary cannot honor the new state. Stop rollout on correctness/authorization failures even when performance targets pass.

## 15. Recommendation disposition and completion

### 15.1 Complete R01–R34 mapping

| ID | Planned treatment | Slice / section |
|---|---|---|
| R01 | First missing/partial usage correction merged in PR #44; durable evidence implemented; estimates and reconciliation remain proposed | P01, §4.1 |
| R02 | Profile-scoped outcomes and durable local receipts | P02, §5.1; Matrix follow-up |
| R03 | Fix reverified current drift only | P04, §5.3 |
| R04 | Checker meaning/limitations guidance, synchronized agent instructions | P16, §5.3 and §13.3; maintainer-owned policy |
| R05 | Preserve/document automatic CI rationale | P16, §5.2–5.3 |
| R06 | Capability handling for actual optional provider parameters | P05, §4.3 |
| R07 | Clock/ID dependency seams with preserved defaults | P06, §7.1 |
| R08 | Explicit, pinned value-equivalence policy | P09, §7.3; D8 |
| R09 | Default/feature-enabled actual persistence round trips first | P03, §6 |
| R10 | Separate gate/snapshot/store benchmarks before optimization | P06, §7.2 |
| R11 | Sparse eligibility characterization and measured retrieval fix | P07, §8.1 |
| R12 | Separate supported Linux and optional Windows sandbox qualification | P12, §8.2 |
| R13 | Child-process fault tier and separately designed recovery improvements | P08, §9; D3 |
| R14 | Linked attempts, finish/exhaustion reasons, enforceable budget context | P05, §4.2 |
| R15 | Extend existing atomic reservation coverage | P05, §4.2; D1 |
| R16 | Conditional monetary report with immutable pricing/evidence | P14, §4.4; D2 |
| R17 | Integer field/range audit and cross-client tests | P11, §11.1 |
| R18 | Direction-specific unknown-field policy | P11, §11.2 |
| R19 | Audit verified authority construction and callers | P10, §10.1 |
| R20 | Extend model evidence labels and trusted boundary tests | P10, §10.2 |
| R21 | Retention inventory/modes before new erasure behavior | P10, §10.3; D4 |
| R22 | Three latency workloads; reporting before calibrated gates | P13, §12.3 |
| R23 | Shared, equivalence-checked gate definitions | P16, §5.2 |
| R24 | Extend existing positive/negative checker/grader controls | P02/P13, §5.1 and §12.1 |
| R25 | Separate research workstream with offline pilot and fair baselines | P13, §12.2; D6 |
| R26 | Frozen manifest/raw outcomes/grading revisions | P13, §12.1 |
| R27 | Review-size trigger with split/rationale, not arbitrary success gate | P16, §13.3; maintainer-owned template |
| R28 | Safe permissioned diagnostic aliases after audience review | P05, §4.3; D7 |
| R29 | Release input audit belongs to the release-owning workflow | Coordination only, §13.3 |
| R30 | Paid campaign planning includes unresolved exposure | Coordination only, §13.3; no spending authorization |
| R31 | Conditional supported embedded tier/MSRV/selection fixture | P15, §13.1; D5 |
| R32 | Targeted production panic audit and incremental lint | P15, §13.2 |
| R33 | Waivers remain known gaps with original outcomes retained | P02/P16, §2.2 and §13.3 |
| R34 | Environment availability before dependent qualification | Coordination only, §13.3 |

### 15.2 Validation of this planning change

Run the documentation and repository checks in §14.2 on the publication commit and record their results and any unavailable coverage in its PR. Documentation validation does not qualify the proposed product improvements. PR #44 records the separate implementation tests and CI for the first P01 correction; those results do not establish completion of later slices.

### 15.3 What counts as completion

This planning task is complete when this document is indexed, its current-source references are checked, all recommendation IDs have a disposition, and documentation validation outcomes are recorded. That does not complete any implementation slice.

An implementation slice is complete only when its behavior, compatibility, migration/rollback constraints, tests, and documentation meet its exit criteria. A research slice can complete with rejection or inconclusive evidence if that is an allowed preregistered outcome. An unavailable environment or accepted waiver can permit a separately recorded release decision, but cannot manufacture qualification evidence.

P02–P04 are merged in PR #46. The remaining actionable work includes P01 reconciliation/reporting, D1/D7 decisions for broader P05 changes, and any separately adopted stronger recovery guarantees beyond the P08 application-process scope in §9.1. Their outputs should refine the estimates and contracts for later slices before additional architecture is committed.
