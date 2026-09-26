# Token budgets — the per-call output ceilings

Every paid model call the server makes carries a `max_tokens` ceiling: the
most output tokens the provider may generate for that one call. It is a
**ceiling, not spend** — a non-reasoning model answering in 140 tokens is
billed 140 — but a reasoning model draws its hidden reasoning from the same
budget, and can spend the whole ceiling before writing a visible word. That
is the failure this page exists to configure away: on 2026-09-02
`z-ai/glm-5.2`, the OpenRouter capable tier, spent 1,024 + 4,096 tokens on
hidden reasoning over an advisory question and answered with nothing.

This page is the reference for the eight budgets, the three places each one
can be set, the API that replaces them, and the client calls. The daily
**spending caps** (`spec.budgets.dailyTokens` on a provider config,
`/v1/reports/budgets`) are a different thing — a limit on a day's spend, not
a per-call ceiling — and are documented with the provider configs.

## The eight budgets

| Field | Which call | Built-in | Runbook knob that overrides it |
|---|---|---|---|
| `turn_completion` | a session turn's answer (`POST /v1/sessions/{id}/turns`, `…/turns/stream`); the truncation-aware retry pays one re-ask at **4×** this | 2,048 | `completion.maxTokens` |
| `query_expansion` | the `modelQueryExpansion` variant-generation call before retrieval | 256 | `retrieval.modelQueryExpansion.maxTokens` |
| `complete_default` | Provider completion without `max_tokens`; vocabulary generation; `/v1.2/answers`; `/v1.2/query` without a collection output budget | 1,024 | Provider request `max_tokens`, or collection query policy as described below |
| `healthai_probe` | each of the nine `/healthai` probe completions | 512 | — |
| `hierarchy_classifier` | the evidence hierarchy's one-word question classifier | 32 | — |
| `hierarchy_intent` | the evidence hierarchy's semantic-intent task (names only, never SQL) | 480 | — |
| `runbook_advisory` | the AI advisory pass of runbook validation | 2,048 | — |
| `authoring_assist` | the guided-authoring assist draft | 8,192 | — |

The built-ins are the 2026-09-02 doubling of every value the server carried
before (1,024 / 128 / 512 / 256 / 16 / 240 / 1,024 / 4,096). The two budgets a
runbook can also declare keep the runbook grammar's validation ranges —
`turn_completion` 256..=16,384 and `query_expansion` 32..=512 — so the API
cannot set a value a runbook could not; the rest accept 1..=65,536.

## Precedence

Session completion enlarges its ceiling once, by four, only for a provider
`max_tokens` or `length` stop reason. Empty output with another stop reason,
refusal, unsupported tool/continuation output, or malformed output fails without
an enlargement. If the enlarged call still exhausts its allowance, the turn fails
with a provider error instead of returning a blank or incomplete answer as a
successful completion. Corrective verification calls also reject incomplete
output and do not trigger another enlargement. Every dispatched attempt goes
through admission and settlement; a denied retry retains the first call's usage.

Claude effort/thinking can be pinned per model in the
[provider configuration](guides/managing-key-and-secrets.md#claude-effort-and-thinking).
These settings do not alter the ceilings or spending caps. Prompt wording and
effort changes need model evaluation; an offline protocol test cannot establish
answer quality or an appropriate production allowance.

At the moment of a call, the first of these that applies wins:

1. **The runbook's own declaration**, where the grammar has one
   (`completion.maxTokens`, `retrieval.modelQueryExpansion.maxTokens`). A
   runbook that declares a budget is saying something about its corpus and
   its models, and nothing below this line overrides it.
2. **The tenant's replacement** — the whole object last sent to
   `POST /v1/max-tokens`.
3. **The process environment** — `MUNARIUM_MAX_TOKENS_*` variables set on
   the container.
4. **The built-ins** in the table above.

`GET /v1/max-tokens` reports which of 2 and 3/4 is in effect as `source`
(`tenant` or `environment`); it does not know about runbooks, which are
resolved per call.

Server 1.2.1 collection queries use the exact-clearance entry in
`query.model_routes` when present, otherwise `query.max_output_tokens`, otherwise
`complete_default`. Multiple selected collections combine their explicit output
budgets using the smallest value. These query policy values accept 1–100,000
tokens and are independent of session runbook budgets. Vocabulary generation
always uses `complete_default`, including when the collection defines query model
routes. See [collection policy](guides/collection-vocabularies.md#collection-queries-and-publication-governance-121)
for routing and context limits.

## Environment variables (the container)

One optional variable per budget, all under one prefix. An unset variable
leaves the built-in; a set one must parse as an unsigned integer inside the
field's range, or the server **refuses to start** — a budget that silently
fell back to a built-in would be a setting that lies.

| Variable | Field |
|---|---|
| `MUNARIUM_MAX_TOKENS_TURN_COMPLETION` | `turn_completion` |
| `MUNARIUM_MAX_TOKENS_QUERY_EXPANSION` | `query_expansion` |
| `MUNARIUM_MAX_TOKENS_COMPLETE_DEFAULT` | `complete_default` |
| `MUNARIUM_MAX_TOKENS_HEALTHAI_PROBE` | `healthai_probe` |
| `MUNARIUM_MAX_TOKENS_HIERARCHY_CLASSIFIER` | `hierarchy_classifier` |
| `MUNARIUM_MAX_TOKENS_HIERARCHY_INTENT` | `hierarchy_intent` |
| `MUNARIUM_MAX_TOKENS_RUNBOOK_ADVISORY` | `runbook_advisory` |
| `MUNARIUM_MAX_TOKENS_AUTHORING_ASSIST` | `authoring_assist` |

The environment is the value a restart comes back to for every tenant that
has not replaced its set. Set these wherever your deployment declares the
server's environment (Helm values or your overlay, a compose file), and roll
configuration changes with or before the image they belong to — a new image
may require newly declared configuration.

## The API

Both routes live in `max_tokens_api.rs`; the OpenAPI document carries their
schemas (`MaxTokensBudgets`, `MaxTokensResponse`). Server 1.2 also exposes
them through the named `ServerApiService` RPCs and every language's
[`ServerApiClient`](../../clients/docs/guides/server-1.2.md). The historical
typed provider planes retain their REST-only budget methods.

### `GET /v1/max-tokens`

Any authenticated role — the numbers shape spend, they are not secrets.
Answers the effective set for the caller's tenant:

```json
{
  "turn_completion": 2048,
  "query_expansion": 256,
  "complete_default": 1024,
  "healthai_probe": 512,
  "hierarchy_classifier": 32,
  "hierarchy_intent": 480,
  "runbook_advisory": 2048,
  "authoring_assist": 8192,
  "source": "environment"
}
```

After a replacement, `source` is `"tenant"` and `updated_at` carries the
RFC 3339 instant of the write.

### `POST /v1/max-tokens`

Static **rw** role, like provider configs and runbooks (403 otherwise). The
body is the eight fields — **all of them, every time**:

```json
{
  "turn_completion": 4096,
  "query_expansion": 256,
  "complete_default": 1024,
  "healthai_probe": 512,
  "hierarchy_classifier": 32,
  "hierarchy_intent": 480,
  "runbook_advisory": 2048,
  "authoring_assist": 8192
}
```

It **replaces the whole set**. There is no partial update by construction:
the wire type has eight required fields, a body missing one is 400
`invalid-input` (naming the field), an out-of-range value is 400
`invalid-input` (naming the field and its range), and the store writes the
object as one row. Extra fields are ignored, so a `GET` body — `source` and
`updated_at` included — round-trips into a `POST`. The answer is the same
shape `GET` returns, with `source: "tenant"`.

To return a tenant to the environment values, post them: `GET` shows what
they are. There is no delete.

### Persistence and replicas

On Postgres the replacement is one row per tenant in `max_tokens_budgets`
(migration `0031`), so it survives restarts and is shared by every replica.
Each replica caches per tenant and re-reads after
`MUNARIUM_REGISTRY_TTL_SECS` (default 15 s) — the same convergence promise,
and the same limit, that provider configs and shapes make: the replica that
took the `POST` answers the new values immediately; the others within the
TTL. On the memory store the replacement is process-local, which is exact,
because config validation confines the memory store to one replica.

A stored row this binary cannot read as a whole valid object (a newer
writer's field, a hand edit) fails **closed** to the environment values and
says so in the log — never to a mix of old and new.

## Client libraries

All four official clients carry the pair on their REST transport; on gRPC
the calls raise the client's usual "unsupported on this transport" error.

| Client | Read | Replace |
|---|---|---|
| Rust `munarium-client` | `max_tokens()` | `replace_max_tokens(&MaxTokensBudgets)` |
| Python `munarium_client` | `max_tokens()` | `replace_max_tokens(budgets)` (sync and async) |
| .NET `Ioka.Munarium.Client` | `GetMaxTokensAsync()` | `ReplaceMaxTokensAsync(MaxTokensBudgets)` |
| Java `io.ioka.munarium` | `maxTokens()` | `replaceMaxTokens(MaxTokensBudgets)` (sync and async) |

All four hang the pair on the **providers** plane (`client.providers` /
`client.Providers`), beside `GET`/`POST /v1/providers`. Each takes and
returns typed `MaxTokensBudgets` / `MaxTokensResponse` models; a 400 decodes
to the client's invalid-input error, a 403 to its forbidden error. The
read-modify-replace flow is typed in each: Rust reuses the server's own
DTOs (`munarium_client::dto`, `#[serde(flatten)]` on the budgets); Python's
`MaxTokensResponse` subclasses `MaxTokensBudgets`, so a GET result passes
straight into `replace_max_tokens` (the eight fields alone reach the wire);
.NET's `MaxTokensResponse.ToBudgets()` lifts a read into a `required`-field
record, so a partial body cannot compile; Java's `MaxTokensResponse.budgets()`
plus per-field withers (`withTurnCompletion(long)` …) do the same.

## Sizing notes

- **A ceiling is not spend.** Raising `turn_completion` costs nothing on a
  model that answers in 140 tokens. It matters for the spending-cap
  *reservation*, which estimates the effective request and output ceiling before the call and
  settles to actuals after — oversizing inflates transient holds, not bills.
- **The retry is part of the budget.** In Server 1.3, a turn with explicit
  `max_tokens`/`length` termination is re-asked at most once at 4× the base.
  Empty text alone does not trigger this retry. A still-incomplete final answer
  fails instead of being returned as complete. The two output ceilings sum to
  5× the base before corrective verification calls and physical HTTP retries;
  this is not a bound on total input/output usage or charges.
- **Reasoning-always-on models** (`z-ai/glm-5.2`, `z-ai/glm-5.3`) measured
  ~5k hidden tokens on hard questions. A base of 2,048 (retry 8,192) covers
  that; history-revolution declares 4,096 in its runbook. See
  [guides/retrieval-sizing.md](guides/retrieval-sizing.md) for the runbook
  side and the measurements.

## Late token evidence and accounting quality

The `effective-json-bytes-v1` estimator counts the serialized effective request
(system text, prompt, tools, schema and settings), rounds bytes up in groups of
four, adds 32 tokens for framing, then adds the normalized output ceiling.
It is a versioned heuristic, not a guaranteed upper bound on provider billing.
Reservations retain its revision; legacy rows retain unknown provenance.
Complete provider counts can settle below the estimate, including observed zero.
For partial usage, missing components retain their corresponding estimates and
the total cannot fall below the original reservation. Provider retries still
have the physical-attempt limitations in the inventory below.

Management credentials can read `GET /v1/budgets/{id}/evidence`, read immutable
history at `GET /v1/budgets/{id}/adjustments`, and submit late evidence with
`POST /v1/budgets/{id}/adjustments`. All are tenant-scoped. The request is:

```json
{
  "id": "receipt-001",
  "expected_revision": "0",
  "accounted_units": "120",
  "usage": {"input_tokens": "100", "output_tokens": "20", "source": "provider_reported"},
  "evidence_ref": "fictional-provider-receipt-001"
}
```

Amounts and revisions use canonical unsigned decimal strings, preserving browser
precision. PostgreSQL accounted amounts are limited to signed 64-bit storage.
IDs and evidence references accept only letters, digits, dot, dash and underscore,
at most 160 bytes; supply an opaque receipt identifier, never raw provider data.
This is operator-attested evidence, not automatic verification of a receipt.
Unknown fields and malformed numbers are rejected. Only settled reservations can
be corrected. A live hold must finish, or be conservatively swept after its stale
deadline, before reconciliation. Released work cannot be charged by this API.
Partial or unverified late evidence cannot reduce the prior accounted liability
or undercount its known subtotal; complete observed counts can correct it downward.

An exact repeated correction ID returns its original result even after later
corrections; changed content returns `idempotency-mismatch`, and stale revisions
return `head-conflict`. The update and its append-only before/after evidence are
atomic. A correction retains the original UTC accounting day and can exceed a
cap: that debt blocks subsequent same-day admissions, rather than rejecting
truthful usage. It does not consume a different day's fresh allowance.

`GET /v1/reports/budget-usage?day=YYYY-MM-DD` reports complete, partial and unknown
observations and their reservations for the original day. Its scope is explicitly
`recorded_token_reservations`; `unrecorded_invocations: null` means unknown, not
zero. Legacy absent evidence and overflowing totals stay unknown. More than 10,000 reservations is an
explicit error, never a silently truncated report. This report and corrections
are separate from the optional monetary ledger.

Migration 0038 adds nullable estimator provenance, revision zero for existing rows,
and an immutable adjustment table. The unique index can briefly block concurrent
writes while it is created; size and schedule upgrades accordingly. Already-running
old writers settle held rows only and do not overwrite corrections to settled
rows. Restarting an older binary is a separate compatibility question: its embedded
migrator does not know migration 0038. Retain the schema and use a binary that
recognizes it; never remove migration history or evidence to make rollback start.
Prefer a roll-forward fix when these accounting methods or estimator are required.

## Dispatch accounting inventory

The following describes the implemented policy, including uncapped paths.
The [implementation roadmap](lessons-from-vcp-impl.md#42-p05-make-the-admission-coverage-explicit--r15-and-r14)
records D1's opt-in config-wide policy and D7's separate diagnostic access work.

Set `spec.budgets.dailyTotalTokens` to a nonnegative signed 64-bit integer to
reserve capacity before **each physical completion or embedding HTTP attempt**
through the gateway. Zero refuses paid work; omission preserves legacy behavior.
This uses the existing shared ledger's `all` scope, keyed by tenant, resolved
config and UTC day. It covers explicit models without assigning a tier, every
helper using that config, structured requests, and transport retries. Embedding
cache hits create no new reservation; the existing rate check still precedes
the cache lookup. Embedding estimates use effective serialized request bytes
rounded up by four plus 32 framing tokens. Completion estimates use the existing
effective-request estimator. Attempt evidence records `physical-attempt-v1`.

The `all` scope and legacy `fast`/`capable`/`frontier` scopes are independent
ceilings over overlapping work: **do not add their totals as a provider bill**.
Legacy tier reservations remain per logical completion. A reservation is not a
guarantee of an exact bill: observed usage can exceed its estimate and blocks later
admission when that creates debt. Missing, partial or malformed usage retains at
least its estimate and observed subtotal. Failed and cancelled submissions remain
held until the conservative stale sweep; a successful retry never refunds them.
Durable PostgreSQL money records retain invocation and physical-attempt IDs;
token evidence retains its own reservation identity and original accounting day.

This is a config-wide gateway ceiling, not an account-wide limit. Other configs,
legacy health probes, external callers and local index embedding are outside it.
Provider configuration writers remain trusted to set or remove caps. All replicas
using a capped config must run this implementation; an older binary ignores the
new optional field. Upgrade replicas before enabling it, and remove the policy
only deliberately before rolling back. No schema migration is needed.

| Dispatch path | Accounting and retry policy |
|---|---|
| REST and native gRPC direct completion | Both use `providers_api::op_complete`. Rate and legacy tier checks are retained; an opted-in daily total reserves each HTTP attempt. |
| Structured completion | `op_complete_structured` uses the same gateway, reservation and settlement rules; schema handling does not create another dispatch. |
| Session answer, truncation re-ask, corrective re-asks | Each completion re-enters the gateway with the resolved config/model/tier. One truncation re-ask permits four times the base output ceiling; checked multiplication rejects overflow before that re-ask. Corrective re-asks retain the current ceiling and the existing maximum of two. |
| Session query expansion | `sessions_api` uses the gateway with the separately resolved expansion task. An optional failed expansion does not refund dispatched work. |
| Evidence hierarchy classifier and semantic intent | `evidence_hierarchy` calls the gateway with the resolved intent task; each helper is independently admitted. |
| Runbook advisory and authoring assist | `runbooks_api` and `authoring_api` call the gateway with their resolved model task. Response parsing happens after provider accounting. |
| Checked answers and collection queries | `answers_api` uses structured completion with an explicit tier; collection query processing reaches this same answer path. |
| Vocabulary generation | `vocabulary_api` uses structured completion with its configured tier. A later vocabulary-validation failure does not refund the completion. |
| Explicit model or fallback without a tier | Rate limits and an opted-in daily total apply. No tier is invented. Naming an explicit model together with a tier still permits tier-cap enforcement. |
| API embeddings | `op_embed` checks the rate estimate before its cache. Each HTTP attempt on a miss reserves the opted-in daily total and records reported or unknown usage. Hits create no spend record. Existing invocation token projections remain zero, not measured embedding usage; use budget evidence for usage quality. |
| Index construction | Uses the local embedder independently of API provider embeddings; no hosted completion charge is inferred. |
| `/healthai` | Legacy mode retains authenticated any-role paid default-provider probes. Opt-in managed mode requires management access and uses capped tenant configurations through the gateway; see below. |
| Provider health and provider listing | Health calls model-list endpoints directly (Ollama uses its local health endpoint); it has no token reservation and requires management access in managed mode. Listing stays free for any authenticated role; aliases require the separate management diagnostics endpoint. |

Hosted and Ollama HTTP adapters retry 429 and 5xx responses at most twice (three
physical submissions) inside a single logical call, honoring a bounded
`Retry-After`. Transport errors are not retried by this loop. Without a daily total,
legacy tier admission still has one reservation per logical call, settled using
the final response. With a daily total, every retry requires a fresh grant and
earlier unresolved attempts retain their estimated liability.

Cancelling an unpolled request submits no work. Cancellation after reservation
may leave it held even if no response arrives; stale sweeping settles the original
estimate. The reservation is not refunded merely because the future was dropped.
This also covers uncertainty between reserving and sending. Complete observed
usage may settle below the estimate; missing/partial usage follows the conservative
rules in [invocation provenance](architecture.md#93-invocation-provenance).

Session `completion` progress events use a zero-based sequence across the initial
answer, truncation re-ask and corrective re-asks: `0, 1, 2, ...`. These are logical
successful completion events, not physical HTTP retry counters. `verify` events
retain their separate zero-based verification-pass sequence. Expansion and hierarchy
events retain their own task meaning; this slice does not introduce global attempt
IDs or a durable record of every physical submission.

The scripted `dispatch_retries_cancellation_and_uncapped_policy` test counts actual
loopback HTTP arrivals for 5xx-then-success, exhausted 429, cancellation after send,
unpolled cancellation, admission denial and an uncapped explicit model, against
memory and PostgreSQL. `turn_retry_attempts_and_ceiling_are_bounded` exercises real
session turns and validates event ordinals, ceilings and overflow before retry.

## Operator diagnostics

Set `MUNARIUM_MANAGED_PROVIDER_DIAGNOSTICS=true` to require management access for
`GET /healthai` and `GET /v1/providers/{name}/health`, including their native and
typed gRPC counterparts. Omission retains the legacy probe audience and default
model selection. Development authentication-disabled mode still permits management
operations; this flag does not replace authentication.

Managed `/healthai` probes applied configurations belonging to the authenticated
tenant, using their resolved tiers and the tenant's `healthai_probe` output limit.
A missing `budgets.dailyTotalTokens` or unavailable credential skips that config;
there is no fallback to a synthesized default. Zero capacity refuses submission.
Every actual completion and retry passes through the shared gateway's rate, daily
and usage accounting. Each probe has a 30-second deadline; timed-out work retains
its unresolved liability. Details identify the config and a bounded outcome,
without returning raw upstream errors. At least one actual probe must succeed and
every attempted probe must succeed for `healthy: true`. This is a paid operator
action, not readiness or a qualification of all configured models.

`GET /v1/providers/{name}/diagnostics` is a separate, **free, management-only**
REST/native RPC operation. It returns `config_name`, `provider`, `credential_ok`,
`credential_source` (`env`, `file`, `none`) and optional `credential_alias`.
Set `spec.credentialAlias` to an intentionally public operator label of 1–64
letters, digits, dots, underscores or dashes. Never place a secret, environment
reference or path in that label. Aliases are not inferred from credentials and
do not appear in the ordinary provider list. This endpoint never calls a provider.

Credential-resolution errors disclose source kind only. Provider request errors
retain status/category without raw upstream bodies or endpoint URLs; unsafe
endpoint fingerprints are unavailable. Operators must use their provider's own
logs when they need detailed upstream diagnostics. No API returns secret suffixes
or credential hashes. Alias configuration is additive and needs no migration;
rolling back removes the new audience enforcement, so do not roll back while
relying on it for paid diagnostic access control. Upgrade all replicas before
enabling managed mode. Existing token and accounting records remain readable.
