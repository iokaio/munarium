# MCP client qualification receipts

The official TypeScript SDKs passed both protocol revisions on a local
ClearView build. See the [implementation ledger](../design/implementation-ledger.md#21-november-local-qualification-rehearsals).
ClearView A3 still requires one real, versioned client run for each protocol
revision, `2026-07-28` (A3-T2) and `2025-11-25` (A3-T3), on the exact release
candidate. [Interoperability cases](../design/mcp-analysis.md#105-interoperability-record)

## Before a run

1. Build or pull the candidate and record its Server revision and artifact
   digest.
2. Start it against an isolated, disposable PostgreSQL with
   `MUNARIUM_MCP=enabled`, `MUNARIUM_TOKEN_SECRET` set,
   `MUNARIUM_TOKEN_REVOCATION_CHECK=true` and a short `MUNARIUM_TOKEN_TTL_SECS`
   (for example 300). Use loopback bindings and test credentials only.
3. Load a fictional collection (for example the getting-started corpus) and a
   runbook over it. For the generating tools, configure a synthetic or local
   provider and a generation policy with small allowances; never a paid
   production credential.
4. Issue tokens through `POST /v1/access-tokens` as a broker would: one with
   `["mcp", "query"]`, one with `["mcp"]` only (for the denied call), and one
   short-lived token for the expiry step.
5. Record the client name and exact version, the relay and its version when a
   stdio relay is used, and the configuration file with the bearer replaced by
   a placeholder.

## The sequence (each revision)

| Step | Modern client (`2026-07-28`) | Legacy client (`2025-11-25`) |
|---|---|---|
| Connect | Authenticated `server/discover`; check `supportedVersions` and the `tools` capability | `initialize`; check the returned `protocolVersion`, then `notifications/initialized` |
| List | `tools/list`; the nine query tools are listed | same, under the negotiated revision |
| Call | `list_collections`, then `search_collection` with a client-kept `operation_key`; check the expected synthetic hit | same |
| Denied call | With the `["mcp"]` token, a direct `search_collection`: a tool error `scope-missing`, nothing dispatched | same |
| Lost reply | Repeat `open_session` with the same key after discarding the first reply; then the key alone: the receipt names the same session, no second session exists | same |
| Expiry and renewal | Let the short token expire: 401 with `WWW-Authenticate: Bearer`; renew through the broker and reconnect; no interrupted call is replayed | same |
| Size bound | Call `get_session` with a page that exceeds the bound (lower `MUNARIUM_MCP_RESULT_MAX_BYTES` for the run): a `result-too-large` tool error naming a smaller limit | same |

A client that supports both revisions may supply both runs.

### Rehearse with the official TypeScript SDKs

From `server/`, use the [compatibility harness](../design/compat-local-receipts.md#harness)
with `up`, `seed`, `fork` and `mcp` and a local candidate image. The `mcp`
step enables MCP, checks revocation and sets a 16 KiB response bound. Install
the versioned SDKs in separate temporary directories and run the driver against
the loopback candidate. Keep both output transcripts under ignored scratch.

```powershell
npm install --prefix <modern-dir> --ignore-scripts --no-audit @modelcontextprotocol/client@2.4.0
npm install --prefix <legacy-dir> --ignore-scripts --no-audit @modelcontextprotocol/sdk@1.32.2
$env:MUNARIUM_COMPAT_MCP_MGMT = '<disposable management token from the harness>'
node tools/compat/mcp_sdk.mjs modern <modern-dir>
node tools/compat/mcp_sdk.mjs legacy <legacy-dir>
```

The driver logs wire methods, negotiated versions, result and refusal outcomes,
and the operator view's status. It drops one successful reply at the HTTP
transport boundary and checks that replay and renewed credentials recover the
same operation and session. Confirm the one stored session per client UID in the
disposable database. Run the harness's `down` step after collecting evidence.

## What to attach

For each client and revision: the Server revision and digest, client name and
version, observed wire revision, transport, authentication mode, the redacted
transcript of the successful sequence, the denied call, the expiry and
renewal, the lost-reply recovery, the operator-visible error and result size,
and the `/admin/mcp` view after the run. Redact bearer values, hostnames and
any identifier that is not fictional. Published client support is limited to
the exact combinations recorded.

## Candidates

The clients surveyed in the design (§10.2–§10.4) — Claude Code, Cursor,
VS Code, Gemini CLI, Codex CLI, Claude Desktop and hosted connectors, and stdio
clients through a pinned relay — have no recorded run. Record any additional
client's exact version and observed outcome.
