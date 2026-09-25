# Lessons from downstream use: a Server engineering agenda

**Status:** public recommendations and investigations, with the first usage-accounting correction merged.
**Updated:** 2026-09-23, against Munarium `08f9033952fbb65030ae0bbaceca423403c91e04`.

Downstream use of Munarium in VCP prompted a review of provider accounting,
validation, persistence, governance, and retrieval. This page presents the
resulting Server questions using sources available in this repository. It makes
no downstream performance, spending, productivity, or qualification claims.

The [implementation plan](lessons-from-vcp-impl.md) supplies the detailed designs,
compatibility decisions, test matrices, and delivery slices. Recommendation IDs
R01–R34 are stable cross-references between these documents. They identify proposed
work, not adopted product commitments or completed tests.

## Reading the evidence

Source inspection establishes what a particular implementation does. A regression
test establishes behavior on its exercised path. Neither establishes a performance
result, recovery guarantee, or supported deployment that was not tested.

The implementation plan records its original source baseline. Its P01 status has
been updated for [PR #44](https://github.com/iokaio/munarium/pull/44). Other findings
must be rechecked against the implementation revision before starting a fix.
A suspected defect that cannot be reproduced remains an investigation.

## 1. Preserve uncertainty in provider accounting

A successful answer does not establish that its provider reported complete usage.
Zero observed tokens and missing counts require different accounting treatment.
The [provider types](../src/munarium-core/src/provider.rs) now distinguish those
cases without changing existing response structs or required provider methods.

The first correction merged in PR #44. Complete observed counts settle their
checked sum, including explicit zero. Missing, partial, malformed, or legacy
unverified usage retains at least the reservation and available subtotal. Ollama
continues to require valid counts. The
[current architecture](architecture.md#93-invocation-provenance) documents the
remaining reporting and rollout limits.

R01 follow-ups concern durable evidence, better estimates, and reconciliation.
R15 audits the scope of admission that already exists in the
[memory budget store](../src/munarium-store-mem/src/budget.rs) and
[PostgreSQL budget store](../src/munarium-store-pg/src/budget.rs). Embeddings,
health probes, retries, and completion calls must each have an explicit policy
before making broader accounting claims. Optional money reporting (R16) depends
on reliable usage evidence and versioned prices; unknown amounts cannot become
zero merely to produce a numeric total.

## 2. Make validation outcomes explain what ran

A process returning successfully is only one part of validation. A requested
check may lack a prerequisite, return before exercising its target, or produce a
successful transport response containing a failed semantic result.

Review [test.ps1](../test.ps1), [gates.ps1](../gates.ps1), and the configured
[Server CI](../../.github/workflows/server-ci.yml) together. R02 proposes explicit
step outcomes, source identity, and completion records. A failed or interrupted
run must retain its original result; a waiver cannot turn it into a pass.

R24 extends known-good and known-bad checker controls. R23 proposes a shared gate
catalog only after equivalence with required CI coverage is demonstrated. R05
preserves automatic testing. R03 updates documentation after current-source
verification, retaining dated examples in their original context.

## 3. Test actual persistence under the selected feature graph

A serializer risk needs a fixture through a real write and reopen path. R09
covers supported JSON values in PostgreSQL, typed transport values, and datastore
artifacts under default and explicitly selected serializer features.

The [datastore canonicalizer](../src/munarium-datastore/src/canonical.rs) and
[round-trip fixtures](../src/munarium-datastore/tests/round_trip.rs) describe a
bounded format, not arbitrary JSON everywhere. Preserve supported values, reject
unsupported inputs before acknowledging a write, and keep stable artifact hashes.
A dependency update or decoder replacement requires a reproduced failure and a
compatibility rationale.

## 4. Measure governance and retrieval before changing them

R07 introduces controlled clock and identity inputs where the
[memory store](../src/munarium-store-mem/src/lib.rs) owns nondeterminism. Existing
production constructors and defaults should remain available.

R10 separates the cost of [gate evaluation](../src/munarium-core/src/gates.rs),
snapshot construction, store work, and adapter replay. R11 characterizes sparse
eligible sets and candidate work in
[PostgreSQL collection search](../src/munarium-retrieval-pg/src/collections.rs)
and the datastore. An end-to-end delay alone cannot identify which layer needs an
optimization. Bounded approximate search must report its actual recall and work
limits rather than promise complete recall unconditionally.

R08 is a separate compatibility design. The
[existing value comparison](../src/munarium-core/src/ledger.rs) folds whitespace
and case. Exact comparison must have a policy identity and historical replay
rules before it can replace that interpretation for any history.

R12 qualifies filesystem permissions independently for each supported platform.
Artifact immutability does not imply that opening an index needs no writable
scratch space; inspect the [lexical engine](../src/munarium-datastore/src/lexical.rs)
and test the actual serving identity and directory permissions.

## 5. Name recovery and retention guarantees

R13 extends existing [mirror fault tests](../src/munarium-retrieval/tests/mirror_integration.rs)
with process termination at named barriers followed by reopened-state assertions.
Application-process recovery, database restart, host failure, and backup restore
are separate guarantees. A durable result receipt also needs a stated relationship
to the mutation it records; a handler-level lock cannot establish cross-replica
atomicity.

R19 audits construction and use of [AccessCtx](../src/munarium-access/src/lib.rs)
while preserving verified claims and current authorization. R20 extends tests of
[checked answer evidence](../src/munarium-server/src/answers_api.rs): retrieved
passages provide data and citations, never permission to mutate or approve.

R21 inventories retained data before introducing stronger deletion promises.
The current [soft-removal workflow](guides/platform-features.md) and
[physical deletion procedure](ops/index-deletion-runbook.md) have different
contracts. New erasure behavior would need a design covering derived content,
caches, holds, shared sources, recovery, and restored backups.

## 6. Keep compatibility and evaluation claims bounded

R17 checks integer ranges through actual storage and transport paths. R18 defines
unknown-field behavior separately for requests, responses, events, and persisted
formats. Both preserve existing wire contracts until a versioned change is
explicitly designed and tested with the supported clients.

R25 and R26 propose a separate evaluation workstream with frozen fictional
histories, fair baseline updates, independent grading controls, and immutable
results. Authorization remains mandatory in every comparison arm. A rejected or
inconclusive result is valid evidence; a revised grader creates a new result
instead of overwriting the original one. The
[public example grader](../../docs/lab/example/grade.py) and
[performance guide](guides/measuring-performance.md) provide existing starting
points. R22 adds latency reporting before any calibrated performance gate.

Embedded library support (R31), optional monetary reporting (R16), and stronger
recovery or erasure guarantees are decisions, not consequences of publishing this
agenda. P15 took the first: only the datastore is a supported embedded library
([embedded-support.md](embedded-support.md)). The public [capability table](../README.md#what-is-built-and-what-is-not)
remains the starting point for what Server supports.

## Recommendation index

The implementation plan's [complete mapping](lessons-from-vcp-impl.md#151-complete-r01r34-mapping)
assigns each recommendation to a slice and acceptance criteria.

| ID | Engineering question or proposed work |
|---|---|
| R01 | Preserve usage certainty; first settlement correction merged, durable evidence follow-ups pending |
| R02 | Explicit validation outcomes and completion receipts |
| R03 | Correct verified current documentation drift |
| R04 | Explain checker coverage and limitations in contributor guidance |
| R05 | Preserve automatic required CI coverage |
| R06 | Handle optional provider parameters through tested capabilities |
| R07 | Inject clock and identity inputs where stores own them |
| R08 | Version value-comparison policy without reinterpreting history |
| R09 | Qualify real JSON persistence under relevant feature graphs |
| R10 | Measure gate, snapshot, store, and adapter costs separately |
| R11 | Characterize sparse-scope candidate collection and bounded work |
| R12 | Qualify supported filesystem permissions per platform |
| R13 | Add process-death and reopen tests for named recovery guarantees |
| R14 | Record attempt relationships, exhaustion reasons, and budget context |
| R15 | Audit and extend existing atomic admission coverage |
| R16 | Decide whether to add monetary reporting with immutable prices |
| R17 | Audit integer ranges across storage, transports, and clients |
| R18 | Document unknown-field policy by direction and format |
| R19 | Audit verified authority construction and production entry paths |
| R20 | Test evidence handling and trusted effect boundaries |
| R21 | Define retention modes and derived-content treatment |
| R22 | Calibrate latency reporting before enforcing regression limits |
| R23 | Share gate definitions while proving required coverage equivalent |
| R24 | Extend positive and negative checker and grader controls |
| R25 | Compare governance with fair baselines in a separate evaluation |
| R26 | Freeze manifests and preserve original outcomes and grading revisions |
| R27 | Require splitting rationale when review scope becomes too large |
| R28 | Review visibility before adding operator credential aliases |
| R29 | Audit release inputs in the workflow that owns releases |
| R30 | Include unresolved liability in separately authorized campaign budgets |
| R31 | Decide embedded library support and qualify the selected API/MSRV; decided in P15 (datastore only, [embedded-support.md](embedded-support.md)) |
| R32 | Audit production panic boundaries before tightening lints; completed in P15 ([panic-boundaries.md](panic-boundaries.md)) |
| R33 | Record waivers separately from successful qualification |
| R34 | Check environment availability before dependent qualification |

## Next work

P02 validation receipts, P03 persistence characterization, and P04 verified
documentation corrections can proceed independently after the merged P01 fix.
Keep the remaining accounting evidence and reconciliation work explicit. Use each
slice's tests and compatibility decisions to refine later work before committing
to a broader architecture. This documentation does not authorize paid tests,
deployments, releases, or changes to repository protections.
