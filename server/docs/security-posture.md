# Security posture: why the API-management layer is the security boundary

**Status:** normative for the platform surface (multi-user, capability tokens, compartmentalized
retrieval). Read this before wiring munarium-server into any environment that
serves more than one human.

## 1. The trust model in one page

```
  users ──► enterprise API-management layer ──► munarium-server ──► PostgreSQL
            (authn: SSO/OIDC/MFA, TLS, WAF,      (governance:      (cell)
             rate limits, credential lifecycle)   tenancy, levels,
                                                  audit, lifecycle)
```

munarium-server is **deliberately not an identity provider**. It never sees a
user credential, holds no user directory, and performs no login. The
enterprise API-management layer in front (APIM, Apigee, Kong, custom BFF —
"the manager") is the security boundary: it authenticates humans, terminates
TLS, rate-limits, and asserts *who is calling* downstream.

What munarium-server does own is **governance of what an already-authenticated
caller may touch**:

- tenant scoping (structural — every table keyed by tenant, handles pre-scoped),
- capability attenuation — short-lived, least-privilege JWTs it mints *for*
  the manager (§3),
- access-level + compartment filtering of retrieval collections at query time,
- an immutable, uid-attributed interaction audit (every request/response),
- a never-delete index lifecycle (soft removal only; physical deletion is a
  documented manual DBA operation — `docs/ops/index-deletion-runbook.md`).

## 2. Division of responsibilities

| Concern | API manager | munarium-server |
|---|---|---|
| User authentication (SSO/OIDC/MFA, sessions with the human) | ✅ owns | ❌ never |
| TLS termination, WAF, DDoS, rate limiting | ✅ owns | ❌ (network policy assumed) |
| Credential lifecycle (rotation, lockout, revocation of *user* creds) | ✅ owns | ❌ |
| Asserting the end-user id (`X-Munarium-Uid`) | ✅ owns | verifies consistency only |
| Tenant scoping | ❌ | ✅ structural |
| Access levels / compartments over collections | policy decided upstream | ✅ enforced at query time |
| Per-uid audit of every interaction | optional (gateway logs) | ✅ always (interactions table) |
| Token issuance audit + optional deny-list | ❌ | ✅ (access_tokens) |
| Index/collection deletion | ❌ no API exists | ❌ no API deletes a collection or its active index data — DBA runbook only (`retireOld` reclaims inactive versions' chunks) |

## 3. The uid contract

Every `/v1` or `/v1.2` (REST) and `mmp.v1` (gRPC) call must carry the end-user id the
manager authenticated:

- REST: `X-Munarium-Uid: <uid>` header
- gRPC: `munarium-uid` metadata

Missing uid → `400 uid-required`, with one substitution: when the bearer is a
capability JWT, its `sub` supplies the uid (the header there can only agree or
be rejected, so its absence is unambiguous). The uid is recorded on every
interaction row and every tracing span. When the bearer is a capability JWT
and the header is present, the server cross-checks `uid == token.sub` →
`403 uid-mismatch` on disagreement, so a stolen token cannot be replayed as a
different user without detection.

`MUNARIUM_REQUIRE_UID=false` relaxes the contract (uid defaults to `anonymous`)
— a migration escape hatch for single-user dev rigs, never production.

## 4. Capability tokens (what they are, and what they are NOT)

`POST /v1/access-tokens` (callable **only** with a static `mgmt`-role token)
mints an HS256 JWT with claims:

```json
{ "sub": "<uid>", "ten": "<tenant>", "lvl": 2, "cmp": ["eng"],
  "scopes": ["query","ingest"], "rb": ["field-support"],
  "jti": "tok-…", "iat": …, "exp": … }
```

- `lvl` — hierarchical access level; a collection at level L requires `lvl >= L`.
- `cmp` — need-to-know compartment tags; a collection's tags must be a subset
  of the token's. (Bell-LaPadula "simple security" with categories — one
  comparison, no policy language.)
- `scopes` — `query` (sessions, search, checked answers and publication
  authorization), `ingest` (file upload), `vocabulary` (read/edit/refresh a
  collection vocabulary, subject to collection clearance),
  `findings` (file warn/info findings on a lineage via
  `POST /v1/versions/{id}/findings`), and/or `evidence` (seal and resolve artifacts on the structured-evidence plane). The last two
  are the scopes Munarium Matrix's service token holds; each grants nothing
  else, and an `ro` token never has either. A static `rw` token carries all
  five. A `vocabulary` token grants no query, ingest, provider configuration or
  token-minting privilege; a `query` token can read the vocabulary revision for
  cache invalidation but cannot read or edit the terms.

  **`evidence` does not widen what a token may read.** The scope says "this
  principal participates in the evidence plane"; the artifact's authorization
  class says "and only within this clearance". Domination is still checked per
  artifact, in both directions: an under-cleared `evidence` token is refused
  on a read, *and* refused when it tries to SEAL above its own clearance —
  otherwise a low-clearance service could mint high-clearance evidence that
  every later reader would trust.

  Two additive scopes (ClearView). **`mcp`** is a transport-entry
  scope for the opt-in MCP endpoint: it admits authenticated discovery and a
  scope-filtered tool listing, and nothing else. Each MCP tool also needs its
  ordinary scope, and `mcp` grants nothing on any native route, so adding it
  never widens a token and a deployment that never enables MCP never honours
  it. **`evidence_read`** is the read-only evidence capability: it opens the
  manifest and row reads (`GET /v1/evidence/{id}`, `/rows`) natively and the
  two MCP evidence readers, with the same per-artifact domination, retention
  and access audit as `evidence`, and it can never seal, upload or commit.
  Neither scope is carried by a static token. See [MCP endpoint
  authority](#mcp-endpoint-clearview).
- `rb` — optional runbook-name allowlist.
- `exp` — short TTL (default `MUNARIUM_TOKEN_TTL_SECS=3600`, hard cap 24 h).

Server 1.2.1 publication snapshots are authored with a static `rw` credential,
including reads of the governance API. A scoped query names parent collections;
Server selects their governing publications and checks clearance and snapshot
revisions again after completion. A publisher's explicit binding delegates access
to internal source indexes through that parent, without granting direct access to
the internal collections. The internal source cannot require a higher access level
than the parent. Original-file authorization returns an identity, not bytes or a
download grant; the application also enforces its own file permissions. See
[collection governance](guides/collection-vocabularies.md#collection-queries-and-publication-governance-121).

**The intended flow:** the manager authenticates a human, decides their
level/compartments from its own directory/groups, exchanges its long-lived
`mgmt` credential for this short-lived token, and forwards the token (plus
the uid header) on the user's data-plane calls. The token *attenuates* trust
that already exists; it does not establish it.

### Sealed evidence is regulated data

An evidence artifact is the exact typed result an answer was computed from, and
it is treated as data of the same class as the documents it sits beside — not
as metadata:

- **Authorization comes from the row, never from the path.** Artifact bytes
  live in the object store under a reserved `evidence/<id>` keyspace, which the
  document ingress refuses (`refuse_reserved_document_path`), so a document can
  never collide with an artifact. But no backend infers authorization from that
  path: the class lives in `evidence_artifacts` and is checked there.
- **Turn-time citation resolution uses the SESSION's clearance**, never the
  sealing service's. A citation is readable exactly when the reader dominates
  the artifact's class.
- **A refusal describes nothing.** `evidence-forbidden` names neither the
  artifact, its class, nor its source — "this exists and is above you" is
  itself a disclosure. `evidence-grant-invalid` likewise covers unknown,
  expired, spent and mis-bound grants under one slug, so a caller cannot probe
  for valid grant ids.
- **The resolution audit records that a read happened, never what was read.**
  `evidence_access` holds uid, kind, outcome and time. An audit table holding
  the regulated data it audits would be a second copy of the problem. Denials
  are audited alongside successes — a denial is the more interesting row.
- **Reading the audit is mgmt-only.** A service that can seal evidence has no
  business enumerating who read it.
- **Retention deletes bytes; the metadata row survives.** An expired artifact
  keeps its row with `purged_at`, so a citation resolves `evidence-expired` — a
  statement about a retention policy — instead of `not-found`, which would read
  as though the citation had been fabricated. Purge and legal hold are
  **mgmt-only**: a service that can seal evidence must not be able to destroy
  it, nor to lift a hold somebody placed on it. A **legal hold blocks deletion
  and never reading**; an instruction to preserve evidence that also hid it
  would be a strange instruction. The janitor is **off by default**
  (`MUNARIUM_EVIDENCE_PURGE_INTERVAL_SECS=0`) — deleting regulated data on a
  schedule nobody chose is worse than not deleting it — and it deletes bytes
  before marking the row, so an interrupted sweep is retried rather than
  leaving an artifact that reports itself purged while its bytes remain.

**What this deliberately is not:**

- No login, no user store, no password/OTP handling.
- No JWKS, no OIDC federation, no token introspection endpoint. Verification
  is a local HS256 check against one server-held secret
  (`MUNARIUM_TOKEN_SECRET` / `MUNARIUM_TOKEN_SECRET_FILE`, ≥ 32 bytes, injected
  from your secret store).
- No per-object ACLs, roles, or policy engine. Levels + compartments, nothing
  else.
- Revocation is *optional* (`MUNARIUM_TOKEN_REVOCATION_CHECK=true` adds one
  indexed deny-list lookup per verify; default off keeps verification pure
  CPU). The primary bound on a stolen token is its `exp`.

**Why not duplicate real auth here?** Two systems asserting identity means
two sources of truth, two credential stores to defend, and drift between
them. The manager already has the directory, the MFA, the session semantics.
munarium-server adding a second, weaker copy would widen the attack surface
while making the audit trail ambiguous about who decided what. The server
therefore does the one thing only it can do — enforce data compartments and
record the evidence — and refuses to pretend it knows who a human is.

### MCP endpoint (ClearView)

`POST /mcp` exists only with `MUNARIUM_MCP=enabled` ([operator guide](ops/mcp.md)).
Its security properties, each covered by a named test in `mcp_tests.rs`:

- **Entry.** Only a valid, unrevoked, tenant-bound capability JWT carrying
  `mcp`; static role tokens are refused even with full native authority. 401
  answers carry `WWW-Authenticate: Bearer`; an asserted uid must equal the
  token subject. Requests with a non-allowlisted `Origin` get 403.
- **Authority is the intersection.** Every tool needs its own operation scope
  (`query`, or `evidence_read` for the two evidence readers) on the listing
  and on a direct call, and the native handler then runs with the caller's own
  bearer and applies tenant, clearance, compartment, resource, runbook and
  retention checks. The two listing tools filter by the same rules before
  ordering, counting and paging. Session tools also require that the live
  token still owns and covers the session's recorded clearance (stricter than
  native). A v1.4 `evidence` token does not open the MCP evidence readers.
- **Closed table.** Eleven tools, each compiled to one fixed native route or
  dedicated read; the caller supplies no route, method or query. No tool
  reaches index lifecycle, ingestion, providers, budgets, tokens, evidence
  writes, governed memory, reports or platform operations.
- **Audit before execution.** A content-free admission row (`mcp_invocations`:
  invocation id, tool, tenant, uid, token id, request id, time) is committed and
  its acknowledgment confirmed before any tool reads data; otherwise the call is
  refused and nothing runs. A memory-only deployment refuses every tool call.
  Interaction rows on the `mcp` plane carry no arguments or results.
- **Spend.** No MCP call reaches a model without `MUNARIUM_MCP_PAID_TOOLS=enabled`
  and a trusted policy naming a budget owner and request and UTC-day token
  allowances for every contributing collection. Each physical attempt,
  including adapter retries, is reserved before it is sent; tool arguments
  cannot choose the policy or payer.
- **Recovery without replay.** Operation keys are bound to tenant, uid and
  tool; duplicates and recovery reads are admitted separately, recheck current
  authority before revealing anything, never re-execute, and treat absent and
  inaccessible keys alike. Receipts hold no bodies, transcripts, answers or
  evidence.
- **Residual risks.** `mcp` is not a confinement: the same token's `query`
  scope works on native routes. Revocation applies to MCP only with
  `MUNARIUM_TOKEN_REVOCATION_CHECK=true`. Tool results are untrusted content
  for the agent's model; prompt-injection resistance belongs to the client.
  The generation policy file is trusted configuration; protect it like other
  deployment secrets' configuration. Real-client interoperability is not yet
  qualified ([pending receipts](ops/mcp-client-qualification.md)).

## 5. Static token roles

`MUNARIUM_STATIC_TOKENS="token:tenant:role"` with role ∈ `rw | ro | mgmt`:

- `rw` — control plane writes (shapes, providers, runbooks, ledger commands).
- `ro` — control plane reads.
- `mgmt` — the manager's role: token issuance, reports, admin. **mgmt cannot
  write the ledger and rw cannot mint tokens** — a leak of either credential
  is bounded to its plane.

## 6. Residual risks, stated honestly

| Risk | Bound / mitigation |
|---|---|
| Stolen capability JWT | `exp` (≤ 24 h), `lvl`/`cmp` least privilege, `scopes`, `rb` allowlist, uid-mismatch detection, optional deny-list. |
| Stolen `mgmt` static token | Can mint arbitrary tokens for its tenant. Rotate via env / your secret store; network policy should restrict:8080/:50051 ingress to the manager. Issuance is audited (`access_tokens.issued_by`). |
| Manager compromise | **Game over by design** — the manager is the security boundary. munarium-server's audit trail is the forensic record, not a prevention layer. |
| Interaction log contains user content | Bodies capped at `MUNARIUM_INTERACTION_BODY_MAX` (32 KiB default; larger stored as sha256+length). Retention/TTL is a governance decision (M14); the table lives inside the same tenant-scoped database. |
| Secret compromise (`MUNARIUM_TOKEN_SECRET`) | Rotate the secret (all outstanding tokens die — TTL makes this cheap). Dual-secret rotation windows are the M16 hardening. |
| Ops plane (:9090) is unauthenticated | By deployment posture it is never exposed through the gateway/ingress (internal-only, like a K8s scrape target). `/metrics` carries **no tenant or user data by construction** — the metric cardinality rules (munarium-server/src/metrics.rs) forbid tenant/uid labels; per-tenant analytics live behind the mgmt-role reports API. `/healthz` and `/readyz` return bare status. Network policy should still keep :9090 off any public path. |
| `/admin` dashboard cookie | By default (`MUNARIUM_CONSOLE_SESSIONS=legacy`, the v1.4 login) the browser login stores the static mgmt token itself in an `HttpOnly; SameSite=Strict; Path=/admin` cookie (`__munarium_admin`) — equivalent in power to the mgmt token it holds, bounded the same way (row above). No `Secure` attribute because the default compose profile serves plain http on loopback; deployed environments terminate TLS at the ingress in front of it. The opt-in hardening is revocable console sessions (next rows): with `MUNARIUM_CONSOLE_SESSIONS=enabled` the cookie (`__munarium_console`, or `__Host-munarium_console` with `Secure; Path=/` when `MUNARIUM_CONSOLE_COOKIE_SECURE=true`) carries only an opaque session handle, the `__munarium_admin` cookie is removed at sign-in and ignored, and the token never reaches the browser after the login form. |
| CSRF on `/admin` action forms (2026-08-19; control plane 2026-08-27) | The console's state-changing forms are token issue, the token-revocation confirmation, the run-step approval confirmation, and, with console sessions, sign-out and the session-revocation confirmation (the authoring forms were removed 2026-08-27). The v1.4 `POST /admin/tokens/{jti}/revoke` keeps its CSRF check but now returns the revocation review instead of acting (next row). Defense is layered: `SameSite=Strict` on the cookie, plus a stateless synchronizer token in every form — `hex(sha256(boot_secret \|\| sha256(boot_secret \|\| credential)))` over a per-boot CSPRNG secret, constant-time compared on every POST. No storage; the accepted caveat is that a server restart invalidates in-flight forms (they re-render with a "stale form" notice). Bearer-header callers are unaffected — a custom Authorization header cannot be forged cross-site. For a console session (`MUNARIUM_CONSOLE_SESSIONS=enabled`) the same construction runs over the session's own random `binding_secret` (stored server-side, never sent) and its session id instead of the per-boot secret and the credential, and the U4 review plans use that secret too. The token therefore validates on every replica that shares the session store, survives restarts, and dies with the session at sign-out, revocation or expiry. |
| Reviewed, typed confirmation of destructive console actions (ClearView U4) | Token revocation is the first destructive console action behind review and confirmation. The tokens list links to a read-only review (`GET /admin/tokens/{jti}/revoke`; the v1.4 POST to that URL returns the same review) that states the token's jti, uid, claims and the enforcement consequence. Only `POST /admin/tokens/{jti}/revoke/confirm` revokes, and only when the operator has typed the jti. The review embeds a stateless plan, `hex(sha256(boot_secret \|\| sha256(boot_secret \|\| fields)))` over the requesting credential's CSRF token, the tenant, the action, the target and a digest of the token row as reviewed. At submission the server rechecks the mgmt role, view-only refusal, the CSRF token, the typed identifier, the target (tenant-scoped) and the plan, so another operator's plan, a target changed or revoked since review, and a restart all refuse with nothing revoked. An already-revoked token offers no confirmation. The write is conditional on the token still being unrevoked, so a duplicate or racing confirmation acts at most once and reports that nothing changed. `POST /v1/access-tokens/{jti}/revoke` is unchanged. Run-step approval follows the same pattern: the run page links each awaiting step to a read-only review (`GET /admin/runs/{run_id}/steps/{ordinal}/approve`; the v1.4 POST to that URL returns the same review, ignores any rw token it carries and never approves) that states the run, the step, the steps that will run next and the ledger consequence. Only `POST /admin/runs/{run_id}/steps/{ordinal}/approve/confirm` approves, and only with the typed run id, the rw credential (next row) and a plan bound to the operator, tenant, action, run and step, and a digest of the run and step as reviewed. It then needs a heavy REST admission slot ([admission classes](ops/admission.md)), and the approval rechecks the step's state under the run lock, so a duplicate, racing or post-`/v1` confirmation approves nothing. **Behavior change for review under COMPAT-T5:** the v1.4 console `POST /admin/runs/{run_id}/steps/{ordinal}/approve` now returns the review instead of approving; `POST /v1/runs/{run_id}/steps/{ordinal}/approve` is unchanged and remains the automation route. |
| Revocable console sessions (ClearView U10, opt-in) | With `MUNARIUM_CONSOLE_SESSIONS=enabled`, a successful mgmt login creates a session: the cookie carries a random opaque handle (`mcs1_` + 64 hex, 244 random bits) and the store keeps only its SHA-256. On PostgreSQL the sessions are rows in `console_sessions` (migration 0050) shared by every replica; on the memory store (single replica) they are in process. Each session is bound to the tenant and role it signed in with and to a session-salted digest of the static credential; every request re-resolves that credential against the replica's current `MUNARIUM_STATIC_TOKENS`, so a rotated or removed token, a moved tenant or a lost mgmt role ends it. Idle (`MUNARIUM_CONSOLE_SESSION_IDLE_SECS`, default 1800) and absolute (`MUNARIUM_CONSOLE_SESSION_MAX_SECS`, default 28800) expiry are checked on every request by the database clock, using the stricter of the values recorded at sign-in and the replica's settings. Sign-out (`POST /admin/logout`) and console revocation (`/admin/console-sessions`, behind the U4 review and typed confirmation) set `revoked_at`, and every replica refuses the handle from its next request. Malformed, unknown, tampered, ended or no-longer-authorized handles redirect to the login and clear the cookie; a store error refuses (fail closed). Bearer callers and the view-only proxy are unchanged. The default stays the v1.4 login; switching, the mixed-version cutover order and rollback (a legacy replica never reads the handle; revoke outstanding sessions before re-enabling) are in [console sessions](design/console-sessions.md). |
| `/admin` actions and the mgmt/rw split (2026-08-27) | The console never lets the mgmt credential do what the mgmt role cannot do on `/v1`. Token issue/revoke are mgmt operations and run on the admin's own credential, exactly like `POST /v1/access-tokens` and `/revoke`. Approving a runbook gate is an **rw** operation (it appends ledger events when the run names a lineage), so the approval confirmation takes the rw token per submission, authenticates it, requires the rw role, and refuses a token from any other tenant — it is never stored, never cookied. A leaked mgmt token therefore still cannot write the ledger through the browser. Publishing shapes/runbooks stays on the API/CLI by design (the deploy artifact is the applied bytes; GitOps is their source of truth). |
| Untrusted text on `/admin` (ClearView U9) | Every user-derived string passes through one escaper, `charts::esc`. It escapes the five HTML metacharacters and renders each invisible or direction-changing character as a visible `<U+XXXX>` marker: C0 and C1 controls other than tab, line feed and carriage return; bidi marks, embeddings, overrides and isolates; zero-width and word-joiner characters; soft hyphen; BOM; line and paragraph separators; tag characters. A right-to-left override therefore cannot make a file name, citation or token `uid` read differently from its bytes. The marker is plain text and is safe in attributes. ZWJ and ZWNJ pass through because scripts and emoji depend on them. Only the console display changes; API responses are unchanged. |
| Clearing a fleet artifact block (ClearView ART2, opt-in) | With `MUNARIUM_ARTIFACT_DAMAGE_MODE=enforce`, two independent replicas' authoritative mismatches block an artifact for the whole fleet. No `/v1` route, REST role or console action can clear the block. Clearance requires a repair workflow preauthorized through `munarium-server artifact-damage authorize-repair`, which runs with the deployment's database credential, the same boundary as migrations. The authorization is scoped to one incident, and it neither changes bytes nor clears anything itself. The ledger clears the block only after two different replicas each fetch the repaired object from the artifact store after the authorization and match the pinned digest. It records the workflow, the attempt and both verifications. Residual risk: replicas share the database credential, so verifier independence rests on distinct instance ids. Anyone holding that credential can already write the catalog directly. A per-replica authenticated verifier identity is an open question. A deployment that declares `MUNARIUM_ARTIFACT_DAMAGE_REPLICAS=single` replaces the second replica with the same replica's second run and fetch at least the configured interval later (default 5 minutes). The incident records this as `single_replica` assurance, because one systematically wrong verifier could then both block and clear. [Design](design/artifact-damage.md) · [Procedure](ops/artifact-damage.md) |
| View-only proxies (2026-08-27) | A trusted GET-only proxy in front of `/admin` (a read-only console, say) sends `X-Munarium-Admin-View-Only: 1`; the server then renders every action form as a note and refuses the action routes outright under that header. The header is advisory for rendering only — it grants nothing; authorization is unchanged, and a proxy that forwards POSTs without the header would still need the mgmt credential (and, for approval, an rw token) to change anything. |

## 7. Source-store and third-party-service credential posture

The same discipline that governs the BYOK provider keys governs where
document bytes live (`MUNARIUM_SOURCE_STORE` — see
[guides/source-stores.md](guides/source-stores.md)):

- **Azure Blob:** managed identity — **no secret exists** (the example AKS
  module's storage account disables shared-key auth entirely; do the same). The only non-ambient path is a
  SAS **by reference** (`MUNARIUM_BLOB_SAS_REF`: an env-var name or `file:/path`)
  for off-Azure tooling.
- **AWS S3:** the ambient chain — env credentials, IRSA web identity, or the
  instance profile. Static keys exist only as the pair
  `MUNARIUM_S3_ACCESS_KEY_ID` + `MUNARIUM_S3_SECRET_KEY_REF`, both-or-neither
  (half a credential is refused at startup), the secret always through the
  same `resolve_secret` ref seam: env-var name or `file:/path`, never inline
  configuration.
- **GCS:** `GOOGLE_APPLICATION_CREDENTIALS` / the metadata server, or a
  service-account key via `MUNARIUM_GCS_CREDENTIALS_REF` — again a ref, never
  the JSON inline.
- **Recorded URIs never carry credentials.** Every source row's `blob_uri`
  is credential-free by construction (test-enforced), so the audit trail can
  name where bytes live without ever becoming a way in.

One egress caveat deserves its own line: **`MUNARIUM_DOCINTEL=azure` sends
document bytes to a third-party Azure AI endpoint.** Everything else in the
source-store path stays inside your storage perimeter; document intelligence
is the one deliberate exception, which is why it is off by default and an
explicit per-environment opt-in — see
[guides/document-intelligence.md](guides/document-intelligence.md) before
enabling it anywhere new.

## 8. Deployment checklist

- [ ] `MUNARIUM_TOKEN_SECRET` ≥ 32 random bytes, from your secret store — never in compose files.
- [ ] `MUNARIUM_REQUIRE_UID=true` (default — do not relax in production).
- [ ] Exactly one `mgmt` token, held only by the API-management layer.
- [ ] Network policy: :8080/:50051 reachable only from the manager; :9090 ops reachable only from the platform.
- [ ] `MUNARIUM_TOKEN_TTL_SECS` tuned to the manager's session length (shorter is better).
- [ ] Decide `MUNARIUM_TOKEN_REVOCATION_CHECK` per compliance posture.
- [ ] Leave `MUNARIUM_MCP` unset unless agents need the endpoint; if enabled, set `MUNARIUM_TOKEN_REVOCATION_CHECK=true`, restrict `MUNARIUM_MCP_ALLOWED_ORIGINS`, and have the broker issue `mcp` only with the narrowest data-plane scopes ([operator guide](ops/mcp.md)).
- [ ] Postgres credentials distinct from any app secret; the DBA deletion runbook (`docs/ops/index-deletion-runbook.md`) gated by change control.
