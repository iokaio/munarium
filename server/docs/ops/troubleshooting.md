# Troubleshooting: symptom → check → fix

The incident-shaped distillation of dev-guide §11's triage catalog and
§2's gotcha almanac. The guide teaches the mechanisms; this
page is for the 2 a.m. read. Every entry was earned, not imagined —
sources are the guide chapters and script comments cited inline.

## The server won't start

| Symptom | Meaning | Fix |
|---|---|---|
| `config error: …`, **exit 2** | fail-closed env parse; nothing was touched | the message names the variable AND the way out — do what it says (README env table is the reference) |
| `startup error: …`, **exit 1** | config parsed; the world refused (bad DB password, unreachable postgres, migration failure) | read the wrapped error; for `migration N was previously applied but has been modified` see the next row |
| migration checksum refusal | a shipped migration was edited in place (legal pre-1.0) | local compose: `docker compose down -v`; anywhere else: drop and recreate the database deliberately (destructive) — there is no in-place repair, by design |
| bind panic on the REST or gRPC port | the port is taken; both planes die loudly at bind since 2026-08-17 | find the owner; if it is a stale `munarium-server` on an alternate test port, the scripts reap it by process identity — copy that pattern, never a bare kill by port |
| new migration "does not run" (table missing, server green) | sqlx embeds migrations at COMPILE time of munarium-store-pg; adding a file doesn't dirty the crate | `cargo clean -p munarium-store-pg`, rebuild (dev-guide §2's stale-embed gotcha) |

## Requests failing

| Symptom | Meaning | Fix |
|---|---|---|
| 400 `uid-required` | the contract: every /v1 call carries `X-Munarium-Uid` | send the header (no request id on this rejection — it is pre-span, a documented sharp edge) |
| 403 `uid-mismatch` | JWT `sub` disagrees with the asserted uid | use the token owner's uid |
| 409 `head-conflict` | NORMAL optimistic concurrency, not an error to page on | read again, decide again, retry with a fresh Idempotency-Key |
| 422 `idempotency-mismatch` | same key, different body | new key for new work; byte-identical replay returns the recorded response |
| 409 `session-not-open` | turn against a closed/expired session (2026-08-17) | open a new session; the detail names the actual state |
| 409 `run-locked` | this runbook run is executing on ANOTHER instance (the cluster advisory lock, 2026-08-17) | poll `GET /v1/runs/{run_id}`; retry when settled — see [clustering.md](clustering.md) for the crash-orphan diagnosis |
| 503 `overloaded` + `Retry-After: 1` | the instance is at `MUNARIUM_MAX_CONCURRENCY` in-flight /v1 requests | honor Retry-After; if chronic, raise the ceiling or add instances |
| 400 naming a chronology rules asset | a version armed `chronology_rules` metadata but the asset is not applied | `POST /v1/chronology-rules` (or `mmctl apply -f`) the asset first — arming fails loud by design |

## Deployed-environment classics

| Symptom | Meaning | Fix |
|---|---|---|
| boots green, FIRST blob call hangs ~its timeout | the platform exposes a credential endpoint (`IDENTITY_ENDPOINT`) rather than classic IMDS, and only `from_env()`-style constructors read it; or the pod simply has no identity | the note in munarium-store-objects/src/lib.rs — check the `source bytes store backend=` startup line first, then that the pod carries the workload-identity annotation the chart renders when `workloadIdentity.clientId` is set |
| gRPC works in CI, "transport error" on Windows | tonic tolerates a scheme-less endpoint on Linux, rejects it on Windows | always `http://host:port` |
| terraform gets "Too many command line arguments" | PowerShell splits an unquoted `-flag=value` at dots/paths | quote the whole argument (`'-var-file=terraform.tfvars'`) |
| a cloud CLI mangles `/subscriptions/...`-style ids under Git Bash | MSYS path conversion rewrites arguments that look like POSIX paths | `MSYS_NO_PATHCONV=1`, or run the CLI from PowerShell |
| 8080/8443 taken locally | 8080 is popular; 8443 is often Windows-excluded (Hyper-V/WinNAT) | use the +10000 alternates (18080/15051/19090/18443); `netsh interface ipv4 show excludedportrange protocol=tcp` shows the reserved ranges |
| deploy "succeeded", old behavior serves | the rollout never completed — old pods stayed healthy and kept serving behind the stable hostname while the new one crash-looped | `kubectl rollout status`, then the running pods' image tag, `/version`, and the served `/openapi.json` path count against the committed spec ([deployment-runbook.md](deployment-runbook.md) §4) |

## What to look at, in order

1. **The five startup INFO lines** — starting (with `instance=`), store,
   source-store backend, plane listeners. A missing line localizes the
   failure.
2. **`/readyz` on either plane** — `ok` / `not ready` (store probe
   failed) / `draining` (stop signal received; 2026-08-17).
3. **`GET /metrics` on :9090** — error-class counters by route, pool
   size/idle, audit-writer queue depth and drops, load sheds
   (2026-08-17; no tenant data, safe to curl).
4. **The `/admin` operator console** (mgmt login) — traffic/error/latency
   series, slowest endpoints, and (2026-08-27) the control plane itself:
   hosted runbooks/shapes with their applied YAML, run step machines
   (approve a waiting gate with an rw token), collections and index
   versions, sessions turn by turn, capability tokens (issue/revoke), the
   audit trail with the same filters as `/v1/reports/audit`, and recent
   gate findings across every lineage. `/admin/health` shows the effective
   non-secret configuration — the fastest way to confirm what the instance
   actually booted with.
5. **`x-munarium-request-id`** from the failing response → `MUNARIUM_LOG`
   span → `GET /v1/reports/audit` row (mgmt). One id, three surfaces.
6. **`/healthai`** — six PAID model probes; a diagnostic, never a
   monitor loop.
