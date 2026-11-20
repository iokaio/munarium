# MCP client qualification receipts

**Status: pending.** No real-client transcript has been recorded for the MCP
endpoint yet. ClearView A3 requires one real, versioned client run for each
protocol revision, `2026-07-28` (A3-T2) and `2025-11-25` (A3-T3), against the
exact release candidate, and §10.5 of the [MCP analysis](../design/mcp-analysis.md#105-interoperability-record)
lists what each run must attach. Until both are recorded the endpoint stays
unqualified, whatever the automated suite reports. This page is the
procedure; the results belong in the release evidence, not here.

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
clients through a pinned relay — are all **untested**. Mark each as qualified,
incompatible or blocked, with the reason, only after a run on a named version.
