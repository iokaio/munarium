# Ollama provider

Server 1.1 supports local Ollama chat completions and embeddings through the
existing REST and gRPC provider APIs. Models run in a separate Ollama service;
Munarium does not download models or include their weights in its image.

Use Server **1.1.1** for the examples. Its session-turn fix applies an allowed
model override to both query expansion and answer generation; 1.1.0 always used
the runbook default for expansion. See the [changelog](../../CHANGELOG.md#111).

For Docker Desktop on Windows, start with the
[isolated Compose evaluation](../../deploy/ollama/README.md). It supplies pinned
Ollama and PostgreSQL images, persistent model/database volumes, CPU limits, and
repeatable model checks. The example uses Qwen3 `qwen3:1.7b` for completion and
`all-minilm:22m` for embeddings. These small models are evaluation choices; measure
your own corpus and answer quality before selecting a model for an application.

## Register and inspect

Start the Compose Server profile, then run these PowerShell commands from the
repository root:

```powershell
$base = 'http://127.0.0.1:28080'
$headers = @{ Authorization = 'Bearer ollama-evaluation-token'; 'X-Munarium-Uid' = 'evaluator' }
$yaml = Get-Content server/runbooks/providers/example-ollama.yaml -Raw
Invoke-RestMethod "$base/v1/providers" -Method Post -Headers $headers -ContentType text/yaml -Body $yaml
Invoke-RestMethod "$base/v1/providers" -Headers $headers
Invoke-RestMethod "$base/v1/providers/example-ollama/health" -Headers $headers
```

The configuration is:

```yaml
apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata: { name: example-ollama }
spec:
  provider: ollama
  endpoint: http://ollama:11434
  models:
    complete: [qwen3:1.7b]
    embed: [all-minilm:22m]
    fast: qwen3:1.7b
    capable: qwen3:1.7b
  budgets: { rpm: 60, tpm: 100000 }
```

An endpoint is required. It is an HTTP(S) base URL, optionally including a reverse
proxy path, without an embedded username, password, query or fragment. The adapter
appends `/api/chat`, `/api/embed` and `/api/tags`; do not append `/v1` for OpenAI
compatibility. A Server container in the example network uses `http://ollama:11434`.
A Server process running directly on Windows uses `http://127.0.0.1:11434`.

Omit `credentialRef` for the local service. For a proxy requiring a bearer token,
use `credentialRef: { env: MUNARIUM_OLLAMA_PROXY_KEY }` or a `{ file: /path }`
reference. The key resolves on every outbound request; an unresolved or empty
configured key fails the call. Anthropic, OpenAI and OpenRouter still require
credentials. API authentication to Munarium is separate and remains required.

Provider inventory's `credential_ok` is true when credentials resolve **or** when
the Ollama config needs none; it does not prove reachability. Named provider
health checks reach Ollama and verify that the configured model and tier names
are installed. They do not run inference. `/healthai` remains the diagnostic for
the nine built-in cloud models and does not probe tenant-applied Ollama configs.

## Complete and embed

```powershell
$body = @{ prompt = 'The project code is MAPLE. What is the project code? Reply with only the code.'; max_tokens = 64; temperature = 0 } | ConvertTo-Json
Invoke-RestMethod "$base/v1/providers/example-ollama/complete" -Method Post -Headers $headers -ContentType application/json -Body $body
$body = @{ inputs = @('The project code is MAPLE.', 'The curator is Mira Chen.') } | ConvertTo-Json
Invoke-RestMethod "$base/v1/providers/example-ollama/embed" -Method Post -Headers $headers -ContentType application/json -Body $body
```

Without a model or tier, completion selects the first configured complete model;
embedding selects the first configured embed model. Explicit model names take
precedence. Ollama has no built-in tier table: configure each tier you use. The
example maps `fast` and `capable` to the same small model, which is a routing
choice, not a quality rating. An unconfigured `frontier` tier is rejected.

To select a family explicitly, send `provider: ollama` to
`/v1/providers/default/complete` or `/v1/providers/default/embed`. This selects the
first usable applied Ollama config for the tenant, sorted by name. No Ollama
default is synthesized. Automatic `default` selection without a family override
retains the existing Anthropic → OpenAI → OpenRouter order. Prefer a named config
when an application must always use a particular local endpoint.

The equivalent gRPC operations are `ProviderService.ApplyProviderConfig`,
`ProviderHealth`, `Complete` and `Embed`; set `config_name` to `example-ollama` or
use `config_name: default` with `provider: ollama`. Existing wire fields and MMP
major version remain unchanged. Add `version_id` to completion/embedding requests
to record invocation provenance in that lineage.

The adapter sends `stream: false` and `think: false`. It returns answer content,
input/output token counts, and a normalized stop reason; `length` retains the
existing session truncation retry behavior. `max_tokens` becomes `num_predict`.
Tools are unsupported by this text completion contract; Ollama adapter tool requests and gRPC `tools_json` requests
are rejected. Cloud reasoning/tool APIs and Ollama Cloud are outside this feature.

Embedding batches preserve input order and require nonempty vectors of consistent
dimension with finite values. Oversized inputs fail (`truncate: false`); split
documents appropriately for the chosen model. MiniLM's context is 512 tokens and
its vectors have 384 dimensions. Repeated requests use Munarium's tenant-scoped
embedding cache; provider family, endpoint, model and inputs participate in its
identity. Pin model digests operationally: changing weights behind the same model
tag requires clearing the process cache or using a new model name.

## Use a runbook

Add a named model policy to your runbook, then create a session for that runbook
version and request a turn with `complete: true`:

```yaml
  models:
    default: { provider: example-ollama, tier: fast }
    tasks:
      completion: { provider: example-ollama, tier: fast }
```

Keep a completion prompt with `{context}` and `{query}` and explicit output/context
limits, as in [getting started](getting-started.md). The local integration script
tests ingestion, index construction, approved cutover, retrieval provenance and
an Ollama-generated answer against two synthetic documents.

**Index construction still uses Munarium's existing local embedder.** Configuring
an Ollama embedding model enables the provider embedding API; it does not replace
the index builder's embedder. This pre-existing runbook limitation remains visible
as `models.embedding-not-consumed` when an embedding task is declared.

## Troubleshooting and limits

| Symptom | Check |
|---|---|
| Missing-model health or completion error | Pull the exact model tag in Ollama; Munarium never pulls automatically. |
| Connection refused | Check the Server's network context, endpoint and Ollama container health. |
| Slow first request | Allow for cold loading; lower concurrent requests or choose a smaller model. |
| Output stops early | Inspect `stop_reason` and increase the appropriate output budget. |
| Oversized embedding input | Reduce chunk size for the embedding model; input is not silently truncated. |
| No default provider configured | Use the named Ollama config or explicitly select `provider: ollama`. |
| 429 response | Inspect configured Munarium budgets and upstream capacity. |

Requests retain the gateway's 10-second connection timeout and 300-second
per-attempt timeout, with at most two retries for 429/5xx and bounded Retry-After
waits. A failed call can span multiple attempts. Remote error bodies are not
relayed by the Ollama adapter because they can contain prompt or credential text.
Keep the unauthenticated local Ollama port on loopback or a private container
network; configure a proxy and TLS when remote access is needed.

Server 1.1 needs no database migration for this provider. The upgrade rehearsal
preserves existing facts and cloud-provider configurations. Server 1.0 cannot use
Ollama configurations: before rolling an application back, route its runbooks to
a provider supported by 1.0 and retain a database backup.

See [Ollama chat](https://docs.ollama.com/api/chat) and
[embedding](https://docs.ollama.com/api/embed) API documentation for the upstream
request formats, and [managing secrets](managing-key-and-secrets.md) for Munarium's
credential references.
