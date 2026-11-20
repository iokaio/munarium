# Revocable console sessions (U10 · LOGIN1)

This page specifies the shared session store that the [U10 open item](README.md#open-items) asks for, and how an existing deployment moves to it and back. The requirements are [U10](airk-analysis.md#login1), [LOGIN1](airk-analysis.md#login1) with its tests [LOGIN1-T1](airk-analysis.md#login1-t1) and [LOGIN1-T2](airk-analysis.md#login1-t2), and [COMPAT-T5](README.md#compat-t5). The security summary is in the [security posture](../security-posture.md).

## Status

Implemented and **off by default**. `MUNARIUM_CONSOLE_SESSIONS=legacy`, the default, is exactly the v1.4 login: the `__munarium_admin` cookie holds the static mgmt token. Sessions take effect only when an operator sets `enabled` on every replica, in the order given under [cutover](#cutover). The automated cutover preflight that LOGIN1 asks for (refuse the switch while a v1.4 or unknown replica serves console traffic) is not implemented. The operator checks it today, using each replica's Health page. The LOGIN1 receipts still need the cluster qualification run ([plan](cluster-qualification-plan.md#plan-login1)).

## Settings

| Variable | Default | Meaning |
|---|---|---|
| `MUNARIUM_CONSOLE_SESSIONS` | `legacy` | `legacy` (v1.4 cookie) or `enabled` (revocable sessions). Any other value is a startup error. Under `legacy` the three settings below are not read. |
| `MUNARIUM_CONSOLE_SESSION_IDLE_SECS` | `1800` | Idle expiry: a session with no console request for this long is refused. 60 to 86400. |
| `MUNARIUM_CONSOLE_SESSION_MAX_SECS` | `28800` | Absolute expiry, counted from sign-in, whatever the activity. 300 to 604800, and at least the idle value. |
| `MUNARIUM_CONSOLE_COOKIE_SECURE` | `false` | `true` names the cookie `__Host-munarium_console` and sets `Secure` and `Path=/`, as the `__Host-` prefix requires. Use it whenever browsers reach the console over HTTPS, including through a trusted TLS-terminating gateway. Leave it off only for plain HTTP on loopback. |

Every replica that serves console traffic must use the same values. The store records the idle value and the absolute expiry at sign-in. Each request is checked against the stricter of those recorded values and the replica's own settings, so a replica with looser settings never extends a session.

## Store

On the PostgreSQL store the sessions are rows in `console_sessions` ([migration 0050](../../src/munarium-store-pg/migrations/0050_console_sessions.sql), additive). The row holds the following fields.

- `handle_digest`: SHA-256 of the handle. The handle itself is never stored or logged.
- `session_id`: the public identifier the console lists and revokes by. Knowing it grants nothing.
- `tenant_id` and `role`: the tenant and authority of the credential that signed in.
- `credential_digest`: a SHA-256 of that static credential, salted with the session id.
- `binding_secret`: a random per-session secret for CSRF and review plans (below). It never leaves the server.
- `created_by`, `created_at`, `last_seen_at`, `idle_secs`, `expires_at`: the replica that signed in, the times and the recorded expiry values.
- `revoked_at` and `revoked_reason`: `sign-out` or `revoked`. A revoked row is never live again.

The [retention inventory](../../retention/inventory.json) declares the table. A sign-in deletes rows that are seven days past their absolute expiry. Nothing else deletes them, so revocations stay visible on the sessions page for a week.

## Handle and sign-in

A successful `POST /admin/login` with a mgmt token (or any login under `MUNARIUM_AUTH_MODE=disabled`) creates a row and sets the cookie to a new handle. The handle is `mcs1_` followed by 64 hex characters from two random v4 UUIDs (244 random bits from the operating system's generator). The response also clears any `__munarium_admin` cookie, so the static token no longer sits in the browser. The cookie is a browser-session cookie with `HttpOnly` and `SameSite=Strict`. It has `Path=/admin` by default, or the `__Host-` form above.

On every console request the replica:

1. rejects a cookie that is not a well-formed handle, without a store read;
2. in one conditional `UPDATE`, finds the row by handle digest and records the request time. The row matches only if it is not revoked, is before its recorded absolute expiry, is within the replica's absolute lifetime, and is within the stricter idle bound. The database clock decides, so replica clocks do not matter;
3. re-resolves the bound credential against the replica's current static tokens (`MUNARIUM_STATIC_TOKENS`). The session is refused unless that credential still exists and still authenticates to the same tenant and role with mgmt authority. Rotating or removing a token therefore ends its sessions on each replica as soon as that replica runs the new configuration.

Any failure redirects to `/admin/login` and clears the session cookie. A store error is also a refusal: the console fails closed.

A request with an `Authorization: Bearer` header is unaffected in both modes. Scripts, the demo view-only proxy and the `/v1` automation routes keep their v1.4 behaviour. Under `enabled` the v1.4 cookie is ignored.

## Revocation and sign-out

- **Sign-out.** Session pages show a "sign out" button (a CSRF-protected `POST /admin/logout`). It sets `revoked_at` on the row and clears the cookie. Every replica that shares the database refuses the handle from its next request. If the store cannot record the sign-out, the page says so and the session is unchanged. Under `legacy`, `POST /admin/logout` just clears the v1.4 cookie.
- **Revocation.** `GET /admin/console-sessions` lists the tenant's sessions, newest 100 first: state, sign-in time, last request, expiry and the replica that signed in. The current session is marked. Each live row links to a read-only review, `GET /admin/console-sessions/{session_id}/revoke`. Only `POST /admin/console-sessions/{session_id}/revoke/confirm` revokes, with the [U4 pattern](../security-posture.md): a typed session id and a plan bound to the operator, tenant, action, target and a digest of the row as reviewed. The digest leaves out `last_seen_at`, which every request moves. The mgmt role, view-only refusal, CSRF, target and plan are rechecked on submit, and the write is conditional on the row still being unrevoked. Revocation is tenant-scoped, and another tenant's operator gets a 404.

## CSRF binding

Under `legacy`, and for every bearer request, the CSRF token and U4 plans are unchanged. They derive from the per-boot secret and the presented credential, so they are valid only on the replica that rendered the form and only until it restarts.

For a console session they derive from the session's `binding_secret`: the CSRF token is `hex(sha256(s || sha256(s || ":session:" || session_id)))`, and plans use the same construction over their fields. Every replica that reads the row computes the same values, so a form rendered on one replica can be submitted to another (LOGIN1-T2). The values become useless when the session ends. `SameSite=Strict` on the cookie remains the first layer.

## Cutover

The switch is an explicit operator action, made only when every replica that serves console traffic supports sessions.

1. **Upgrade.** Roll out the build that contains migration 0050 and this feature, leaving `MUNARIUM_CONSOLE_SESSIONS` unset. Every replica keeps the v1.4 login. A v1.4 replica in the mixed fleet does not know the table and ignores it. Upgrading the last replica does not change the login (LOGIN1-T1).
2. **Preflight.** Confirm that no v1.4 replica, and no replica of unknown version, serves console traffic. Open `/admin/health` on each pod, by pinning the request or port-forwarding to it. It shows the build's "console login" row. Confirm that `MUNARIUM_REPLICA_COUNT` matches the replicas you checked, and that all of them share one database. If any replica is v1.4 or unknown, stop: the cutover is refused and the v1.4 login continues. This check is manual until an automated preflight exists (open question below).
3. **Switch.** Set `MUNARIUM_CONSOLE_SESSIONS=enabled`, the same idle and absolute values, and `MUNARIUM_CONSOLE_COOKIE_SECURE=true` behind TLS on every replica, then do a rolling restart. During the restart a browser can meet both modes. A legacy replica reads only the v1.4 cookie, and an enabled replica reads only the session cookie and removes the v1.4 one at sign-in. A browser moved between the two may therefore be asked to sign in again. That never grants access, and it stops when the last replica restarts. Use session affinity or a short window if the prompts matter.
4. **Verify.** Sign in through one pod and act through another, then revoke and check that both refuse the handle (LOGIN1-T2).

## Rollback

To return to the v1.4 login, set `MUNARIUM_CONSOLE_SESSIONS=legacy` (or unset it) on every replica and do a rolling restart. A v1.4 binary is equally valid, because the table is additive. Outstanding handles then behave as follows.

- A legacy replica never reads the session cookie. Once every serving replica is legacy, no handle is honoured anywhere, and operators sign in again through the v1.4 login, which sets `__munarium_admin` again.
- Rows remain in the table, unused, until pruning. If the fleet is switched back to `enabled` before a handle's absolute expiry, a browser that still holds it would be accepted again. As the last rollback step, revoke all outstanding sessions. The console sessions page still lists them in legacy mode and revokes them per tenant. Fleet-wide, run `UPDATE console_sessions SET revoked_at = now(), revoked_reason = 'revoked' WHERE revoked_at IS NULL`. Alternatively, wait `MUNARIUM_CONSOLE_SESSION_MAX_SECS` before switching again.
- Rolling back to a build before migration 0050 leaves the table in place. That build never reads it, and the migration does not have to be reverted.

## Memory store

The memory store keeps sessions in process, with the same handle format, binding, expiry rule, sign-out and revocation. The process clock decides expiry. The memory store is single-replica by validation (`MUNARIUM_REPLICA_COUNT > 1` requires PostgreSQL), so the process is the whole fleet. A restart ends every session, as it does every CSRF token today.

## Open questions

- **Automated preflight (LOGIN1-T1).** LOGIN1 asks for a cutover that refuses while a v1.4 or unknown replica serves console traffic. Because the switch is an environment setting, a v1.4 replica cannot be detected from the database. Closing this needs a capability report from each replica, compared against the expected replica set, and a fleet-wide mode that the operator flips once instead of a per-replica setting.
- **Fleet-wide rollback.** Revoking all sessions on rollback is a manual step today. A recorded fleet epoch would let a re-enabled fleet refuse every handle issued before a rollback.
- **Pagination.** The sessions list shows the newest 100 rows. The U8 keyset pagination can be applied if a tenant needs more.
