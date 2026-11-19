# AIRK patterns for Munarium Server

**Status:** analysis, 19 November 2026. Nothing here is implemented, scheduled or accepted. It is input to design decisions for Munarium Server only.

**Compared:** AIRK at commit `55eebc96` (source version 1.8.207) against the Munarium Server workspace under [`server/src/`](../../src/) at `bb18085`. Sections 8 and 9, on semantic embedding and reranking and on the admin web interface, were written against the later AIRK commit `f200897c`.

## 1. Summary

AIRK and Munarium Server solve different problems: AIRK is a single-user local index and retrieval service for coding agents, and Munarium Server is a multi-tenant governed-memory service on PostgreSQL and object storage. They share a design temperament (immutable derived data, explicit refusal instead of silent fallback, content-addressed identity, typed errors), so most of AIRK's foundations are already present in Server in a form suited to a cluster.

The useful differences are narrower. AIRK is more developed in three places:

1. **What a read tells the caller about itself.** AIRK replies state which mode was requested and which was served, which members of a multi-index read completed, and whether a count is known or only a lower bound. Server returns hits and a provenance envelope.
2. **Background work as an operator-visible object.** AIRK jobs have a fencing generation, content-free failure codes, cooperative cancellation, progress with honest unknowns, a transactional event ring and aging fairness. Server's build queue has leases and attempt ceilings and little else.
3. **Containing hostile or expensive inputs.** AIRK parses PDFs in a resource-limited child process and admits heavy work through exact, job-bound grants. Server parses PDFs in-process with no time bound and admits every request through one semaphore per plane.

The twelve recommendations in section 4 follow from those three. Section 5 lists what Server already has, and section 6 lists AIRK practices that should not be copied, including some AIRK is visibly paying for. Section 8 is a separate study of AIRK's optional local Qwen models and of how Server can adopt a semantic embedder and a reranker through an inference endpoint, without a model runtime in its image. Section 9 compares the two admin web interfaces.

| # | Recommendation | Server code touched | Size | Priority |
|---|---|---|---|---|
| R1 | Search execution record on every search reply | `munarium-core`, `munarium-api-types`, `munarium-retrieval` | Small | High |
| R2 | Per-member outcomes for multi-collection reads | `munarium-server` (`sessions_api`, `query_api`) | Medium | High |
| R3 | Isolate PDF extraction in a limited child process | `munarium-extract`, `munarium-retrieval-pg` | Medium | High |
| R4 | Fencing generation on build jobs | `munarium-store-pg` (`jobs`) | Small | High |
| R5 | Content-free failure codes for jobs | `munarium-store-pg`, `munarium-server` (`datastore_jobs`) | Small | High |
| R6 | Cooperative cancellation of running builds | `munarium-server`, `munarium-retrieval` | Medium | Medium |
| R7 | Work-class admission with a reserved control slot | `munarium-server` (`middleware`, `state`) | Medium | Medium |
| R8 | Fair claim order across tenants | `munarium-store-pg` (`jobs`) | Small | Medium |
| R9 | Job progress and a transactional event ring | `munarium-store-pg`, `munarium-server` | Medium | Medium |
| R10 | Durable, bounded artifact damage record | `munarium-store-pg`, `munarium-datastore` | Medium | Medium |
| R11 | Typed retry guidance on problem responses | `munarium-api-types`, `munarium-server` (`error`) | Small | Low |
| R12 | Validating identifier newtypes at new boundaries | `munarium-core`, `munarium-store-pg` | Incremental | Low |

## 2. Method and limits

AIRK is about 650,000 lines of Rust in 19 crates with roughly 330 documentation files. It was read in four passes (storage and publication; retrieval and the response contract; service runtime; code structure), each starting from AIRK's documents and then confirming the named types and functions in code. A sample of the claims from each pass was checked again directly against the AIRK source, and every statement about Munarium Server below was checked against the Server file it links.

Limits a reader should keep in mind:

- **AIRK's own maturity.** Its documents repeatedly describe the current line as an unpublished development candidate. Installed-platform, macOS, Linux and workload qualification are recorded as open. A pattern listed here exists in AIRK's code; that is not evidence it has been proven under production load.
- **AIRK's documents lag its code in places.** Three examples found during this review: `docs/embedding-cache-packs.md` states pack limits far below the constants in `crates/airk-store/src/vector_cache.rs`; `docs/arbiter/relaxations.md` says a default budget was kept at a value the code no longer uses; `docs/combined-reads.md` describes a fan-out of one where the code runs members concurrently. Where the two disagreed, this analysis follows the code.
- **Not read.** AIRK's installer, VS Code extension, admin UI, model training tools and most of its `docs/history/` tree were out of scope. In Server, the dashboard, platform-identity and runbook step machine were not examined for these patterns.
- **AIRK line numbers are omitted** in favour of file paths and symbol names, which survive edits better.

## 3. What AIRK is, in the terms that matter here

AIRK builds a per-project index (SQLite with full-text search, plus a DiskANN graph stored as authenticated records) and serves bounded, cited evidence over a local HTTPS endpoint. Three properties shape everything it does:

- **One resident process, many in-process owners.** Indexing, watching and cleanup run as owners inside one service. A stuck owner cannot be killed without killing the service, so AIRK quarantines it instead.
- **One current index per project.** Since its 1.2 line AIRK keeps a single current index and no historical generations (`docs/retention.md`). Publication is a journaled in-place update or a sibling-directory swap.
- **Local trust.** Any process that can reach the loopback endpoint can read every index. Coordination uses advisory OS file locks.

None of these three transfers to Server, and several AIRK mechanisms exist only to compensate for them. Section 6 names those.

## 4. Recommendations

### R1. Put a search execution record on every search reply

**AIRK.** Every search response carries `SearchExecution { version, requested_mode, served_mode, incomplete, incomplete_reason }` (`crates/airk-core/src/search_execution.rs`). `incomplete_reason` is a closed enum. Truncation by count or bytes is a separate flag, and readiness is a third. A saved policy (`PartialHybrid`, `LexicalWhilePending`, `RequireComplete`) decides whether a default search may degrade; an explicit vector or hybrid request never degrades and fails with a typed `SearchUnavailable` instead. The effect is that a lexical-only answer can never be mistaken for a hybrid one.

**Server today.** `SearchResult` is `hits` plus a `ProvenanceEnvelope` of chunk ids, source ids, paths, content hashes, `index_version`, `event_watermark` and `provider_fingerprint` ([`retrieval.rs`](../../src/munarium-core/src/retrieval.rs)), and `SearchResponse` mirrors it on the wire ([`munarium-api-types/src/lib.rs`](../../src/munarium-api-types/src/lib.rs)). The envelope says what the index was. It does not say how this query ran:

- A collection built without vectors is legitimately lexical-only (`PreparedSearchQuery.embedding` is `None` by design). The caller can only infer that from every hit having `vector_rank: null`.
- A scope rolled out to the datastore and one served from PostgreSQL return the same shape. The envelope has no engine field, although [`serving.rs`](../../src/munarium-retrieval/src/serving.rs) goes to some length to keep the two engines from being confused internally.
- The datastore already produces exactly the right observations per leg, in `CandidateDiagnostics` ([`diagnostics.rs`](../../src/munarium-datastore/src/diagnostics.rs)): `requested`, `fetched`, `accepted`, `rejected`, `work_limit`, and `exhausted: Option<bool>` where `None` means unknown. They stop at the adapter and never reach the response.

**Borrow.** Add an optional, versioned `execution` object beside `envelope`: the engine that served (`postgres` or `datastore`), the legs that ran (`lexical`, `vector`, or both), and per leg the candidate limit, the number fetched and `exhausted` as a three-state value. Keep it additive and `skip_serializing_if` absent so existing clients and golden vectors are unaffected, and mirror it in the gRPC message.

**Why it fits.** Server's stated product is reproducibility. An answer that cannot say which engine and which legs produced it is reproducible only by someone who also knows the rollout selector's state at that moment. The data exists; this is plumbing and a contract addition.

**Cost.** One contract change across REST, gRPC and four SDKs. The PostgreSQL adapter cannot report `exhausted` without extra work and should say `unknown` instead of guessing, which is the point of the three-state field.

### R2. Report an outcome for every member of a multi-collection read

**AIRK.** A combined read returns `CombinedReadResult` with one `ComposedMember` per index. Each member carries its budget `allocation`, its unmodified native `result`, `elapsed_ms`, and `outcome: MemberOutcome::{Complete, Exhausted, Failed, Unqueried}` (`crates/airk-kernel/src/combined_reads.rs`). The caller chooses `MemberAvailability::Report` (return what succeeded and say what did not) or `Require` (fail the whole read with `member_unavailable` and the list of failed members). Budgets (`limit`, bytes, deadline) are split across members deterministically by `SharedBudgets::allocate`, in canonical id order, so a rerun divides the same way.

**Server today.** `search_collections_bounded` in [`sessions_api.rs`](../../src/munarium-server/src/sessions_api.rs) fans out with a semaphore and joins every task, which is sound. What the caller learns afterward is coarser:

- A member that returns `KernelError::NotFound` is added to `skipped`, a list of collection names with no reason attached.
- Any other member error returns from the whole operation (`Err(e) => return Err(e.into())`), discarding the members that succeeded.
- There is no shared deadline. Each member runs under whatever the request as a whole is allowed.

**Borrow.** Replace `skipped: Vec<String>` with a per-member record: collection, outcome (`complete`, `no_active_index`, `failed`, `not_queried`), a content-free reason code and elapsed time. Add a request-level availability policy with `require` as the default, so current fail-whole behaviour is preserved, and `report` as an opt-in for callers such as research profiles that prefer a partial answer they can see is partial.

**Why it fits.** Server already refuses to let a truncated table support a completeness claim (G4 in [`hierarchy.rs`](../../src/munarium-core/src/hierarchy.rs)). A multi-collection answer missing one collection is the same hazard one level up, and today it is either invisible (`skipped` with no reason) or fatal.

**Cost.** The `report` mode needs care with the evidence hierarchy: a partial fan-out must mark every downstream completeness claim as unsupported. Without that, this recommendation makes things worse. Keep `skipped` populated for one release for compatibility.

### R3. Run PDF text extraction in a resource-limited child process

**AIRK.** The PDF parser runs as a child of the service's own executable (`std::env::current_exe()` with a private role argument) under `ProcessLimits { memory_bytes: 512 MiB, cpu_seconds: 10 }`, with output capped at 64 MiB and a page cap (`crates/airk-ingest/src/pdf_worker.rs`, `provider_process.rs`). Limits are applied through a Job Object on Windows and `setrlimit` on Unix (`crates/airk-process/src/child_group.rs`). A service-wide `PdfSlot` admits one parse at a time with FIFO order and a 60-second wait; its `admission_step` is a pure function over an injected clock. A document that exceeds its budget is recorded with the skip code `extraction_budget_exceeded`, distinct from `extraction_failed`, and counted in coverage. AIRK's documents are candid that Office parsing is still in-process and that in-process parser calls "are not forcibly preempted".

**Server today.** [`pdf.rs`](../../src/munarium-extract/src/pdf.rs) wraps `pdf_extract::extract_text_from_mem` in `catch_unwind`, and `extract_source` in [`munarium-retrieval-pg/src/lib.rs`](../../src/munarium-retrieval-pg/src/lib.rs) moves it to the blocking pool. The comment there names the remaining gap precisely: "a PDF parse with no time bound". Three consequences follow:

- A blocking task cannot be cancelled. A pathological PDF occupies a blocking thread until it finishes or the process exits, and a build that awaits it holds its job lease throughout.
- `catch_unwind` contains a panic. It does not contain unbounded allocation, and an out-of-memory kill takes the whole replica, including its serving planes.
- The input is tenant-supplied, up to the 256 MiB source ceiling.

DOCX is in better shape: [`docx.rs`](../../src/munarium-extract/src/docx.rs) bounds the inflated XML at 128 MiB and checks both the declared and the actual size.

**Borrow.** A hidden extraction role in the existing `munarium-server` binary, started per document with address-space and CPU limits, a wall-clock deadline enforced by the parent, bounded output over a pipe, and a small concurrency cap. Map the outcomes to distinct extraction statuses: failed, budget exceeded, deferred. Budget-exceeded documents should be eligible for the existing document-intelligence escalation in the same way empty ones are.

**Why it fits.** Re-executing the same static binary needs no additional file in the distroless image and no C dependency, so it is compatible with the musl constraint that rules out the usual sandboxing libraries. On Linux the limits are `setrlimit` calls before `exec`.

**Cost.** Process start per document is slower than a function call; a warm worker pool is a later optimisation and should not be the first version. Limits need to be configuration with conservative defaults, since legitimate large PDFs exist. This also changes extraction outcomes for some documents, so `EXTRACTOR_SET_VERSION` in [`munarium-extract/src/lib.rs`](../../src/munarium-extract/src/lib.rs) must be considered: a document that now exceeds a budget produces different index content.

### R4. Add a fencing generation to build jobs

**AIRK.** Every claim, completion, progress update and recovery is checked against a per-project `fencing_generation` that increments each time a worker is admitted (`crates/airk-store/src/service/registry/jobs.rs`, `registry.rs`). A stale holder gets `JobError::StaleWorker`. Identity of the holder is not enough; the generation is what makes a late write from a previous holder harmless.

**Server today.** [`jobs.rs`](../../src/munarium-store-pg/src/jobs.rs) guards `heartbeat` and `complete` with `state = 'running' AND claimed_by = $node`. The node id is `Config.instance_id`, resolved from `MUNARIUM_INSTANCE_ID`, then `HOSTNAME`, then `COMPUTERNAME` ([`config.rs`](../../src/munarium-server/src/config.rs)). The guard therefore holds between two differently named replicas and fails in these cases:

- A replica restarts under the same name (a StatefulSet pod, a compose service, or an operator-set `MUNARIUM_INSTANCE_ID`) and reclaims its own lapsed job. Both the old and the new execution satisfy `claimed_by`.
- One process claims the same job twice after a lapse, which the claim query allows whenever `attempts < max`.

In both, a slow first attempt can complete over the second.

**Borrow.** `attempts` already increments atomically inside the claim transaction. Return it from `claim_any`, carry it in the builder loop, and add `AND attempts = $n` to `heartbeat` and `complete`. No migration is needed and the change is local to one file and its caller in [`datastore_jobs.rs`](../../src/munarium-server/src/datastore_jobs.rs).

**Cost.** Small. The artifact attempts table ([`attempts.rs`](../../src/munarium-store-pg/src/attempts.rs)) has its own lease semantics and was not reviewed for the same weakness; it should be checked at the same time.

### R5. Store a content-free failure code for jobs, and keep raw causes in the log

**AIRK.** `JobOutcome::Failed { code }` must be at most 128 characters of `[a-z0-9_]` and is validated on write. Log records obey the same rule (`crates/airk-core/src/service_logs.rs`). The raw error chain goes to a separate local ring that is excluded from reports and from every client-facing response. Workers map error types to codes by downcast, never by matching message text.

**Server today.** The builder loop calls `run_one_job(...).map_err(|e| e.to_string())` and stores the string in `index_build_jobs.error`; `job_dto` returns that column unchanged in `IndexBuildJobDto.error` ([`datastore_jobs.rs`](../../src/munarium-server/src/datastore_jobs.rs)). For `KernelError::Storage`, the string is the database driver's message. Server has already decided that is not client-facing: `client_facing_error` in [`error.rs`](../../src/munarium-server/src/error.rs) replaces a storage error's text with "storage error; see the server log" for per-item ingest results, for the stated reason that the message carries table, column and constraint names. The job path does not go through it.

**Borrow.** Store a stable code (`storage`, `datastore_unavailable`, `invalid_input`, `provider`, `extraction_budget_exceeded`, `lease_lost`) derived from the `KernelError` variant, log the full text server-side with the job id, and return only the code and a fixed message in the DTO. Document the codes in the error registry beside the problem slugs, and extend the registry check in [`docs_coverage.rs`](../../src/munarium-server/src/docs_coverage.rs) (`every_problem_slug_is_in_the_error_registry`) to cover them.

**Cost.** Small, with one compatibility question: `error` is a documented field. Adding `error_code` and routing `error` through `client_facing_error` keeps the shape and removes the leak.

### R6. Make cancellation of a running build cooperative

**AIRK.** Cancelling a running job inserts a durable cancellation intent; the job stays `running` until its fenced worker reaches a safe boundary and completes it as cancelled. A cancel that arrives after an atomic publication leaves that publication committed.

**Server today.** `BuildJobs::cancel` sets `state = 'cancelled'` for pending and running jobs. For a running job, the next heartbeat returns `false`, the heartbeat task logs and stops, and nothing else happens: `run_one_job` is not signalled and runs to the end, after which `complete` is a no-op. The doc comment on `cancel` describes this accurately ("cancellation stops the JOB from being re-offered"). An operator who cancels a long direct build to free a builder gets the builder back only when the build would have finished anyway.

**Borrow.** A cancellation token created per claimed job, cancelled by the heartbeat task when it observes a lost or cancelled lease, and checked at the build's natural boundaries: between sources during extraction, between embedding batches, before seal, before upload. The mirror build already has a strict ordering in [`mirror.rs`](../../src/munarium-retrieval/src/mirror.rs) in which the manifest is written last, so stopping before that point leaves nothing advertised.

**Cost.** Each boundary needs a decision about what is left behind (staging directories, a `sealed` catalog row) and a test for it. This pairs naturally with R4, since both change what the builder loop does when a heartbeat fails.

### R7. Admit by work class, with a slot reserved for control

**AIRK.** One typed setting, `query_concurrency`, accepts only 4, 8, 16, 32 or 64. The broker allocates one more than the setting and refuses ordinary queries at `allowance - 1`, so a status or control request always has a slot (`crates/airk-kernel/src/service_broker.rs`). Heavy work is admitted separately through `WorkAllowance` grants that name the exact job and carry thread, working-memory and background-IO amounts; candidates are ranked so that a waiter older than five seconds moves to the front (`crates/airk-kernel/src/service_resources.rs`). Lowering a limit never revokes a grant: existing charges remain and new grants stop until the total drains below the new value.

**Server today.** [`state.rs`](../../src/munarium-server/src/state.rs) creates `rest_permits` and `grpc_permits`, each sized `max_concurrency`, and [`middleware.rs`](../../src/munarium-server/src/middleware.rs) takes one with `try_acquire_owned` for every `/v1/` and `/v1.2/` request, shedding with 503 and `Retry-After: 1` at the ceiling. Shedding early instead of queueing is the right default. Two properties of the single pool are worth changing:

- **Control shares the pool with load.** Health and metrics are on the separate ops port and are safe. But the operator's levers are on the REST plane: `PUT /v1/retrieval-rollout`, job cancel, artifact promotion, token revocation. When queries saturate the pool, the request that would relieve the saturation is shed with them.
- **A permit is a permit.** A point read and a synchronous index build each cost one. The synchronous build endpoints deliberately remain ([`datastore_jobs.rs`](../../src/munarium-server/src/datastore_jobs.rs) explains why), so `max_concurrency` inline builds can hold every permit while each also occupies a blocking thread.

**Borrow.** Three things, in order of value. First, hold back a small number of permits that only management-role routes may take. Second, give heavy routes (inline builds, bulk ingest, extraction) their own smaller pool so they cannot exhaust the general one. Third, when capacity becomes runtime-configurable, apply AIRK's rule that a decrease drains and never revokes.

**Cost.** Route classification must be explicit and reviewed; a route silently placed in the wrong class is a regression that load tests may not reveal. AIRK's memory-based grants (observed RSS, hysteresis) are sized for one machine and should not be copied; in a cluster the equivalent limit belongs to the pod's resource requests.

### R8. Claim build jobs fairly across tenants

**AIRK.** Work candidates are ranked by kind, then by whether the project was the last one served, then by sequence, with the aging rule from R7. Per-project ordering is preserved; global ordering is not strictly FIFO.

**Server today.** `claim_any` selects with `ORDER BY created_at ... FOR UPDATE SKIP LOCKED LIMIT 1`, globally across tenants. The module comment explains why the claim is global (a builder serves every tenant). The ordering has a side effect the comment does not discuss: a tenant that enqueues several hundred backfill jobs is served to completion before a job any other tenant enqueues afterward.

**Borrow.** Order by each tenant's oldest pending job, preferring tenants with no job currently running, then by `created_at`. This is one query change. Keep AIRK's aging guard in spirit: a job older than a threshold is taken regardless, so no ordering rule can starve work indefinitely.

**Cost.** The query becomes slightly more expensive and needs an index review at realistic queue depths. Fairness by tenant is a policy choice; some operators will want priority instead, and that should be a later, explicit feature.

### R9. Give jobs progress and a transactional event ring

**AIRK.** Two mechanisms.

- **Progress** is one bounded observation per project, `WorkProgress`, in which totals are `Option`: an unknown total is null, never zero and never an estimate. Updates are fenced and counters may not decrease. Why a job is waiting travels separately as a transient `WaitObservation { job, reason, age_ms }` with a closed `WaitReason` enum, and is never persisted.
- **Events** are written by database triggers on the job and project tables, so an event commits or rolls back with the state change that caused it and a replayed request cannot create a duplicate. A trigger also bounds the table to the newest 1,024 rows. Readers page with a cursor that pins a snapshot, and the page reports `retained_prefix_lost` when older events have been pruned (`crates/airk-store/src/service/registry/events.rs`).

**Server today.** A build job exposes `state`, `attempts`, `claimed_by` and, at the end, `result` or `error`. Between claim and completion there is nothing to observe except the heartbeat moving `claimed_at`. Builds on real corpora run for a long time; the heartbeat comment in [`jobs.rs`](../../src/munarium-store-pg/src/jobs.rs) cites the newspaper batches as longer than the lease. An operator cannot distinguish a build that is embedding its last batch from one stuck on its first source.

**Borrow.** A small fenced progress column on the job row (phase, units done, units total where known, last advance time), updated at the same boundaries R6 uses. Separately, an append-only `index_build_job_events` table populated by triggers on `index_build_jobs`, with the gap flag on read.

**Why triggers.** Server's audit posture is that a state change and its record are one fact. A trigger gives that for free and cannot be forgotten by a new code path. Both tables are additive migrations.

**Cost.** Triggers are a new technique in this schema and need a conformance test against both store backends; the in-memory store must produce the same events by other means. Retention needs a decision: AIRK bounds by row count, while Server would more naturally bound by age per tenant and list the table in the retention inventory.

### R10. Record artifact damage durably, with a bound and without overclaiming

**AIRK.** Detected damage is written to an `IntegrityObservation` receipt holding at most 32 `KnownDamage` entries. When full, a new record is refused with "repair required" instead of evicting an old one. The receipt's `PhysicalAudit` type has a single variant, `Unknown`: the absence of recorded damage is never presented as a clean audit (`crates/airk-store/src/service/collection/integrity.rs`). Verification detects and records; it never authorises an automatic repair.

**Server today.** [`hydrate.rs`](../../src/munarium-datastore/src/hydrate.rs) quarantines a corrupt cache key in `CacheState.quarantined`, an in-memory set, keyed correctly by the full `ArtifactCacheKey` so one tenant's corruption does not affect another's identical content. No quarantine state appears in `munarium-store-pg`. So:

- A restart forgets the quarantine. The replica hydrates the same bytes again, verifies them again and fails again.
- Each replica discovers the damage independently. The catalog continues to show the artifact as `verified`, and other replicas keep routing to it.
- The executor does recheck `ArtifactState::Verified` on every query ([`executor.rs`](../../src/munarium-retrieval/src/executor.rs)), so a catalog state change would take effect everywhere at once. Nothing currently makes that change when a replica finds corruption.

**Borrow.** When hydration fails integrity against the object store's bytes (as opposed to a local disk fault), write a bounded damage record to the catalog: artifact, component, expected and observed digest, observing instance, time. Let that record gate serving in the same place the `Verified` check already runs. Follow AIRK's two rules: bound the record count and refuse to evict, and never report "no damage recorded" as "verified intact".

**Cost.** The hard part is the distinction in the first sentence. A local disk error must not condemn a good artifact for every replica. Requiring a second fetch to fail the same way before recording, or two independent replicas to agree, is the kind of rule that needs its own design note.

### R11. Typed retry guidance on problem responses

**AIRK.** `ReadRetryHint { status, retryable, code, retry_after_ms, action }` is produced only by downcasting to a known typed error; text that merely looks like a busy error is never retryable (`crates/airk-kernel/src/retrieval_retry.rs`). The `action` field separates "repeat the same request" from "retry later without replay". On the admin contract, validation rejects a body that carries `retry_after_ms` on a non-retryable error.

**Server today.** `Problem` ([`munarium-api-types/src/lib.rs`](../../src/munarium-api-types/src/lib.rs)) has no retry member. Retry semantics are conveyed by status code, a `Retry-After` header on 429 and on shed 503, and prose in the error registry. The guarded command recovery document has to state in words that `command-unresolved` "is **not** a retryable head conflict" ([`command-recovery.md`](../ops/command-recovery.md)), because a client cannot tell the two 409s apart mechanically except by slug.

**Borrow.** Two optional extension members, `retryable` and `retry_action`, with a closed set of actions: `repeat_same_request` (head conflict, overloaded), `retry_later` (rate limited, daily cap), `do_not_retry_inspect` (command unresolved), `do_not_retry` (validation, authorisation). Carry the same names in the gRPC `ErrorInfo` metadata, as the existing members are. A table-driven test over every registered slug keeps the mapping total.

**Cost.** Low, and low priority: SDKs can already switch on the slug. The value is that generic middleware and agents stop guessing.

### R12. Validating identifier newtypes at new boundaries

**AIRK.** A `hex_identity!` macro produces opaque id types that validate on deserialisation through `#[serde(try_from = "String")]` (`crates/airk-core/src/identity.rs`); `catalog_id!` produces prefixed ids such as `idx_` and `grt_`. Uncertain states are enum variants: `FenceUnknown`, `OutcomeUnknown`, `MutationPossible::{Yes, No, Maybe, Unknown}`.

**Server today.** Mixed. `ArtifactCacheKey` is a proper composite key and its documentation explains exactly why. `JobKind` is an enum with `parse`. But `BuildJob` in [`jobs.rs`](../../src/munarium-store-pg/src/jobs.rs) carries `tenant_id`, `job_id`, `kind` and `state` as `String`, the DTO's `state` is a `String` documented by a comment listing its six values, and most function signatures across the store take `tenant_id: &str` next to other `&str` identifiers. Swapping two arguments compiles.

**Borrow.** For new code and for types touched by R4 to R10: an enum for job state, and newtypes for tenant, job and artifact ids constructed only by validating parsers. Do not attempt a workspace-wide conversion.

**Cost.** AIRK shows the limit of this pattern. Its contract-generation pilot concluded that generators cannot replace validators because `try_from` rules are invisible to schema derivation (`docs/simplify/contract-generation-pilot.md`). Server generates OpenAPI from code with utoipa, so each newtype needs an explicit schema annotation or the published contract will silently lose the constraint.

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
- *Bit-exact verification of cached embeddings on rebuild.* AIRK recomputes on a full rebuild and requires bit equality with the cache. Server's embeddings come from remote providers that do not promise determinism, so the check would fail for reasons unrelated to corruption.
- *Bounded change retention with coverage floors.* The idea that pruned history must report where complete coverage begins is good, and Server's append-only ledger does not prune in the way that makes it necessary.
- *Arbiter (optional local rerank and answer models).* The enrollment and qualification-gate design is careful, but no AIRK profile is qualified and Server's model path is bring-your-own-key through the provider gateway.

## 7. Suggested order

The recommendations are independent except where noted. One reasonable sequence:

1. **R4 and R5** together. Both are small, both are confined to the build-job path, and R5 closes a disclosure that Server's own code treats as a defect elsewhere.
2. **R1.** A contract addition with no behavioural change, and it makes the datastore rollout observable from the client side.
3. **R3.** The largest reduction in exposure to tenant-supplied input. Needs a design note for limits, statuses and the extractor-version question.
4. **R6, R8 and R9** as one piece of work on the builder. R6 and R9 share their boundaries; R8 is a query change that is easiest to test once events exist.
5. **R2**, after deciding how a partial fan-out interacts with completeness claims.
6. **R7 and R10**, each of which needs its own design decision recorded first.
7. **R11 and R12** opportunistically, when the affected types are being changed anyway.

Each item that changes a wire contract (R1, R2, R5, R9, R11) is additive and must go through the documented contract process, with REST and gRPC parity and SDK conformance updated together.

## 8. Semantic embedding and reranking through an inference endpoint

This section studies AIRK's Arbiter feature, which runs small Qwen models locally to improve retrieval, and asks how Munarium Server can gain a semantic embedder and a reranker without taking a model runtime, a platform dependency or model weights into the Server image. It is a feasibility analysis. It proposes no contract and commits to no model.

### 8.1 Conclusions first

1. **The largest available gain is in the vector leg.** Server's only embedder, at index time and at query time, is `local-hash@1`: a 256-dimension bag-of-tokens feature hash whose own doc comment says it is "not semantically deep" ([`munarium-retrieval-pg/src/lib.rs`](../../src/munarium-retrieval-pg/src/lib.rs)). Hybrid search in Server today is therefore lexical search fused with a second, hashed form of lexical overlap. A real embedding model changes what the vector leg can find. AIRK made the same choice: its always-on retrieval model is a 384-dimension MiniLM, and the Qwen models are optional additions on top of it.
2. **A reranker is the natural second step.** AIRK's "Focus" stage scores each retrieved passage against the query with a 0.6B-parameter Qwen reranker and reorders the top of the list. It needs no reindex, it is additive, and it can be switched on per collection.
3. **Server should implement one seam and host nothing.** Both capabilities are calls to an inference endpoint through the existing provider gateway: the OpenAI-style embeddings route, which the gateway already speaks, and a rerank route, which it does not yet. Server's static image, its rule against native dependencies and its core crate are untouched, and a deployment that enables neither capability behaves exactly as it does today (8.6).
4. **The operator's own endpoint is the primary deployment model.** An operator running vLLM, or another server implementing the same two routes, on their own hardware configures Server to use it. Retrieval then sends nothing to a public model service (8.7).
5. **A packaged GPU image, Munarium Lens, is one implementation of that seam and a separate project.** Munarium Lens is the proposed name for a reference inference image for RTX-class GPUs, with pinned and qualified weights, built in its own repository (`munarium-lens`). Model catalogs, weight downloads, precision variants and fine-tuning belong there. Server contains no code specific to it (8.8).
6. **AIRK's Arbiter is a source of design, not of evidence.** Its controls and refusal rules are carefully built. It has no retrieval-quality result for either Qwen stage, every shipped profile is labelled `unqualified-local-test`, and no FP16 or GPU variant has completed a single qualified forward pass. Server would have to produce its own measurements, and it has better tools for that than AIRK does (8.10).

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

AIRK's records are candid, and a reader evaluating this feature for Server should take them at face value.

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
- **Provider configurations belong to the tenant.** The gateway "speaks to LLM endpoints with the TENANT's credentials at the TENANT's endpoints". An index embedder is a property of a collection, which raises a question of ownership that 8.9 takes up.
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
- *Query-time cost.* Each query now makes one network call before the vector leg can run. That call needs a deadline and a stated behaviour when it fails; see the policy in 8.9.
- *Identity.* The embedder identity in `index_version` must cover more than model and dimension. Several current embedding models, the Qwen3 embedding family among them, embed a query with an instruction prefix and a passage without one. The query instruction, the passage instruction, normalization and any dimension truncation all change the vectors and all belong in the identity.
- *Storage.* The datastore artifact format carries the dimension per artifact. The PostgreSQL path does not: both chunk tables declare `VECTOR(256)` (8.4), so a second dimension needs an additive migration. The pgvector HNSW index accepts at most 2,000 dimensions for the `vector` type, which admits the 1,024-dimension Qwen3-Embedding-0.6B and excludes the larger members of that family unless their output is truncated or stored at half precision.
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
| In-process pure-Rust runtime (`rten`, already used for OCR) | Yes | Small encoders are plausible; larger decoder-based graphs are unverified for operator coverage and speed | Memory is shared with the replica; panics can be contained | A bounded spike, feature-flagged and off by default. The only route to semantic retrieval with no second service |
| In-process ONNX Runtime (AIRK's approach) | No. AIRK's static build targets MSVC and glibc only, and it is a C++ dependency | All | None. A native crash or out-of-memory kill takes the replica and its serving planes with it | Not recommended |
| A sidecar speaking a protocol private to Munarium | Yes | All | Process and container boundary | Not recommended. It ties Server to one implementation when a widely implemented protocol already exists |

**Two operations.** The seam is two traits in `munarium-core`, each asynchronous and fallible, with the provider gateway as their first implementation.

- *Embed.* `ModelProvider::embed` already exists and the OpenAI-compatible adapter already posts to `{endpoint}/embeddings` ([`munarium-providers/src/lib.rs`](../../src/munarium-providers/src/lib.rs)). What is missing is the caller: `QueryEmbedder` has to become asynchronous and fallible, and the build and query paths have to use it.
- *Rerank.* A new `rerank` operation: one query and a list of passages in, one score per passage out. This is a distinct endpoint shape, separate from chat completion, so Server never needs access to a model's logits. vLLM's documentation, as read on 19 November 2026, lists `/rerank`, `/v1/rerank` and `/v2/rerank` as implementing Jina's rerank API and compatible with Cohere's, with a request of `model`, `query`, `documents` and optional `top_n`, and a response of `results[]` carrying `index` and `relevance_score`. It lists `/v1/embeddings` as compatible with OpenAI's Embeddings API. Other inference servers implement the same shapes. The exact paths and fields should be confirmed against the vLLM version targeted before a contract is fixed.

**Rules Server applies to a rerank reply.** These carry AIRK's three rules across a network boundary.

- Server uses the order only. A `relevance_score` is not comparable between models and is not blended with the fusion score. Ties keep fused order, so the result is deterministic for a given set of scores.
- The reply must account for every passage sent, each index exactly once. A missing, repeated or out-of-range index, a non-finite score, a timeout or a transport error means the stage is not applied and fused order is returned unchanged with a typed notice.
- Input that exceeds the model's token limit is a refusal for that request, and is not silently truncated by Server. The candidate cap and the token cap are set by Server's budget, not left to the endpoint's defaults.

**An `openai-compatible` provider kind.** The existing `openai` kind can already be pointed at such an endpoint with `spec.endpoint`, but it was shaped for a hosted service. A dedicated kind, modelled on the `ollama` one, fits a self-hosted endpoint better:

- an explicit endpoint is required, validated as the Ollama endpoint is (an HTTP or HTTPS base URL with no credentials, query or fragment);
- the credential is optional, since a private endpoint may be protected at the network layer. `openai` requires `credentialRef` today;
- models are listed explicitly per capability (`complete`, `embed` and a new `rerank`), with no built-in tier table;
- it is never chosen by automatic default selection;
- its usage is recorded in tokens and calls on a route with no price, so it appears in provider accounting without entering monetary totals.

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

### 8.8 Munarium Lens: a reference inference image

Munarium Lens is the proposed name for a separate project, in its own repository (`munarium-lens`), that packages one qualified implementation of the endpoint for deployments that want a supported starting point and have an NVIDIA RTX-class GPU. The name describes the role: it brings evidence into focus and produces none of its own. Nothing under that name exists yet. It is an implementation of the seam in 8.6 and nothing more: Server contains no code specific to it, and anything it can do, an operator's own endpoint can do.

- **Serving.** vLLM on a CUDA base image, exposing the embeddings and rerank routes on one port. The vLLM pages read for this section describe a single served model per server, so the working assumption is one process per model behind a small router. For encoder-class models, Hugging Face's Text Embeddings Inference is a lighter server implementing the same shapes and is worth comparing.
- **Starting models.** Qwen3-Embedding-0.6B and Qwen3-Reranker-0.6B, both Apache-2.0 and both listed as supported in vLLM's documentation. The official reranker checkpoint needs a documented architecture override, or a checkpoint already converted to a sequence-classification head. At half precision each is roughly 1.2 GB of weights, which leaves room on an 8 to 12 GB card; the larger members of the family are candidates for 24 GB cards and must be measured, not assumed, to be better.
- **Weights outside the image.** The image carries no weights. At start it fetches them by a manifest that pins every file by SHA-256 and by upstream revision, verifies them, and refuses to serve on a mismatch. An offline import path serves deployments without egress. This is where AIRK's compiled catalog and digest-pinned download belong; Server's object store and build queue are not involved.
- **Precision.** FP16 is the GPU default. AIRK's three practices apply here unchanged: a converted variant is accepted by a parity gate against FP32 on a stated evaluation set and not by inspection; each precision is a separate served name with its own identity; and a variant that does not fit falls back to another precision of the same model or refuses, never to a different model.
- **Identity.** The served model name encodes model, upstream revision and precision. Server's receipt and `index_version` then distinguish variants with no knowledge of the image.
- **Qualification.** The repository holds the parity gate and a benchmark on public corpora, and publishes their results with each release. Qualification on a deployment's own collections remains a Server activity (8.10).
- **Isolation.** No egress after weights are loaded, authentication on the port, and a network policy admitting only Server. No request content is logged.
- **Custom weights.** The first release should use publisher weights with instruction text written for evidence retrieval. Both of AIRK's fine-tuning attempts failed to beat the originals. Fine-tuning, most plausibly of the reranker, is justified only by held-out evidence of a gain, on training data whose licence permits it, and never on a customer's corpus for weights that are distributed to others.

### 8.9 Enablement and governance

**Two levels of "on".**

- *Deployment capability.* Whether this deployment has a provider configuration offering `embed` or `rerank` with a passing probe. Report it through the existing capability mechanism in [`capabilities.rs`](../../src/munarium-retrieval/src/capabilities.rs), which already records each capability with the reason it is on or off.
- *Enrollment.* Which collections use it, set through a management-role API with compare-and-set on a revision, in the same style as the retrieval rollout selector. AIRK's equivalent is per-index enrollment with `expected_revision` on every change. Enrollment per collection lets an operator qualify one corpus at a time.

**Whose endpoint a collection uses.** Provider configurations carry a tenant's credentials to a tenant's endpoint, and are selected per call. An index's embedder is different: it is fixed when the collection is built, and every later query against that collection must use the same model whoever the caller is. The binding therefore belongs to the collection and not to the caller. The proposal is that a collection's index settings name one provider configuration and model for embedding, and one for reranking, resolved in the scope that owns the collection, and that a query never substitutes the caller's own configuration. This needs a recorded decision before IR-2, because it also determines whose budget an embedding call is charged to.

**States a caller can see.** Disabled, not enrolled for this collection, endpoint not configured, probe failed, endpoint unavailable, over budget, ready. For any state but ready, the reply says the stage was not applied and why.

**Refusal or degradation.** Server's rule is that serving refuses and optional telemetry degrades. Both stages sit between the two: they are additive, but they change the answer. The proposal is a per-collection policy with two values, mirroring AIRK's search policy.

- `prefer`: apply the stage when it is ready. Otherwise the reranker returns fused order, and a query whose embedding call fails runs without the vector leg. Either way the reply discloses it.
- `require`: fail the request when the stage cannot run.

Governed query policies that pin behaviour for reproducibility would use `require` or leave the feature off. A collection built with a semantic embedder has no hash vectors to fall back to, so `prefer` for the embedder means a lexical-only answer, and that should be stated in the collection's settings where the operator chooses it.

**Budgets.** One absolute deadline carried through every step and checked before dispatch and before delivery; a cap on candidates and on input tokens; reservation before queueing and settlement after. AIRK's `Budget` type and its rule that work finishing after the deadline is discarded are the model to follow. Retries against the endpoint count against the same deadline.

**Tenancy.**

- The model is shared by all tenants; the inputs are not. No request state may persist at the endpoint, and one request's inputs must not affect another's outputs. A stricter rule, that passages from different tenants never share a batch, cannot be enforced from Server against a server that batches continuously, as vLLM does. Batch companions can change a result in the last bits of a floating-point score, which matters for reproducibility and not for confidentiality. A deployment that needs bit-stable results across load should say so in its endpoint configuration and measure it.
- Each rerank request carries one caller's query and passages only. Server does not combine callers into one request.
- Usage goes through the provider gateway's accounting like any other provider call, in tokens and calls. A self-hosted endpoint has no price, so it contributes nothing to monetary totals.
- Admission belongs in the heavy work class proposed in R7, with a small bounded queue, so that a slow endpoint cannot occupy the serving plane.

### 8.10 Evaluating before enabling

AIRK built the machinery before it had the measurements. Server can reverse that order.

1. **Shadow first.** Run the reranker or the new embedder as a shadow candidate on sampled production queries. The user's answer is unchanged, and the comparison record shows how often and how far the order moved, by query fingerprint.
2. **Judge a fixed set per corpus.** Use frozen evaluation records with graded judgments to measure nDCG and recall at the cut-off Server serves, with and without the stage.
3. **Enroll per collection on a recorded result.** A collection is enrolled with a result for that corpus and that model identity on record. This is the operator's decision on a bring-your-own endpoint; Server's part is to make the measurement easy and the record durable. AIRK's device of letting unqualified profiles run while the version number is below a threshold should not be copied.
4. **Start with publisher weights.** Specialization should begin with the instruction text given to the embedder and the reranker and with measurement on Munarium corpora, and move to fine-tuning only with held-out evidence that it helps (8.8).

One caution specific to instruction text: AIRK's weak code-search result came from using the publisher's web-search instruction on a different domain. Server's corpora are documents in natural language, which is closer to what these models were trained on, but that is a reason to expect a better result, not a measurement of one.

### 8.11 What not to take from Arbiter

- **In-process native inference.** The reasons are in 8.6.
- **A model catalog and weight download inside the product.** AIRK needs them because it is the only process on the machine. In Server they would add egress, storage and verification duties to a service that should hold none of them. Munarium Lens is the place for them (8.8).
- **The 8,192-token Context configuration.** Eighty-six seconds on CPU and a 120-second deadline are outside anything a request-serving plane can offer.
- **GPU custody as built.** It is Windows-only CUDA with runtime libraries placed beside the executable. In a cluster, GPU allocation belongs to the scheduler and the inference container.
- **Local asset custody checks.** File identity, access-control lists and local-volume checks have no meaning for a remote endpoint.
- **The scale of the process.** Arbiter's plan document is about 156 KB, with eleven steps in progress and five blocked. The useful core for Server (typed availability, budgets, permutation-only reranking with a receipt, per-index enrollment) is a small fraction of that.

### 8.12 A possible sequence

Server work:

| Step | Work | Depends on | New runtime in the image |
|---|---|---|---|
| IR-0 | Fallible asynchronous embedder trait and a reranker trait in `munarium-core`; `rerank` on the provider seam; embedder and reranker identity and applied flags in the execution record | R1 | None |
| IR-1 | `openai-compatible` provider kind with explicit `embed` and `rerank` model lists; conformance probe | IR-0 | None |
| IR-2 | Additive migration for a second vector dimension; endpoint-backed embedder for enrolled collections, built side by side and compared in shadow | IR-1; the collection-binding decision in 8.9; R6, R8, R9 make large rebuilds manageable | None |
| IR-3 | Reranker after fusion through the endpoint, shadow only, measured on two or three corpus families | IR-1 | None |
| IR-4 | Enrollment API, per-collection `prefer` or `require` policy, capability reporting and admission class | IR-2 or IR-3 results; R7 | None |
| IR-5 | Shorter context packing when the reranker has applied, measured for answer quality and token use | IR-4 | None |
| IR-6 | Spike: small encoder and small cross-encoder on the pure-Rust runtime, in-process | IR-0 | A feature flag, off by default |

Munarium Lens work, in its own repository and on its own schedule:

| Step | Work | Depends on |
|---|---|---|
| LENS-1 | Image serving the embeddings and rerank routes with publisher weights fetched by pinned manifest; passes Server's conformance probe | The contract from IR-0 and IR-1 |
| LENS-2 | FP16 parity gate against FP32 and a public-corpus benchmark, published with each release | LENS-1 |
| LENS-3 | Instruction text for evidence retrieval; fine-tuning only on held-out evidence of a gain | LENS-2, and results from IR-2 and IR-3 |

IR-0 to IR-3 give any operator with a conforming endpoint a semantic vector leg and a reranker, and do not wait for Munarium Lens. Munarium Lens depends on Server only for the contract.

## 9. The admin web interface

This section compares AIRK's service admin pages with Server's `/admin` operator console and lists what Server could take from AIRK's design and organization. As elsewhere, it proposes and commits to nothing.

### 9.1 Conclusions first

1. **AIRK teaches as much by its mistakes as by its design.** Its shipped admin pages were reviewed by AIRK's own team and found to feel "like API work": long stacked forms, raw JSON as the main view, and internal identifiers typed by hand. The review, and the redesign that followed from it, are the most useful material for Server. The lessons below come from both the pages and the critique.
2. **Server's console is sound in its foundations and thin in its organization.** Server-rendered pages with no JavaScript, real URLs, charts with table twins and honest notices are strengths AIRK does not have. What Server lacks is grouping, a page pattern, a status-first overview, and care around actions.
3. **Four changes matter most.** Group the fourteen flat navigation links; make the overview answer "does anything need me?"; put a review step and a typed confirmation in front of destructive actions; and stop showing a failed read as zero.
4. **Almost all of it can be done without JavaScript.** The recommendations below are written for Server's existing constraint. The two places where a script would help are named.

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
| Runbooks | runbooks, shapes, runs |
| Access and audit | tokens, audit, findings |
| Platform | providers, matrix, health |

Six top-level links, each opening on its first page with the others as sub-tabs, which are ordinary links. Use sentence case for labels, and mark the current page with `aria-current="page"` as AIRK does. No attribute of that kind appears in the console's HTML today.

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

This matters more as actions are added. Switching a scope's retrieval engine, promoting an artifact and cancelling a build job are the levers named in R6 and R7, and each deserves the same two steps. An action whose plan is empty should say there is nothing to do and offer no confirmation at all.

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

**U10. Improve the session without waiting for a new identity model.**

Server's posture is that the API-management layer in front of it is the security boundary, and that stands. Within that posture, four things AIRK does are usability and safety improvements that the console can adopt:

- a sign-out link that clears the cookie;
- a cookie that holds an opaque session value derived from the credential instead of the credential itself, so that the management token is not replayed on every request and does not sit in the browser's cookie store;
- the `Secure` attribute whenever the request arrived over TLS, and the `__Host-` prefix with it;
- an idle expiry.

A launch flow like `airkctl admin open`, in which a command-line tool that already holds the credential mints a single-use code so the token is never pasted into a page, is a natural later step for `mmctl`. These changes touch the security posture document and belong in a reviewed change of their own.

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
| 6 | U10 (session improvements) | Medium; needs its own security review |
| 7 | U11, U13, U14 as pages are touched; U12 when R9 provides progress data | Incremental |
