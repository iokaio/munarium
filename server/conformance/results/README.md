# Public evaluation results

This index retains sanitized P13 records. The
[record schema](records.schema.json) defines manifests, raw outcomes and grading
receipts; [the guide](../../docs/guides/frozen-evaluation.md) explains production,
validation, regrading and measured-result markers.

Records named `sha256-*.json` are immutable by convention and checked by content
identity. Append revisions and diagnostic reruns; retain original raw evidence,
failed rows and old scores. Do not copy private datasets or ignored run residue
into this index. The committed pilot uses only the public fictional fixture.

The offline pilot is a recording/grader control, with no Server, live provider,
governance-benefit comparison or product latency claim. Its token usage is unknown.

## Scripted pilot 001

Five planned rows completed and passed the lexical/evidence checks across four
fictional histories. All five rows retain unknown usage. This result qualifies
the producer → persisted evidence → grader → receipt path only.
<!-- measured-result: sha256:0832c26d34776ec34202d49e35df342a61509b618cbfd3d3ecb7035453c9f225 source: sha256:09b3ee2c2ede003112199782f306f7852b6006ae5abaa240dffe8ecdf2b615e2 -->

| Record | Immutable artifact |
|---|---|
| Frozen manifest | [Manifest](sha256-0a1f85e9584852268814288e29179886fe77d44b8b7139ca456048a67c84a1e0.json) |
| Raw answers/evidence | [Raw outcomes](sha256-2d16e13f302d0892a79951cd6a0cd6edf22a9691c46e8b3b71b7b276d247b185.json) |
| Original grading receipt | [Revision 1](sha256-ecf8f8da33ea111256a7010fc03ff9638a2031c88a4faebe3c914d74d1a18794.json) |
| Formatting/lint regrade | [Revision 2](sha256-71b4a91d80581e1a104933104b6963b60db21361dba48a97540c8c5644921d89.json) |
| CLI failure-precedence regrade | [Revision 3](sha256-0cc3332afd571582467df50a72556ffa52b496785521130e04ce60cb67b14bce.json) |
| Current grading receipt | [Revision 4](sha256-0832c26d34776ec34202d49e35df342a61509b618cbfd3d3ecb7035453c9f225.json) |

The manifest was frozen on disk before this diagnostic pilot; it was not a
committed preregistration or a qualifying held-out campaign. Run ID:
`p13-scripted-pilot-001`. No tokens, private data or machine resource names were
used. The Python version and hashes in the manifest identify the local producer.
Revision 2 regrades the same raw bytes after source formatting and lint fixes;
it retains the original result and records the updated scorer/evaluator hashes.
Revision 3 retains those records and regrades the same raw bytes after fixing
malformed/whitespace completion handling and failure precedence in the CLI.
Revision 4 retains the same scores after adding offline Git inclusion proofs to
the record validator; no raw evidence changed.
To execute the current producer, freeze a new manifest; replaying the original
manifest against changed producer source correctly fails its identity check.

## Local validation

- `py -m unittest discover -s scripts -p 'test_*.py'`: 24 tests passed,
  including positive/negative grader, producer and documentation-marker controls.
- `cargo test --manifest-path server/Cargo.toml -p munarium-server docs_coverage`:
  all six documentation tests passed.
- `py scripts/frozen_eval.py verify server/conformance/results`: all retained
  records validate. The JSON Schema was also checked with `jsonschema` locally;
  production scripts require only the standard library.
- `py scripts/docs_linkcheck.py`, `py check_license.py`, and
  `py clients/check_compatibility.py`: passed.
- Ruff lint passed for all changed Python files; formatting passed for the three
  new scripts. `git diff --check`: passed.
- `py scripts/private_material_scan.py`: failed with 350 existing findings in
  local scratch files and nested snapshots. The unchanged scanner, scoped to
  the P13 diff, passed. No files or scanner exceptions were removed or changed
  to hide the full-scan findings.

That initial scripted stage performed no paid run, remote CI run, live Server
campaign, product latency measurement or D6 held-out qualification. The following
stage records the subsequent live work separately.

## Local live qualification 001

The manifest and implementation were committed in
`b034436cd3461ea8eee0c2090796bd36c6f863c8` **before** the held-out run.
It used a native release Server and an isolated, pinned PostgreSQL/pgvector
container with capability authentication, public fictional histories and a
deterministic responder. There were no paid calls. All owned processes,
containers and anonymous volumes were cleaned up.

The run completed all 300 planned arm outcomes over 60 held-out histories.
The approved D6 usefulness criterion was **rejected**: Munarium achieved 100%
supported correctness and zero unauthorized disclosures, but conventional
retrieval also achieved 100%, yielding no five-point improvement.
The campaign's exit 1 is the retained qualification verdict, not a runner failure.
<!-- measured-result: sha256:2b52c59ce70b031bb1fda14ad29e08c36f86bb6e81033a41806ae691e78e6bf8 source: sha256:bd4c7ca82ca6dbc7f78a744f1f97dd6caf031f52d6cf50eb507f665b154ac896 -->

| Arm | Correct histories | Unauthorized disclosures |
|---|---:|---:|
| No memory | 20 / 60 | 0 |
| Maintained versioned notes | 60 / 60 | 0 |
| Conventional retrieval | 60 / 60 | 0 |
| Munarium | 60 / 60 | 0 |
| Same-pipeline governance ablation | 40 / 60 | 0 |

The paired difference against conventional retrieval was 0 percentage points;
the deterministic history-bootstrap interval was [0, 0]. These histories share
six templates and a value vocabulary. That result does not establish equivalence
on a broader workload or model-answer quality. The useful-effect threshold was
not lowered after seeing the tie. See the
[complete analysis](analysis-d19983b374b0889af99cacdf02e164af65db8852cde18311b6b7f554ddab467b.json).

All 630 latency samples passed their correctness checks. Each path ran three
fixed repetitions. Budgets were frozen from calibration pilot 003 before this run.

| Product path | Samples per repetition | Observed p95 range | Frozen local absolute p95 limit | Result |
|---|---:|---:|---:|---|
| Retrieval, 200 documents | 100 | 33.3–36.2 ms | 51 ms | Within local budget |
| Ledger append, four workers, initial depth 32 | 100 | 51.6–53.8 ms | 91 ms | Within local budget |
| Process-cold readiness, warm database/OS cache | 10 | 534.3–537.7 ms | 805 ms | Within local budget |

No path triggered the frozen sustained 20% relative regression rule. Exact
percentiles, goodput, working-set observations, raw samples and database sizes
are retained. This calibrates a small local workload on a shared workstation;
it does not establish release-wide SLOs, power-on cold-cache performance, model
latency or unmeasured internal Server stage/candidate work. No required CI gate
or hosted runner configuration changed.

| Evidence | Artifact |
|---|---|
| Preregistered held-out manifest | [Manifest](sha256-a4cef43f2d3fd66382fe6bc9f91b068b7d61504f9d02598f50bdf931c230e5eb.json) |
| Raw live outcomes and all latency samples | [Raw outcomes](sha256-6874ac22350c30c6e274e6d2edd627349b47032a2aee139f72c54bca33587171.json) |
| Original grading | [Revision 1](sha256-38f5b1dced3f7bebabc81f6b83e35ef449b1b4db65ef34824d46822370c05d38.json) |
| Current grading, unchanged scores | [Revision 2](sha256-2b52c59ce70b031bb1fda14ad29e08c36f86bb6e81033a41806ae691e78e6bf8.json) |
| Git commit/tree/blob inclusion proof for shallow checkouts | [Proof](preregistration-a4cef43f2d3fd66382fe6bc9f91b068b7d61504f9d02598f50bdf931c230e5eb-b034436cd3461ea8eee0c2090796bd36c6f863c8.json) |
| Live analysis structure | [Schema](live-analysis.schema.json) |

The Git proof establishes exact manifest inclusion in the preregistration commit;
it does not supply a trusted external timestamp. The qualification was executed
after that local commit. The archival validator was subsequently extended with
this proof mechanism and regraded the unchanged raw record; the original grade
and its source identities remain retained.

## Retained diagnostic history and final checks

| Run | Outcome |
|---|---|
| [Live pilot 001](analysis-2d3c1b57e7cc56a3474de0acfa85f0aa7d6f0c2ff0a857eb5b48231c9fe18f75.json) | Six histories; completed initial setup/scoring/timing checks |
| [Live pilot 002](analysis-a7ddf28ae702582e1943c9766f47c2553ba6d5bd971f6560563ebef1cd17ff5e.json) | Setup failed before cases; 300 unexecuted rows and all planned latency slots retained; cleanup completed |
| [Live pilot 003](analysis-376558f53857f97d2db52bb0bd2d96baa787bf5d3c73ccc024013651c2f5572b.json) | TCP readiness corrected; 60 histories and all 630 latency samples completed; frozen calibration baseline |

The failed setup exposed PostgreSQL's temporary initialization socket: the rig
now waits for its final TCP listener before starting Server. Diagnostic reruns
have distinct run IDs and manifest identities. Their raw records and journals
remain in this index; no run was overwritten or discarded to obtain success.

Final local checks: 44 Python tests, six Server documentation tests, Python lint
and formatting, record/analysis validators, JSON Schemas, license and client
compatibility checks passed. A simulated shallow checkout validates the Git
inclusion proof and rejects missing/tampered proofs. The full private-material
scan still reports 350 existing findings, all under `server/scratch`; P13's
changed files pass the same scanner scoped to the diff. No paid provider,
production deployment or remote CI run was performed.
