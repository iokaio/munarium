# Guarded command recovery

PostgreSQL tenants may opt into `guarded-v1` command recovery. A durable claim
precedes execution, so concurrent requests and application restart cannot blindly
repeat a command whose outcome is unknown. The default remains `legacy`.
This is an application-process guarantee while PostgreSQL remains available;
it does not qualify database crashes, power loss or exactly-once remote effects.

## Activation and compatibility

Upgrade **every command writer**, drain in-flight commands, then use a management
credential for the tenant to call `POST /v1/command-recovery` with
`{"mode":"guarded-v1"}`. `GET /v1/command-recovery` reports the durable policy.
Both operations also have generated native `ServerApiService` RPCs. In-memory
deployments cannot activate this policy. Activation is idempotent and one-way;
there is no deactivate or automatic lease-expiry operation.

Migration 0039 is additive. Existing completed receipts retain their original
encoding and remain replayable; they are not evidence of pre-execution claims.
New completions also write the legacy receipt in the same transaction as their
completed claim. An older binary can read that response, but bypasses unresolved
claims. Mixed old/new writers are therefore unsupported after activation.
Roll forward, or roll back only to a binary that implements this protocol.
Keep a restored database isolated until activation and unresolved claims have
been reconciled with the authoritative recovery records; an older backup alone
cannot establish that a command has never executed.

## Covered commands and outcomes

The policy covers the seven keyed REST commands under `/v1/versions` and all
eight typed `CommandService` RPCs, including gRPC `UpsertDigest`. It does not add
idempotency to unkeyed writes, ingestion, provider calls or runbook execution.
Runbook checkpoint recovery retains its separately documented contract.

Claims bind the authenticated tenant, a 1–256-byte idempotency key, operation,
target and request hash. REST and typed gRPC encodings remain distinct. A
different operation, target, body or plane returns `idempotency-mismatch`.
Completed calls replay the original response. Legacy receipts retain their
historical hash binding; activation cannot retroactively add missing evidence.

A duplicate pending call, cancellation, handler failure or crash before receipt
commit returns `command-unresolved` on a subsequent identical request: HTTP 409,
gRPC `FAILED_PRECONDITION`. This error is **not** a retryable head conflict.
The original request may have applied no effects, some effects or all effects.
The command mutation and receipt are not one database transaction. The durable
claim intentionally prevents a second execution even when the first owner died
before applying an effect.

Management callers can inspect metadata using
`GET /v1/command-recovery/receipt?key=<original-key>`. It returns operation, state
and timestamps, never the stored response payload. Inspect the ledger and other
affected systems before explicitly authorizing a new command/key; a new key is
a new execution, not a repair or continuation of the old claim. No automatic
resubmission or manual "mark complete" endpoint is provided.

## Retention and qualification

Completed claims follow `MUNARIUM_IDEMPOTENCY_TTL_SECS`; zero disables expiry.
As with legacy receipts, replay protection ends after successful receipts expire.
Unresolved claims and activation records never expire through this janitor.
They may contain identifiers and hashes and must remain in backup/restore scope.

`guarded_command_process_recovery` kills and reopens the application on both
REST and typed gRPC at pre-execution claim, post-effect/pre-receipt, and committed
receipt barriers, with uninterrupted controls. The two-pool race fixture checks
cross-instance admission, legacy replay, authority, tenant/target binding and
unresolved retention. These fixtures use isolated PostgreSQL and no paid provider.
