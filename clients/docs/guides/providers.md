# Providers: the BYOK gateway

The server speaks to LLM endpoints with the **tenant's** credentials,
resolved through the secrets seam at call time (`credentialRef: {env: ...}`
or `{file: ...}`) — keys never appear in messages, the ledger, or the
config responses. Credentials fail **closed**: a missing secret is a typed
provider error, never a silent skip.

```yaml
apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata: { name: prod-anthropic }
spec:
  provider: anthropic          # anthropic | openai | openrouter
  models: { complete: [claude-sonnet-4-6] }
  credentialRef: { env: MUNARIUM_SECRET_ANTHROPIC }
```

**Rust**
```rust
let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("user-1"))?;
client.providers.apply_config(provider_yaml).await?;
let health = client.providers.health("prod-anthropic").await?;
let out = client.providers.complete("prod-anthropic", dto::CompleteRequest {
    prompt: Some("Summarize the mesh in one line.".into()),
    version_id: Some(v.clone()),   // records an invocation-provenance event
    ..Default::default()
}).await?;
```

**Python**
```python
client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))
client.providers.apply_config(provider_yaml)
health = client.providers.health("prod-anthropic")
out = client.providers.complete(
    "prod-anthropic", prompt="Summarize the mesh in one line.", version_id=v)
```

**.NET**
```csharp
await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "user-1" });
await client.Providers.ApplyConfigAsync(providerYaml);
var health = await client.Providers.HealthAsync("prod-anthropic");
var result = await client.Providers.CompleteAsync(
    "prod-anthropic", "Summarize the mesh in one line.", versionId: v);
```

**Java**
```java
client.providers.applyConfig(providerYaml);
var health = client.providers.health("prod-anthropic");
var result = client.providers.complete("prod-anthropic",
        new Params.CompleteOptions("Summarize the mesh in one line.",
                null, null, null, null, null, null, v)); // versionId records provenance
```

## Default rule, tiers, and any-model selection

The reserved config name **`default`** engages the server's default rule —
anthropic first, openai second, openrouter third; the first family with a
usable credential serves the request (applied configs beat the server's
env-backed defaults `MUNARIUM_SECRET_ANTHROPIC|OPENAI|OPENROUTER`). `provider`
forces a family; `tier` picks the built-in tier model (`fast` =
claude-haiku-4-5 / gpt-5.4-mini / deepseek/deepseek-v4-flash, `capable` =
claude-sonnet-5 / gpt-5.4 / z-ai/glm-5.2, `frontier` = claude-fable-5-1 /
gpt-5.6-sol / z-ai/glm-5.3); an explicit `model` always wins
and may name **any** model the selected provider supports. Responses echo
the serving `provider` and resolved `model`.

```python
client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))
# fast tier on whichever default provider has a key (anthropic first)
out = client.providers.complete("default", prompt="Say OK.", tier="fast")
# force OpenRouter, any model it serves
out = client.providers.complete(
    "default", prompt="Say OK.", provider="openrouter", model="qwen/qwen3-coder")
```

**`health_ai()`** (`HealthAiAsync` in .NET) live-probes all nine built-in
default models — three families × three tiers — with a tiny completion each and
returns per-check `ok/skipped/latency_ms/detail` plus overall `healthy`.
It spends real provider tokens, and it is REST-only (gRPC raises the typed
`Unsupported` error).

**`list()`** is the free introspection twin (`GET /v1/providers`, REST-only):
every provider config visible to the tenant — applied configs plus the
synthesized env-backed defaults — with each one's resolved
`fast`/`capable`/`frontier` tier models and `credential_ok`. Zero provider calls, and the credential
itself is never echoed; use it to disclose which model a request WOULD get
before spending anything (Rust `client.providers.list()`, Python
`client.providers.list()`, .NET `client.Providers.ListAsync()`, Java
`client.providers.list()`).

Notes:

- Passing `version_id` records the invocation (request hash, model, token
  counts, latency — never keys or bodies) as a ledger event and returns
  `invocation_event_id`. Works on **both** transports since C5 — the proto
  gained the field and both planes now run the same shared `op_complete` /
  `op_embed`, so provenance can never exist on one plane only.
- Embeddings are cached by request hash (`cache_hit` on the result).
- Rate limits (`rpm`/`tpm` budgets) surface as typed rate-limited errors.
  The clients deliberately do NOT auto-retry them — pace your own calls.
  They read a `Retry-After` header opportunistically into the error's
  `retry_after`, but the server does not emit one today, so treat that
  hint as absent and use your own backoff.
- Provider calls are never auto-retried (a completion is not replayable).
- gRPC sentinel note: `temperature: 0.0` / `max_tokens: 0` cannot ride the
  proto3 wire and are rejected — use REST when you need explicit zeros.

## Per-call token budgets (`/v1/max-tokens`, 2026-09-02)

The server's eight per-call output-token ceilings — the `max_tokens` it
hands a provider for a session turn, the query-expansion call, a bare
`complete`, the `/healthai` probes, the evidence hierarchy's classifier and
intent tasks, runbook validation's advisory pass and the authoring assist —
are one object, readable by any authenticated role and **replaceable as a
whole** by the rw role. There is no partial update: every replacement sends
all eight fields, and the server answers 400 `invalid-input` for a missing
or out-of-range one. The response is the same eight fields plus `source`
(`tenant` after a replacement, `environment` while the container's
`MUNARIUM_MAX_TOKENS_*` defaults apply) and `updated_at`, so a read
round-trips into a write. REST-only; on the gRPC transport the calls raise
the client's usual unsupported-transport error. The full reference,
precedence against a runbook's own `maxTokens`, and the ranges are in
[server/docs/tokenbudgets.md](../../../server/docs/tokenbudgets.md).

| Client | Read | Replace |
|---|---|---|
| Rust | `max_tokens()` | `replace_max_tokens(&MaxTokensBudgets)` |
| Python | `max_tokens()` | `replace_max_tokens(budgets)` — sync and async |
| .NET | `GetMaxTokensAsync()` | `ReplaceMaxTokensAsync(MaxTokensBudgets)` |
| Java | `maxTokens()` | `replaceMaxTokens(MaxTokensBudgets)` — sync and async |

All four hang the pair on the providers plane, beside `apply`/`list`. The
usual pattern is read, change one number, write the whole object back —
every field, every time; the read's `source` and `updated_at` never reach
the wire on the write, so the response's budgets round-trip:

```rust
let now = client.providers.max_tokens().await?;               // MaxTokensResponse
let mut budgets = now.budgets;                                // MaxTokensBudgets (the server DTO)
budgets.turn_completion = 4096;
client.providers.replace_max_tokens(&budgets).await?;
```

```python
now = client.providers.max_tokens()                # MaxTokensResponse ⊂ MaxTokensBudgets
client.providers.replace_max_tokens(now.model_copy(update={"turn_completion": 4096}))
```

```csharp
var now = await client.Providers.GetMaxTokensAsync();
await client.Providers.ReplaceMaxTokensAsync(now.ToBudgets() with { TurnCompletion = 4096 });
```

```java
var now = client.providers.maxTokens();
client.providers.replaceMaxTokens(now.budgets().withTurnCompletion(4096));
```

A body missing a field is refused client-side where the language can
(Python's model validation, .NET's `required` members, Java's primitive
record) and by the server's 400 `invalid-input` everywhere.
