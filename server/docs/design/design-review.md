# ClearView v1.5 Design Review: Consolidated

Nov 19, 2026 · @Tyler Jensen

## Bottom line

The design is ready to start Phase 0, the Matrix inventory, the Lens scaffold and Phase 1. It is not ready to implement as a whole, and the README's opening line should say so.

Reviewed: `server/docs/design/` on `feat/clearview` at commit `8896279` (five documents, 33,054 words), checked against the server code on the same branch. Findings are tagged by origin: **\[A\]** the merged code-level review, **\[B\]** the v1.5 design review, **\[New\]** found in this pass.

The engineering is careful and both earlier reviews hold up well. Three things matter most:

1. **The release shape is the largest risk.** Seven programs must qualify on one candidate, several behind gates the team does not control. Correctness fixes that are small and testable today wait behind a 16-repository Lens census. \[B\]
2. **The build-job state machine has three holes** that the design inherits or creates: no terminal state after retries run out, an undefined outcome when a cancel lands after publication, and a cancelled target that can be re-enqueued while the old worker is still running. \[A, New\]
3. **Two defaults quietly defeat stated policy.** The Helm chart ships `replicas: 2` with no replica count set, so provider rate limits are doubled on a default install today. A9 lets a 2,000-token collection be debited 8,000 per call. \[A, B, New\]

Everything else is either a missing specification (retention, migrations, login parameters, verifier definition) or document structure. None of it undermines the compatibility, opt-in and fencing goals.

## How the two reviews hold up

Every material claim I rechecked against the branch stands, with five corrections of degree. Carry the corrected wording into the design edits.

| Claim | From | Check against the branch | Verdict |
| --- | --- | --- | --- |
| Quarantine state already reaches PostgreSQL | A | `datastore_serving.rs` writes `residency_state = 'quarantined'` to `index_artifact_residency_snapshots` (migration 0026) on an integrity error | Confirmed. It is per-node monitoring state, not catalog truth |
| Exhausted jobs stay `running` and block the target | A | `claim_any` skips lapsed rows at `attempts >= max`; nothing moves them out of `running`; the open-target unique index covers `pending` and `running` | Confirmed, but not permanent: `cancel` still accepts a `running` row and frees the target. The row's state is still false until someone does |
| Cancel after publication is contradictory | A | `cancel` writes `cancelled` at once; `complete` only updates a `running` row; R6-T1 keeps both and adds a post-publication case | Confirmed as an unspecified outcome. "Live index" overstates it for mirror builds, where activation is a separate step |
| Wrong main-tree commit in the baselines | A | `c49382f` is the first docs commit on `feat/clearview`. `main` is at `bb18085` | Confirmed. The same SHA is repeated in `repository-handoffs.md` |
| 14 of 27 decisions pending | A | 27 register rows; 14 say "Hub record pending", 1 says "Measurement pending" | Confirmed. Review B's "12 rows" is a miscount |
| Vector size frozen at 1,024 | A | The text proposes 1,024 "for the first trial" and lists dimensions as an open item | Overstated. The real issue is a storage shape that hard-codes one extra dimension |
| Chart does not set the replica count | A, B | `deployment.yaml` renders `replicas` only; `values.yaml` defaults to `replicas: 2`; the binary defaults `MUNARIUM_REPLICA_COUNT` to 1 | Confirmed, and worse than either review says: the default install is already affected |
| A9 largest limit wins | A, B | `mcp-analysis.md` section 7.4, with the Legal 2,000 and Finance 8,000 example | Confirmed. The doc does give a rationale; Review B says it does not |
| 11-tool gate conflicts with the idempotency rule | B | Section 5.2 allows a state-changing tool if its outcome can be identified in a bounded way, and A5-T5 tests that | Partly. The rule has an escape hatch. The gap is that no tool lets a caller find a session after a lost `open_session` reply |
| One Mermaid diagram in 33,000 words | B | One block, in `scale-out.md` | Confirmed |
| Failed read shows as 0 in the console | B | `dashboard/monitoring.rs` uses `.unwrap_or(0)` for overview tiles | Confirmed |
| Console cookie holds the static token, no expiry | B | `dashboard/mod.rs` sets `HttpOnly; SameSite=Strict; Path=/admin`, no `Secure`, no `Max-Age` | Confirmed |

## 1. Release structure

Ship the large features dark and gate their activation, not the binary. That keeps one release, keeps every receipt, and stops known defects waiting on a Lens census.

### 1.1 Separate correctness fixes from new products \[B\]

R4 (a stale claimant can overwrite a newer attempt), R3 (unbounded PDF parse that can take down a replica), R5 (driver text in job errors), R6, R7, U5 and U9 are defects or hardening. Under the current plan none can ship until LENS-D2 passes on all 16 roots, including the restricted one.

Every large feature already has an operator switch. Use it:

- **v1.5 binary** carries phases 0 to 4, U1 to U9, SCALE-1 and SCALE-3, the Matrix transfer, and the MCP, inference, login and artifact-block code switched off.
- **Each feature becomes "supported"** only when its own receipts pass on a named build: A5 for MCP, I3 and LENS-D2 for Lens-backed retrieval, LOGIN1 for the new login, ART1 and ART2 for fleet blocking.
- **Lens releases on its own train.** Server records which Lens versions it has qualified.

If separate trains are preferred, the same split works as v1.5 and v1.6. Either way, update REL1, A5 and LENS-D1 to say "gates activation" where they now say "holds the release".

### 1.2 Put every release gate in one table \[New, B\]

REL1 is titled as the release decision but names only LOGIN1 and ART1/ART2. The other blockers are scattered: A5 in the MCP doc, LENS-D1 and LENS-D2 in the handoffs, I3 in the AIRK study, SC1 in scale-out, M1 in the handoffs, COMPAT-T1 to T5 in the README. Add one table to the README with, per gate: who needs it in v1.5, what slips if it fails, and the fallback. Today nothing says why a missing MCP client transcript should block a PDF hardening fix.

### 1.3 Replace the status line with per-phase readiness \[A, B\]

The decision register already shows which phases are blocked. Fourteen of 27 rows wait on a hub record and one on measurement. Mapped to phases:

| Phase | Hub records it waits on | Open values | Startable now |
| --- | --- | --- | --- |
| 0 Baseline freeze | None | Qualification operators, access, budget | Yes |
| M Matrix | M1 | Descriptor format | Inventory and reconciliation only |
| L Lens | SC2 | Language, runtime, model pair, dimensions, hardware | Scaffold only |
| 1 Fencing, failure codes, U5, U9 | C2 | None | Yes. R5's wire shape waits on C2 |
| 2 Retrieval disclosure | C1 | None | After C1 |
| 3 Admission, PDF | None | B3 reserve sizes, B4 limits | Yes. Limits are measured during the phase |
| 4 Cancel, fair claims, progress | None | B2 event count | After phase 1 and the job state fixes in section 2 |
| 5 MCP | A1, A2, A4, A6, A8, A9 | Concurrency default, client builds, result bounds | No |
| 6 Inference | I1, I2 | I3 thresholds, shadow allocation | No |
| 7 Console | None | Session design for U10 | U1 to U9 yes. U10 after section 3.4 |
| 8 Artifact damage | ART1, ART2 | Verifier definition | No |
| SCALE | SC2 for Lens runs | None | SCALE-1 and SCALE-3 yes |

Suggested status line: "Ready to begin phases 0, 1 and 3, the Matrix inventory and the Lens scaffold. Other phases start when their rows above clear." Make "every pending row is decided or deferred with a named owner and date" the Phase 0 exit criterion.

### 1.4 One dependency picture, named owners, sizes \[B\]

Dependencies live in three places: the phase table, the critical-path prose and sections 7 and 8.12 of the AIRK study. Replace them with one Mermaid `flowchart` covering phases 0 to 8, M, L and SCALE. Give each phase a named person and a T-shirt size. Owners are roles today, and the plan itself says no environment is booked. Number the phases in execution order; the current order is 0, M, L, 1 to 6, SCALE, 7, 8.

## 2. Defects and contradictions to fix in the design

Five places where the design is wrong about the code or inconsistent with itself. Fix these before Phase 0 closes.

### 2.1 The build-job state machine has three holes \[A, New\]

R4, R6, R8 and R9 all build on `index_build_jobs`. The design never draws its states, and three transitions are missing or undefined.

**Hole 1: no exit after the last attempt.** `claim_any` only reclaims a lapsed row while `attempts < max`. A job whose last holder crashed stays `running` with no error. The code comment says "its last error stands; an operator re-enqueues deliberately", but re-enqueue returns `AlreadyOpen` because the open-target index still counts the row. The only way out today is to cancel a job that is not running.

- Add a sweep in the claim transaction: a lapsed row at the ceiling moves to `failed`, which v1.4 readers already understand.
- Give it a C2 code such as `attempts_exhausted` and list it in the error registry.
- Add the case to R4-T1 and C2-T1, and to U3's attention strip ("failed or stuck build jobs").

**Hole 2: cancel after publication has no defined result.** R6-T1 keeps the immediate `cancelled` state for v1.4 readers and also says an already-published manifest stays. But `complete` and the proposed R4 fence only write to a `running` row. A worker that publishes and then sees the cancel can record nothing: no attempt ID, no result, no note that output exists.

- Recommended: keep `cancelled` as the visible state and add a nullable `cancel_outcome` (`stopped_before_publish`, `published_before_cancel`, `unknown`). Let the fenced holder make exactly one closing write to a `cancelled` row, matched on owner and attempt number.
- Alternative: AIRK's model, where the row stays `running` with a cancel intent until the worker acknowledges. Cleaner, but it changes what v1.4 readers see and contradicts R6-T1 as written.
- Either way, state the expected job row in R6-T1 for each of the four cut points it lists.

**Hole 3: a cancelled target can be rebuilt while the old worker is still running.** `cancelled` drops the row out of the open-target index at once. A second job for the same target can be enqueued and claimed before the first worker reaches a safe boundary. R4's fence is per job row, so it does not arbitrate between two jobs. Before R3 lands, the first worker may be inside an unbounded PDF parse.

- Either extend the open-target index to rows with a cancel still unacknowledged, or state that attempt-level leasing arbitrates and prove it.
- Add a two-job case to R6-T1: cancel, re-enqueue, claim on another builder, then let the first worker reach seal.

Also record in the rollback cutoff that a v1.4 builder returning to service is unfenced. R4 says to drain v1.4 builders first; make that a checked runbook step, and require fair scheduling and fenced progress to be off before a binary rollback. \[B\]

### 2.2 R10 misstates where quarantine lives \[A\]

`airk-analysis.md` says quarantine exists only in memory and that no quarantine state appears in `munarium-store-pg`. The serving warmer already writes `quarantined` to `index_artifact_residency_snapshots`. That row is per-node monitoring state and the migration says it is not truth, so R10's conclusion still holds. The baseline does not.

- Correct the baseline paragraph and name the snapshot as the existing observation feed.
- Decide where the fleet block is authoritative. The executor already rechecks `ArtifactState::Verified` on every query, so a catalog state transition is the cheapest enforcement point. A separate findings table then holds the evidence, not the switch.
- Say what a restarted replica does with a known-bad local copy before the block clears. ART2-T2 tests it; the plan text should state it.

### 2.3 The default chart already doubles provider rate limits \[A, B, New\]

`values.yaml` ships `replicas: 2`. The deployment template never sets `MUNARIUM_REPLICA_COUNT`, which defaults to 1. Each pod therefore enforces the full configured rpm and tpm, and the cluster-mode startup checks are not armed. This is the default install, not only a scaled one. SCALE-1 says to "keep the existing chart default on omission", which preserves the fault.

- Treat it as a defect. Render `MUNARIUM_REPLICA_COUNT` from `.Values.replicas` by default, with an explicit override key for operators who want the old behavior.
- Record it as a deliberate COMPAT-T2 exception: per-process shares halve on a default install, and the startup checks may now refuse a per-process store that was silently tolerated.
- Add a startup warning when a replica sees more live peers than its configured count. \[B\]
- Note in the autoscaling paragraph that a shared limiter is the prerequisite, since per-process shares cannot track a changing fleet. \[B\]

### 2.4 Three MCP tools have no stated recovery path \[B, refined\]

Section 5.2 forbids exposing a state-changing or budget-consuming tool unless a lost reply has a bounded way to identify its outcome, or the tool takes an idempotency key. A5 then requires all eleven tools. Review B reads this as a contradiction. It is closer to a missing table: the rule can be met without idempotency keys, but the design never shows how for each tool.

| Tool | After a lost reply, the caller can | Gap |
| --- | --- | --- |
| `ask_session` | Read the transcript with `get_session` | Works only if the bounded read reaches the newest turn. State that |
| `close_session` | Read state with `get_session`; closing twice is harmless | None if close is idempotent. State that |
| `open_session` | Nothing. The session ID was in the lost reply | No tool lists the caller's sessions. An orphan session is unreachable |
| `ask_collections` | Nothing to recover, but tokens were spent | Budget is retained under A8. The caller cannot see that it was |

- Add this table to section 5.2 with one recovery read per tool, and bind A5-T5 to it.
- For `open_session`, choose one: a caller-supplied key through the additive contract, a `list_sessions` tool limited to the caller's own sessions, or leave it out of the first profile.
- Qualify a read-only profile first (`list_collections`, `list_runbooks`, `search_collection`, `get_session`, `authorize_publication`, `get_evidence`, `read_evidence_rows`). With activation gating from section 1.1, A5 can pass per profile.
- Rename `authorize_publication`. It is a GET that grants nothing. `get_publication_reference` says what it does. \[B\]

### 2.5 Source baselines name a branch commit as the main tree \[A\]

The README and `repository-handoffs.md` both give `c49382f` as the Munarium head for the Matrix reconciliation. That is the first docs commit on `feat/clearview`. `main` is at `bb18085`, which the same table already uses for the AIRK and MCP comparisons. The trees match in code, but the SHA will not exist on `main` after a squash merge. Use `bb18085` in both places and re-pin at implementation as the text already requires.
