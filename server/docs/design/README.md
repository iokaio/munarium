# Design Plan for v1.5

**Status:** proposed plan, 19 November 2026. Nothing here is accepted, scheduled or implemented. It organizes the two design documents in this directory into phases for Munarium Server 1.5, the release after 1.4.0. The source documents remain the authority for detail and rationale; where this plan and a source document disagree, the source document is right and this plan needs correcting.

**Baseline:** the Server workspace at `bb18085` (1.4.0).

## 1. The two design documents

| Document | What it is | Identifiers it defines |
|---|---|---|
| [airk-analysis.md](airk-analysis.md) | A comparison of AIRK, a single-user local retrieval service, with Server, and the patterns worth borrowing | R1 to R12 (Server runtime), IR-0 to IR-6 (inference endpoint), LENS-1 to LENS-3 (separate project), U1 to U14 (admin console) |
| [mcp-analysis.md](mcp-analysis.md) | A proposal to host a Model Context Protocol endpoint in the `munarium-server` binary | Design rules D1 to D7, phase 1 and phase 2 tools, ten open decisions |

### 1.1 airk-analysis.md

AIRK and Server share a temperament (immutable derived data, explicit refusal, typed errors), so most AIRK foundations already exist in Server. The document finds AIRK ahead in three places and derives twelve recommendations from them:

- **What a read says about itself.** Server returns hits and provenance, and does not say which engine and legs ran (R1) or what happened to each collection in a multi-collection read (R2). Problem responses carry no machine-readable retry guidance (R11).
- **Background work as an operator-visible object.** Build jobs have leases and attempt ceilings only. Proposed: a fencing generation (R4), content-free failure codes (R5), cooperative cancellation (R6), fair claiming across tenants (R8), and progress with a transactional event ring (R9).
- **Containing hostile or expensive input.** PDFs are parsed in-process with no time or memory bound (R3), every request shares one permit pool per plane (R7), and artifact damage is remembered only in one replica's memory (R10).

R12 (validating identifier newtypes) is applied only to types the other items touch.

Two further studies sit in the same document:

- **Semantic embedding and reranking (section 8).** Server's only embedder is `local-hash@1`, a 256-dimension feature hash, so hybrid search today fuses lexical search with a second form of lexical overlap. The proposal is one seam and no hosted model: an asynchronous, fallible embedder and a reranker behind the provider gateway, calling an operator's own inference endpoint over the OpenAI-style embeddings route and a rerank route. A reference GPU image, Munarium Lens, is a separate project in its own repository.
- **Admin console (section 9).** Server's `/admin` console is sound in its foundations (server-rendered, no JavaScript, real URLs) and thin in organization. Fourteen changes (U1 to U14) cover grouped navigation, a status-first overview, review and typed confirmation before destructive actions, and never showing a failed read as zero.

The document also records what Server already has and what should not be copied; neither list creates work.

### 1.2 mcp-analysis.md

Server has deliberately had no MCP surface. The proposal adds one for the query plane only:

- `POST /mcp` on the REST listener, off unless `MUNARIUM_MCP=enabled`, speaking stateless Streamable HTTP (specification revision `2026-07-28`) with a stateless answer to the legacy `initialize`.
- A transport and not an authority: each tool call is dispatched in-process through the existing router with the caller's own token, as gRPC is today.
- Capability JWTs carrying a new `mcp` scope only. Static `rw`, `ro` and `mgmt` tokens are refused.
- A closed, compiled table of eleven tools: discover, search, ask, converse and resolve citations. No administrative operation is reachable, enforced by the closed table, by the credential kind, and by tests.
- A second phase, governed memory tools, needs a contract decision and is not part of phase 1.

The MCP design depends on the first document in three places: admission (R7), the cost of search once an embedder and reranker exist (IR-2, IR-3), and retry guidance for agents (R11).

## 2. Goals for 1.5

1. **A reply states how it was produced.** Engine, legs, applied stages, per-collection outcomes and retry guidance are on the wire, additively.
2. **Builds are operable.** A stale worker cannot complete over a newer one, a cancel stops work, one tenant cannot starve another, and an operator can see progress.
3. **A tenant-supplied document or a traffic spike cannot take a replica's control surface away.** PDF extraction is bounded, and control requests have reserved capacity.
4. **Agents can reach the query plane under the same governance as applications**, with no path to administration.
5. **The seam for semantic retrieval exists and has been measured**, with nothing enrolled by default.
6. **The console tells an operator what needs attention and guards its destructive actions.**

### Constraints on every phase

- The native REST and gRPC contracts change only additively, through the documented contract process, with REST and gRPC parity and SDK conformance updated together. Existing contract versions and golden vectors are preserved.
- A deployment that enables no new feature behaves as 1.4.0 does. New surfaces and stages are off by default.
- Migrations are additive.
- The image stays a static musl binary with no model runtime and no new native dependency.
- `munarium-core` stays independent of HTTP, database and provider layers.
- Tenant isolation, access levels, compartments and per-user audit hold on every new path.
- Shared semantic choices are recorded in a hub decision record before consumer implementation.
- Each phase is delivered as several reviewable changes; the phase is a grouping, not a pull request.

### Not in 1.5

| Item | Reason |
|---|---|
| MCP phase 2 (governed memory tools) | Needs a hub decision record on ledger authentication first |
| MCP streaming, tasks, resources, prompts, OAuth authorization server | Deferred by the MCP document, section 11 |
| Ingestion over MCP | Excluded; must not precede R3 |
| IR-5 (shorter context packing) | Depends on enrollment results from IR-4 |
| IR-6 (in-process pure-Rust inference spike) | Optional spike, no dependency on the release |
| LENS-1 to LENS-3 | Separate repository and schedule; depends on Server only for the contract |
| Option scoring, local generation, hosting model weights | Ruled out of scope for Server |

Section 5 asks whether this cut is right.

## 3. Phases

Phases are ordered by dependency. Phases 1 to 3 are independent of one another and can run in parallel; later phases state what they wait for.

| Phase | Theme | Items | Waits for |
|---|---|---|---|
| 0 | Decisions and design notes | Section 5 of this plan | Nothing |
| 1 | Build-job correctness and console corrections | R4, R5, U5, U9 | Nothing |
| 2 | Reply disclosure contracts | R1, R11, R2 | Phase 0 for R2 |
| 3 | Admission and input containment | R7, R3 | Phase 0 |
| 4 | Builder operability | R6, R8, R9, R12 where touched | Phase 1 |
| 5 | MCP phase 1 | `/mcp`, `mcp` scope, eleven tools | R7, R11; R1 preferred |
| 6 | Inference endpoint seam | IR-0, IR-1, IR-2 and IR-3 in shadow, IR-4 | R1, R7; phase 4 for large rebuilds |
| 7 | Admin console | U1 to U4, U7, U8, U10 to U14, MCP console requirements | R9 for U12; phase 5 for MCP views |
| 8 | Durable artifact damage record | R10 | Its own design note |

### Phase 0. Decisions and design notes

**Goal.** Settle the questions in section 5 that block a later phase, and record each shared semantic choice where the process requires it.

**Acceptance criteria**

- Every question in section 5 marked as blocking has a recorded answer, or its dependent item is moved out of 1.5.
- Hub decision records exist for the `mcp` scope, the search execution record, the per-member outcome and availability policy, the retry members, and the collection binding for embedder and reranker.
- Design notes exist for R3 (limits, statuses, extractor version), R7 (route classification) and R10 (when damage is recorded).
- The MCP specification pages listed as unread in the MCP document, section 13, have been read and the design corrected where needed.

### Phase 1. Build-job correctness and console corrections

**Goal.** Close two small defects in the build-job path and two in the console. All four are corrections with no design dependency.

**Work.** R4 fencing on `heartbeat` and `complete` using the existing `attempts` counter. R5 stable failure codes, with raw causes in the server log only. U5 failed reads shown as unavailable. U9 invisible and direction-changing characters shown as visible escapes.

**Acceptance criteria**

- A test in which the same instance name claims a job twice shows the first attempt's `heartbeat` and `complete` are rejected.
- The artifact attempts table has been reviewed for the same weakness and the result recorded.
- No job DTO returns a database driver message. `error_code` is populated from the `KernelError` variant, and `error` passes through `client_facing_error`.
- Job failure codes are listed in the error registry, and the registry check in `docs_coverage` fails when one is missing.
- No migration is required for R4.
- An overview tile whose source read fails renders "unavailable" with a reason, never 0.
- Untrusted text containing a right-to-left override renders as a visible escape on every console page that shows it.

### Phase 2. Reply disclosure contracts

**Goal.** Put on the wire what a caller needs to interpret a reply, before any feature that makes replies vary more.

**Work.** R1 an optional, versioned `execution` object beside the provenance envelope. R11 optional `retryable` and `retry_action` problem members. R2 a per-member outcome record for multi-collection reads and a request-level availability policy defaulting to `require`.

**Acceptance criteria**

- `execution` names the serving engine, the legs that ran, and per leg the candidate limit, the number fetched and a three-state `exhausted`. The PostgreSQL path reports unknown where it cannot know.
- All three additions are absent-when-unset, so existing clients and golden vectors pass unchanged.
- REST and gRPC carry the same members, and the four SDKs and their conformance suites cover them.
- A table-driven test maps every registered problem slug to a retry action, and fails for an unmapped slug.
- With the default policy, multi-collection behaviour is unchanged. With `report`, a failed member no longer discards the others, and every downstream completeness claim is marked unsupported.
- `skipped` stays populated for this release.

### Phase 3. Admission and input containment

**Goal.** Reserve capacity for control, separate heavy work, and bound PDF extraction.

**Work.** R7 permits held back for management-role routes, a smaller pool for heavy routes, and the drain-never-revoke rule where capacity is runtime-configurable. R3 PDF extraction in a re-executed, resource-limited child of the same binary, with a parent-enforced deadline, bounded output and a small concurrency cap.

**Acceptance criteria**

- Every route names its work class, and a test fails for a route with none.
- With the general pool saturated, a token revocation and a rollout change still succeed.
- Inline builds at the heavy ceiling do not exhaust the general pool.
- A PDF that exceeds its memory, CPU or wall-clock limit ends with a distinct budget-exceeded status, the replica stays up, and the job lease is released on time.
- Budget-exceeded documents are eligible for document-intelligence escalation as empty ones are.
- Limits are configuration with documented conservative defaults.
- The effect on `EXTRACTOR_SET_VERSION` is decided and documented.
- The image still passes static verification and gains no file.

### Phase 4. Builder operability

**Goal.** Make a long build something an operator can see, stop and share fairly. Large embedding rebuilds in phase 6 depend on this.

**Work.** R6 a cancellation token per claimed job, checked between sources, between embedding batches, before seal and before upload. R8 claim order by each tenant's oldest pending job with an aging guard. R9 a fenced progress column and a trigger-populated `index_build_job_events` table. R12 an enum for job state and validating newtypes on the types these items touch.

**Acceptance criteria**

- Cancelling a running build frees its builder at the next boundary, and each boundary has a test for what is left behind. A cancel after publication leaves the publication committed.
- A tenant enqueueing several hundred jobs does not delay another tenant's later job behind all of them, and no job waits past the aging threshold.
- The claim query has been reviewed against an index at realistic queue depth.
- Progress reports phase, units done and last advance time. An unknown total is null, never zero, and counters do not decrease.
- An event commits or rolls back with the state change that caused it. A read reports when older events have been pruned.
- The in-memory store and the PostgreSQL store produce the same events under a shared conformance test.
- The event table is listed in the retention inventory with a decided bound.
- Each new newtype carries an explicit schema annotation, and the generated OpenAPI document keeps the constraint.

### Phase 5. MCP phase 1

**Goal.** An agent can discover, search, ask, converse and resolve citations through `/mcp`, governed exactly as a REST caller with the same token.

**Work.** The `mcp` scope in `munarium-access`. An `mcp.rs` module holding the envelope, version and header checks, the tool table and the dispatcher. Nine routed tools and two native listing tools. Audit and metrics labelled with the plane and tool. Agent traffic as an R7 work class. Configuration, Helm value, reference page and security-posture section.

**Acceptance criteria**

- With `MUNARIUM_MCP` unset, `/mcp` answers 404 and the deployment behaves as 1.4.0. `/mcp` is not served under disabled authentication.
- Static tokens and JWTs without `mcp` are refused. An expired token produces a 401 whose JSON-RPC message names the slug, with no `resource_metadata` challenge.
- No native route, DTO, proto message or SDK method changes.
- The same token returns the same body over REST and over MCP for the same request.
- A maximally scoped MCP token cannot build or activate an index, apply a runbook, mint a token or read a report, by any tool name or argument.
- Every table entry is in a checked-in route allowlist and names its work class.
- The interaction row for a tool call carries `plane = "mcp"`, the tool name, uid and `jti`. The native listing tools write their own row and hide collections above the token's clearance.
- At the agent class ceiling, `/mcp` sheds while application requests and a management revocation still succeed.
- A result above the size limit is refused with the argument that reduces it, and is never truncated.
- Consecutive calls succeed across replicas with no session affinity.
- Removing the static-token refusal, the allowlist test or the scope check each fails a named test.
- Each client in the MCP document's section 10.5 has been connected to a real Server at a recorded version and taken through connect, list, call and token expiry, with results published in the reference page.
- Release notes and the security posture call out the new network surface, the scope, the OAuth position and the exposure required by hosted model APIs.

### Phase 6. Inference endpoint seam

**Goal.** Any operator with a conforming endpoint can build a semantic vector leg and a reranker and measure both, without changing what any caller is served by default.

**Work.** IR-0 asynchronous fallible embedder and reranker traits in `munarium-core`, `rerank` on the provider seam, identities and applied flags in the execution record. IR-1 an `openai-compatible` provider kind and a conformance probe. IR-2 an additive migration for a second vector dimension and an endpoint-backed embedder built side by side. IR-3 a reranker after fusion, in shadow. IR-4 the enrollment API, the per-collection `prefer` or `require` policy, capability reporting and admission class.

**Acceptance criteria**

- A deployment with no `embed` or `rerank` provider configuration builds and serves byte-identical results to 1.4.0.
- The embedder identity in `index_version` covers model, dimension, query and passage instructions, normalization and truncation. A change to any of them is a new index version.
- A rerank reply is used for order only. A missing, repeated or out-of-range index, a non-finite score, a timeout or a transport error returns fused order unchanged with a typed notice; no partial rerank is ever exposed.
- Input over the model's token limit is refused, not truncated.
- A failed probe leaves the collection as it was. The probe result is recorded with the endpoint fingerprint.
- The reply states, for every state other than ready, that the stage was not applied and why. Under `require` the request fails instead.
- A query never substitutes the caller's provider configuration for the collection's.
- One deadline covers every step including retries, and work finishing after it is discarded.
- Embedding and rerank calls are admitted through the heavy class and recorded in tokens and calls with no price.
- Shadow comparison and frozen evaluation results exist for at least two corpus families before IR-4 is released, and enrollment requires a recorded result for that corpus and model identity.
- Nothing is enrolled by default.

### Phase 7. Admin console

**Goal.** The console answers "does anything need me?", guards its actions, and shows agent activity.

**Work.** U1 grouped navigation. U2 a common page header. U3 an attention strip. U4 review and typed confirmation, starting with token revocation. U7 a complete-result disclosure and the equivalent command. U8 one task per view and pagination on every list. U10 session improvements. U11 page purpose and documentation links. U12 a build-job page. U13 visual vocabulary. U14 structural page tests. From the MCP document, section 9.3: the `mcp` scope marked and filterable on the tokens page, `mcp` as a plane on the traffic pages, and two agent conditions on the attention strip.

**Acceptance criteria**

- Six navigation groups, sentence case, the current page marked with `aria-current="page"`.
- Every page shows its purpose, tenant, rendering instance, store mode and observed-at time.
- The overview lists only abnormal conditions, each linked, and says so in one line when there are none.
- No destructive action completes in one submission. The review page states exactly what will change, and an empty plan offers no confirmation.
- Tokens, sessions and findings paginate as the audit list does.
- The session cookie no longer holds the management token, carries `Secure` over TLS, expires when idle, and can be cleared by signing out. The change has had its own security review and the posture document is updated.
- A test fails when a console documentation link does not resolve.
- The build-job page shows unknown totals as unknown and refreshes only while a job runs.
- Every page passes a structural test (one `h1`, header fields, marked navigation item), and redesigned pages pass the viewport checklist in the AIRK document's U14.
- The console still ships no JavaScript and no external assets, apart from any script explicitly approved in section 5.

### Phase 8. Durable artifact damage record

**Goal.** Corruption found by one replica gates serving on all of them, without a local disk fault condemning a good artifact.

**Work.** R10 a bounded catalog record of damage, gating serving where the `Verified` check already runs.

**Acceptance criteria**

- The rule that separates object-store damage from a local fault is written down and tested in both directions.
- A recorded artifact stops being served by every replica, and a restart does not forget it.
- The record is bounded and refuses a new entry when full instead of evicting.
- No report presents "no damage recorded" as "verified intact".

This phase is the most likely to move to a later release; see section 5.

## 4. Release acceptance

1.5 is ready for release qualification when:

- every phase kept in scope meets its criteria, and every phase moved out is recorded with its reason;
- the Server gates, the client conformance suites and the documentation checks pass on the release head;
- an upgrade from 1.4.0 with no new configuration shows no change in served results, contracts or defaults;
- a `guides/server-1.5.md` release guide records changes, migrations, upgrade and rollback steps, and qualification limits, and the new pages are linked from the [documentation index](../README.md);
- contract candidates have gone through the documented export and re-vendoring process. This plan does not record human contract acceptance.

## 5. Open questions

These need answers before implementation continues. "Blocks" names the first phase that cannot start without one.

### Scope and ownership

1. **Is the scope cut right?** The MCP document assumes "most" of the AIRK recommendations will be built without saying which. This plan puts all of R1 to R12, IR-0 to IR-4, MCP phase 1 and most of U1 to U14 in 1.5. Is that one release, or should phases 6, 7 or 8 move to 1.6? *Blocks: phase 0.*
2. **Who owns each phase**, and which phases may run in parallel given reviewer capacity?
3. **Is the Munarium Lens name and repository approved**, and who owns it? Server needs only that the contract from IR-0 and IR-1 is stable enough for it to target.
4. **Release vehicle for MCP.** Is an additive, off-by-default network surface and token scope acceptable in a minor release, as the MCP document proposes?

### Contracts and compatibility

5. **R2 and completeness claims.** How does a partial fan-out under `report` interact with the evidence hierarchy? Without a rule, R2 makes answers less safe. *Blocks: R2 in phase 2.*
6. **R5 and the documented `error` field.** Is adding `error_code` and routing `error` through `client_facing_error` an acceptable change to a documented field? *Blocks: phase 1.*
7. **The rerank contract.** The route paths and fields were read from vLLM documentation on 19 November 2026 and must be confirmed against the targeted version. Which version is targeted? *Blocks: IR-0.*
8. **The second vector dimension.** Which dimension or dimensions does the additive migration admit, given the 2,000-dimension limit on the pgvector HNSW index? *Blocks: IR-2.*
9. **`interactions.plane`.** Does the column or any report view constrain its values? If so, MCP needs an additive migration. *Blocks: phase 5.*

### Design decisions still to be made

10. **R3 limits and statuses.** What are the default memory, CPU, wall-clock and output limits, and does a budget-exceeded outcome change `EXTRACTOR_SET_VERSION`? *Blocks: R3.*
11. **R7 route classification.** Which routes are control, heavy and general, and who reviews the list? *Blocks: phase 3 and phase 5.*
12. **MCP relative to R7.** Does MCP wait for work-class admission, or ship on an interim pool that is later removed? This plan assumes it waits.
13. **R8 policy.** Is fairness by tenant the intended policy, and what is the aging threshold?
14. **R9 mechanics.** Are database triggers acceptable as a new technique in this schema, how does the in-memory store produce the same events, and is the event table bounded by row count or by age per tenant? *Blocks: R9.*
15. **R10 recording rule.** A second failed fetch, agreement between two replicas, or something else? *Blocks: phase 8.*
16. **R4 and the artifact attempts table.** Does it have the same lease weakness?
17. **Collection binding.** In which scope is a collection's embedder and reranker configuration resolved, and whose budget pays for the calls? *Blocks: IR-2.*
18. **`prefer` or `require` by default** for an enrolled collection, given that `prefer` for an embedder means a lexical-only answer when the endpoint is down?
19. **Bit-stable rerank results.** Is batching at the endpoint an acceptable source of last-bit variation for governed query policies, or must those policies use `require` with a dedicated endpoint?

### MCP decisions carried from the MCP document, section 12

20. **Is `mcp` a separate required scope**, or is any `query` token accepted on `/mcp`? *Blocks: phase 5.*
21. **A read-only evidence scope.** Do `get_evidence` and `read_evidence_rows` wait for a scope that cannot seal?
22. **Agent spend and load ceiling.** Is the provider-configuration daily cap enough, or does MCP need a per-token or per-uid budget before paid tools default to enabled? The same question applies to retrieval calls once phase 6 lands.
23. **Legacy protocol window.** How long is the pre-2026 `initialize` answered?
24. **A reference token broker.** Does Server ship one, for example as an `mmctl` subcommand, or leave it to each deployment?
25. **Default lifetime for agent tokens**, given that most surveyed clients cannot renew.
26. **Phase 2 memory tools and ingestion over MCP.** Both are outside 1.5 here. Is a hub decision record for phase 2 wanted during 1.5 so that it can follow promptly?

### Admin console

27. **Script in the console.** Two places would benefit from a few lines of JavaScript (a copy button in U7, and live refresh in U12, which has a no-script alternative). Does the no-JavaScript rule stay absolute?
28. **U10 session changes.** These alter the security posture. Who reviews them, and is the `mmctl` launch flow in or out of 1.5?
29. **The navigation grouping in U1** names shapes and runs pages. Do those pages exist, or are they new work?

### Evidence still missing

30. **MCP specification pages not yet read**: the discovery result schema, the base error-code table, and the progress, cancellation, subscription and task pages of revision `2026-07-28`. *Blocks: phase 5.*
31. **Client behaviour not yet tested.** Every client configuration in the MCP document is from vendor documentation and has not been run.
32. **Which corpus families** are used to measure the embedder and reranker, and what result is enough to release IR-4?
33. **The cost of building the REST router a second time** for in-process MCP dispatch has not been measured.
