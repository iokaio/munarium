# Revocable console sessions (U10 · LOGIN1)

This page specifies the shared session store that the [U10 open item](README.md#open-items) asks for, and how an existing deployment moves to it and back. The requirements are [U10](airk-analysis.md#login1), [LOGIN1](airk-analysis.md#login1) with its tests [LOGIN1-T1](airk-analysis.md#login1-t1) and [LOGIN1-T2](airk-analysis.md#login1-t2), and [COMPAT-T5](README.md#compat-t5). The security summary is in the [security posture](../security-posture.md).

## Status

Implemented and **off by default**. With no operator action every replica serves exactly the v1.4 login: the `__munarium_admin` cookie holds the static mgmt token. A replica serves sessions only when both of these hold:

- its own `MUNARIUM_CONSOLE_SESSIONS=enabled` allows them (a per-replica upper bound), and
- the **console fleet mode** of its deployment environment is `enabled`. The mode is stored in the database and changed once for the whole fleet with `munarium-server console-fleet` (see [cutover](#cutover)). That command refuses to enable sessions while a live replica cannot serve them or the live replicas do not match the expected count (the LOGIN1 preflight).

Switching the fleet mode back to `legacy` increments a recorded fleet epoch. A re-enabled fleet refuses every handle issued before (see [rollback](#rollback)). The LOGIN1 receipts still need the cluster qualification run ([plan](cluster-qualification-plan.md#plan-login1)).

## Settings

| Variable | Default | Meaning |
|---|---|---|
| `MUNARIUM_CONSOLE_SESSIONS` | `legacy` | `legacy` (v1.4 cookie, always) or `enabled` (revocable sessions **when the console fleet mode is also `enabled`**; the v1.4 login until then). Any other value is a startup error. Under `legacy` the three settings below are not read, and neither is the fleet mode. |
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
- `fleet_environment` and `fleet_epoch` ([migration 0087](../../src/munarium-store-pg/migrations/0087_replica_heartbeats_console_fleet.sql), additive): the deployment environment and console fleet epoch the session was issued under. A row is honoured only while that environment's fleet mode is `enabled` at the same epoch. Rows written before 0087 have neither and are never honoured again.

Migration 0087 also adds two tables. `console_fleet_mode` holds one row per deployment environment (`MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID`, default `local`): the mode (`legacy` or `enabled`), the epoch, and when and by whom it was last changed. No row means `legacy` at epoch 0. `server_replica_heartbeats` holds each running replica's heartbeat ([replica heartbeats](#replica-heartbeats)).

The [retention inventory](../../retention/inventory.json) declares the table. A sign-in deletes rows that are seven days past their absolute expiry. Nothing else deletes them, so revocations stay visible on the sessions page for a week.

## Handle and sign-in

When the replica serves sessions, a successful `POST /admin/login` with a mgmt token (or any login under `MUNARIUM_AUTH_MODE=disabled`) creates a row and sets the cookie to a new handle. The insert takes the epoch from the fleet mode in the same statement. If the mode was switched back to `legacy` in the meantime, no row is written and the login page asks the operator to sign in again. The handle is `mcs1_` followed by 64 hex characters from two random v4 UUIDs (244 random bits from the operating system's generator). The response also clears any `__munarium_admin` cookie, so the static token no longer sits in the browser. If the request already carried a session cookie, that handle is signed out first, as `POST /admin/logout` would, so signing in again never leaves the replaced session live and unseen until it expires. If that sign-out cannot be recorded, no new session is started and the login page says so. The cookie is a browser-session cookie with `HttpOnly` and `SameSite=Strict`. It has `Path=/admin` by default, or the `__Host-` form above.

On every console request the replica first decides which login it serves. Under `MUNARIUM_CONSOLE_SESSIONS=legacy` that is the v1.4 login, and nothing is read. Otherwise it reads its environment's fleet mode: `legacy` means the v1.4 login, and a mode that cannot be read refuses the request (fail closed). Under sessions the replica then:

1. rejects a cookie that is not a well-formed handle, without a store read;
2. in one conditional `UPDATE`, finds the row by handle digest and records the request time. The row matches only if it is not revoked, is before its recorded absolute expiry, is within the replica's absolute lifetime, is within the stricter idle bound, and was issued in this replica's environment at the epoch that environment's fleet mode is `enabled` at now. The database clock decides, so replica clocks do not matter. A page's own automatic reload is not the operator's activity: the build-job page's `<meta http-equiv="refresh">` points at the page with `?refresh=1`, and such a request is checked by the same predicate in a plain `SELECT` that does not record the request time, so a page left open cannot keep an unattended session alive;
3. re-resolves the bound credential against the replica's current static tokens (`MUNARIUM_STATIC_TOKENS`). The session is refused unless that credential still exists and still authenticates to the same tenant and role with mgmt authority. Rotating or removing a token therefore ends its sessions on each replica as soon as that replica runs the new configuration.

Any failure redirects to `/admin/login` and clears the session cookie. A store error is also a refusal: the console fails closed.

A request with an `Authorization: Bearer` header is unaffected in both modes. Scripts, the demo view-only proxy and the `/v1` automation routes keep their v1.4 behaviour. Under `enabled` the v1.4 cookie is ignored.

## Revocation and sign-out

- **Sign-out.** Session pages show a "sign out" button (a CSRF-protected `POST /admin/logout`). It sets `revoked_at` on the row and clears the cookie. Every replica that shares the database refuses the handle from its next request. If the store cannot record the sign-out, the page says so and the session is unchanged. Under `legacy`, `POST /admin/logout` just clears the v1.4 cookie.
- **Revocation.** `GET /admin/console-sessions` lists the tenant's sessions, newest 100 first: state, sign-in time, last request, expiry and the replica that signed in. The current session is marked. Each live row links to a read-only review, `GET /admin/console-sessions/{session_id}/revoke`. Only `POST /admin/console-sessions/{session_id}/revoke/confirm` revokes, with the [U4 pattern](../security-posture.md): a typed session id and a plan bound to the operator, tenant, action, target and a digest of the row as reviewed. The digest leaves out `last_seen_at`, which every request moves. The mgmt role, view-only refusal, CSRF, target and plan are rechecked on submit, and the write is conditional on the row still being unrevoked. Revocation is tenant-scoped, and another tenant's operator gets a 404.

## CSRF binding

Under `legacy`, and for every bearer request, the CSRF token and U4 plans are unchanged. They derive from the per-boot secret and the presented credential, so they are valid only on the replica that rendered the form and only until it restarts.

For a console session they derive from the session's `binding_secret`: the CSRF token is `hex(sha256(s || sha256(s || ":session:" || session_id)))`, and plans use the same construction over their fields. Every replica that reads the row computes the same values, so a form rendered on one replica can be submitted to another (LOGIN1-T2). The values become useless when the session ends. `SameSite=Strict` on the cookie remains the first layer.

## Replica heartbeats

Every Server process on the PostgreSQL store writes a heartbeat to `server_replica_heartbeats` at startup and every 15 seconds. The row expires 45 seconds after its last write, by the database clock. It records:

- a random replica id generated once per process (not derived from the host);
- the instance id (`MUNARIUM_INSTANCE_ID`, else the host name, as the scale report and the sessions page already show it);
- the build version, `MUNARIUM_DEPLOYMENT_PLANE` (`rest` by default) and the configured `MUNARIUM_REPLICA_COUNT`;
- its capabilities: `console-sessions` when the build supports sessions and its `MUNARIUM_CONSOLE_SESSIONS=enabled` allows them.

Rows are scoped by `MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID`. Expired rows are ignored by every reader and deleted by the next heartbeat in the environment. A failed write is logged and never stops the replica: its row then expires, which is the conservative reading. The memory store is single-replica by validation, so it reports itself without a table. The [scale report](scale-out.md#scale-3-replica-membership) shows the same rows.

## Cutover

The switch is one operator action for the whole fleet, made with the database credential, like `artifact-damage`:

```text
munarium-server console-fleet status  [--environment E]
munarium-server console-fleet enable  --expected-replicas N --actor A [--environment E]
munarium-server console-fleet disable --actor A [--environment E]
```

The command runs against the configured `MUNARIUM_DATABASE_URL` and exits. `--environment` defaults to `MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID` (`local`). It prints JSON: the mode, epoch, when and by whom it was changed, and (for `status`) the live replicas. It is deliberately not a `/v1` route or a console form: the mode applies to every tenant's console, and a mgmt token is scoped to one tenant.

1. **Upgrade.** Roll out the build that contains migrations 0050 and 0087. Every replica keeps the v1.4 login, whatever its `MUNARIUM_CONSOLE_SESSIONS`, because the fleet mode is `legacy`. A v1.4 replica in the mixed fleet does not know the tables and ignores them. Upgrading the last replica does not change the login (LOGIN1-T1).
2. **Allow.** Set `MUNARIUM_CONSOLE_SESSIONS=enabled`, the same idle and absolute values, and `MUNARIUM_CONSOLE_COOKIE_SECURE=true` behind TLS on every replica, and set `MUNARIUM_REPLICA_COUNT` to the number of Server processes (serving and builder). Do a rolling restart. The login still does not change: each replica now reports the `console-sessions` capability.
3. **Switch.** Run `console-fleet enable --expected-replicas N`, where N is the number of Server processes your orchestrator runs (for example the ready pods of every Server group). It is refused, writes nothing and exits with status 3 unless all of the following hold, in one transaction under an environment lock:
   - every live replica reports `console-sessions` (otherwise `replica_lacks_console_sessions`);
   - every live replica is configured with `MUNARIUM_REPLICA_COUNT` = N (otherwise `replica_count_mismatch`);
   - exactly N replicas are live (otherwise `replica_shortfall` or `replica_excess`);
   - the heartbeats can be read (otherwise `membership_unavailable`).

   The codes are listed in [errors](../api/errors.md#console-fleet-cutover-refusals). On success every replica serves sessions from its next console request, without a restart. A browser that still holds the v1.4 cookie is asked to sign in again; that never grants access.
4. **Verify.** Sign in through one pod and act through another, then revoke and check that both refuse the handle (LOGIN1-T2). `/admin/health` on each pod shows its setting, the fleet mode and epoch, and what it serves now.

**How a v1.4 or unknown replica is detected, and the limits.** A v1.4 process never writes a heartbeat, and neither does a v1.5 build from before migration 0087, so the database cannot see it directly. The preflight detects it as a shortfall instead: the operator states N from the orchestrator, every live replica must be configured with that same N, and exactly N must be live. A running v1.4 replica, or one whose heartbeat fails, leaves fewer than N live, so the switch is refused. The check rests on N being right. It cannot detect:

- a process the operator leaves out of N, if every heartbeating replica is also configured with that smaller N. Example: three pods, one of them v1.4, `MUNARIUM_REPLICA_COUNT=2` and `--expected-replicas 2`;
- a v1.4 replica that shares the database but is deployed in another environment, or a v1.5 replica configured with another `MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID`. It is counted in its own environment, not this one;
- a replica that starts after the switch. A new v1.5 replica with `MUNARIUM_CONSOLE_SESSIONS=enabled` joins in sessions mode, and one with `legacy` serves the v1.4 login (the upper bound). A v1.4.0 binary cannot start against a database with migrations it does not know ([rollback](../guides/server-1.5.md#rolling-back-to-140)), so no new v1.4 replica can join.

The scale report and Health page warn when more replicas are live than configured, which catches part of the first case after the fact.

## Rollback

To return to the v1.4 login, run `munarium-server console-fleet disable --actor A`. It is never refused. It records `legacy` and increments the fleet epoch. Every replica serves the v1.4 login from its next console request, without a restart. Operators sign in again through the v1.4 login, which sets `__munarium_admin` again.

- **Epoch.** Every session row records the epoch it was issued under, and a replica honours a handle only at the epoch the fleet mode is `enabled` at now. After `disable` and a later `enable`, the fleet is at the new epoch, so every handle issued before the rollback is refused on every replica, even if it is unrevoked and unexpired. New sign-ins work at once. The sessions page labels such rows "ended by fleet rollback". This replaces the manual "revoke all on rollback" step.
- **Alternative: per-replica rollback.** Setting `MUNARIUM_CONSOLE_SESSIONS=legacy` on every replica and doing a rolling restart also restores the v1.4 login, but it does not move the epoch. If those replicas are later allowed again while the fleet mode is still `enabled`, outstanding handles are honoured again until they expire. In that case, revoke them before allowing sessions again: per tenant on the console sessions page (it still lists them on a legacy replica, labelled from their recorded idle and absolute expiry), or fleet-wide with `UPDATE console_sessions SET revoked_at = now(), revoked_reason = 'revoked' WHERE revoked_at IS NULL`, or wait `MUNARIUM_CONSOLE_SESSION_MAX_SECS`. Prefer `console-fleet disable`.
- Rows remain in the table, unused, until pruning.
- Returning to a v1.4.0 binary means restoring the pre-upgrade backup ([rolling back to 1.4.0](../guides/server-1.5.md#rolling-back-to-140)), which also discards data written since the upgrade, including these tables.

## Memory store

The memory store keeps sessions in process, with the same handle format, binding, expiry rule, sign-out and revocation. The process clock decides expiry. The memory store is single-replica by validation (`MUNARIUM_REPLICA_COUNT > 1` requires PostgreSQL), so the process is the whole fleet: its `MUNARIUM_CONSOLE_SESSIONS` setting is the fleet mode, it reports itself as the one live replica, and `console-fleet` does not apply. A restart ends every session, as it does every CSRF token today.

## Open questions

- **Automated preflight (LOGIN1-T1).** Resolved 20 November 2026: replicas write expiring heartbeats with their capabilities, and the fleet-wide mode is switched once with `munarium-server console-fleet`, refused while a live replica cannot serve sessions or the live count disagrees with the expected count ([cutover](#cutover), including the limits of detecting v1.4 replicas).
- **Fleet-wide rollback.** Resolved 20 November 2026: `console-fleet disable` increments a recorded fleet epoch, and a re-enabled fleet refuses every handle from an earlier epoch ([rollback](#rollback)). The manual revocation remains documented for the per-replica rollback.
- **Pagination.** The sessions list shows the newest 100 rows. The U8 keyset pagination can be applied if a tenant needs more.
