# AIRK patterns for Munarium Server

**Status.** v1.5 design and acceptance record, 19 November 2026. R identifies Server work, IR the Server inference integration and U the operator console. The release plan owns the implementation order and outstanding assignments. [Implementation plan](README.md) · [Open items](README.md#open-items)

Use the published v1.4 Server and SDKs for compatibility fixtures. The roadmap records the separate implementation snapshot and both AIRK revisions used in this comparison. [Source baselines](README.md#source-baselines) · [Wire evolution policy](../wire-compatibility.md)

## 1. Summary

An upgraded Server keeps the v1.4 retrieval path, wire defaults and existing cluster login until the relevant policy changes. v1.5 adds three capabilities drawn from the comparison. Callers can request search execution and per-collection outcomes. Operators can diagnose, fence, cancel and schedule builds across tenants. Replicas can protect serving capacity, isolate expensive PDF extraction and enforce a verified artifact block across the fleet.

| Work | v1.5 behavior | Principal Server component |
|---|---|---|
| [R1](#r1) | Opt-in search execution disclosure and receipts for the new inference stages | Retrieval coordinator, API types, wire adapters |
| [R2](#r2) | Explicit availability reporting within the caller's authorized scope | Session and query APIs |
| [R3](#r3) | Bounded child-process PDF extraction under a distinct, opt-in profile | Extractor, index builder |
| [R4](#r4) | Fence writes from a previous build claim | PostgreSQL job store |
| [R5](#r5) | Safe job failure messages and stable codes for classified failures | Job store and DTO |
| [R6](#r6) | Stop cancelled builds at safe boundaries using the claim fence | Builder and retrieval |
| [R7](#r7) | Classify load and optionally reserve control capacity inside each plane's ceiling. MCP depends on admission. | Serving-plane middleware |
| [R8](#r8) | Opt-in equal claim turns across tenants with eligible work | PostgreSQL job store |
| [R9](#r9) | Fenced job progress and a bounded event history for each tenant | Job store and API |
| [R10](#r10) | Fleet-wide blocking and verified recovery are required release qualifications | Artifact catalog, datastore and readers |
| [R11](#r11) | Typed retry actions are a prerequisite for MCP retry guidance | Problem types and transport adapters |
| [R12](#r12) | Validate distinct identifiers at the boundaries changed by this work | Core and store types |

Section 8 describes the Server consumer for semantic embedding and reranking. Lens owns its runtime and must qualify it before Server v1.5 releases. Section 9 covers the console, including the required shared-login upgrade and rollback. The repository handoffs define the Matrix transfer and Lens acceptance criteria. [Release requirements](README.md#rel1) · [Repository handoffs](repository-handoffs.md)

## 2. Method and limits

The historical AIRK review examined storage and publication, retrieval contracts, service runtime, inference and console behavior. AIRK paths and symbols refer to the runtime snapshot for sections 3–6 and the later snapshot for inference and console observations. The AIRK checkout and raw measurements are external to this repository. No public source link for these revisions is recorded here. Reproducing an AIRK observation requires access to that revision and, for a reported measurement, its original environment and inputs. The Server links identify the implementation to compare against the published v1.4 fixtures.

Where AIRK documents and code differed, the review used the code at the inspected revision. Examples include pack-size limits in `crates/airk-store/src/vector_cache.rs` and member concurrency in combined reads. AIRK's recorded platform and workload qualification was incomplete. Its historical CPU timings, profile limits and UI observations motivate experiments. v1.5 release evidence comes from the Server and Lens acceptance tests on their own authorized inputs. [Open implementation and qualification items](README.md#open-items)

## 3. What AIRK is, in the terms that matter here

AIRK builds a per-project index (SQLite with full-text search, plus a DiskANN graph stored as authenticated records) and serves bounded, cited evidence over a local HTTPS endpoint. Three properties shape everything it does:

- **One resident process, many in-process owners.** Indexing, watching and cleanup run as owners inside one service. A stuck owner cannot be killed without killing the service, so AIRK quarantines it instead.
- **One current index per project.** Since its 1.2 line AIRK keeps a single current index and no historical generations (`docs/retention.md`). Publication is a journaled in-place update or a sibling-directory swap.
- **Local trust.** Any process that can reach the loopback endpoint can read every index. Coordination uses advisory OS file locks.

None of these three transfers to Server, and several AIRK mechanisms exist only to compensate for them. Section 6 names those.

## 4. Recommendations

<a id="r1"></a>

### R1. Disclose search execution on request

**AIRK observation.** Every search response carries `SearchExecution { version, requested_mode, served_mode, incomplete, incomplete_reason }` (`crates/airk-core/src/search_execution.rs`). `incomplete_reason` is a closed enum. Truncation by count or bytes is a separate flag, and readiness is a third. A saved policy (`PartialHybrid`, `LexicalWhilePending`, `RequireComplete`) decides whether a default search may degrade; an explicit vector or hybrid request never degrades and fails with a typed `SearchUnavailable` instead. The effect is that a lexical-only answer can never be mistaken for a hybrid one.

**Server baseline.** `SearchResult` is `hits` plus a `ProvenanceEnvelope` of chunk ids, source ids, paths, content hashes, `index_version`, `event_watermark` and `provider_fingerprint` ([`retrieval.rs`](../../src/munarium-core/src/retrieval.rs)), and `SearchResponse` mirrors it on the wire ([`munarium-api-types/src/lib.rs`](../../src/munarium-api-types/src/lib.rs)). The envelope says what the index was. It does not say how this query ran:

- A collection built without vectors is legitimately lexical-only (`PreparedSearchQuery.embedding` is `None` by design). The caller can only infer that from every hit having `vector_rank: null`.
- A scope rolled out to the datastore and one served from PostgreSQL return the same shape. The envelope has no engine field, although [`serving.rs`](../../src/munarium-retrieval/src/serving.rs) goes to some length to keep the two engines from being confused internally.
- The datastore already produces exactly the right observations per leg, in `CandidateDiagnostics` ([`diagnostics.rs`](../../src/munarium-datastore/src/diagnostics.rs)): `requested`, `fetched`, `accepted`, `rejected`, `work_limit`, and `exhausted: Option<bool>` where `None` means unknown. They stop at the adapter and never reach the response.

**v1.5 plan.** Add an optional, versioned `execution` object beside the envelope. A caller requesting disclosure receives the serving engine, actual legs, per-leg candidate limit, observed fetch count and exhaustion state. Use unknown for an unmeasured count or exhaustion state, even on an empty result. Omission preserves the v1.4 response, including existing goldens. For typed gRPC allocate fresh tags with presence and test the native bridge separately. Retain the old envelope on the legacy path.

**Acceptance criteria**

<a id="r1-t1"></a>

**R1-T1.** Compare the same authorized search on PostgreSQL and Datastore with and without disclosure. Assert the actual engine and legs, unknown PostgreSQL exhaustion where unobserved, and exact v1.4 shapes on omission. Check REST, typed gRPC, the native bridge and the four SDKs. A configured enrolled-stage fallback carries the [I1](#i1) disclosure.

<a id="r2"></a>

### R2. Report outcomes for authorized members of a multi-collection read

**AIRK observation.** A combined read returns `CombinedReadResult` with one `ComposedMember` per index. Each member carries its budget `allocation`, its unmodified native `result`, `elapsed_ms`, and `outcome: MemberOutcome::{Complete, Exhausted, Failed, Unqueried}` (`crates/airk-kernel/src/combined_reads.rs`). The caller chooses `MemberAvailability::Report` (return what succeeded and say what did not) or `Require` (fail the whole read with `member_unavailable` and the list of failed members). Budgets (`limit`, bytes, deadline) are split across members deterministically by `SharedBudgets::allocate`, in canonical id order, so a rerun divides the same way.

**Server baseline.** `search_collections_bounded` in [`sessions_api.rs`](../../src/munarium-server/src/sessions_api.rs) fans out with a semaphore and joins every task, which is sound. What the caller learns afterward is coarser:

- A member that returns `KernelError::NotFound` is added to `skipped`, a list of collection names with no reason attached.
- Any other member error returns from the whole operation (`Err(e) => return Err(e.into())`), discarding the members that succeeded.
- There is no shared deadline. Each member runs under whatever the request as a whole is allowed.

<a id="c1"></a>

**v1.5 plan.** C1 specifies explicit availability reporting. Omission of an availability policy keeps the v1.4 behavior, including the `NotFound` skip and failure on other member errors. Preserve `skipped` and its existing meanings. A stricter `require` policy, if offered, must be explicit.

With explicit `report`, an unavailable optional collection leaves verified, authorized results from the other members available. Report the gap only within the caller's visible scope. Apply authorization and scope-readability checks before composing the response. An authorized search that completes with zero hits has a successful empty outcome. An unavailable authorized member has an incomplete outcome and a content-free reason. A hidden collection contributes no outcome, name, count or diagnostic. Distinguish selection by the existing routing probe from a deep search that failed.

**Evidence rule.** The v1.4 hierarchy already excludes document hits from proving exhaustive coverage, even when every collection responds. Preserve evidence-kind and pin checks on each assertion. An independently complete count can still support a claim about its own authorized scope. An assertion spanning an incomplete fan-out needs separate, complete evidence for that whole scope. [Evidence hierarchy and completeness tests](../../src/munarium-core/src/hierarchy.rs)

**Acceptance criteria**

<a id="c1-t1"></a>

**C1-T1.** Withhold each claim that depends on a missing member, including absence and exhaustive coverage across that scope. If the requested answer requires the missing evidence, refuse that answer. Separately verified citations and claims about a narrower, independently supported scope remain available.

<a id="c1-t2"></a>

**C1-T2.** Exercise both unary and streamed turns with the same good result paired in turn with an unavailable optional member, missing required evidence, a hidden member and an authorized empty result. Check the final answer, citations, completeness fields and disclosures in each case.

<a id="r3"></a>

### R3. Run PDF text extraction in a resource-limited child process

**AIRK observation.** The PDF parser runs as a child of the service's own executable (`std::env::current_exe()` with a private role argument) under `ProcessLimits { memory_bytes: 512 MiB, cpu_seconds: 10 }`, with output capped at 64 MiB and a page cap (`crates/airk-ingest/src/pdf_worker.rs`, `provider_process.rs`). Limits are applied through a Job Object on Windows and `setrlimit` on Unix (`crates/airk-process/src/child_group.rs`). A service-wide `PdfSlot` admits one parse at a time with FIFO order and a 60-second wait; its `admission_step` is a pure function over an injected clock. A document that exceeds its budget is recorded with the skip code `extraction_budget_exceeded`, distinct from `extraction_failed`, and counted in coverage. AIRK's documents are candid that Office parsing is still in-process and that in-process parser calls "are not forcibly preempted".

**Server baseline.** [`pdf.rs`](../../src/munarium-extract/src/pdf.rs) wraps `pdf_extract::extract_text_from_mem` in `catch_unwind`, and `extract_source` in [`munarium-retrieval-pg/src/lib.rs`](../../src/munarium-retrieval-pg/src/lib.rs) moves it to the blocking pool. The comment there names the remaining gap precisely: "a PDF parse with no time bound". Three consequences follow:

- A blocking task cannot be cancelled. A pathological PDF occupies a blocking thread until it finishes or the process exits, and a build that awaits it holds its job lease throughout.
- `catch_unwind` contains a panic. It does not contain unbounded allocation, and an out-of-memory kill takes the whole replica, including its serving planes.
- The input is tenant-supplied, up to the 256 MiB source ceiling.

DOCX is in better shape: [`docx.rs`](../../src/munarium-extract/src/docx.rs) bounds the inflated XML at 128 MiB and checks both the declared and the actual size.

<a id="b4"></a>

**v1.5 plan.** B4 leaves the numerical limits to measurement. Opt in to a bounded extraction profile. The parent re-executes the existing binary with a private extraction role, a minimal environment, bounded input/output, OS-enforced memory and CPU limits, a wall-clock deadline and a small concurrency cap. Kill and reap the child on timeout or cancellation. Distinguish a parser failure, a document over budget and work deferred by admission in the new profile's diagnostics. Paid document intelligence still requires the collection's explicit configuration, eligible input and available budget.

Use the same executable in the distroless image. The Linux prototype can apply OS limits before executing the parser. Qualify the mechanism independently on each supported platform.

**Acceptance criteria**

<a id="b4-t1"></a>

**B4-T1.** Leave the legacy extractor identity and output available to v1.4 rebuilds. Give the opt-in profile a new extractor identity covering settings that change indexed output. Rebuild side by side and require ordinary verification and activation before an index switches. Qualify limits against representative public PDFs on the supported platforms with tests for each bound, child cleanup and zero unapproved escalation calls. Record CPU, memory, elapsed time, output size, concurrency and service overhead on representative normal and near-limit inputs. Freeze the production limits only after reviewing those results. [Extractor version source](../../src/munarium-extract/src/lib.rs)

<a id="r4"></a>

### R4. Add a fencing generation to build jobs

**AIRK observation.** Every claim, completion, progress update and recovery is checked against a per-project `fencing_generation` that increments each time a worker is admitted (`crates/airk-store/src/service/registry/jobs.rs`, `registry.rs`). A stale holder gets `JobError::StaleWorker`. Identity of the holder is not enough; the generation is what makes a late write from a previous holder harmless.

**Server baseline.** [`jobs.rs`](../../src/munarium-store-pg/src/jobs.rs) guards `heartbeat` and `complete` with `state = 'running' AND claimed_by = $node`. The node id is `Config.instance_id`, resolved from `MUNARIUM_INSTANCE_ID`, then `HOSTNAME`, then `COMPUTERNAME` ([`config.rs`](../../src/munarium-server/src/config.rs)). The guard therefore holds between two differently named replicas and fails in these cases:

- A replica restarts under the same name (a StatefulSet pod, a compose service, or an operator-set `MUNARIUM_INSTANCE_ID`) and reclaims its own lapsed job. Both the old and the new execution satisfy `claimed_by`.
- One process claims the same job twice after a lapse, which the claim query allows whenever `attempts < max`.

In both, a slow first attempt can complete over the second.

**v1.5 plan.** `claim_any` increments `attempts` in its transaction but reads the returned job after commit. Capture the claimed row and generation with `UPDATE ... RETURNING` inside that transaction. This prevents a delayed read from picking up a later claimant's generation. Carry the captured value through the builder loop and require matching tenant, job, running state, owner and attempt number on heartbeat, progress and completion. An old claimant receives a stale/no-op outcome. R4 uses the existing counter and needs no migration. [Builder loop](../../src/munarium-server/src/datastore_jobs.rs)

Drain builders running v1.4 before relying on the new fence. Their UPDATE statements still check only node and state. Artifact attempts have a separate lifecycle. A reclaim expires the old running row and generates a fresh `att-` UUID. Heartbeats and transitions target the attempt ID within the tenant.

**Acceptance criteria**

<a id="r4-t1"></a>

**R4-T1.** Exercise both a same-node job reclaim and a distinct attempt ID in PostgreSQL, including sealed-attempt reconciliation. A passing job-row test alone says nothing about artifact publication. [Artifact attempts](../../src/munarium-store-pg/src/attempts.rs)

<a id="r5"></a>

### R5. Return stable failure codes for build jobs

**AIRK observation.** `JobOutcome::Failed { code }` must be at most 128 characters of `[a-z0-9_]` and is validated on write. Log records obey the same rule (`crates/airk-core/src/service_logs.rs`). The raw error chain goes to a separate local ring that is excluded from reports and from every client-facing response. Workers map error types to codes by downcast, never by matching message text.

**Server baseline.** The builder loop calls `run_one_job(...).map_err(|e| e.to_string())` and stores the string in `index_build_jobs.error`; `job_dto` returns that column unchanged in `IndexBuildJobDto.error` ([`datastore_jobs.rs`](../../src/munarium-server/src/datastore_jobs.rs)). For `KernelError::Storage`, the string is the database driver's message. Server has already decided that is not client-facing: `client_facing_error` in [`error.rs`](../../src/munarium-server/src/error.rs) replaces a storage error's text with "storage error; see the server log" for per-item ingest results, for the stated reason that the message carries table, column and constraint names. The job path does not go through it.

<a id="c2"></a>

**v1.5 plan.** Under C2, the existing `error` field carries a safe human-readable message. Only genuinely classified failures receive an `error_code`. Capture that stable machine-readable code from the typed cause when recording the failure and store it in a nullable field. For a historical failed row containing only diagnostic text, return a generic build-failure message in `error` and omit the `error_code` member entirely. The same rule applies to a free-text failure written by a v1.4 worker during rollout.

Leave historical rows intact. The read path must never infer a code by parsing old messages.

**Acceptance criteria**

<a id="c2-t1"></a>

**C2-T1.** Exercise both list and get with a seeded historical failure, a v1.4 writer's failure during rollout, a new typed failure and a successful job. Check exact member presence, safe text, the documented code for the classified case, and the existing omission of failure fields on success. Document the change in displayed text and check the typed mappings against the error registry. [Error registry](../api/errors.md) · [Registry checks](../../src/munarium-server/src/docs_coverage.rs)

<a id="r6"></a>

### R6. Make cancellation of a running build cooperative

**AIRK observation.** Cancelling a running job inserts a durable cancellation intent; the job stays `running` until its fenced worker reaches a safe boundary and completes it as cancelled. A cancel that arrives after an atomic publication leaves that publication committed.

**Server baseline.** `BuildJobs::cancel` sets `state = 'cancelled'` for pending and running jobs. For a running job, the next heartbeat returns `false`, the heartbeat task logs and stops, and nothing else happens: `run_one_job` is not signalled and runs to the end, after which `complete` is a no-op. The doc comment on `cancel` describes this accurately ("cancellation stops the JOB from being re-offered"). An operator who cancels a long direct build to free a builder gets the builder back only when the build would have finished anyway.

**v1.5 plan.** A cancellation token created per claimed job, cancelled by the heartbeat task when it observes a lost or cancelled lease, and checked at the build's natural boundaries: between sources during extraction, between embedding batches, before seal, before upload. The mirror build already has a strict ordering in [`mirror.rs`](../../src/munarium-retrieval/src/mirror.rs) in which the manifest is written last, so stopping before that point leaves nothing advertised.

**Acceptance criteria**

<a id="r6-t1"></a>

**R6-T1.** Preserve the cancel response and the immediate `cancelled` state visible to v1.4 readers. A fenced worker releases its slot after reaching a safe boundary. Exercise cancellation before and after seal, upload and the publication commit. Remove only staging data owned by that attempt. An already-published manifest and its audit history remain available to recovery. A failed heartbeat read is an uncertain lease, and a worker must revalidate its fence before a shared write.

<a id="r7"></a>

### R7. Admit by work class, with a slot reserved for control

**AIRK observation.** One typed setting, `query_concurrency`, accepts only 4, 8, 16, 32 or 64. The broker allocates one more than the setting and refuses ordinary queries at `allowance - 1`, so a status or control request always has a slot (`crates/airk-kernel/src/service_broker.rs`). Heavy work is admitted separately through `WorkAllowance` grants that name the exact job and carry thread, working-memory and background-IO amounts; candidates are ranked so that a waiter older than five seconds moves to the front (`crates/airk-kernel/src/service_resources.rs`). Lowering a limit never revokes a grant: existing charges remain and new grants stop until the total drains below the new value.

**Server baseline.** [`state.rs`](../../src/munarium-server/src/state.rs) creates `rest_permits` and `grpc_permits`, each sized `max_concurrency`, and [`middleware.rs`](../../src/munarium-server/src/middleware.rs) takes one with `try_acquire_owned` for every `/v1/` and `/v1.2/` request, shedding with 503 and `Retry-After: 1` at the ceiling. Shedding early instead of queueing is the right default. Two properties of the single pool are worth changing:

- **Control shares the pool with load.** Health and metrics are on the separate ops port and are safe. But the operator's levers are on the REST plane: `PUT /v1/retrieval-rollout`, job cancel, artifact promotion, token revocation. When queries saturate the pool, the request that would relieve the saturation is shed with them.
- **A permit is a permit.** A point read and a synchronous index build each cost one. The synchronous build endpoints deliberately remain ([`datastore_jobs.rs`](../../src/munarium-server/src/datastore_jobs.rs) explains why), so `max_concurrency` inline builds can hold every permit while each also occupies a blocking thread.

<a id="b3"></a>

**v1.5 plan.** Under B3, keep each plane's configured concurrency ceiling as the limit across all its work classes. When the operator enables a control reserve, allocate those slots within that ceiling and leave at least one slot available for ordinary work. Bound heavy requests inside the same overall limit. Reject an opt-in reserve that exhausts ordinary capacity, such as one reserved slot on a one-slot plane. A v1.4 configuration with a one-slot ceiling and no reserve remains valid.

Reserve eligibility requires the route's existing authentication and authorization, including the write role for job cancellation. Classify typed gRPC, the native bridge and MCP dispatch consistently and charge nested dispatch only once per applicable resource. A decrease in a configured limit drains existing work.

**Acceptance criteria**

<a id="b3-t1"></a>

**B3-T1.** Saturate ordinary admission on a configured plane. Verify the next ordinary request is shed, an authorized control operation can use its reserve and a caller without the required authority cannot consume reserved capacity. Track concurrent requests across classes and verify their combined count never exceeds the plane's configured ceiling. Exercise REST and gRPC, including a heavy request path, a valid legacy single-slot configuration and rejection of an infeasible opt-in reserve.

<a id="b3-t2"></a>

**B3-T2.** Review the route classification table against the actual registered routes, including nested dispatch, and verify that an unclassified route cannot obtain a reserved slot. Size the pod for its own resource limits.

<a id="r8"></a>

### R8. Claim build jobs fairly across tenants

**AIRK observation.** Work candidates are ranked by kind, then by whether the project was the last one served, then by sequence, with the aging rule from R7. Per-project ordering is preserved; global ordering is not strictly FIFO.

**Server baseline.** `claim_any` selects with `ORDER BY created_at ... FOR UPDATE SKIP LOCKED LIMIT 1`, globally across tenants. The module comment explains why the claim is global (a builder serves every tenant). The ordering has a side effect the comment does not discuss: a tenant that enqueues several hundred backfill jobs is served to completion before a job any other tenant enqueues afterward.

<a id="b1"></a>

**v1.5 plan.** Under B1, keep the v1.4 global FIFO policy unless an operator explicitly enables tenant-fair scheduling. In fair mode, give each tenant with eligible work an equal turn. A turn is one successfully committed job claim. Within the selected tenant, take the oldest eligible job and use a stable job-ID tie-break. Different build durations can produce different shares of worker time even when claim counts are equal.

Advance the tenant's last-served position and the R4 claim generation in the claim transaction. A failed or rolled-back claim leaves the turn available. Retain the current eligibility rules for pending jobs and expired leases, the reclaim attempt ceiling and stale-worker fences.

**Acceptance criteria**

<a id="b1-t1"></a>

**B1-T1.** Start with A, B and C each continuously eligible, with at least three jobs apiece, no locks and one builder. Give A a much larger backlog and longer jobs. Across nine successful claims each tenant must receive three, with one per round and its own oldest eligible job first. Check the committed claim order and counts independently of finish times.

<a id="b1-t2"></a>

**B1-T2.** Repeat with competing builders, a temporarily locked candidate, a rolled-back claim and a reclaimed lease. Verify progress after locks clear, single ownership per generation and rejection of stale writes. With fair scheduling disabled, verify the v1.4 FIFO fixture. Measure claim cost at representative queue depths.

<a id="r9"></a>

### R9. Give jobs progress and a transactional event ring

**AIRK observation.** Two mechanisms.

- **Progress** is one bounded observation per project, `WorkProgress`, in which totals are `Option`: an unknown total is null, never zero and never an estimate. Updates are fenced and counters may not decrease. Why a job is waiting travels separately as a transient `WaitObservation { job, reason, age_ms }` with a closed `WaitReason` enum, and is never persisted.
- **Events** are written by database triggers on the job and project tables, so an event commits or rolls back with the state change that caused it and a replayed request cannot create a duplicate. A trigger also bounds the table to the newest 1,024 rows. Readers page with a cursor that pins a snapshot, and the page reports `retained_prefix_lost` when older events have been pruned (`crates/airk-store/src/service/registry/events.rs`).

**Server baseline.** A build job exposes `state`, `attempts`, `claimed_by` and, at the end, `result` or `error`. Between claim and completion there is nothing to observe except the heartbeat moving `claimed_at`. Builds on real corpora run for a long time; the heartbeat comment in [`jobs.rs`](../../src/munarium-store-pg/src/jobs.rs) cites the newspaper batches as longer than the lease. An operator cannot distinguish a build that is embedding its last batch from one stuck on its first source.

**v1.5 plan.** A small fenced progress column on the job row (phase, units done, units total where known, last advance time), updated at the same boundaries R6 uses. Separately, an append-only `index_build_job_events` table populated by triggers on `index_build_jobs`, with the gap flag on read.

**Atomicity.** A PostgreSQL trigger on meaningful state transitions can cover both v1.4 and v1.5 writers. This schema already uses triggers. Skip heartbeat-only writes and coalesce progress observations to keep volume bounded. Verify rollback, replay and terminal-state races with two writers. Describe separately which transitions a v1.4 writer can report. [Governance migration](../../src/munarium-store-pg/migrations/0034_collection_governance.sql)

<a id="b2"></a>

Under B2, retain the newest bounded count of build events independently for each tenant. Measure event rates, row and index storage, read and prune cost, and the history window operators need before selecting the count. Record the measured bound in the retention inventory. When older events for the requested history have expired, report the gap explicitly, including when no rows in the requested window survive. The job row keeps its final state and outcome independently of the event history. [Retention inventory](../ops/retention-inventory.md)

**Acceptance criteria**

<a id="b2-t1"></a>

**B2-T1.** Interleave events from tenants A and B. Push A beyond a test bound while B stays below it. Verify A retains exactly its newest events, B keeps its complete history, and both job rows still report their recorded final states.

<a id="b2-t2"></a>

**B2-T2.** Bind pagination cursors to their tenant and snapshot. A page request crossing the retention boundary must identify the gap. Test a current cursor, an expired cursor and a cursor from another tenant, including pruning between page reads and concurrent commits. Preserve the existing PostgreSQL-only build queue and its documented memory-mode response. [Build job routes](../../src/munarium-server/src/datastore_jobs.rs)

<a id="r10"></a>

### R10. Record artifact damage durably, with a bound and without overclaiming

**AIRK observation.** Detected damage is written to an `IntegrityObservation` receipt holding at most 32 `KnownDamage` entries. When full, a new record is refused with "repair required" instead of evicting an old one. The receipt's `PhysicalAudit` type has a single variant, `Unknown`: the absence of recorded damage is never presented as a clean audit (`crates/airk-store/src/service/collection/integrity.rs`). Verification detects and records; it never authorises an automatic repair.

**Server baseline.** [`hydrate.rs`](../../src/munarium-datastore/src/hydrate.rs) quarantines a corrupt cache key in `CacheState.quarantined`, an in-memory set, keyed correctly by the full `ArtifactCacheKey` so one tenant's corruption does not affect another's identical content. No quarantine state appears in `munarium-store-pg`. So:

- A restart forgets the quarantine. The replica hydrates the same bytes again, verifies them again and fails again.
- Each replica discovers the damage independently. The catalog continues to show the artifact as `verified`, and other replicas keep routing to it.
- The executor does recheck `ArtifactState::Verified` on every query ([`executor.rs`](../../src/munarium-retrieval/src/executor.rs)), so a catalog state change would take effect everywhere at once. Nothing currently makes that change when a replica finds corruption.

**v1.5 plan.** Store the tenant, pinned artifact and component, expected digest and each verifier's identity, fetch time and observed digest. Apply the shared block at the existing verification check once the two observations satisfy ART1. Bound the register and preserve existing findings when it is full. Report the absence of a finding as unknown integrity.

<a id="art1"></a>

**ART1.** The first integrity failure immediately isolates the affected local copy. Fleet-wide blocking requires two independent verifiers to fetch the same pinned object directly from the authoritative store. Each must compute a digest that differs from the expected digest for that pin. Count a verifier's observation once. A duplicate report, a retry by the same verifier, a timeout or observations for different pins cannot establish this condition. Implement and qualify the rule before Server v1.5 releases.

<a id="art2"></a>

**ART2.** After a separately authorized repair, two independent verifiers must each fetch the repaired object from the authoritative store and compute a digest matching the trusted pin. Bind both observations to the tenant, object, component, active damage record and current repair attempt. Once both match, the preauthorized repair workflow automatically clears that block and durably records the two verifications, workflow identity and clearance. Its clearance authority is scoped to that record. Permission to change object bytes is separate.

Keep the v1.4 default through a mixed-version upgrade. Activate fleet blocking and automatic clearance only when every serving reader can enforce the durable state or incompatible readers are drained. Demonstrate healthy reads on every serving replica after verified clearance. Rollback must keep an outstanding block effective or keep the affected artifact withdrawn before a v1.4 reader returns to service. [Release requirements](README.md#rel1)

**Acceptance criteria**

<a id="art1-t1"></a>

**ART1-T1.** Pair each insufficient case above with a test that the local copy stays isolated and no fleet block is issued. Also test a clean authoritative fetch after local cache damage.

<a id="art1-t2"></a>

**ART1-T2.** Once two independent fetches confirm mismatches for the same pin, all active readers must refuse that artifact, including on a warm cache, after restart and during a concurrent read on another replica. Test tenant isolation and a full register that preserves existing findings and fails closed for the newly confirmed damage.

<a id="art2-t1"></a>

**ART2-T1.** Make clearance and its audit entry one atomic, idempotent transition. Duplicate or pre-repair observations, reports for a different pin or incident, a timeout, and two successes from the same verifier leave the block in force. A fresh mismatch during verification invalidates prior successes for that repair attempt.

<a id="art2-t2"></a>

**ART2-T2.** Test a concurrent failure, repeated workflow delivery, restart, and a late report from an earlier incident. A new failure after clearance follows ART1 again. Each replica must verify its own quarantined copy before serving it. Exercise verified recovery across every serving reader and rollback with an outstanding block.

<a id="r11"></a>

### R11. Typed retry guidance on problem responses

**AIRK observation.** `ReadRetryHint { status, retryable, code, retry_after_ms, action }` is produced only by downcasting to a known typed error; text that merely looks like a busy error is never retryable (`crates/airk-kernel/src/retrieval_retry.rs`). The `action` field separates "repeat the same request" from "retry later without replay". On the admin contract, validation rejects a body that carries `retry_after_ms` on a non-retryable error.

**Server baseline.** `Problem` ([`munarium-api-types/src/lib.rs`](../../src/munarium-api-types/src/lib.rs)) has no retry member. Retry semantics are conveyed by status code, a `Retry-After` header on 429 and on shed 503, and prose in the error registry. The guarded command recovery document has to state in words that `command-unresolved` "is **not** a retryable head conflict" ([`command-recovery.md`](../ops/command-recovery.md)), because a client cannot tell the two 409s apart mechanically except by slug.

**v1.5 plan.** Add optional `retryable` and `retry_action` to REST problems and gRPC `ErrorInfo` metadata. A head conflict calls for a fresh read and decision. An unresolved command calls for inspecting the recorded outcome. A retry of a shed or rate-limited request requires evidence it was rejected before execution, a safe read or documented idempotency. Respect existing `Retry-After` headers and idempotency keys. An unknown or absent action authorizes no automated replay. This mapping supplies the recovery actions consumed by MCP. [Problem registry](../api/errors.md)

**Acceptance criteria**

<a id="r11-t1"></a>

**R11-T1.** Exercise every registered problem slug against REST and gRPC. Include the two 409 actions, a shed request, a missing or unknown action, and a v1.4 client. Verify the recovery hint, header and actor behavior together, including zero automatic replay of an unresolved command.

<a id="r12"></a>

### R12. Validating identifier newtypes at new boundaries

**AIRK observation.** A `hex_identity!` macro produces opaque id types that validate on deserialisation through `#[serde(try_from = "String")]` (`crates/airk-core/src/identity.rs`); `catalog_id!` produces prefixed ids such as `idx_` and `grt_`. Uncertain states are enum variants: `FenceUnknown`, `OutcomeUnknown`, `MutationPossible::{Yes, No, Maybe, Unknown}`.

**Server baseline.** Mixed. `ArtifactCacheKey` is a proper composite key and its documentation explains exactly why. `JobKind` is an enum with `parse`. But `BuildJob` in [`jobs.rs`](../../src/munarium-store-pg/src/jobs.rs) carries `tenant_id`, `job_id`, `kind` and `state` as `String`, the DTO's `state` is a `String` documented by a comment listing its six values, and most function signatures across the store take `tenant_id: &str` next to other `&str` identifiers. Swapping two arguments compiles.

**v1.5 plan.** Use distinct types at new call boundaries touched by R4 to R10. Preserve the documented v1.4 wire and storage domains, including previously issued opaque IDs. A new validator requires old-data fixtures proving it accepts those IDs. Limit stronger syntax rules to genuinely new types and endpoints.

An OpenAPI generator can omit a constraint enforced only by a deserializer. Declare each new wire type's documented constraint in its schema.

**Acceptance criteria**

<a id="r12-t1"></a>

**R12-T1.** Round-trip previously issued opaque IDs through the changed boundary, exercise a valid and invalid value for each new constrained type, and check the generated OpenAPI schemas against the actual validators.

## 5. Already present in Server

These AIRK patterns were examined and found to have a Server equivalent of comparable or greater strength. They are listed so they are not proposed again.

| AIRK pattern | Server equivalent |
|---|---|
| Fallback is disclosed, never substituted | The no-fallback rule in [`serving.rs`](../../src/munarium-retrieval/src/serving.rs) and `KernelError::DatastoreUnavailable`; "serving refuses, mirror and shadow degrade" in [`capabilities.rs`](../../src/munarium-retrieval/src/capabilities.rs) |
| Publication order as the safety property; pointer swap; conservative recovery | Manifest written last and read back in [`mirror.rs`](../../src/munarium-retrieval/src/mirror.rs); `.partial` then atomic rename then `COMPLETE` marker in [`hydrate.rs`](../../src/munarium-datastore/src/hydrate.rs); compare-and-swap promotion on two generations |
| Format identity independent of application version | `artifact@1`, `idx2-` logical ids free of the engine, `ReaderCapabilities` with format range and feature bits in [`verify.rs`](../../src/munarium-datastore/src/verify.rs) |
| Limits applied before allocation | `Limits` in [`verify.rs`](../../src/munarium-datastore/src/verify.rs) |
| Unknown outcome is its own state and never authorises replay | `guarded-v1` command recovery and `command-unresolved` ([`command-recovery.md`](../ops/command-recovery.md)); Server's version is stronger because the claim is durable server-side before execution |
| Acceptance and completion as separate facts | Job versus attempt in [`jobs.rs`](../../src/munarium-store-pg/src/jobs.rs); enqueue deduplicates against open jobs for the same target |
| Self re-exec child killed at a named stage | The `barrier(phase)` harness in [`munarium-store-pg/src/crash_recovery.rs`](../../src/munarium-store-pg/src/crash_recovery.rs) and [`munarium-server/src/crash_recovery.rs`](../../src/munarium-server/src/crash_recovery.rs); the supervision child test |
| Residency classes and watermark eviction | `Residency` and `CacheBudget` in [`hydrate.rs`](../../src/munarium-datastore/src/hydrate.rs), which AIRK has no counterpart for |
| Rank fusion across members, never raw scores | Engine-neutral pooled fusion in [`fusion.rs`](../../src/munarium-datastore/src/fusion.rs) and the probe-stratum rule in [`retrieval.rs`](../../src/munarium-core/src/retrieval.rs) |
| Panic and lint policy | Server denies `unwrap`, `expect`, `panic` and related lints per crate outside tests. AIRK has no workspace lint table and no crate-level deny attributes; here Server is ahead |
| Enforced crate boundary | The CI-checked independence of `munarium-datastore`. AIRK's allowlist is broader but was seeded from whatever edges existed, so it freezes sprawl instead of expressing a design |
| Code registry checked against documents | `every_problem_slug_is_in_the_error_registry` in [`docs_coverage.rs`](../../src/munarium-server/src/docs_coverage.rs). AIRK checks code enums against fixtures but has no mechanical check against its documents |

## 6. Do not borrow

**Mechanisms that exist because AIRK is local and single-process.**

- Advisory OS file locks as leases, gates and writer locks. Server's equivalents are row locks and advisory locks in PostgreSQL, and they are already in use.
- The budget-enforcing SQLite VFS, WAL allowances and the on-disk quota ledger. PostgreSQL owns its WAL. The underlying idea (reserve space durably before the first write) is sound, and the one place it might apply is the builder's local staging directory, which this review did not find any free-space check for. That is a possible follow-up, not a recommendation.
- Quarantining a stuck in-process owner and leaking its thread until a manual service restart. A Server replica that cannot drain should exit and be replaced, which [`supervision.rs`](../../src/munarium-server/src/supervision.rs) already arranges.
- The single-current-index model with in-place journaled publication. Server's immutable artifacts with retained versions are what AIRK had in its v2 line and gave up. For reproducing past answers, Server's model is the stronger of the two.
- Verifying cited sources by hashing local files. Server's sources are content-addressed in a store it controls, and the hash is checked before commit.
- Unauthenticated loopback access. AIRK removed agent tokens in favour of "any local process can read every index". That is a defensible choice for a desktop tool and the opposite of Server's tenant, level and compartment model.

**Mechanisms that conflict with stateless replicas.**

- Terse mode's per-client status memory, in which the server remembers which readiness facts a client has already seen and omits repeats. The stateless half of the idea (project one canonical result into a compact form after the handler runs, so the two forms cannot diverge) is fine if Server ever wants a compact representation.
- The client-side request journal as files in a control directory. Server's idempotency keys and durable claims put this state where it belongs for a cluster.

**Practices AIRK is visibly paying for.**

- **Narrative ledgers.** `docs/simplify/one-process-ledger.md` is about 790 KB. AIRK's README carries roughly 200 lines of dated historical notices ahead of its description of the product. The registers that have no check mode have already decayed: the ignored-test inventory reports 64 entries against about 160 in the tree. The lesson is the inverse of the practice: a register is worth keeping only if a test fails when it goes stale. Server's retention inventory and gate catalog already follow that rule.
- **Bulk supply-chain exemptions.** AIRK's own README records 319 of 366 third-party versions as "mechanically recorded, unreviewed". The lockfile identity guard is real protection; the exemption list is ceremony that every dependency bump must touch in three files.
- **A patch version per local build.** The version reached 1.8.207 this way, and AIRK's versioning documents are mostly superseded history under a short current header.
- **Retrofitted visibility.** AIRK carries scripts named `widen_visibility.py` and `widen_glob_imports.py`, and a baseline that records a 20-module cycle in its core crate. Server's smaller crates with declared forbidden dependencies are the better starting point.

**Considered and set aside.**

- *Revision-bound continuation cursors.* AIRK binds a cursor to the publication token, tool and filters, and rejects it when any changes. Server search has `top_k` and no continuation, so there is nothing to bind. If paged retrieval is ever added, bind the cursor to `index_version` and the prepared-query digest from the first day.
- *Bit-exact verification of cached embeddings on rebuild.* Preserve the existing deterministic hash path. For a future endpoint-backed index, verify the stored bytes against their artifact digests and test model repeatability separately.
- *Bounded change retention with coverage floors.* The idea that pruned history must report where complete coverage begins is good, and Server's append-only ledger does not prune in the way that makes it necessary.
- *Arbiter's in-process runtime and option-scoring answer model.* Server uses an endpoint for semantic embedding and bounded passage reranking. Section 8 carries forward Arbiter's permutation checks, per-collection enrollment and typed refusals. Lens owns runtime packaging and model qualification.

## 7. Suggested order

Use the [implementation plan](README.md) to schedule the work. R4 precedes cooperative cancellation and fenced progress. R1 supplies receipts for IR-2 and IR-3. R7 and R11 must be ready before enabling MCP admission and retry guidance. [REL1](README.md#rel1) requires both R10 fleet-wide corruption blocking and U10 cluster login to pass qualification. Exercise [ART1](#art1), [ART2](#art2) and [LOGIN1](#login1) in a mixed-version rollout. Apply R12 at a touched boundary and U6 on each redesigned page.

For each wire change, record the affected REST, typed gRPC and native bridge shapes, the prior/default encoding and what a v1.4 reader does with an unfamiliar value. Add fixtures in both directions to each affected SDK and retain the existing contract goldens. Shared semantics require a hub decision and the documented contract export process.

## 8. Semantic embedding and reranking through an inference endpoint

Server v1.5 adds a vendor-neutral endpoint for semantic embeddings and passage reranking. Operators can continue using the v1.4 hash retrieval path. The release depends on a qualified Lens image and its end-to-end exercise against the workspace. AIRK's Arbiter study informs Server's bounded reranking and typed refusals. The source baselines identify which external results are available for comparison. [Lens completion](repository-handoffs.md#lens-d1) · [Workspace qualification](repository-handoffs.md#lens-d2) · [Source baselines](README.md#source-baselines)

### 8.1 Conclusions first

1. **Evaluate the vector leg first.** Server uses the 256-dimensional `local-hash@1` feature hash for indexing and query retrieval. Semantic embeddings could recover relevant passages that share few query terms. The size of that gain and the cost per query need measurement against the hash baseline. [Retrieval implementation](../../src/munarium-retrieval-pg/src/lib.rs)
2. **A reranker is the natural second step.** AIRK's "Focus" stage scores each retrieved passage against the query with a 0.6B-parameter Qwen reranker and reorders the top of the list. It needs no reindex, it is additive, and it can be switched on per collection.
3. **Keep inference behind the provider gateway.** Add embeddings and rerank operations at the retrieval coordinator, with transport-neutral types. Server continues to ship its static binary. A deployment with no new settings keeps the v1.4 retrieval path (8.6).
4. **The operator's own endpoint is the primary deployment model.** An operator running vLLM, or another server implementing the same two routes, on their own hardware configures Server to use it. Retrieval then sends nothing to a public model service (8.7).
5. **Lens completion gates the Server v1.5 release.** The Lens repository owns the scaffold, real embeddings and rerank image, precision and retrieval qualification, and evaluated retrieval instructions. Server owns the vendor-neutral consumer and probe. Lens remains optional for operators upgrading an existing deployment. [Lens work and acceptance gates](repository-handoffs.md#lens-foundation-lens-0)
6. **Qualify the serving profile on the target workload.** AIRK recorded exploratory and component measurements but lacked a release-qualified profile. Evaluate the candidate on held-out documentation and source code under [I3](#i3).

### 8.2 What AIRK's Arbiter is

Arbiter is off by default and has two independent stages, each with its own model, enablement, device policy and per-index enrollment. A third model, the bundled embedder, is not part of Arbiter but completes the picture.

| Role | Model (pinned upstream revision) | What it does | Shipped precision and size | Generates text |
|---|---|---|---|---|
| Embedding (always on) | `all-MiniLM-L6-v2` | 384-dimension passage and query vectors for the vector leg | Mixed INT8, about 69 MB, bundled in the installer | No |
| Focus (optional) | `Qwen3-Reranker-0.6B` | Scores one query and one passage per forward pass; reorders the head of the fused list | 8-bit block-quantized weights with FP32 activations, 1.12 GB, downloaded | No |
| Context Compact (optional) | `Qwen3-1.7B` | Picks one option from a closed list of at most eight ("read this next", "sufficient", "clarify") | Exact FP32, 6.88 GB, downloaded | No |
| Context Large (optional) | `Qwen3-4B` | Same task, larger model | Exact FP32, 16.09 GB, downloaded | No |

All four are Apache-2.0. The Qwen weights are converted from the publisher's checkpoints and not retrained.

**Focus in detail.** The prompt is the publisher's reranking template with its web-search instruction unchanged (`crates/airk-retrieval/src/arbiter/compiler/focus.rs`). Each query and passage pair is tokenized into a fixed 512-token graph; overflow is refused, never truncated. The model's output is read at one position as two logits, for the tokens "yes" and "no", and relevance is the two-way softmax of those (`FocusScore::from_logits` in `crates/airk-core/src/arbiter/focus.rs`), which the code comments say is not a calibrated confidence. Three rules govern how the scores are used, and all three are worth copying:

- **Permutation only.** The scored prefix is sorted by relevance and the unscored tail is appended in its original order. Nothing is added, dropped or blended with the fusion score.
- **All or nothing.** If the stage is disabled, over budget, past its deadline or fails, the original order is returned unchanged with a typed not-applied notice. A partially reranked list is never exposed.
- **A receipt.** The reply carries the original order, the ranked order, the per-candidate logits, the counts scored and omitted, tokens reserved against tokens observed, and the identity of the model profile that produced them.

The shipped Focus profile reranks only four candidates (`max_candidates: 4` in `crates/airk-retrieval/src/arbiter/release-catalog.json`), which reflects its CPU cost.

**Context in detail.** Context never writes an answer. It is given a state, a question and a lettered list of options that the native code has already verified, and the exported graph returns one score per option by reading the model's output for the single-token symbols A to H (`crates/airk-retrieval/src/arbiter/compiler/qwen.rs`). AIRK's stated reasons for scoring options instead of generating text are that the model cannot invent a path or a command, that the cost is one bounded forward pass, and that raw scores can be calibrated with an abstain threshold. Only the generic "read" option class is a candidate for qualification; the terminal classes are blocked.

**No query-rewriting model exists.** AIRK's "intent parser" is an 852 KB hashed-feature linear classifier, not a language model.

### 8.3 What AIRK has measured, and what it has not

These figures were reported in the historical AIRK review at `f200897c`. The raw runs are unavailable in this repository. Use them to identify measurements to repeat on the Lens candidate. [Evidence limits](#2-method-and-limits)

**Quality.** There is no nDCG, recall or baseline-against-reranked comparison for Focus or Context in the product pipeline. The one exploratory Focus measurement, on 85 code-search pairs, found a macro AUROC of about 0.55, which is close to chance: the publisher's web-search reranker barely separated relevant from irrelevant code. A fine-tuning pilot did not improve it significantly and nothing was promoted. AIRK's own fine-tuned MiniLM exports failed their recall floors, and the original publisher weights were retained.

**Latency and memory**, all on one workstation (32-core Threadripper PRO, RTX A6000) and all component timings, not end-to-end:

| Model and device | Latency | Memory |
|---|---|---|
| Focus, 8-bit weights, 4 CPU threads, 512 tokens | p95 about 2.1 s (AIRK's records do not say whether per pair or per request of four) | within a 4 GiB reservation |
| Context Compact FP32, 4 CPU threads, 8,192 tokens | p95 about 86 s | peak resident memory about 15.1 GB |
| Context Compact FP32, CUDA | p95 about 3.2 s; load about 18 s | host memory about 15.5 GB; VRAM peak not measured |
| Context Large FP32, CPU, 8,192 tokens | misses the 120 s deadline | about 33.8 GB |
| Context Large FP32, CUDA | warm p95 about 8.5 s; load about 35 s | sampled VRAM about 35 GiB |

**Precision.**

- *8-bit weights.* Accepted for Focus. Rejected for Context Compact: the raw score drift was 0.291 against a limit of 0.25, even though the top choice matched.
- *FP16.* A Compact FP16 conversion exists (weights 3.44 GB, half of FP32) and differs from FP32 by about 0.012 in raw score with the same top choice. It has not run in the product: strict CUDA loading refused it because ONNX Runtime placed some small integer nodes on the CPU. The "FP16 quality gate" recorded in AIRK's history is an accepted rule (FP16 must reach at least 98% of FP32's judged success rate), not a passed result.
- *Conversion recipe.* Embeddings and projection weights go to half precision; normalization, positional rotation, activations, residual sums and the final readout stay FP32. The recipe is in `scripts/arbiter-fp16-export/CONTRACT.md`.
- *Device targeting.* CPU runs the 8-bit Focus and FP32 Context. The only GPU path is Windows x64 CUDA. AIRK's own statement is "No Arbiter GPU or FP16 model variant is qualified yet."

**Status.** All three downloadable profiles are named `unqualified-local-test-*` with no qualification record. They run only because the current build's version is below 1.9.0; the gate in `crates/airk-retrieval/src/arbiter/runtime.rs` refuses them at 1.9.0 and above.

### 8.4 Where Server stands today

- **Vector leg.** `local_embed` is called for every chunk at build time and for every query ([`collections.rs`](../../src/munarium-retrieval-pg/src/collections.rs), [`direct.rs`](../../src/munarium-retrieval-pg/src/direct.rs)). The index manifest records the embedder as `local/local-hash@1/256`, and that identity is part of `index_version`, so changing the embedder already forces a side-by-side rebuild by design.
- **The PostgreSQL vector columns are fixed at 256 dimensions.** `index_chunks` and `collection_chunks` both declare `embedding VECTOR(256)` with an HNSW index ([`0006_retrieval.sql`](../../src/munarium-store-pg/migrations/0006_retrieval.sql), [`0011_collections.sql`](../../src/munarium-store-pg/migrations/0011_collections.sql)). A different dimension needs an additive migration on that path.
- **Provider embeddings exist and are not used for indexing.** `ModelProvider::embed` is implemented for the OpenAI-compatible adapter and for Ollama, and is reachable through the providers API ([`providers_api.rs`](../../src/munarium-server/src/providers_api.rs)). The Anthropic adapter has no embeddings. No index build or query path under `server/src` calls `embed`. The architecture document describes provider-backed embedding as a pipeline step; the code has not reached that.
- **A self-hosted endpoint is already reachable, with conditions.** `spec.endpoint` on a provider configuration overrides the base URL, and its doc comment names vLLM and enterprise gateways as intended targets ([`munarium-providers/src/lib.rs`](../../src/munarium-providers/src/lib.rs)). The `openai` adapter appends `/chat/completions`, `/embeddings` and `/models` and sends a bearer credential. `credentialRef` is required for `anthropic`, `openai` and `openrouter`; only `ollama` may omit it.
- **Provider configurations belong to the tenant.** The gateway resolves credentials and endpoints in tenant scope. Section 8.9 records the budget payer and caller attribution for explicitly enrolled collections, and proposes how to bind an endpoint to the index.
- **The embedder seam is narrow.** `QueryEmbedder` in [`retrieval.rs`](../../src/munarium-core/src/retrieval.rs) is synchronous and infallible (`fn embed(&self, text: &str) -> Vec<f32>`), which suits a hash function and does not suit a model that can be unavailable, slow or over budget.
- **No reranking stage exists, and the provider seam has no rerank operation.** Results leave fusion in their final order. The nearest mechanisms are rule-based: query expansion rules, content demotions and the collection-evidence leg in `MergeWeights`.
- **A local-model precedent exists.** [`ocr.rs`](../../src/munarium-extract/src/ocr.rs) runs neural OCR on the pure-Rust `rten` runtime behind the `ocr` feature, and states the delivery rule that applies here too: models "are `.rten` files fetched at startup ... not embedded: they are tens of MB against a <30 MB image target." No code in `munarium-server` loads those models yet.
- **The build forbids what AIRK does.** The image is a static musl binary verified with `xx-verify --static` ([`Dockerfile`](../../Dockerfile)), and `munarium-extract` records that a native dependency "is not a tradeoff, it is a build failure". AIRK's ONNX Runtime provisioning script lists two targets, `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu`, and no musl target.
- **Server can measure a retrieval change without serving it.** The shadow plane ([`shadow.rs`](../../src/munarium-retrieval/src/shadow.rs)) runs a sampled copy of a live query against a second engine, compares leg by leg and fused top-k, and stores only a query fingerprint. Frozen evaluation records ([frozen-evaluation.md](../guides/frozen-evaluation.md)) give a graded, reproducible offline path. AIRK has neither.

### 8.5 What the feature does in Server

Two capabilities, in the order they are likely to pay off. Both are calls to an inference endpoint through the provider gateway; neither puts a model in the Server image.

**A. A semantic embedder for the vector leg.**

- *What changes.* Passages and queries are embedded by an encoder model instead of the feature hash. Candidates that share meaning but not vocabulary start to appear in the vector leg.
- *Why first.* It addresses a known weakness (the note on `MergeWeights.vector` records that the hash embedder "rewards the shortest chunk sharing any token"). It is paid mostly at index time. The index-identity rule already handles the rollout: a new embedder is a new `index_version`, built side by side and activated by pointer swap.
- *Query-time cost.* The hash path computes a prepared vector once. For an enrolled fan-out, resolve the pinned collection indexes first and group only members with the same authorized provider binding, query instructions and index embedding identity. Account for both original and expanded query inputs, including retries. A single request can have several groups. Keep different tenants, bindings and model revisions separate and reuse prepared vectors only within an authorized group.
- *Identity.* The embedder identity in `index_version` must cover more than model and dimension. Several current embedding models, the Qwen3 embedding family among them, embed a query with an instruction prefix and a passage without one. The query instruction, the passage instruction, normalization and any dimension truncation all change the vectors and all belong in the identity.
- *Storage.* Retain both existing `VECTOR(256)` columns and HNSW indexes. Propose one additional supported dimension of 1,024 for the first trial, with separate nullable storage or a companion table keyed by tenant, collection, index version and chunk. Constrain dimension and select the matching index at build and query time. Preserve the original manifest encoding and `index_version` for the v1.4 hash embedder. The proposed dimension is within pgvector's 2,000-dimension HNSW limit for `vector` and the publisher's range for the 0.6B embedding model. [pgvector HNSW](https://github.com/pgvector/pgvector#hnsw) · [Qwen model card](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B)
- *Cost.* Every enrolled collection must be rebuilt. Embedding a corpus is a long build job, which raises the priority of R6, R8 and R9.

**B. A reranker after fusion.**

- *What changes.* The top N fused candidates are scored against the query by a cross-encoder and reordered, following AIRK's three rules: permutation only, all or nothing, with a receipt.
- *Why it fits Server's governance.* The model sees only passages that authorization, compartments and publication pins have already admitted, and it can neither add a hit nor remove one. It cannot widen what a caller sees. When the endpoint is inside the deployment, passage text does not go to an external provider for this step.
- *No reindex.* The reranker is additive and can be enrolled or withdrawn per collection without rebuilding anything.
- *Cost.* One forward pass per candidate per query. AIRK's figure of about two seconds on four CPU threads for a 0.6B model explains its four-candidate limit. Reranking the 50 candidates Server fuses by default (`candidate_n: 50`) with a model that size is a GPU workload, which is why the endpoint, and not the Server replica, runs it.
- *Reproducibility.* A reranked answer and an unreranked one differ, so the reranker's identity and an applied flag must be part of what the reply records. This depends on R1.

**Effect on external token spend.** Neither capability replaces a hosted call that Server makes today: the hash embedder costs nothing. Any saving is indirect. A better-ordered candidate list lets Server pack fewer passages into the context it sends to an external model, and may reduce follow-up retrieval turns. Server has to act on that for the saving to appear, by packing a shorter list when the reranker has applied, and the change has to be measured for answer quality before it is made a default. The direct saving available from the same endpoint is different in kind: a small generative model served beside the retrieval models can take the `fast` tier, which the existing endpoint override already permits.

**Out of scope for Server.**

- *Option scoring for closed decisions* (AIRK's Context stage, for example to choose which collections receive the deep search). This is where AIRK's evidence is weakest and its cost highest, and Server's routing rules are measured and deterministic. Reconsider it only if A and B are in service and routing is shown to be the remaining bottleneck.
- *Local text generation and local query rewriting as retrieval features.* Server already has provider-backed vocabulary expansion with leases and revision checks, and answer composition belongs to the provider gateway.
- *Hosting, downloading or verifying model weights.* That work belongs to whoever runs the endpoint (8.7, 8.8).

### 8.6 The seam: an inference endpoint behind the provider gateway

| Option | Fits the static image | Model classes | Isolation | Assessment |
|---|---|---|---|---|
| Inference endpoint through the provider gateway (embeddings and rerank over HTTP) | Yes. No model runtime in the image | Any model the endpoint serves, on any hardware the operator gives it | Process, container and usually node boundary; scales independently of Server | Recommended. The one seam Server implements |
| In-process pure-Rust runtime (`rten`, already used for OCR) | Yes | Small encoders are plausible; larger decoder-based graphs are unverified for operator coverage and speed | Memory is shared with the replica; panics can be contained | Deferred experiment outside v1.5. Evaluate memory use, operator coverage and latency before any runtime proposal |
| In-process ONNX Runtime (AIRK's approach) | No. AIRK's static build targets MSVC and glibc only, and it is a C++ dependency | All | None. A native crash or out-of-memory kill takes the replica and its serving planes with it | Not recommended |
| A sidecar speaking a protocol private to Munarium | Yes | All | Process and container boundary | Not recommended. It ties Server to one implementation when a widely implemented protocol already exists |

**Two operations.** The retrieval coordinator selects the hash or endpoint-backed path. Transport-neutral interfaces describe the input, result and failure. The provider gateway owns network calls and credentials.

- *Embed.* Add an asynchronous, fallible preparation path for enrolled collections at the retrieval coordinator. Keep the synchronous hash implementation and its `blend` behavior intact for old plans. The provider gateway already implements an embeddings call. Keep endpoint transport, credential resolution and metering outside `munarium-core` and the embedded datastore. [Provider adapter](../../src/munarium-providers/src/lib.rs)
- *Rerank.* Propose `POST /v1/rerank` with `model`, `query`, `documents` and a full-permutation response in `results[]` with zero-based `index` and finite `relevance_score`. Treat `POST /v1/embeddings` as a separate operation. Specify base-URL composition once so a configured `/v1` prefix is applied only once. The vLLM 0.10.2 reference documents these paths and shapes. Choose the implementation version separately, pin its build and verify both routes on that exact build before claiming support. [Versioned vLLM API](https://docs.vllm.ai/en/v0.10.2/serving/openai_compatible_server.html#re-rank-api)

**Rules Server applies to a rerank reply.** These carry AIRK's three rules across a network boundary.

- Server uses the order only. A `relevance_score` is not comparable between models and is not blended with the fusion score. Ties keep fused order, so the result is deterministic for a given set of scores.
- The reply must account for every passage sent, each index exactly once. For `prefer`, invalid output, timeout or transport failure returns the original, unmodified order and a typed explanation. For `require`, fail the request. For shadow mode, record only the candidate outcome. Treat endpoint-returned passages as untrusted and keep the authorized originals.
- Input that exceeds the model's token limit is a refusal for that request, and is not silently truncated by Server. The candidate cap and the token cap are set by Server's budget, not left to the endpoint's defaults.

**Multi-collection recommendation.** Group authorized candidates by compatible reranker, processing policy and retrieval-budget binding. Reorder each group's candidates within the positions it held in the fused result. For example, given `A1, B1, A2, C1`, a reranker that prefers `A2` to `A1` produces `A2, B1, A1, C1`. Test with B using a different model and C unenrolled. This preserves citation identities and avoids comparing scores across models. Each enrolled group's outage behavior follows I1 and its accounting follows I2.

**An `openai-compatible` provider kind.** The existing `openai` kind can already be pointed at such an endpoint with `spec.endpoint`, but it was shaped for a hosted service. A dedicated kind, modelled on the `ollama` one, fits a self-hosted endpoint better:

- an explicit endpoint is required, validated as the Ollama endpoint is (an HTTP or HTTPS base URL with no credentials, query or fragment);
- the credential is optional, since a private endpoint may be protected at the network layer. `openai` requires `credentialRef` today;
- models are listed explicitly per capability (`complete`, `embed` and a new `rerank`), with no built-in tier table;
- it is never chosen by automatic default selection;
- count attempts, calls and observed tokens. Preserve missing token or tariff data as unknown in the existing accounting model. An unpriced call is excluded from priced totals and remains visible in coverage. [Accounting guide](../guides/monetary-accounting.md)

**A conformance probe before enrollment.** Compatibility claims vary between servers and versions, so Server checks rather than assumes. Before a collection can be enrolled, a management operation calls the endpoint and verifies that the named models are listed, that an embedding has the declared dimension and is finite, and that a rerank reply has the shape above. The probe result is recorded with the endpoint fingerprint, and a failed probe leaves the collection as it was.

**What Server can and cannot know about the model.** Server cannot inspect weights behind an endpoint. The identity it records is the provider configuration name, the model name the endpoint reports, the endpoint fingerprint and the parameters Server itself controls (instructions, normalization, truncation, candidate cap). This is weaker than AIRK's compiled catalog, in which the binary pins every file by digest. An operator who needs that assurance gets it from the endpoint's own packaging (8.8), and should give each model revision and precision a distinct served name so that a change of weights is a change of identity that Server can see.

### 8.7 Bring-your-own endpoint

This is the primary deployment model. An operator who already runs vLLM, or another server implementing the same two shapes, on their own hardware configures Server to use it and sends nothing to a public model service for retrieval.

**What works today.** A provider configuration with `provider: openai` and an endpoint override reaches a self-hosted server for chat completion and for embeddings through the providers API. The [key and secrets guide](../guides/managing-key-and-secrets.md) states the condition: the endpoint must match the adapter's paths, request format and authentication.

**What is missing.**

1. The embedder wiring into index builds and queries (IR-0, IR-2).
2. The `rerank` operation and the stage after fusion (IR-0, IR-3).
3. The `openai-compatible` provider kind and the conformance probe (IR-1).
4. A documented all-local configuration, in which the completion tiers of a runbook point at the same endpoint, for deployments that use no public model service at all.

**Division of responsibility.** The operator chooses the models, supplies the hardware, and keeps the endpoint reachable only from Server. Server supplies the seam, the probe, the per-collection policy, and the means to measure a model on the operator's own corpora before it serves an answer (8.10). Server publishes no list of approved models for this path. It can publish the configurations it has itself measured, with the corpus families and results, as guidance.

### 8.8 Munarium Lens and the Server boundary

The maintainer requires Lens to be complete before Server v1.5 releases. LENS-0 supplies the public contributor artifacts, `src/` stubs, indexed `docs/` designs and offline checks. LENS-1 through LENS-3 deliver the working reference endpoint and its qualification. The Lens repository records publishable qualification evidence and references restricted coverage according to its handling policy. [Lens completion and acceptance](repository-handoffs.md#lens-d1)

| Lens design | First implementation gate |
|---|---|
| Inference HTTP contract | Document both operations, input bounds, stable model identity, precedence of refusals and a synthetic conformance fixture. Test a valid and invalid response on an independent endpoint as well as Lens. Server retains the generic consumer probe. |
| Runtime and model custody | Compare a single service with routing to two model processes. Pin the selected runtime, model revisions, tokenizer, templates, precision and file digests. Keep weights outside the image. Document verified import for offline installations and notices for each distributed dependency. |
| Capacity and availability | Before advertising GPU support, record exact GPU/driver, peak memory, cold start, concurrency and p95 latency for both models together under the planned bounds. Readiness requires verified assets and successful probes. A mismatch prevents enrollment. |
| Model quality | Qwen3-Embedding-0.6B and Qwen3-Reranker-0.6B are candidates. Compare each precision and instruction revision against its baseline on held-out public data. Treat any change of representation as a distinct served identity. The model cards describe upstream models. Only the qualification record can support Lens-specific results. [Embedding model card](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B) · [Reranker model card](https://huggingface.co/Qwen/Qwen3-Reranker-0.6B) |
| Operations | Describe authenticated transport, minimum network access, logging with no request content, failure diagnosis, resource limits, readiness, upgrade and rollback. Include offline test doubles so routine contributor checks need neither GPU nor credentials. |

The Server v1.5 release requires the pinned Lens image and the Server candidate to complete the build, query, citation and rollback exercise across all Munarium-related documentation and source code (section 8.10). Record the supported hardware and acceptance thresholds before running it. Also exercise the v1.4 upgrade with Lens disabled, retaining native query, index builds and existing settings. Operators may still use another endpoint that passes the generic probe.

### 8.9 Enablement and governance

An upgraded Server can distribute enrolled work across Lens replicas that serve the same approved model profile. The scale-out design traces a build and a query through those replicas and defines how to qualify cache identity, capacity and tenant accounting during a resize. [Scaling Server and Lens](scale-out.md)

**Two levels of "on".**

- *Deployment capability.* Whether this deployment has a provider configuration offering `embed` or `rerank` with a passing probe. Report it through the existing capability mechanism in [`capabilities.rs`](../../src/munarium-retrieval/src/capabilities.rs), which already records each capability with the reason it is on or off.
- *Enrollment.* Which collections use it, set through a management-role API with compare-and-set on a revision, in the same style as the retrieval rollout selector. AIRK's equivalent is per-index enrollment with `expected_revision` on every change. Enrollment per collection lets an operator qualify one corpus at a time.

**Endpoint binding proposal.** Bind the collection's embedding index and reranking policy to versioned provider configuration in the owning tenant. Verify access, compartment, retention and external-processing policy before sending any passage or query text, including shadow traffic. On provider drift, expire the probe and require requalification. Resolve a pinned historical index with its own embedding identity. The runtime, endpoint profile and its selection remain separate implementation decisions. [Collection processing policy](../../src/munarium-server/src/answers_api.rs)

**Stage diagnostics.** Distinguish disabled, unenrolled, unconfigured endpoint, failed probe, unavailable endpoint, exhausted budget and ready. For enrolled reads, the response or typed refusal identifies the configured stage that could not run and its content-free reason. Preserve v1.4 response defaults when the collection is unenrolled.

<a id="i1"></a>

**I1. Governed reads.** On an explicitly Lens-enrolled collection, `require` is the default for every configured inference stage. Only an explicit operator selection of `prefer` permits that stage's documented fallback.

- Under `prefer`, an unavailable semantic embedder permits a disclosed lexical-only search. An unavailable reranker permits the original fused order. Record the stage, reason and effective policy revision. Keep all authorization and evidence checks in force.
- Under `require`, an unavailable embedder or reranker produces a typed refusal for the governed collection read. This includes a failed or invalid response, expired deadline or insufficient budget. Never mark the failed stage as applied.

<a id="i1-t1"></a>

**I1-T1.** Test each stage's failure with omitted policy and explicit `prefer`. Check the refusal or served order and receipt, including each stage in a combined path. For a multi-collection read, [C1](#c1) can return other independently verified results when an optional collection refuses. It cannot use that collection's unapproved fallback or support an answer that requires its missing evidence. An authorized shadow failure affects only its comparison record. Unenrolled v1.4 collections continue on their original path.

<a id="i2"></a>

**I2. Enrolled collection budgets.** The collection's tenant-owned budget pays for embedding and reranking. Attribute every endpoint call, including retries and build work, to the initiating caller in the audit record. Carry that identity through queued work. Meter explicitly authorized shadow evaluations separately from serving calls, including failed or discarded attempts. Size the shadow quota and set its allocation within the tenant budget during implementation. [Open items](README.md#open-items)

**Admission and reconciliation.** Reserve capacity against the responsible budget before dispatch. Enforce a single request deadline across queueing, attempts and delivery. Record actual usage when known and reconcile each attempt once. A multi-collection read must preserve each collection's authorization and payer. Reuse a prepared embedding only within a compatible authorized group sharing the same budget binding. Charge a shared physical call once and retain which collections it served. Keep tenants' reservations, usage and credentials isolated.

<a id="i2-t1"></a>

**I2-T1.** Exercise two callers racing for the final reservation, two tenants using the same model, a queued build and one serving call with an authorized shadow comparison. Assert the responsible tenant and initiating caller on every attempt, separate shadow totals and zero outbound calls after denied admission.

<a id="i2-t2"></a>

**I2-T2.** Run a multi-collection read through compatible and incompatible billing groups. Reconcile retries once per actual attempt, charge shared calls once, and retain unknown usage when the endpoint omits it. Enforce the request deadline and the bounded heavy-work class on serving calls.

Worker placement follows [SC2](repository-handoffs.md#sc2). Share workers only under an explicit policy covering all participating tenants and their compatible processing rules. Use a dedicated pool whenever isolation is required.

### 8.10 Evaluating before enabling

The Lens owner specifies the release exercise across all 16 workspace repositories, from per-source inventory and candidate build to fresh-session queries, citation resolution and rollback to the previous approved index. Run restricted inputs and retain their detailed evidence in each repository's permitted environment. The quality study below uses separate held-out queries. [Workspace release qualification](repository-handoffs.md#lens-d2)

<a id="i3"></a>

**I3. Qualification for documentation and source code.** A fixed serving profile intended for both families must independently meet each family's predeclared retrieval-quality and latency thresholds. Freeze the model, precision, instructions, index and serving parameters, hardware, query sets, judgments, cut-off and thresholds before the run. Evaluate documentation and source code separately against the v1.4 hash baseline, embedder alone, reranker alone and combined stages. Each family receipt records the profile identity, recall@k, nDCG@k, p95 latency, failures and authorization outcomes. [Frozen evaluation procedure](../guides/frozen-evaluation.md)

<a id="i3-t1"></a>

**I3-T1.** Require two passing family receipts for the same profile before claiming support for both. A single-family pass or a pooled score cannot qualify both families.

<a id="i3-t2"></a>

**I3-T2.** A missed threshold fails that family and missing evidence leaves its qualification incomplete. Preserve the separate file-by-file build, query, citation and rollback receipts across all 16 repositories. Evaluate instruction changes under their own identity and pursue fine-tuning only with held-out evidence of a gain.

AIRK's code-search result under a web-search instruction does not establish how either family will perform with the target instructions. Freeze and evaluate the instructions used for qualification.

### 8.11 What not to take from Arbiter

- **In-process native inference.** The reasons are in 8.6.
- **A model catalog and weight download inside the product.** AIRK needs them because it is the only process on the machine. In Server they would add egress, storage and verification duties to a service that should hold none of them. Munarium Lens is the place for them (8.8).
- **The 8,192-token Context configuration.** Eighty-six seconds on CPU and a 120-second deadline are outside anything a request-serving plane can offer.
- **GPU custody as built.** It is Windows-only CUDA with runtime libraries placed beside the executable. In a cluster, GPU allocation belongs to the scheduler and the inference container.
- **Local asset custody checks.** File identity, access-control lists and local-volume checks have no meaning for a remote endpoint.
- **The scale of the process.** Arbiter's plan document is about 156 KB, with eleven steps in progress and five blocked. The useful core for Server (typed availability, budgets, permutation-only reranking with a receipt, per-index enrollment) is a small fraction of that.

### 8.12 Implementation sequence

Server work:

| Step | Work | Depends on | New runtime in the image |
|---|---|---|---|
| IR-0 | Add asynchronous, fallible embedding preparation and reranking at the retrieval coordinator with transport-neutral types. Retain the synchronous v1.4 path. Identify selected and applied stages in R1. | R1 | None |
| IR-1 | `openai-compatible` provider kind with explicit `embed` and `rerank` model lists; conformance probe | IR-0 | None |
| IR-2 | Build the semantic index alongside the previous approved index over the complete source inventory. Compare in shadow before promotion. | IR-1 and authorized collection bindings and budgets. R6, R8 and R9 support large rebuilds. | None |
| IR-3 | Rerank fused candidates with the real endpoint, compare in shadow and serve reranked results in enrolled qualification sessions. | IR-1 and a passing rerank probe | None |
| IR-4 | Apply explicit per-collection enrollment, verify served-stage and citation receipts, then restore and query the previous approved index. | IR-2 and IR-3, R7, and verified index/policy revisions | None |
| IR-5 · outside v1.5 | Shorter context packing when the reranker has applied, measured for answer quality and token use | IR-4 | None |
| IR-6 · outside v1.5 | Experiment with a small encoder and cross-encoder on a pure-Rust runtime | Outside the v1.5 implementation plan | Deferred in-process experiment |

The Lens owner defines the prerequisite milestones from the repository foundation through the real model image, serving-profile qualification and held-out instruction evaluation (LENS-0 through LENS-3). Prepare the repository while Server defines the protocol. [Lens work and acceptance](repository-handoffs.md#lens-d1) · [Release qualification](repository-handoffs.md#lens-d2)

IR-2, IR-3 and IR-4 must work through the production serving path in the authorized qualification environment before Server v1.5 releases. The upgrade default continues to serve with the hash embedder, with no enrollment or outbound inference calls.

## 9. The admin web interface

The v1.5 console work uses Server's existing server-rendered pages and management authorization. The release requires a shared, revocable login with an explicitly controlled transition for existing deployments. AIRK's historical admin review also informs the navigation, state displays and action flows below. [Release requirements](README.md#rel1) · [Existing-deployment login](#login1)

### 9.1 Console priorities

1. **Qualify shared login across replicas.** Verify the existing mode throughout a rolling upgrade, operator cutover, cross-replica revocation and rollback under LOGIN1.
2. **Expose operating state.** Group the navigation, surface problems on the overview and distinguish failed reads from a genuine count of zero.
3. **Make actions reviewable.** Show what a destructive action will change, require confirmation and preserve the existing role and CSRF checks. Retain real URLs and accessible server-rendered pages.

### 9.2 What AIRK's admin interface is

**Stack.** A Preact single-page application in `packages/admin`, built to a bundle that is committed and embedded in the service binary; a check fails when the committed bundle differs from its sources. Styles are one file of design tokens with light and dark values and no external assets. Pure presentation logic (navigation, help, usage formatting) lives in a separate `packages/ui` package with its own tests.

**Organization.** The first version had eleven flat pages. The current shell (`packages/admin/src/app.tsx`, `packages/ui/src/navigation.ts`) groups them into seven destinations with an icon each, and uses sub-tabs inside two of them:

| Destination | Contains |
|---|---|
| Overview | Service state, searchable indexes, background work |
| Search | Query, and guided evidence tools |
| Indexes | Index library, index sets, grants |
| Activity | Activity, Logs, Usage (sub-tabs) |
| Agents | Connecting a client |
| Service | Service, Storage, Recovery (sub-tabs) |
| Help | Offline topics and tool reference |

**Shared components** (`packages/admin/src/render.tsx`). A small set does most of the work:

- `Outcome` renders every call result the same way, including a distinct state for an outcome that is unknown, with the instruction not to repeat the change.
- `Confirm` is a typed confirmation: the action button stays disabled until the user types the exact name of the target.
- `JsonBlock` shows the complete native result with Copy and Download, so a readable summary never replaces the full data.
- `Command` shows the equivalent command-line invocation, ready to copy.
- `Text` renders every value as text and shows invisible and direction-changing characters as visible escapes such as `<U+202E>`.

**Changes are reviewed before they are applied.** Every mutation is first drafted or previewed by the service and shown in full; applying it sends back a review token, not the request. A cleanup shows its exact plan and a digest, and the button reads "Purge exactly this plan".

**Session.** The pages are opened with `airkctl admin open`, which exchanges a single-use, 60-second code for a `__Host-` cookie that is `Secure`, `HttpOnly` and `SameSite=Strict`, plus an anti-forgery token. Sessions end after 30 idle minutes or 12 hours, on sign-out, or on service restart. The pages never ask the user to paste a token.

### 9.3 What AIRK's own review found

AIRK's team captured every screen of the running interface and wrote a review (`docs/split/codex-admin-review.md`) with a proposed design for each. Its opening finding is that the interface "exposes much of AIRK's administrative capability, but long stacked forms, expanded native JSON and internal identifiers make ordinary tasks feel like API work."

The measurements behind that are concrete. At a 1,440-pixel-wide desktop viewport:

| View | Height as shipped | Height as redesigned |
|---|---|---|
| Overview | 2,088 px | 1,000 px |
| Index detail | 5,042 px | 1,000 px |
| Agents | 2,415 px | 1,000 px |
| Usage report | 2,296 px | 1,000 px |

The index detail page stacked, in one scroll: a JSON dump, sources, memberships, progress, rename, source changes, policy, split, split recovery, retirement, a new-index form, index sets and grants.

The review's layout rules are the part to keep. In its words:

- "One task per view."
- "A useful first viewport": purpose, selected context, current state, the primary action and essential warnings together.
- "Tabs for separate tasks, disclosures for supporting facts."
- "Bound lists rather than the whole application."
- "Readable summaries, complete evidence."
- "Keep action and feedback together."
- "Do not trade scrolling for unreadability."

Three other findings generalize:

- **State vocabulary.** The review asks that "service running", "index searchable", "indexing active", "source verification", "freshness" and "agent connected" stay separate, because "no single success badge can establish all of them." It also found a progress panel that read "Waiting to run Finished (finished)" because a retained completed observation and newly queued work shared one display.
- **Empty destructive plans.** A cleanup plan with nothing in it still showed the full typed confirmation and a purge button. The review asks for "Nothing to clean up in this plan" instead.
- **Stale help.** The built-in Help was the highest-priority problem: several articles described superseded setup and transport behaviour while the page displayed the current version number. The review's conclusion is that a version label "does not prove that every paragraph was reviewed for that version."

The redesign images are labelled as concepts and not implemented. The grouped navigation with icons and sub-tabs is the part that has reached the code.

### 9.4 Server's admin console today

The console is [`dashboard/`](../../src/munarium-server/src/dashboard/mod.rs) with styles and charts in [`charts.rs`](../../src/munarium-server/src/charts.rs).

- **Rendering.** Server-rendered HTML with inline SVG charts, no JavaScript and no external assets. Every page is a real URL, so browser history, bookmarks and reload work without any routing code.
- **Navigation.** Fourteen links in one wrapping top bar, in lower case and without grouping: overview, traffic, endpoints, usage, providers, runbooks, collections, storage, sessions, tokens, audit, findings, matrix, health.
- **Pages.** A title and a body. There is no line saying what the page is for, which tenant is shown, which replica rendered it, or when the data was read.
- **Overview.** Eight count tiles, each linking to its page, then traffic charts for the last 24 hours.
- **Actions.** Three: issue a capability token, revoke one, and approve a runbook gate. Every form carries a CSRF token. The gate approval takes the read-write credential in the form on each submission, so the management credential alone cannot write the ledger. Behind a view-only proxy, forms are replaced by a note.
- **Sign-in.** A form into which the operator pastes the static management token. The cookie holds that token itself, with `HttpOnly` and `SameSite=Strict`, without `Secure`, and with no expiry and no sign-out route. The module comment records this as a demonstration posture.
- **Style.** Design tokens with light and dark values from a validated palette; tiles, cards, data tables, notices and state swatches.
- **What it does well already.** Chart data is available as a table under each chart. A page whose source is missing says so ("needs the postgres store") instead of rendering empty. State colours always sit beside a text label. The audit list has keyset pagination.

### 9.5 Recommendations

Each is written for server-rendered HTML. "No script needed" is stated where that might be in doubt.

**U1. Group the navigation.**

Fourteen equal links make the operator read the whole bar to find anything. AIRK went from eleven flat pages to seven destinations with sub-tabs. A grouping for Server that follows what operators do:

| Group | Pages |
|---|---|
| Overview | overview |
| Traffic | traffic, endpoints, usage |
| Retrieval | collections, storage, sessions |
| Runbooks | runbooks hub, with links to shape and run details |
| Access and audit | tokens, audit, findings |
| Platform | providers, health, existing Matrix integration entry point |

Use six top-level links with ordinary links for sub-tabs. Mark the active page with `aria-current="page"`. The runbooks hub already lists shapes and runs and links to `/admin/shapes/{shape_ref}` and `/admin/runs/{run_id}`. Standalone index URLs for those lists would be new work. Keep v1.4 bookmarks usable through the reorganization. Matrix-side administration and product guidance belong in the Matrix repository. The existing Server-side `/admin/matrix` entry point must continue to work through the handoff. [Runbooks hub](../../src/munarium-server/src/dashboard/runbooks.rs) · [Repository handoffs](repository-handoffs.md)

**U2. Give every page the same header.**

AIRK's review asks for "a consistent page title, one-sentence purpose, selected context and observed-at label." For Server the context is more important than it is for AIRK, because Server has tenants and replicas:

- the tenant the credential is bound to;
- the instance that rendered the page, since replicas can disagree about process-local state such as gauges and cache residency;
- the store mode, which today surfaces only as a notice when something is missing;
- the time the data was read, and the window where one applies.

One helper in `page()` covers every page.

**U3. Make the overview answer "does anything need me?"**

AIRK's redesigned overview leads with three statements: the service is available, two of two indexes are searchable, one job is active. Server's overview leads with eight counts of equal weight. A count of 12 collections is not a status.

Put an attention strip above the tiles, listing only what is abnormal, each item linking to its page: runs awaiting approval, blocking gate findings in the last day, a datastore scope that a replica cannot serve, a provider whose credential check fails, failed or stuck build jobs, and unresolved guarded commands. When nothing is abnormal the strip says so in one line. The counts stay below it.

**U4. Review, then confirm, before a destructive action.**

Revoking a token is a single click on a red button in a table row ([`inventory.rs`](../../src/munarium-server/src/dashboard/inventory.rs)); a mis-click revokes the wrong row. AIRK's pattern has two parts and both work without script:

1. The first submission returns a review page that states exactly what will change: the token's `jti`, its `uid`, its scopes, and the consequence. Server already has the right sentence for the consequence on the tokens page, where it explains whether revocation is enforced at verification time.
2. The review page carries a form in which the operator types the target's identifier, and only that second submission acts.

Bind the confirmation to the authenticated operator and tenant, target, intended action and current revision where one exists. Recheck the role, CSRF token and target at submission. An empty or stale plan offers no confirmation. Exercise refresh, duplicate submission and a target changed between preview and confirmation. Preserve the underlying v1.4 API.

**U5. Never show a failed read as zero.**

The overview computes two of its tiles with `.unwrap_or(0)` ([`monitoring.rs`](../../src/munarium-server/src/dashboard/monitoring.rs)): if listing shapes or providers fails, the tile reads 0. AIRK's rule, stated in its review and enforced in its usage page, is that unknown is never rendered as zero and an estimate that does not exist reads "not available". Server follows this rule in its API design (R9, and the three-state `exhausted` field) and should follow it on its own front page: show "unavailable" with the reason, in the style of the existing store notice.

**U6. Keep separate facts separate.**

A collection has several independent states: whether it is active, whether its active index version exists, which engine serves it, whether the serving artifact is resident on this replica, and whether a build is running. AIRK's review found that one badge cannot carry several such facts and that merging them produced a contradictory display. Server's storage page already separates tiers and nodes well. Apply the same separation wherever a row carries a single state swatch, and label each progress number with what it counts.

**U7. Summary first, complete result one step away.**

AIRK's failure here was using JSON as the primary view; its strength is that the full result is always one disclosure away with Copy and Download. Server mostly renders tables, which is the right default, and falls back to a JSON block for search hits in a session turn. Two additions:

- A consistent "complete result" disclosure on detail pages, using the `<details>` element the charts already use.
- Beside it, the equivalent API path and `mmctl` command as selectable text. AIRK's `Command` component does this for `airkctl`, and it teaches the operator the scripted route while they use the console. A copy button needs a few lines of script; selectable text needs none.

**U8. One task per view, and bound every list.**

The tokens page puts the issue form, its explanation, a filter and the full list in one view. Following AIRK's rule, issuing a token is its own page reached from a button, and the list is the default view. The audit list's keyset pagination should be applied to tokens, sessions and findings too, each with a count and an "older" link.

**U9. Show invisible and direction-changing characters.**

`charts::esc` escapes the five HTML metacharacters and passes everything else through. The session pages display user queries, model answers and passages from tenant documents. Text containing a right-to-left override or other invisible control characters can therefore make a value on an operator's screen read differently from its bytes: a file name, a citation, or a token's `uid`. AIRK renders these characters as visible escapes in every value it displays. One change to `esc`, or a second function used for untrusted text, closes this for the whole console.

**U10. Ship and qualify the new console login.**

U10 is a v1.5 release requirement. Within the existing API-management security boundary, issue a random, opaque, revocable handle for a successful console login. Bind the session to its authenticated tenant and authority. The proposed session flow includes these controls.

- Sign-out revokes the server-side session across replicas and clears the browser cookie.
- The browser cookie carries the session handle with `HttpOnly` and `SameSite=Strict`.
- Set `Secure` for the TLS deployment, support trusted TLS termination and satisfy the `__Host-` cookie requirements when using that prefix.
- Enforce idle expiry on every replica.

<a id="login1"></a>

**LOGIN1. Upgrades from v1.4.** Keep legacy login active throughout the rolling upgrade and after the last replica upgrades. Changing the active mode requires an explicit operator cutover. Its preflight must confirm that every replica serving console traffic supports the same shared, revocable sessions. If any serving replica is incompatible or its capability is unknown, the cutover is refused and legacy login continues. Preserve existing login and management URLs, token authorization and view-only proxy behavior throughout.

<a id="login1-t1"></a>

**LOGIN1-T1.** Exercise cutover with a v1.4 replica, an unknown peer and a fully capable fleet. Verify the last replica upgrade alone leaves the active mode unchanged.

<a id="login1-t2"></a>

**LOGIN1-T2.** After the operator switches modes, sign in on one replica, use a CSRF-protected action with the correct role on another, revoke the session and verify both reject its handle. Test tenant isolation, idle expiry, denied roles and a rolling restart. Rollback invalidates the new handles and requires fresh authentication through the restored legacy flow. The separate single-use `mmctl` launch proposal is outside this login requirement.

**U11. Explain each page, and link to documentation instead of copying it.**

AIRK bundles a full Help section and paid for it with stale articles. Server has the documents already, checked by tests for links and route coverage. The lesson is to link, not to duplicate:

- one sentence under each page title saying what the page is for (part of U2);
- a link from each page to its guide or runbook under `docs/`, and from each problem slug shown in the console to its entry in the error registry;
- a test, alongside the existing documentation checks in [`docs_coverage.rs`](../../src/munarium-server/src/docs_coverage.rs), that every such link resolves.

AIRK's proposed task-first landing page ("Create an index", "Connect an agent", "Find missing results") suggests the same for Server's overview when a deployment is empty: three links to the first tasks instead of eight tiles reading zero.

**U12. Show job progress when there is progress to show.**

AIRK's progress panel lists phase, items committed of items found, elapsed time, time since the last advance, stalled and paused flags, and time per phase, and refreshes about every two seconds while work runs. Server has no build-job page; R9 proposes the data. When that exists, the page should show the same fields with unknown totals left unknown. Without script, a `<meta http-equiv="refresh">` emitted only while a job is running gives the live view, and stops when the job ends.

**U13. Tighten the visual vocabulary.**

Both interfaces use design tokens, system fonts and automatic dark mode, and Server's palette is the more carefully chosen of the two. AIRK's stylesheet (`packages/admin/src/app.css`) has a few things Server's lacks:

- A button hierarchy: primary, secondary, small and danger, plus a visible disabled state. Server has one button style and a danger variant.
- Form fields laid out with the label above the input and a marked invalid state. Server's action forms are inline rows with widths set by inline `style` attributes.
- Text colours for success, warning and danger that are legible on the surface colour, in addition to swatch colours.
- A monospace token used consistently for identifiers, so that an identifier is recognizable as one.
- Pill badges for states in tables, as in AIRK's redesigned index list ("Available", "Updating", "No active job observed").

**U14. Test the pages as pages.**

AIRK has DOM tests for navigation, page titles, help rendering and individual flows, and its review sets acceptance viewports: 1,440 by 1,000 and 1,366 by 768 on the desktop, 390 pixels wide on a phone, and 200% zoom, with the essential state and action visible without scrolling. Server's console tests assert on response bodies, which server rendering makes easy. Extend them with a structural check per page (one `h1`, the header fields from U2, the current navigation item marked) and adopt the viewport list as a manual acceptance checklist for any page that is redesigned.

### 9.6 What Server already does better

- **No build pipeline.** AIRK needs a bundler, a committed bundle and a drift check. Server's pages are Rust functions that return HTML, and there is nothing to drift.
- **Real URLs.** AIRK's review had to ask for linkable detail pages and working browser history. Server has both by construction.
- **Charts with a table twin.** Every chart's data is available as a table beneath it.
- **Honest absence.** A page that cannot read its source says why. AIRK's review asked for exactly this distinction between failed, empty and not yet read.
- **A view-only mode.** One header turns every action form into a note, so a read-only proxy never offers a button that would fail.
- **Role separation in the console.** The one ledger-writing action takes its credential per submission.

### 9.7 What not to copy

- **A single-page application with an embedded bundle.** It brought AIRK a build, a drift check and a routing layer, and its sessions end whenever the service restarts. Server's approach is simpler and sufficient.
- **Raw JSON as the main view,** and forms that ask for internal identifiers, cursors or digests. AIRK's own review rates these as its largest usability problem.
- **Manual "Load" buttons for cheap reads.** Several AIRK pages open empty until a button is pressed. A server-rendered page should arrive with its data.
- **Everything about one object on one page.** The 5,042-pixel index detail page is the example to avoid as Server adds collection and job detail.
- **Help text maintained separately from the documents.** See U11.
- **Mock-up volume.** AIRK's review produced 107 pairs of images. A page pattern, a navigation grouping and two or three reference pages would have carried the same decisions.

### 9.8 Suggested order

| Step | Work | Size |
|---|---|---|
| 1 | U5 (no failed read as zero) and U9 (visible control characters) | Small; both are corrections |
| 2 | U1 (grouped navigation) and U2 (page header) | Small; confined to `page()` and `nav()` |
| 3 | U4 (review and typed confirmation), applied first to token revocation | Small to medium |
| 4 | U3 (attention strip on the overview) | Medium; needs a few new counts |
| 5 | U8 (issue form on its own page; pagination everywhere) and U7 (complete result and equivalent command) | Medium |
| 6 | U10 cluster login, mixed-version transition and rollback | Security review and successful cluster qualification required for v1.5 |
| 7 | Apply U6, U11, U13 and U14 as pages are touched. Add U12 when R9 supplies progress. | Incremental |
