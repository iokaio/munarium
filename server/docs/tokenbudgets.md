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
| `complete_default` | `POST /v1/providers/{name}/complete` when the request omits `max_tokens` | 1,024 | — (the request's own `max_tokens`) |
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

Two routes, REST-only (no gRPC twin — like the evidence read plane and the
reports). Both live in `max_tokens_api.rs`; the OpenAPI document carries
their schemas (`MaxTokensBudgets`, `MaxTokensResponse`).

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
  *reservation*, which estimates `prompt/4 + max_tokens` before the call and
  settles to actuals after — oversizing inflates transient holds, not bills.
- **The retry is part of the budget.** A turn whose stop reason is
  `max_tokens`/`length`, or whose text is empty, is re-asked once at 4× the
  base. The effective ceiling per turn is therefore 5× the base in the worst
  case before verification retries.
- **Reasoning-always-on models** (`z-ai/glm-5.2`, `z-ai/glm-5.3`) measured
  ~5k hidden tokens on hard questions. A base of 2,048 (retry 8,192) covers
  that; history-revolution declares 4,096 in its runbook. See
  [guides/retrieval-sizing.md](guides/retrieval-sizing.md) for the runbook
  side and the measurements.
