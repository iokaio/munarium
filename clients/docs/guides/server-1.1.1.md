# Using the clients with Server 1.1.1

The Rust, Python, C#/.NET, and Java Server clients target **Munarium Server 1.1.1** and retain the Server 1.0 minor baseline. Client packages remain version **1.0.0** and the MMP wire major remains **1**. Matrix clients retain their independent Matrix 1.0 compatibility.

## What changed

Server 1.1.0 added Ollama to existing provider operations; provider names are strings in every client, and configuration is passed as YAML. Server 1.1.1 corrected session routing without adding request fields: an allowed model override controls both model query expansion and completion. No client-side second provider call is needed. Without an override, the tasks keep their separate runbook defaults.

| Feature | Minimum Server | Transport |
|---|---|---|
| Existing ledger, retrieval, and cloud-provider operations | 1.0 | REST and the documented gRPC equivalents |
| Ollama apply, named health, complete, embed | 1.1.0 | REST and gRPC |
| Override applies to expansion and completion; pre-expansion rejection | 1.1.1 | REST unary/streaming and gRPC unary sessions |
| Provider inventory (`list`) and cloud-default live probes (`health_ai`) | 1.0 | REST only |
| Session progress events | 1.0 | REST SSE only; phase progress, not generated-token streaming |

See the [Server changelog](../../../server/CHANGELOG.md) and [client transport gaps](../../README.md#known-transport-gaps-honest-typed-documented). New Server behavior is unavailable on the older baseline; the client does not emulate it. Rolling back to 1.1.0 restores completion-only override routing. Rolling back to 1.0 also requires replacing Ollama-dependent configurations.

## Build and connect

Use a complete, pinned repository checkout. Follow the source installation instructions for [Python](../../python/README.md), [.NET](../../dotnet/README.md), [Java](../../java/README.md), or [Rust](../../rust/README.md). Python uses the committed protobuf stubs; .NET and Java generate from the sibling Server protos; Rust uses the sibling wire crates, with dependency requirements at 1.1.1. The client package version is not the Server version.

Check the deployment's REST `GET /version` (or the client's `server_version` equivalent) before relying on 1.1.1 behavior. The response must identify `munarium-server` and version `1.1.1` for an exact release qualification. That meta endpoint is REST-only even when application traffic uses gRPC. Use a fixed image tag or digest for a repeatable deployment.

Every authenticated client needs a token and uid. For capability tokens, uid must equal the token's subject. Provider configuration belongs to a trusted writer; a desktop or other end-user application should receive a scoped query capability instead. Direct Server gRPC is plaintext in this release; use an appropriately configured TLS proxy for remote traffic.

## Configure a local provider

Apply this YAML through the existing provider configuration method. `endpoint` must be reachable **from Server**, not just from the client process. This example assumes Server and Ollama share a container network. Models must already be installed in Ollama.

```yaml
apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata: { name: local-ollama }
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

Local Ollama may omit `credentialRef`; an authenticated proxy can use an environment/file secret reference resolved by Server. An explicitly configured but unresolved secret fails closed. Cloud providers still require credentials, and all calls to Munarium still require normal API authentication. Use the native Ollama base endpoint, not an OpenAI-compatible `/v1` URL.

| Client | Apply YAML | Named health | Named completion |
|---|---|---|---|
| Python | `client.providers.apply_config(yaml)` | `client.providers.health("local-ollama")` | `client.providers.complete("local-ollama", prompt="Say OK.", tier="fast")` |
| C# | `await client.Providers.ApplyConfigAsync(yaml)` | `await client.Providers.HealthAsync("local-ollama")` | `await client.Providers.CompleteAsync("local-ollama", "Say OK.", tier: "fast")` |
| Java | `client.providers.applyConfig(yaml)` | `client.providers.health("local-ollama")` | `client.providers.complete("local-ollama", options)` with `Params.CompleteOptions` |
| Rust | `client.providers.apply_config(yaml).await?` | `client.providers.health("local-ollama").await?` | `client.providers.complete("local-ollama", request).await?` with `dto::CompleteRequest` |

The existing [provider guide](providers.md) shows request construction in all four languages. Python and Java also expose their async equivalents. Completion and embedding return the actual provider/model, usage, and optional invocation provenance using the existing response types.

Named health checks verify Ollama connectivity and installed model names without inference. Inventory's `credential_ok: true` means credentials resolve **or are unnecessary**; it is not a reachability or model-quality check. `/healthai` probes nine cloud-default models, spends tokens, and does not test named Ollama configurations.

Prefer a named configuration for a specific endpoint. For direct provider calls, `name="default", provider="ollama"` selects the first usable applied Ollama configuration in name order. Automatic selection without an explicit family remains Anthropic → OpenAI → OpenRouter. Ollama has no synthesized default or built-in tier model table: configure every tier used, including `frontier` if requested. An explicit model takes precedence over a tier.

Provider embeddings preserve input order and report dimension and cache status. Index builds still use the existing local embedder; applying an Ollama embedding model does not switch the index builder. Tools are unsupported by this completion contract. See the [Server Ollama guide](../../../server/docs/guides/ollama.md) for configuration limits.

## Session overrides and cost

Allow the named configuration in the runbook's `models.allowOverrides`, then send the existing override with `complete: true`:

```python
turn = client.sessions.turn(
    session.session_id,
    query="Which procedure applies?",
    complete=True,
    model_override={"provider": "local-ollama", "tier": "capable"},
)
```

The session override's `provider` is a configured provider reference; the direct `providers.complete("default", provider="ollama")` argument is a family selector. Do not confuse the named configuration with its family.

For C#, use `TurnRequest.ModelOverride`; Java uses `Params.TurnOptions` with `SessionsApi.ModelOverride`; Rust uses `dto::TurnRequest.model_override`. The [sessions guide](sessions.md#model-overrides--honored-or-refused-never-downgraded) has the language examples. One override reaches both expansion and completion on Server 1.1.1. Their token budgets remain separate. A higher tier may increase both steps' latency and cost.

Invalid/disallowed overrides are rejected before expansion spends provider tokens, even if expansion is optional. Retrieval-only turns reject a nonempty override. An empty override does not select a different model. `complete: false` alone does not guarantee zero model calls: runbook-configured model query expansion may still run.

For streaming turns, inspect expansion's provider, model, lexical terms, and usage, then completion's provider/model and `was_override`. The SDKs preserve these fields. A failed or interrupted turn must not be blindly retried: it may still be running. Read the stored session transcript and keep uncertain work pending. Provider complete/embed calls also retain their existing send-once policy.

## Qualification

Run the existing client unit and conformance suites against an isolated Server 1.1.1 with PostgreSQL, an rw token and mgmt token for the same test tenant, and a token-signing secret. The language READMEs list the commands; `MUNARIUM_REST_URL`, `MUNARIUM_GRPC_URL`, `MUNARIUM_TOKEN`, and `MUNARIUM_MGMT_TOKEN` select that instance. The version check must precede the tests. Skipped live suites are not evidence of compatibility.

Run `python clients/check_compatibility.py` and `python clients/check_license.py` from the repository root. The compatibility check validates the exact target syntax, the Server N/N-1 minor ranges, Matrix's separate scope, and all seven package manifests. Run Python protobuf regeneration and inspect its diff; only client-generated files may change. Existing .NET/Java builds generate their own stubs, and Rust compiles against the Server wire types without modifying Server source.

Release-specific checks should exercise credential-free named Ollama configuration, named health and inventory, completion and embedding, model overrides on unary and streaming turns, and rejection of disallowed or retrieval-only overrides. Use a controlled Ollama-protocol fixture to verify routing and token/provenance handling without paid calls or model downloads. Such a fixture proves protocol behavior; evaluate real model quality separately.

The [release qualification suite](../../python/conformance/test_server_111.py) runs these cases through Python's sync/async REST/gRPC clients and checks streaming progress over REST. Its [Compose stack](../../python/conformance/compose.server-111.yaml) contains Server 1.1.1, disposable PostgreSQL, and a small Ollama-protocol fixture. Run from the repository root in PowerShell after installing the Python client's development dependencies:

```powershell
docker compose -p clients111-qualification -f clients/python/conformance/compose.server-111.yaml up -d
# Wait for Server to start, then verify the exact release before testing.
Invoke-RestMethod http://127.0.0.1:38080/version
$env:MUNARIUM_REST_URL = 'http://127.0.0.1:38080'
$env:MUNARIUM_GRPC_URL = '127.0.0.1:35051'
$env:MUNARIUM_TOKEN = 'citoken'
$env:MUNARIUM_MGMT_TOKEN = 'cimgmt'
$env:MUNARIUM_OLLAMA_FIXTURE_URL = 'http://127.0.0.1:38114'
$env:MUNARIUM_OLLAMA_SERVER_ENDPOINT = 'http://ollama:11434'
python -m pytest clients/python/conformance/test_server_111.py -q
```

Run each language's normal conformance suite serially against this same disposable stack. For .NET, use `http://127.0.0.1:35051` as its gRPC URL. The fixture suite is opt-in and skips when its endpoint variables are absent; ordinary client CI continues to run without an Ollama service. After qualification, `docker compose -p clients111-qualification -f clients/python/conformance/compose.server-111.yaml down -v` removes this test stack and its database volume. Use these fixture credentials only for the loopback-bound test stack.

### Recorded validation — 2026-09-10

Validated the client working tree based on repository commit `9569542d370227be5d0cc0dbe5df685e2f55824f` against the local `iokaio/munarium:1.1.1` image. `GET /version` reported `munarium-server` version `1.1.1`. All four existing conformance suites ran against isolated PostgreSQL-backed Server containers; the final Python run also exercised the supplied Compose stack and controlled provider fixture.

| Client | Unit/build checks | Live Server 1.1.1 checks |
|---|---|---|
| Python | 126 unit tests; Ruff lint/format and strict source mypy passed; protobuf regeneration produced no diff | 49 passed, including 9 release-specific tests; 4 documented chronology skips |
| C#/.NET | 66 unit tests; build passed with zero warnings/errors | 16 passed; 2 documented chronology skips |
| Java | 37 unit tests; generated-proto compilation passed | 26 passed; 1 documented chronology skip |
| Rust | 39 unit tests and 1 doc test; examples built; format and Clippy passed | 14 wire scenario passes across REST/gRPC, 15 plane smokes, and 10 platform smokes |

Compatibility, license, dependency-notice, and local documentation-link checks passed. The additional full .NET formatter check reports existing whitespace findings in method and test code; the .NET edits in this alignment are API documentation comments. The existing chronology skips remain explicit rather than counted as passes. This qualification did not rerun the Server 1.0 baseline or evaluate real Ollama model quality. Server and Matrix source files were unchanged, and the temporary containers were removed.
