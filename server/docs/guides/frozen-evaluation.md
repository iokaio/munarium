# Frozen evaluation records

P13 provides an offline recording and grading path using the existing
[example grader](../../../docs/lab/example/grade.py). It produces three separate
versioned records: a frozen manifest, raw outcomes and a grading receipt.
The [public index](../../conformance/results/README.md) contains sanitized
records and the [JSON schema](../../conformance/results/records.schema.json).

## Run the scripted pilot

From the repository root, using Python 3.10 or later:

```powershell
py scripts/frozen_eval_pilot.py freeze server/scratch/p13-run
# Use the manifest path printed by freeze:
py scripts/frozen_eval_pilot.py run <manifest-path> server/scratch/p13-run --run-id pilot-001
py scripts/frozen_eval.py verify server/scratch/p13-run
py -m unittest discover -s scripts -p 'test_*.py'
```

The producer reads fictional histories and scripted responses, persists raw
answer/evidence rows, reads them back, then calls the example grader and writes
a separate receipt. It makes no network requests and incurs no provider charges.
Its five tasks cover correction, historical facts, conflict, current authorization
after revocation, and removal. Two tasks share a history; these are four independent
histories, not five independent statistical samples. Scripted answers exercise
recording and grading; they do not test Server enforcement or model quality.

Each file is named by SHA-256 of canonical JSON without its `id` field (sorted
keys, compact separators, UTF-8, no ASCII escaping or nonfinite numbers). Source
hashes normalize CRLF to LF; source paths are repository-relative. Exclusive
creation refuses overwrites. Changing any record creates a new identity. This
detects accidental changes; hashes are not signatures or proof against a writer
who replaces an entire campaign. Git review supplies the publication history.

The producer checks the fixture and executable source identities against the
manifest before running. It retains every planned row after an interruption,
including subsequent unexecuted cases. Provider failures retain a sanitized error
class. Evidence and completion shapes fail explicitly. A transport success does
not imply an answer passed. Missing completion yields incomplete coverage;
failure takes precedence over incomplete coverage. Receipt exits follow P02:
0 passed, 1 failed, 3 incomplete. The legacy example CLI retains its exit 2 for
missing completions and invalid command input.

Usage `null`, or a missing component within a usage object, remains unknown.
Explicit zero is known zero. The scripted pilot has unknown token usage because
it invokes no metered model; it is not a cost estimator. Grading remains lexical
and evidence-path based, not a semantic proof or exhaustive disclosure detector.

## Freeze, qualify and regrade

The manifest pins source/build, corpus/query hashes, independent history/task IDs,
splits, expected evidence, retrieval/runbook and model settings, grader identity,
thresholds, failure treatment and resource limits. Version 1 supports the strict
`all_checks_pass` policy only. Histories cannot span pilot and held-out splits.
An adapter using these records must verify its corpus/build, enforce the frozen
resource limits and keep answer keys outside the model context and searchable
corpus. The scripted adapter verifies its fixture hash. The local live adapter
below exercises an isolated release Server/PostgreSQL pair without a paid provider.

Before a qualifying campaign, decide D6: task population, useful effect, mandatory
authorization constraints, budgets and held-out criteria. Set `purpose` to
`qualification`, record `decision_d6`, use held-out cases, and commit the manifest
in the public index **before execution**. The raw record must identify that
`frozen_commit`; validation reads the exact manifest from Git. A commit's presence
is verifiable; actual preregistration timing still needs the operator's reviewed
run record. Pilot and diagnostic records cannot be relabeled by a grading run.
Changed criteria need a new manifest and campaign, including retained failures.

Archived results can carry an accompanying Git commit/tree/blob inclusion proof
for checkouts that lack the preregistration commit's history. The validator hashes
each Git object and checks the exact manifest path and bytes; missing or tampered
proofs fail. This keeps shallow CI offline without relaxing preregistration.

Regrade unchanged raw evidence with:

```powershell
py scripts/frozen_eval.py grade <manifest-path> <raw-path> <output-directory> --revision 2
```

This records the actual scorer and evaluator hashes and a new revision. Preserve
the old grading record. Current-version grading is recomputed during validation;
historical grader records remain structurally readable but require revalidation
or a new grading receipt before supporting a new measured claim.

To bind a documented result, place an HTML comment on its own line immediately
after the claim: `measured-result: RESULT_ID source: SOURCE_MAP_HASH` inside
`<!--` and `-->`. `SOURCE_MAP_HASH` is the canonical hash of the manifest's
`source` object. `scripts/docs_linkcheck.py` validates the public record index and
these explicit markers in root documentation and Server docs. It rejects missing,
malformed, duplicate, mismatched, or incomplete result references. A complete
failed result is valid evidence of failure. The gate does not infer claims from
arbitrary numbers or evaluate the prose's statistical interpretation. Historical
unrecorded statements stay labeled historical; never fabricate receipts for them.

## Latency reporting and governance evaluation boundaries

`frozen_eval.validate_latency` consumes `latency_workloads` in the manifest and
`latency` in raw outcomes, and adds non-blocking reports to the grading receipt.
Freeze all three paths: `retrieval`, `ledger_append` and `cold_readiness`. Each
definition declares build profile, hardware class, concurrency, cache state,
sample count, corpus size/eligible fraction, history depth, database/artifact
bytes and rerun policy. Set non-applicable dimensions explicitly to zero.

Each measurement names the canonical workload hash, elapsed wall time in ms,
and all samples with sequential string IDs starting at `0`. A sample contains
`outcome` (`passed`, `failed`, `harness_error`, `unavailable`), `duration_ms`,
`stages_ms`, `memory_bytes` and `candidate_work`; unavailable counters are null.
Retain raw samples. Reports use nearest-rank p50/p95 over correct work, include
min/max and all outcome counts, and divide correct work by the entire elapsed
interval for goodput. Failed fast requests cannot improve the reported percentile.
These measurements are separate from the scripted-provider timing in the pilot.

Use [the performance guide](measuring-performance.md) and the existing retrieval
benchmark to collect real samples. Calibration requires repeated comparable runs,
variance, absolute targets, sustained relative regression criteria, predefined
confirmation rules and reviewed baseline updates. No product latency budget or
required performance CI gate is introduced by this reporting implementation.

The subsequent governance study compares no memory, maintained versioned notes,
conventional retrieval, Munarium and a same-pipeline governance ablation with
mandatory authorization in every arm. Give each identical events, timestamps,
access facts, model/context and resource limits; count maintenance work. Separate
supported correctness, stale/conflicting answers, abstention, unauthorized
disclosure and citation validity. Analyze paired differences across independent
histories with uncertainty, not repeated queries treated as new histories.
The scripted pilot does not establish fixture difficulty, effect size, statistical
power or equivalence. The local live campaign below implements all five arms for
a narrow deterministic population. Paid execution and model-quality studies remain
separate from this scope.

## Local live qualification and calibration

The standard-library runner [p13_live.py](../../../scripts/p13_live.py) uses
[an owned local rig](../../../scripts/p13_local.py),
[fictional histories](../../../scripts/p13_cases.py), and
[independent report checks](../../../scripts/p13_report.py). It requires Docker
with `pgvector/pgvector:pg16` available locally, Python 3.11+, and a native release
Server. It creates a uniquely named container, random loopback ports, ephemeral
test credentials and a child Server process. It removes its own container and
anonymous volumes and terminates its own process on exit. Logs remain in the
owned, ignored scratch directory; public records are produced directly from
fictional inputs and sanitized observations, never copied from those logs.

```powershell
cargo build --manifest-path server/Cargo.toml --release -p munarium-server
py scripts/p13_live.py freeze --phase pilot --per-family 10 --samples 100 --starts 10 --repetitions 3 --binary server/target/release/munarium-server.exe
py scripts/p13_live.py run --manifest <printed-manifest-path> --binary server/target/release/munarium-server.exe --run-id <unique-pilot-id>
```

Use the executable without `.exe` on platforms with that naming convention.
The runner has no remote-target or hosted-model option. It strips inherited
`MUNARIUM_*` settings and fixes storage, authentication, and provider-free behavior.
The manifest pins the executable, runtime, source inputs, CPU identity/count and
PostgreSQL image. HTTP request counts and wall time are capped; database size is
checked against a frozen upper bound. It checks source and binary identity again
before completion. Before creating resources or output records, it compares the
observed Python version, operating system, architecture, processor identity and
logical CPU count with the manifest; a mismatch refuses execution. Baseline
selection compares those same dimensions. These checks establish agreement on
the recorded host class, not identical physical hardware or background load.
Every case and latency sample has a planned slot. An exclusive
journal persists completed case observations with `fsync`; caught interruptions
retain unexecuted rows and incomplete latency samples in the final raw record.
An uncatchable process/host termination can leave a journal without a finalized
receipt; it is incomplete evidence and must not be called a completed campaign.

The local D6 decision approved for this campaign is zero unauthorized disclosures,
at least 95% supported correctness, and a minimum five percentage-point paired
improvement over conventional retrieval. A 95% history-bootstrap lower bound
must clear that improvement to qualify; an upper bound below it rejects the
usefulness claim, and overlapping uncertainty is inconclusive. Any missing run
coverage, caught run-level error or unconfirmed resource cleanup makes the
qualification incomplete, even if every case finished. Completed case scores
remain available. This is an empirical acceptance rule for the declared
fictional population, not an assurance about arbitrary real-world histories.

The six history families are stable facts, explicit corrections, historical
questions, contradictions/disputes, removals and revoked access. Removal is an
application tombstone encoded as a ledger correction, not a new Server deletion
API. Historical access checks use a previously accessible, explicitly pinned
collection index after raising its current access requirement. No disallowed
response is supplied to the responder, even when a negative control exposes one.

All arms share the scoped current-access probe and deterministic responder:

| Arm | Evidence selection and maintenance |
|---|---|
| No memory | No retained evidence supplied to the responder |
| Versioned notes | Materialized time-stamped snapshots with explicit supersession and conflicts |
| Conventional retrieval | Actual indexed search with timestamp filtering, explicit supersession, tombstones and conflict preservation |
| Munarium | The same retrieval plus actual accepted/disputed ledger slices and supersession at the requested sequence |
| Governance ablation | The same retrieval and responder with supersession resolution removed; current authorization remains mandatory |

The corpus is uploaded without answer keys. Retrieved text and source hashes are
verified against the frozen input bytes before constructing context. The common
responder sees selected evidence only. Shared ledger/index setup work and maintained
note update time/storage are recorded separately. The shared authorization probe
adds common work even to the no-memory arm, so per-arm timings are not independent
end-to-end implementations or a claim of baseline cost superiority.

The held-out histories use disjoint IDs from the pilot. They share the value
vocabulary and six authored scenario templates; this is a small synthetic population, not evidence
of broad task diversity. Correctness, stale/conflicting evidence, abstention,
current-access failures and citation failures are reported separately. Known
model usage is zero because no model is called; model-answer quality is unqualified.

After reviewing the pilot and choosing its immutable raw result as the baseline:

```powershell
py scripts/p13_live.py freeze --phase qualification --per-family 10 --samples 100 --starts 10 --repetitions 3 --baseline-raw <pilot-raw-path> --binary server/target/release/munarium-server.exe
# Review and commit the code and printed frozen manifest before execution.
# Use the configured human contributor's DCO sign-off for that commit.
$p13FreezeCommit = git rev-parse HEAD
py scripts/p13_live.py run --manifest <qualification-manifest-path> --binary server/target/release/munarium-server.exe --run-id <unique-qualification-id> --frozen-commit $p13FreezeCommit
py scripts/frozen_eval.py verify server/conformance/results
py scripts/p13_report.py
```

The runner verifies the committed manifest before creating resources. Its exit is
0 for a completed diagnostic or accepted qualification, 1 for D6 rejection, and 3
for incomplete/inconclusive coverage. Grading receipts independently preserve
failures in the intentionally weaker comparison arms; those failures are not
erased to turn the whole scorecard green. Analyses bind the manifest, raw and
grading IDs and are checked by the existing documentation gate.

Calibration repeats each path three times: 100 retrieval queries against a
200-document collection, 100 appends across four concurrent lineages preseeded to
depth 32, and ten process restarts per repetition. Startup measures spawn to
operations readiness, polling every 20 ms. Database and OS caches remain warm;
PostgreSQL holds all artifacts, so separate Datastore artifact bytes are zero.
The database workload has a 512 MiB upper bound; exact observed database bytes
are retained before/after the campaign and after each measurement repetition.
Process working-set samples are recorded on Windows. Candidate counts and internal
Server stage timings unavailable through this adapter remain unmeasured; client
roundtrip/validation and spawn/readiness boundaries are measured explicitly.

The baseline freezes local absolute limits at 1.5 times its worst repeated p95,
rounded up to a millisecond, plus a 20% relative limit sustained over two consecutive
repetitions. Every planned repetition is retained. Results outside a frozen limit
are reported; the runner does not retry until passing or change the baseline.
These are calibration/reporting thresholds on a shared local workstation, not
release-wide SLOs or new required CI checks. Changing a baseline requires a new
reviewed manifest retaining the prior records.

The [recorded local qualification](../../conformance/results/README.md#local-live-qualification-001)
completed with a rejected usefulness claim and all three paths within their frozen
local latency budgets. Keep that measured rejection; it is not permission to weaken
the conventional baseline or lower D6 retrospectively.
