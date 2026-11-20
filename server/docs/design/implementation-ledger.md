# ClearView implementation ledger

Status recorded **19 November 2026**. The [ClearView plan](README.md) sets the sequence and release criteria. Each row uses the plan's task or decision ID and links to the design that owns it. Planned rows have no implementation evidence recorded here. Verified results apply to the revision and scope named in the row.

When a task advances, add a dated status, link the PR or implementation revision, and record the next unmet prerequisite or acceptance receipt. Link shared decisions and exports in the [decision register](README.md#decision-register). Candidate receipts follow the [release evidence format](README.md#release-evidence).

## Next implementation steps

- **Matrix.** Review the [PR checks](https://github.com/iokaio/munarium-matrix/pull/4/checks), resolve the dependency gate, and collect the installed-peer and standalone Server receipts before switching the consumer.
- **Lens.** The CPU scaffold has passed its tests. The next milestone is the [working image and Server probes](repository-handoffs.md#lens-d1-t2), which depend on the inference integration.
- **Server.** Freeze the Phase 1 job and console fixtures and inventory the Phase 3 routes, permits and extractors.

## Phase 0 · v1.4 baseline and shared decisions

| ID | Task and primary design | Status and next evidence |
|---|---|---|
| P0-T1 | [Freeze the v1.4 baseline and maintain the contract worklist](README.md#p0-t1) | Partial. Released source and image are pinned for Matrix. Record the full release artifact and SDK pins, Phase 1 job and console fixtures, expected results, owners, rollback boundaries and next review dates. |
| Phase 0 | [Prepare qualification environments](README.md#qualification-environments) | Planned. Record named owners, dated availability, access, topology, cost authority and expiry before scheduling each environment. |

<a id="phase-m-matrix-isolation"></a>

## Phase M · Matrix isolation

| ID | Task and primary design | Status and next evidence |
|---|---|---|
| M1 · Step 1 | [Inventory and reconcile the released Matrix surface](repository-handoffs.md#reconcile-the-existing-matrix-repository) | In review. The [transfer inventory](https://github.com/iokaio/munarium-matrix/blob/dfc1f00b273da91e362a15cfe9a95f4a3bca9a15/docs/clearview-transfer.md) pins Server v1.4.0, Server main and Matrix main and records the dispositions. Complete the per-path evidence and owning reviewers' confirmations. |
| M1 · Step 2 | [Make the Matrix destination verifiable](repository-handoffs.md#transfer-order-and-rollback) | In review. [Matrix PR #4](https://github.com/iokaio/munarium-matrix/pull/4) adds the publisher profile, parity tests and operating instructions. See the dated validation below. |
| M1 · Step 3 | [Switch the Server consumer](repository-handoffs.md#transfer-order-and-rollback) | Pending. Pin the reviewed export and qualify the isolated Server source, packages, container and v1.4 rollback under [M1-T1](repository-handoffs.md#m1-t1) and [M1-T2](repository-handoffs.md#m1-t2). |
| M1 · Step 4 | [Remove the superseded Server sources](repository-handoffs.md#transfer-order-and-rollback) | Pending. Coordinate the CI gates and require the matching peer, CLI and reviewer evidence for each removal under [M1-T3](repository-handoffs.md#m1-t3). |

**19 November 2026 · Matrix PR head `dfc1f00`.** Matrix's local gates passed with 372 workspace tests and 49 ignored. The 17 locked wire files reproduced and all eight examples validated. The ten CLI verbs matched a Server source build and the exact `mmctl` binary extracted from the published v1.4.0 image. Server passed 5 contract and 16 evidence-provider tests. Matrix passed 11 worker tests against the published Server image with disposable PostgreSQL.

The PR was open at the status check. DCO, hygiene, contract and unit/store/conformance checks passed. The hosted `cargo-deny` step failed on advisories in the unchanged dependency lockfile. Both hosted interoperability jobs and the image-size job were still running. Standalone candidate packaging and Server-to-Matrix installed-peer qualification are pending. [PR and checks](https://github.com/iokaio/munarium-matrix/pull/4/checks) · [Dependency gate](https://github.com/iokaio/munarium-matrix/actions/runs/44658787326/job/136031541977)

<a id="phase-l-lens-delivery"></a>

## Phase L · Lens delivery

| ID | Task and primary design | Status and next evidence |
|---|---|---|
| LENS-0 | [Build the CPU scaffold](repository-handoffs.md#lens-d1-t1) | Verified from a clean export of [Lens `8cf7f04`](https://github.com/iokaio/munarium-lens/tree/8cf7f040c06bd215cb536b9e06ec3e8f3415bdfb). Both tests passed with `cargo test --locked --offline`, covering loopback-only startup, liveness, typed HTTP 503 responses and shutdown. |
| LENS-1 | [Build the working embedding and reranking image](repository-handoffs.md#lens-d1-t2) | Planned. Pin the runtime, verified model assets and served profile. Run both Server probes on named hardware after the required IR work. |
| LENS-2 | [Qualify hardware, precision and serving behavior](repository-handoffs.md#lens-d1-t3) | Planned. Freeze family thresholds before the evaluation and exercise resource and refusal cases. |
| LENS-3 | [Evaluate retrieval instructions](repository-handoffs.md#lens-d1-t4) | Planned. Record held-out comparison results and the selected instructions. |
| LENS-D1 | [Qualify the completed Lens delivery](repository-handoffs.md#lens-d1) | Partial. The scaffold is verified. The serving image, profile, evaluation and operation receipts are pending. |
| LENS-D2 | [Qualify the workspace end to end](repository-handoffs.md#lens-d2) | Planned. Requires a qualified serving profile and build, query, citation and rollback receipts for all 16 roots. |
| LENS-D2-T1 | [Freeze the 16-root workspace inventory](repository-handoffs.md#lens-d2-t1) | Planned. Resolve all reviewed pins and obtain the corpus owner’s authorization for the restricted root. |
| LENS-D2-T2 | [Build the workspace and verify its indexes](repository-handoffs.md#lens-d2-t2) | Planned. Record per-root build results, baseline pins and verified side-by-side semantic indexes. |
| LENS-D2-T3 | [Query every qualifying source and resolve citations](repository-handoffs.md#lens-d2-t3) | Planned. Exercise enrolled collections with fresh sessions, both inference stages and authorized citation resolution. |
| LENS-D2-T4 | [Roll back and replay the workspace baseline](repository-handoffs.md#lens-d2-t4) | Planned. Restore every prior approved index and policy and verify baseline queries and citations with Lens unavailable. |

## Phases 1–4 · Server runtime

| Phase | ID | Task and primary design | Status and next evidence |
|---|---|---|---|
| 1 | R4 | [Fence worker claims and publication](airk-analysis.md#r4) | Planned. Prepare the v1.4 job fixtures, then qualify reclaim and recovery of a previously authorized write. |
| 1 | R5 · C2 | [Classify build failures and preserve historical errors](airk-analysis.md#r5) | Planned. Resolve the [C2 wire contract](airk-analysis.md#c2) and verify exhausted-attempt recovery and historical serialization. |
| 2 | R1 | [Disclose search execution on request](airk-analysis.md#r1) | Planned. Record shared response semantics and qualify legacy and opted-in REST and gRPC. |
| 2 | R2 · C1 | [Report authorized multi-collection outcomes](airk-analysis.md#r2) | Planned. Resolve [C1 partial-read semantics](airk-analysis.md#c1) and verify required evidence gaps and withheld claims. |
| 2 | R11 | [Provide typed retry and recovery guidance](airk-analysis.md#r11) | Planned. Exercise uncertain outcomes and authorized status recovery. |
| 3 | R7 · B3 | [Reserve admission capacity for authenticated control](airk-analysis.md#r7) | Planned. Inventory routes and permits, then qualify the [B3 hard ceiling and reserve](airk-analysis.md#b3) under saturation. |
| 3 | R3 · B4 | [Contain PDF text extraction in a child process](airk-analysis.md#r3) | Planned. Inventory extractors, then measure and pin the [B4 resource limits](airk-analysis.md#b4). |
| 4 | R6 | [Cancel running builds cooperatively](airk-analysis.md#r6) | Planned. Depends on R4. Exercise cancellation and recovery after worker death. |
| 4 | R8 · B1 | [Claim build jobs fairly across tenants](airk-analysis.md#r8) | Planned. Depends on R4. Verify [B1 per-tenant turns](airk-analysis.md#b1) and preserve the v1.4 queue default. |
| 4 | R9 · B2 | [Record fenced progress and a transactional event ring](airk-analysis.md#r9) | Planned. Depends on R4. Measure and pin [B2 per-tenant event retention](airk-analysis.md#b2). |
| 4 | R12 | [Validate identifiers at new boundaries](airk-analysis.md#r12) | Planned. Verify that malformed IDs are rejected before work and historical IDs still resolve. |

## Phase 5 · Governed MCP endpoint

| ID | Task and primary design | Status and next evidence |
|---|---|---|
| Phase 5 | [Build the opt-in MCP route and tool catalog](mcp-analysis.md#92-sequencing) | Planned. Requires R7, R11 and the applicable shared contracts. Pin router and session response bounds. |
| A1 | [Enforce MCP entry and underlying tool authority](mcp-analysis.md#a1) | Planned. Record the shared scope and test discovery, listing and direct calls. |
| A2 | [Require generative opt-in and a budget owner](mcp-analysis.md#a2) | Planned. Record the shared authority rules and verify physical provider arrivals. |
| A3 | [Qualify both protocol revisions with real clients](mcp-analysis.md#a3) | Planned. Pin schemas and client builds, then attach the two client transcripts. |
| A4 | [Add both readers and the read-only evidence capability](mcp-analysis.md#a4) | Planned. Export the scope and issuance contract and qualify native and MCP access. |
| A5 | [Qualify all eleven tools and native rollback](mcp-analysis.md#a5) | Planned. Run the [complete catalog](mcp-analysis.md#51-query-tools) on the exact candidate, including result parity, default-off upgrade and mixed-replica rollback. |
| A5-T5 | [Recover interrupted calls with a caller-known key](mcp-analysis.md#53-recovering-an-interrupted-call) | Planned. Export and re-vendor the recovery contract. Verify bounded recovery across replicas with separate A6 admission and zero additional execution or spend. |
| A6 | [Commit durable audit admission and attribute outcomes](mcp-analysis.md#a6) | Planned. Export the shared contract, then exercise store failures, lost acknowledgments and recovery. |
| A7 | [Support deployment-operated credential renewal](mcp-analysis.md#a7) | Planned. Qualify expiry, scope preservation and reconnection through the broker. |
| A8 | [Enforce request and daily token allowances](mcp-analysis.md#a8) | Planned. Resolve the policy representation and precedence. Test reservations, retries and settlement. |
| A9 | [Account for shared generation](mcp-analysis.md#a9) | Planned. Resolve contributing scopes and the common payer, then qualify atomic reservations and a single charge. |

## Phase 6 · Inference integration

| ID | Task and primary design | Status and next evidence |
|---|---|---|
| IR-0 | [Prepare embeddings and reranking at the retrieval coordinator](airk-analysis.md#812-implementation-sequence) | Planned. Depends on R1. Retain v1.4 retrieval and disclose stage application. |
| IR-1 | [Add and probe the explicit inference provider](airk-analysis.md#812-implementation-sequence) | Planned. Depends on IR-0. Verify declared embedding and reranking models and their response contracts. |
| IR-2 | [Build and compare a side-by-side semantic index](airk-analysis.md#812-implementation-sequence) | Planned. Requires IR-1, authorized bindings and budgets. Large rebuilds use Phase 4. |
| IR-3 | [Rerank enrolled queries through the inference endpoint](airk-analysis.md#812-implementation-sequence) | Planned. Requires IR-1 and a passing rerank probe. Compare fused and reranked results in shadow. |
| IR-4 | [Enroll, verify and roll back per collection](airk-analysis.md#812-implementation-sequence) | Planned. Requires IR-2, IR-3 and R7. Verify applied stages, citations and prior-index restoration. |
| I1 | [Apply the enrolled inference outage policy](airk-analysis.md#i1) | Planned. Record the shared contract and test refusal and explicitly authorized fallback. |
| I2 | [Account for collection-owned inference](airk-analysis.md#i2) | Planned. Resolve the shadow allocation and reconcile caller attribution, serving and separately metered shadow attempts. |
| I3 | [Approve the retrieval profile for both corpus families](airk-analysis.md#i3) | Planned. Freeze separate documentation and code thresholds, then measure quality, latency, repeatability and resource bounds. |

## Phase SCALE · Server and Lens fleet operations

| ID | Task and primary design | Status and next evidence |
|---|---|---|
| SCALE-1 | [Align Helm replica counts and runtime admission](scale-out.md#scale-1-t1) | Planned. Qualify bare-chart and production installs, legacy upgrade values, surge, `platform-v1` constraints and rollback. |
| SCALE-2 | [Separate serving and builder groups](scale-out.md#scale-2-t1) | Planned. Depends on fenced claims. Add and drain a builder while serving foreground traffic. |
| SCALE-3 | [Expose replica, pool and queue diagnostics](scale-out.md#scale-3-t1) | Planned. Compare displayed counts and bottlenecks against fleet and database observations. |
| SC1 | [Qualify operator-controlled scale-up and drain](scale-out.md#sc1) | Planned. Exercise hash-only Server first, then the combined fleet with enrolled collections. |
| SC2 | [Qualify shared and dedicated Lens placement](repository-handoffs.md#sc2) | Planned. Record the shared placement contract and test approved sharing, incompatible policies and loss of dedicated capacity. |

## Phase 7 · Operator console

| ID | Task and primary design | Status and next evidence |
|---|---|---|
| U1 | [Group navigation by operator task](airk-analysis.md#95-recommendations) | Planned. Preserve bookmarks and existing destinations. |
| U2 | [Use a consistent page header](airk-analysis.md#95-recommendations) | Planned. Roll it out with the navigation work. |
| U3 | [Surface conditions that need attention](airk-analysis.md#95-recommendations) | Planned. Add counts backed by observed state. |
| U4 | [Review and confirm destructive actions](airk-analysis.md#95-recommendations) | Planned. Start with token revocation. |
| U5 | [Show failed reads explicitly](airk-analysis.md#95-recommendations) | Planned. Use Phase 1 console fixtures to distinguish a failed read from empty results. |
| U6 | [Display distinct operational facts separately](airk-analysis.md#95-recommendations) | Planned. Apply as affected pages change. |
| U7 | [Offer the complete result and equivalent command](airk-analysis.md#95-recommendations) | Planned. Qualify both the summarized and complete views. |
| U8 | [Give each task its own view and bound lists](airk-analysis.md#95-recommendations) | Planned. Start with token issuance and pagination. |
| U9 | [Render invisible and direction-changing characters visibly](airk-analysis.md#95-recommendations) | Planned. Exercise untrusted text using Phase 1 console fixtures. |
| U10 · LOGIN1 | [Ship and qualify revocable console sessions](airk-analysis.md#login1) | Planned. Define the shared store, expiry and revocation, then qualify mixed-replica cutover and rollback. |
| U11 | [Explain page use and link to documentation](airk-analysis.md#95-recommendations) | Planned. Apply as affected pages change. |
| U12 | [Show job progress from recorded events](airk-analysis.md#95-recommendations) | Planned. Depends on R9 progress and history. |
| U13 | [Use consistent visual treatment and status cues](airk-analysis.md#95-recommendations) | Planned. Apply as affected pages change. |
| U14 | [Test operator pages through their browser flows](airk-analysis.md#95-recommendations) | Planned. Exercise navigation, roles, keyboard access and confirmation. |
| Phase 7 | [Expose MCP activity and revocation in the console](mcp-analysis.md#93-admin-console) | Planned. Depends on Phase 5. Verify MCP and existing native reports. |

## Phase 8 · Durable artifact damage and repair

| ID | Task and primary design | Status and next evidence |
|---|---|---|
| R10 | [Record bounded, durable artifact damage](airk-analysis.md#r10) | Planned. Define verifier identities, storage pins and durable findings. |
| ART1 | [Isolate bad copies and confirm fleet blocks](airk-analysis.md#art1) | Planned. Require independent confirmations and exercise warm, restarted and in-flight readers at the release fence. |
| ART2 | [Clear blocks through a verified repair](airk-analysis.md#art2) | Planned. Require fresh independent matches on the correct pin and verify recorded clearance. |

## v1.5 release qualification

| ID | Task and primary design | Status and next evidence |
|---|---|---|
| COMPAT-T1 | [Replay REST, gRPC, native bridge and SSE compatibility](README.md#compat-t1) | Planned. Compare both binaries and exercise legacy and opted-in wire cases. |
| COMPAT-T2 | [Replay the CLI, four SDKs and configuration](README.md#compat-t2) | Planned. Exercise both client directions and chart upgrade and rollback, including Matrix. |
| COMPAT-T3 | [Preserve and restore stored state and artifacts](README.md#compat-t3) | Planned. Exercise mixed versions, binary rollback and the documented memory-store surface. |
| COMPAT-T4 | [Preserve default work and spending behavior](README.md#compat-t4) | Planned. Count outbound calls and compare existing indexes and ordering. |
| COMPAT-T5 | [Preserve supported operator workflows](README.md#compat-t5) | Planned. Verify existing URLs, authorization and operator actions during rollout. |
| REL1 | [Qualify console login and artifact protection on the same candidate](README.md#rel1) | Planned. Collect both LOGIN1 and ART1/ART2 receipts on the exact Server v1.5 candidate. |

## Deferred beyond v1.5

| ID | Task and primary design | Status |
|---|---|---|
| IR-5 | [Measure context packing after reranking](airk-analysis.md#812-implementation-sequence) | Deferred. Depends on IR-4. |
| IR-6 | [Evaluate in-process inference](airk-analysis.md#812-implementation-sequence) | Deferred. |
| MCP | [Streaming, ingestion and governed-memory tools](mcp-analysis.md#11-deferred) | Deferred. |
| SCALE | [Automatic scaling](scale-out.md#helm-installation-upgrade-and-rollback) | Deferred. SC1 qualifies operator-controlled resize. |
| Server packaging | [Bundled model weights or a generation runtime](README.md) | Deferred. |
