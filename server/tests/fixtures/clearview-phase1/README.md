<!-- SPDX-License-Identifier: Apache-2.0 -->
# ClearView Phase 1 fixtures

Frozen v1.4.0 inputs and expected results for the Phase 1 work in the
[ClearView plan](../../../docs/design/README.md#phase-1-fence-workers-and-report-observed-failures).
Every source these fixtures describe is byte-identical between the v1.4.0 release
(`25012c22087fbaa8ce29ddd68d2880d6e9499b1c`) and the commit that added them, so
the recorded outputs are v1.4 behavior. All identifiers and text are fictional.

| File | Cases | Checked by | Consumers |
|---|---|---|---|
| `v14-build-jobs.json` | Seven job rows: pending, live lease, expired final claim, historical storage-text failure, v1.4 free-text failure, success and cancellation after claim. Each has its exact v1.4 REST DTO and the list order. | `munarium-server` unit tests `datastore_jobs::tests::v14_*` (DTO bytes); `munarium-store-pg` test `v14_job_fixtures` (PostgreSQL read-back, the v1.4 baseline observations, and the R4 contrast that `heartbeat_fenced` refuses a stale generation on the same row) | R4-T1, R5/C2-T1, R6-T1, R8, R9, U12 |
| `v14-console.json` | Twelve untrusted-text cases for `charts::esc`, including three negative controls, each with its v1.4 output and its U9 output (`expected_v15_esc`). Also five overview failed-read cases. | `munarium-server` unit test `charts::tests::console_fixture_esc_output_matches_u9` | U9, U5, U14, COMPAT-T5 |

## Changing an expectation

A Phase 1 change that alters a recorded v1.4 result must update the fixture and
its test in the same change. It must state which plan item the change implements
and keep every case marked `unchanged`, and every row outside that item's scope,
at its v1.4 value. Never edit an expectation only to make a test pass.

## Coverage limits

- The `expired-final-not-reoffered` observation is established by reading
  `claim_any`. The store test does not call the global `claim_any`, because doing so
  would claim other tests' queued jobs.
- The `failed_reads` cases record the call sites and the value v1.4 renders. No
  automated test exercises them yet: the overview has no seam for a failing
  registry read. U5 adds that seam and the behavioral test.
- These fixtures describe the PostgreSQL job store. The memory store has no build
  jobs.
