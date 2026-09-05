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

Every `/v1` (REST) and `mmp.v1` (gRPC) call must carry the end-user id the
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
- `scopes` — `query` (sessions/turns), `ingest` (file upload),
  `findings` (file warn/info findings on a lineage via
  `POST /v1/versions/{id}/findings`), and/or `evidence` (seal and resolve artifacts on the structured-evidence plane). The last two
  are the scopes Munarium Matrix's service token holds; each grants nothing
  else, and an `ro` token never has either. A static `rw` token carries all
  four.

  **`evidence` does not widen what a token may read.** The scope says "this
  principal participates in the evidence plane"; the artifact's authorization
  class says "and only within this clearance". Domination is still checked per
  artifact, in both directions: an under-cleared `evidence` token is refused
  on a read, *and* refused when it tries to SEAL above its own clearance —
  otherwise a low-clearance service could mint high-clearance evidence that
  every later reader would trust.
- `rb` — optional runbook-name allowlist.
- `exp` — short TTL (default `MUNARIUM_TOKEN_TTL_SECS=3600`, hard cap 24 h).

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
| `/admin` dashboard cookie | The browser login stores the static mgmt token itself in an `HttpOnly; SameSite=Strict; Path=/admin` cookie (`__munarium_admin`) — a default posture, equivalent in power to the mgmt token it holds, bounded the same way (row above). No `Secure` attribute because the default compose profile serves plain http on loopback; deployed environments terminate TLS at the ingress in front of it. A server-side session table is the documented hardening if the dashboard outgrows this posture. |
| CSRF on `/admin` action forms (2026-08-19; control plane 2026-08-27) | The console's state-changing forms are token issue, token revoke, and runbook-gate approval (the authoring forms were removed 2026-08-27). Defense is layered: `SameSite=Strict` on the cookie, plus a stateless synchronizer token in every form — `hex(sha256(boot_secret \|\| sha256(boot_secret \|\| credential)))` over a per-boot CSPRNG secret, constant-time compared on every POST. No storage; the accepted caveat is that a server restart invalidates in-flight forms (they re-render with a "stale form" notice). Bearer-header callers are unaffected — a custom Authorization header cannot be forged cross-site. |
| `/admin` actions and the mgmt/rw split (2026-08-27) | The console never lets the mgmt credential do what the mgmt role cannot do on `/v1`. Token issue/revoke are mgmt operations and run on the admin's own credential, exactly like `POST /v1/access-tokens` and `/revoke`. Approving a runbook gate is an **rw** operation (it appends ledger events when the run names a lineage), so the form takes the rw token per submission, authenticates it, requires the rw role, and refuses a token from any other tenant — it is never stored, never cookied. A leaked mgmt token therefore still cannot write the ledger through the browser. Publishing shapes/runbooks stays on the API/CLI by design (the deploy artifact is the applied bytes; GitOps is their source of truth). |
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
- [ ] Postgres credentials distinct from any app secret; the DBA deletion runbook (`docs/ops/index-deletion-runbook.md`) gated by change control.
