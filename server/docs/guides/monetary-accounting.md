# Optional monetary accounting (P14)

The existing `GET /v1/reports/cost` remains a **session completion token** report:
its turn counts and provider/model totals have not changed. Monetary reporting is
separate and requires PostgreSQL. No prices are downloaded or assumed; without a
catalog the token APIs work as before and monetary amounts are unknown. Local
endpoints are not implicitly free. These reports are accounting evidence, not a
provider invoice or a monetary admission limit.

## Scope and coverage

PostgreSQL-backed gateway completions (including structured/helper calls) and
embedding cache misses record each HTTP submission before sending. Retries share
an invocation ID but have separate attempt IDs and submission-time price pins.
A failure to commit an attempt prevents that submission. Cancellation, transport
failure, exhausted retry, invalid JSON, or a lost observation write leaves an
unresolved attempt at its conservative estimate. A pre-send crash can therefore
overcount attempts; it must never imply zero liability. Failed attempts are not
presumed free, even for HTTP 429. Embedding cache hits create no attempts.
Accounted units retain the gateway's existing token estimate when evidence is
incomplete; that heuristic is not a guaranteed upper bound on provider charges.
Unresolved liability remains explicit even if the estimate is small or zero.

The scope is `recorded_gateway_http_attempts`. Memory servers, legacy records,
older binaries, direct custom providers bypassing the HTTP adapter, `/healthai`
probes, document intelligence and infrastructure bills are outside this scope.
Their unknown attempt count is `unrecorded_attempts: null`; there is no fabricated
backfill. `entire_tenant_bill` is always false. During a rolling upgrade, old
writers keep working but their calls remain outside coverage. Upgrade all gateway
writers before relying on this scope.

## Management API

All operations require the tenant's static management token (or existing
development auth-disabled mode). They share handlers with native gRPC
`ServerApiService`; no separate typed monetary protobuf service is introduced.
Capability and `rw`/`ro` tokens cannot administer tariffs or read reports.
Bodies are UTF-8 JSON on both transports.

| REST | Native RPC | Purpose |
|---|---|---|
| `GET /v1/monetary/prices` | `MonetaryPrices` | Read immutable tenant tariffs |
| `POST /v1/monetary/prices` | `AddMonetaryPrice` | Append a tariff; identical repeats are idempotent |
| `POST /v1/monetary/observations` | `AddMonetaryObservation` | Append usage/cost reconciliation |
| `GET /v1/reports/money?from=...&to=...` | `MonetaryReport` | Submission-window report with coverage and attempt detail |

`from` and `to` are required RFC 3339 instants, inclusive/exclusive respectively.
An empty window is rejected. Windows over 10,000 attempts are rejected rather
than silently truncated; narrow the window. Reports read one PostgreSQL statement
snapshot and select the latest observation per attempt. Returned revision/price
IDs identify that calculation; corrections do not erase original observations.
Authorized database audit readers can inspect all `monetary_observations` rows.

Coverage separately counts priced, missing-price, missing-usage, unknown-category
and unresolved attempts; these categories can overlap. Observed input/output
totals are **known subtotals**, independent of conservative `accounted_units`.
Unknown counts are `null` in individual records. Monetary totals are grouped by
currency, never converted or added across currencies. Micro-unit amounts and
aggregate token counts are decimal integer strings to preserve browser precision.
A subtotal is not the entire tenant bill.

## Immutable fictional tariff

These rates are fictional, not current provider prices. Supply all fields:

```json
{
  "id": "fictional-usd-2026-09",
  "provider": "ollama",
  "route": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "model": "fictional-model",
  "currency": "USD",
  "valid_from": "2026-09-01T00:00:00Z",
  "valid_until": "2026-10-01T00:00:00Z",
  "basis": "inclusive",
  "rates": {
    "input": {"micro_units": 1, "per_tokens": 3},
    "output": {"micro_units": 0, "per_tokens": 1}
  }
}
```

A micro-unit is one millionth of the named currency. Numerators are nonnegative
u64 integers, denominators positive u64 integers; zero explicitly means free.
Use integer-capable JSON clients above 2^53. Unknown fields/category names fail
validation. IDs cannot be reused with different content. Overlapping windows for
the same provider/route/model are rejected, including different currencies, so
selection is unambiguous across replicas.

`route` is lowercase SHA-256 of the adapter's canonical JSON
`{"url": effective_request_url, "routing": configured_openrouter_downstream_or_null}`.
It distinguishes endpoints, completion/embedding paths and configured routing.
Use the hash from a recorded attempt, or compute it with
`munarium_providers::accounting::route_identity` from operator-owned configuration. No URL,
credential, prompt or completion text is stored in the monetary ledger. Provider
and model are the resolved request selection; the tariff must cover the route's
pricing, including any gateway-managed downstream choice.
URLs with userinfo or query parameters other than `api-version` are recorded as
`unpriceable-route`; potentially credential-bearing values are not even hashed
into accounting identifiers. Such attempts retain unknown-price coverage.

`inclusive` applies input/output rates to totals including cache input and
reasoning output. Additional cache/reasoning rates are forbidden to avoid double
charging. Anthropic cache counts are added to its exclusive input count;
OpenAI-compatible cache/reasoning counts are already subsets. Ollama uses its
prompt/evaluation counts. Missing counts remain unknown, never inferred from the
stable response's zero defaults.

`partitioned` requires six rates and six known counts: `input`, `output`,
`cache_read`, `cache_write`, `reasoning`, `context`. Subtract cache from input and
reasoning from output before charging those categories separately. Subsets cannot
exceed their parent. Context is a separate billable unit with an operator-defined
fixed rate; context-tier applicability must be established by operator evidence.
Adapters do not attest every partition count, so partitioned pricing generally
needs reconciliation. Unknown response usage categories prevent automatic pricing.
A missing/expired tariff or absent required rate remains unknown, even for zero
token counts.

For each category calculate `ceil(tokens * micro_units / per_tokens)` with u128
intermediates, sum with checked arithmetic, and require the result to fit u64.
Rounding is per category, per attempt. At the example rate, 1 and 3 input tokens
each cost 1 micro-unit; 4 cost 2. Overflow rejects calculation; automatic recording
leaves the durable attempt unresolved for reconciliation. It never wraps or
saturates into a plausible price.

## Corrections and late evidence

An observation body is:

```json
{
  "id": "fictional-reconciliation-1",
  "attempt_id": "attempt-id-from-report",
  "previous_revision": 0,
  "usage": {
    "input": 4, "output": 2,
    "cache_read": null, "cache_write": null,
    "reasoning": null, "context": null,
    "unknown_categories": false
  },
  "accounted_units": 6,
  "resolved": true,
  "evidence_ref": "fictional-provider-receipt-1",
  "price_id": null
}
```

Use the report's current revision (`0` means no observation). A new ID with a stale
revision fails. Repeating the identical ID/body returns the original calculation
without another charge; reusing an ID with different content fails. Corrections
need a new ID, current revision and evidence reference. Only safe receipt IDs
belong in `evidence_ref`, never invoice bodies, credentials or private URLs.
Accounted units must cover observed tokens; incomplete/unresolved observations
must also retain at least the pre-send estimate. This report does not change the
separate daily token admission ledger.
Management operators attest this evidence; the server does not independently
verify provider invoices.

The submission-time snapshot remains valid for that work after expiry. Late
usage uses that snapshot; today's catalog never reprices historical work. For an
initially unpriced attempt, an operator may name `price_id` with applicability
evidence. It must match provider/route/model and the original submission window.
Once established, that pin cannot change. A request outside a tariff's validity
window cannot be reconciled against that tariff.

## Migration, rollback and validation

Migration `0037_monetary_accounting.sql` adds three tables, indexes and append-only
triggers. It neither rewrites existing data nor estimates legacy usage. Catalog
insertion serializes per tenant; observations serialize per attempt with revision
checks. Database roles with DDL privileges remain trusted operators. Backups
retain price/observation identities; a restore only preserves its recorded
horizon. No erasure or retention deadline is implied; see the
[retention inventory](../ops/retention-inventory.md).

Old binaries can read token state after migration but do not record monetary
attempts. Rollback creates a coverage gap: retain all tables, pins and observations
and resume with a compatible writer. Never edit migration history to hide a gap.
New binaries can continue token-only operation with an empty price catalog.

Focused checks from `server/` (PostgreSQL requires a disposable test database):

```powershell
cargo test --locked --offline -p munarium-core -p munarium-store-pg --test money
cargo test --locked --offline -p munarium-server providers_api::usage_tests
cargo test --locked --offline -p munarium-server money_api::tests
cargo test --locked --offline -p munarium-server --features json-arbitrary-precision money_api::tests
cargo test --locked --offline -p munarium-server docs_coverage
```

Fixtures and tariffs are fictional. No live provider, price lookup or paid
qualification is needed.
Without `MUNARIUM_TEST_DATABASE_URL`, the database-dependent tests print an
unavailable notice and return early; that is not PostgreSQL coverage.
