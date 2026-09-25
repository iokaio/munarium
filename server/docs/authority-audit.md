# Authority construction and rechecks

This is the P10/R19 source audit. It describes the shipped entry points and
their limits; it is not a claim that every operation continuously reauthorizes
throughout its lifetime. See [security posture](security-posture.md), the
[platform guide](guides/platform-features.md), and the
[implementation plan](lessons-from-vcp-impl.md#101-p10-audit-authority-construction-preserve-working-controls--r19).

## Constructors and trusted code boundary

[`AccessClaims` and `AccessCtx`](../src/munarium-access/src/lib.rs) have distinct
roles. Claims are deserializable JWT data. `AccessCtx` does not implement
`Deserialize`, but its fields, `From<AccessClaims>`, and `unrestricted` remain
public Rust APIs for trusted callers. Converting an arbitrary claims value does
not verify it. No HTTP body or protobuf field deserializes directly to a context.

| Constructor / caller | Establishes | Limits and rechecks |
|---|---|---|
| `munarium_access::verify` | HS256 signature and expiration with 30-second leeway | Does not bind an asserted request uid or consult the revocation store. |
| `AppState::authenticate_principal` in [state.rs](../src/munarium-server/src/state.rs) | Static-token configuration match, or verified JWT claims converted to `Principal::Access`; tenant comes from configuration or verified `ten` | The only production JWT-to-context conversion. A static match takes precedence. |
| `Principal::access_ctx` | JWT subject binding to the resolved uid; static `rw` maps to unrestricted, `ro` to query only; `mgmt` is refused on the data plane | Checks uid even when an in-process service caller omits transport middleware. No public constructor or claims representation was removed. |
| Disabled authentication | Static `rw` context for `tenant-default`, with `disabled_mode=true` | Explicit development behavior; passes both `require_rw` and `require_mgmt`. This mode performs no bearer verification. |
| `sessions_api::permitted_collections` | A local `AccessCtx` probe with the session's level and compartments and no scopes or token id | Filters collection eligibility only. It is not a request principal and is never submitted as one to authentication. |
| In-process tests | `unrestricted`, literal contexts, or signed fixture claims | Trusted fixtures are deliberate. Integration fixtures that call `op_*` directly do not establish transport authentication coverage. |

The audit found no production conversion of unverified claims. A new verified
claims wrapper would not close the observed service-call uid assumption, so P10
keeps the existing centralized verifier and adds the uid check to the shared
context conversion instead. Normal transport requests retain the middleware's
typed `uid-mismatch` response; a mismatched direct service call is forbidden too.

## Transport and operation inventory

| Entry point | Authentication and authorization |
|---|---|
| REST `/v1` and `/v1.2` | [Capture middleware](../src/munarium-server/src/middleware.rs) resolves uid from the header, JWT subject, or configured anonymous fallback, and rejects subject mismatch. Handlers authenticate independently. [The data-plane helper](../src/munarium-server/src/rest.rs) resolves the context, requires the operation's scope, and checks revocation. Control routes require a static principal and their rw/mgmt role gate. |
| Typed gRPC sessions/admin | [grpc_platform.rs](../src/munarium-server/src/grpc_platform.rs) resolves metadata, authenticates, checks query scope/revocation for data-plane calls, and calls the same session operations as REST. Admin token operations require mgmt. `GrpcCaptureLayer` supplies uid enforcement and audit attribution; the shared context conversion also binds uid without that layer. |
| Typed gRPC commands, retrieval, ingest, shapes | [grpc.rs](../src/munarium-server/src/grpc.rs) and [grpc_data.rs](../src/munarium-server/src/grpc_data.rs) authenticate static credentials through `AppState::authenticate`; capability JWTs are refused on this older control-plane surface. Writes require rw. Streaming source ingestion retains the authenticated tenant/role while consuming the stream. |
| Native `ServerApiService` | [grpc_api.rs](../src/munarium-server/src/grpc_api.rs) dispatches fixed named operations through the real REST router. Only authorization, uid, idempotency and permitted source metadata are forwarded. Caller-supplied tenant headers cannot set the tenant. The outer gRPC capture layer skips this service to avoid duplicate capture; REST capture still runs. |
| Findings | `rest::record_findings` handles static rw or the `findings` scope directly, with revocation checks. REST capture supplies uid binding. Findings remain warn/info; caller input cannot create a blocking governance finding. The native bridge inherits this route. |
| Dashboard | [dashboard/mod.rs](../src/munarium-server/src/dashboard/mod.rs) requires static mgmt or disabled mode using bearer/cookie authentication. Action forms additionally verify CSRF and, where required, a static rw credential for the same tenant. A view-only header only removes authority. |
| Token issuance | [tokens_api.rs](../src/munarium-server/src/tokens_api.rs) is mgmt-gated; issuance validates scopes, clamps TTL, and records claims metadata. It does not turn issued claims directly into a request context. |

JWT revocation is opt-in through `MUNARIUM_TOKEN_REVOCATION_CHECK`. In PostgreSQL
mode it checks `(tenant_id, jti)` for a non-null `revoked_at`. An unrecorded token
id is not a denial, and an empty token id does not trigger the helper's lookup.
Memory mode has no revocation store. Signature validity alone must not be described
as proof that revocation was checked.

Resource stores and SQL lookups use the authenticated tenant; a resource id never
selects a different tenant. Level dominance and every required compartment are
checked against current collection/artifact metadata on supported data-plane
operations. Static rw/ro contexts have `all_compartments=true`; JWT contexts never
do. The session-specific snapshot behavior below is a separate limitation.

## Sessions, retained work, and current limits

| Work | Retained authority / recheck boundary |
|---|---|
| Session creation and turns | [sessions_api.rs](../src/munarium-server/src/sessions_api.rs) stores uid, runbook ref, level, compartments and originating token id. Each transport request verifies the presented credential and scope/revocation. Turns verify tenant, session owner and open state, then filter current active collections using the stored clearance snapshot. They do not intersect that snapshot with a replacement token's level/compartments or recheck its runbook allowlist. The probe uses explicit compartments rather than `all_compartments`, including for sessions created by static principals. |
| Session transcript reads / close | JWT callers must have query scope, pass optional revocation and own the session. Static tenant principals can read transcripts; static ro cannot close. Transcript reads do not refilter stored text against current collection clearance/status. |
| Streaming session turn | The spawned turn retains a cloned `AccessCtx`; disconnecting the client does not cancel computation/persistence. It has the same snapshot behavior as unary turns and does not reverify token expiration/revocation before output/persistence. |
| Checked answers | [answers_api.rs](../src/munarium-server/src/answers_api.rs) verifies source pins/quotes and authorization before provider submission, then reauthenticates and checks current collection eligibility before releasing the result. A denial after the provider call cannot undo that call. |
| Governed collection query and original references | [query_api.rs](../src/munarium-server/src/query_api.rs) authorizes the logical scope and publication mapping and rechecks token, active status, clearance, governance/vocabulary revisions, and target metadata at its explicit recheck points. A historical pin does not remove those checks. These are discrete checks, not a transaction locking access metadata for the whole response lifetime. |
| Evidence seals and ingest batches | Scope, revocation, tenant and applicable class/collection checks run on entry and in shared operations. Contexts persist for the request/batch; there is no blanket reauthentication between every source upload, grant use, or item. Evidence grant uploads still require an authenticated authorized context. |
| Durable index jobs and reconciliation | [datastore_jobs.rs](../src/munarium-server/src/datastore_jobs.rs) admits jobs with static rw, stores a tenant-scoped job and attribution, and workers act on that job. [Build reconciliation](../src/munarium-server/src/datastore_builds.rs) is process-owned maintenance. Neither retains a capability JWT nor reauthorizes the original caller before each resumed stage. |
| Vocabulary generation | [vocabulary_api.rs](../src/munarium-server/src/vocabulary_api.rs) authorizes manual requests against collection clearance and vocabulary scope. The automatic worker enumerates active tenant collections under server configuration and leases; it is not a continuation of a user's token. |
| Search and shadow tasks | [shadow_plane.rs](../src/munarium-server/src/shadow_plane.rs) retains tenant, prepared query and selected index/scope for detached diagnostic execution. Candidate/store handles are tenant scoped. It does not retain or reverify a bearer. Blocking search/build tasks likewise use already selected handles and scope. |
| Maintenance and audit tasks | `AppState::new`, [interactions.rs](../src/munarium-server/src/interactions.rs), and startup workers run idempotency/session expiry, evidence retention, partitions, artifact readiness/cache maintenance, and audit writes with process-owned stores. Captured uid/jti are attribution, not delegated credentials. Provider health probes use server configuration, not a request `AccessCtx`. |

These lifetime and transcript limits remain explicit follow-up boundaries; P10
does not silently redefine existing session snapshots as continuously attenuated
authority. A stronger contract needs session/transcript semantics and tests for
credential replacement, concurrent changes, streaming cancellation, and resumed
work. Public Rust APIs remain source compatible, and this uid binding change
requires no migration or persisted-format change. Rolling back the code restores
the former direct-service uid assumption without changing stored data.

## Regression evidence

[authority_tests.rs](../src/munarium-server/src/authority_tests.rs), included by the
existing evidence-route fixture, runs offline against memory stores. REST and
native bridge seals reject tampered/expired claims, wrong uid, missing scope,
cross-tenant manifests, insufficient level/compartments, static ro and mgmt.
Every rejection checks that no pending artifact/domain record was registered;
registration and grant creation are atomic in this flow. Valid JWT, static rw and
disabled-mode controls establish that the operation can succeed. Direct typed
`SessionService` tests reject bad credentials/scope/uid before reaching session
storage, and check the accepted context and static-role mappings separately.

[v12_tests.rs](../src/munarium-server/src/v12_tests.rs) uses an isolated PostgreSQL
tenant and a loopback scripted provider through REST and native gRPC. Query and
original-reference cases cover forged/expired claims, uid/scope, tenant,
level/compartment, revocation and retired collections after a successful warm
query. Rejections assert that the provider observer saw no additional request.
Existing checks also cover source provenance, invalid citations and authorization
changes during provider execution. These tests require
`MUNARIUM_TEST_DATABASE_URL`; a skipped scenario is not PostgreSQL evidence.

Focused commands from `server/`:

```powershell
cargo test -p munarium-server authority_tests
cargo test -p munarium-server v12_tests
cargo test -p munarium-server docs_coverage
```

The transport regression scope does not establish continuous authorization or
general prompt-injection immunity. The implementation plan retains those
distinctions when interpreting results.
