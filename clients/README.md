# munarium client libraries

Official clients for [Munarium Server](../server/) — the governed-memory
service. One plane surface, two transports, four languages, all proven by the
server's own conformance scenarios; and three clients for [Munarium Matrix](../matrix/)
under `matrix-python/`, `matrix-dotnet/` and `matrix-java/`.

New to Munarium? [`docs/concepts/`](docs/concepts/) explains the ideas — the fact ledger,
sessions and turns, runbooks as the unit of access, capability tokens, evidence, and the
conformance scenarios read as an executable specification — independent of any one language.
See ["Trying it"](#trying-it) below for how to see it answer real questions.

**License: Apache-2.0** ([LICENSE](LICENSE), [NOTICE](NOTICE)) — the libraries, the guides
and the concept pages alike, the same license as the server and Matrix they talk to.
Contributing is a signed-off pull request with no CLA ([CONTRIBUTING.md](../CONTRIBUTING.md));
suspected vulnerabilities go to the private channel in [SECURITY.md](../SECURITY.md); what is
and is not supported is in [SUPPORT.md](../SUPPORT.md); conduct is the Contributor Covenant.

| | Rust | Python | .NET | Java |
|---|---|---|---|---|
| Package | [`munarium-client`](rust/) (crate) | [`munarium-client`](python/) (import `munarium_client`) | [`Ioka.Munarium.Client`](dotnet/) (net10.0) | [`io.ioka.munarium:munarium-client`](java/) (Java 21 bytecode) |
| Transports | REST + gRPC | REST + gRPC | REST + gRPC | REST + gRPC |
| Sync/async | async (tokio) | sync **and** async | async | sync **and** async (async = virtual-thread offload) |
| Models | `munarium-api-types`, the server's wire-type crate (a path dependency on `server/src`, and the only server crate in the graph besides `munarium-proto`) | pydantic v2 | System.Text.Json source-gen records | Jackson records |
| gRPC stubs | `munarium-proto`, the server's generated proto crate | committed (`scripts/gen_protos.py` over `server/proto`) | Grpc.Tools at build time over `server/proto` | Gradle build-time codegen over `server/proto` |
| Conformance | the 7 wire scenarios of [`server/conformance/SCENARIOS.md`](../server/conformance/SCENARIOS.md), client-native, + 15 plane smokes + 10 platform smokes | 7 ported scenarios × 4 variants (6 exercised, chronology skipped) + 11 platform tests | 7 ported scenarios × 2 transports (6 exercised, chronology skipped) + 10 platform scenarios | 7 ported scenarios × 2 transports + async round-trips + 10 platform smokes (1 documented skip) |

**The contract the clients build from is the server's own**: the ten protos under [`server/proto/mmp/v1/`](../server/proto/mmp/v1/), the REST reference and problem-slug registry under [`server/docs/api/`](../server/docs/api/), and the two Rust wire crates under `server/src/`. A wire change on the server side reaches every client immediately, and `clients-ci` proves them against a server built from the same commit.

## About this repository

The Munarium client libraries begin here, at version 1.0.0. Its design was worked out over an extended period of
private research and development — experiments, measurements, superseded designs, and the
operational records of the environments they ran in — and that history is deliberately not carried
into this repository.

It is omitted because it documents how the design was reached rather than how the software behaves,
and it would give an evaluator, an operator or a contributor nothing they need. What that work
produced is here in full: the implementation, its conformance suite, its API documentation and its
deployment assets. The conformance scenarios are the executable specification, and they are the
record worth reading.

## Compatibility

**[`compatibility.json`](compatibility.json) is the authoritative compatibility record.**
Each Clients minor release supports the current Server
minor and the one before it (N and N-1); a breaking MMP wire change bumps the contract major, and
Server serves both majors for one Server minor release so Clients can move without a flag day;
Clients version independently of Server, so a shared number on a given release (as with this
first one, `1.0.0` on both sides) is a coincidence of that release, not a rule going forward.
`clients/check_compatibility.py` fails CI if `compatibility.json`'s recorded version for a
language ever drifts from what that language's own manifest declares.


All four expose the same **ten planes** and encode the same invariants. The
original six — `commands`, `query`, `ingest`, `retrieval`, `runbooks`,
`providers` — grew with the server's platform surface, and four planes
joined them:

- **`sessions`** — multiturn retrieval sessions over a runbook's
  access-permitted collections, including the SSE streaming turn.
- **`tokens`** — mint/audit/revoke the short-lived end-user capability JWTs
  (mgmt role; not the bearer the client itself authenticates with).
- **`reports`** — nine management reports over the interactions audit
  trail, plus the evidence-hierarchy and Matrix-plane views
  (mgmt role, REST-only).
- **`authoring`** — guided runbook authoring: pattern catalog, drafts,
  validation, AI assist, hash-manifested export, apply (REST-only).

**Per-call token budgets.** Every client carries the REST-only pair over the
server's `GET`/`POST /v1/max-tokens` (read the eight per-call `max_tokens`
ceilings; replace them as a whole — no partial update): Rust `max_tokens` /
`replace_max_tokens`, Python the same on both surfaces, .NET `GetMaxTokensAsync`
/ `ReplaceMaxTokensAsync`, Java `maxTokens` / `replaceMaxTokens` on both planes.
Guide: [docs/guides/providers.md](docs/guides/providers.md); server reference:
[server/docs/tokenbudgets.md](../server/docs/tokenbudgets.md).

**The evidence read plane.** Every client carries an
**evidence read plane** — `evidence.get(id)` for the manifest and
`evidence.rows(id, from, limit)` for a bounded, audited window over the sealed
rows. REST-only in v1; the gRPC transports raise the typed `Unsupported`
error rather than pretending.

**Sealing is deliberately absent from all four.** An artifact's manifest is a
statement about work the *sealer* did, and an SDK offering `seal_evidence`
would invite an application to assert provenance it cannot vouch for. What an
application legitimately needs is the other direction: an answer cites
`[evidence/<id>#<row>]`, and the application resolves that citation to show a
reader what the number was computed from. Access is checked per artifact
against the **session's** clearance, not the sealer's — expect
`evidence-forbidden` (403), `evidence-expired` (410, retention purged the
bytes and the citation was real) and `evidence-not-committed` (409).

The manifest comes back as a raw JSON value in every language
(`serde_json::Value`, `dict`, `JsonElement`, `JsonNode`) rather than a
hand-written mirror. It is defined by the cross-tree contract
(`matrix/contract/evidence-manifest.schema.json`) and returned verbatim; a
mirror per language would be four more definitions of a schema these clients
do not own, and the first thing to drift when the contract adds an optional
field.

**Connector origin.** Every client carries the connector
`origin` block on `Claim` and on the propose input (`ClaimOrigin`), on both
transports, and each ports the server's `ledger.origin-round-trips` scenario
(Rust inherits it verbatim). The block is optional and `null`/absent on every
model-extracted claim; a connector — Munarium Matrix — sets it, nothing else
should.

The invariants:

1. **Disputed ≠ error.** A gate-blocked claim returns SUCCESS with
   `is_disputed` + findings (governance records, never drops).
2. **Head conflicts are normal.** Each client ships a
   `propose_claim_with_retry` write loop: re-read → rebuild → retry, fresh
   idempotency key per attempt.
3. **One pin bounds everything.** `as_of_seq` threads through every query;
   digests are rebuilt under a pin, never served stored.
4. **Every retrieval answer carries a ProvenanceEnvelope** — a required,
   non-optional member.
5. **Append-only.** No update/delete methods against ledger data;
   corrections name `supersedes_id` explicitly. (The one DELETE on the whole
   surface is `authoring.delete_draft` — workspace cleanup, never canon.)
6. **Idempotency keys** auto-generate per command and are caller-overridable.
   Source uploads take a replayable chunk **source** (a factory, not a
   stream), so the transports retry transient failures for you — safe because
   uploads are idempotent by content address. A one-shot iterator is a typed
   error, not a silent empty upload on the second attempt.
7. **Commands are never auto-retried once the request may have been
   delivered.** The server records an idempotency key only AFTER a command
   completes, so a retry that overtakes an in-flight attempt would execute it
   twice. Reads retry any transient failure; commands re-send the SAME
   idempotency key in exactly two cases — the request provably never left
   (a connect-phase failure on REST: refused, DNS, TLS handshake, connect
   timeout) or the server answered the typed `overloaded` (shed BEFORE
   executing). A transient 502/504 from a gateway is NOT one of them: the
   command may still be executing upstream, so it surfaces to you. On gRPC
   no transport failure is provably undelivered (a failed reconnect and a
   broken established stream both read UNAVAILABLE), so commands there
   re-send only on the typed `overloaded` — never on any transport failure.
   The rule is identical in all four clients. Everything else surfaces to
   you — and a session **turn** is never retried and never deadlined,
   because it spends provider tokens a client-side abort cannot stop.
8. **Governance enums decode fail-closed.** An unknown or unset claim status,
   severity, or provenance on the gRPC wire decodes as the CONSERVATIVE value
   (`disputed` / `block` / `emergent`), so a tag this client build cannot name
   can never read as "the gates passed".
9. **Typed errors keyed on the problem-slug registry**
   (errors.md) on both transports — no
   English message text is ever parsed.

## Quickstarts

Every snippet sets a `uid` — the acting end-user id, stamped into audit
records — because the server **requires one by default**
(`MUNARIUM_REQUIRE_UID=true`): a call without it draws the typed `uid-required`
error, so treat `uid` as part of connecting, not an extra.

**Rust**

```rust
use munarium_client::{MunariumClient, MunariumClientOptions};

let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("user-1"))?;
let v = client.commands.create_version(Default::default(), None).await?;
let head = client.query.head(&v.version_id).await?;
```

**Python**

```python
from munarium_client import MunariumClient, ClientOptions

client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))
v = client.commands.create_version()
outcome = client.commands.propose_claim(v, subject="hero", key="eyes", value="green")
```

**.NET**

```csharp
using Ioka.Munarium.Client;

await using var client = MunariumClient.Rest(new MunariumClientOptions
    { Endpoint = "http://127.0.0.1:8080", Token = "devtoken", Uid = "user-1" });
var v = await client.Commands.CreateVersionAsync();
var head = await client.Query.HeadAsync(v);
```

**Java**

```java
import io.ioka.munarium.client.*;
import io.ioka.munarium.client.model.Ledger;

try (var client = MunariumClient.rest(
        MunariumClientOptions.of("http://127.0.0.1:8080")
                .withToken("devtoken").withUid("user-1"))) {
    String v = client.commands.createVersion();
    var outcome = client.commands.proposeClaim(
            v, Ledger.ClaimInput.fact("hero", "eyes", "green"), null, null);
}
```

## Guides

Per-plane usage guides with snippets in all four languages live under
[docs/guides/](docs/guides/):

- [write-loop.md](docs/guides/write-loop.md) — expected_head, the retry
  helper, idempotency keys, disputed-is-success
- [pins.md](docs/guides/pins.md) — point-in-time reads, one pin bounds all
  stores, supersession
- [ingest.md](docs/guides/ingest.md) — streamed content-addressed uploads +
  ingest events
- [ingest-v2.md](docs/guides/ingest-v2.md) — the file/batch plane,
  collection auto-binding, and bulk upload sessions
- [retrieval.md](docs/guides/retrieval.md) — hybrid search and rendering the
  ProvenanceEnvelope
- [runbooks.md](docs/guides/runbooks.md) — shapes, runbook runs, the approve
  flow
- [sessions.md](docs/guides/sessions.md) — multiturn sessions, streaming
  turns, model overrides, transcripts
- [providers.md](docs/guides/providers.md) — the BYOK provider gateway
- [tokens-and-reports.md](docs/guides/tokens-and-reports.md) — capability
  JWTs, the uid contract, and the nine management reports
- [authoring.md](docs/guides/authoring.md) — guided runbook authoring, from
  pattern to hosted

## Known transport gaps (honest, typed, documented)

The REST contract is 93 paths, and all four libraries carry 79 of them.
**Not yet wrapped, and tracked here so the number is
honest:** the datastore plane's fourteen operator routes (`/v1/index-artifacts/*`,
`/v1/index-build-jobs/*`, `/v1/retrieval-rollout*`,
`/v1/collections/{id}/activate-index` — `mmctl datastore` drives them) and
`GET /v1/reports/budgets`. gRPC serves a
genuine twin for most of it — SessionService (create/turn/get/close),
AdminService's token trio (mint/list/revoke), collections, the runbook v2
surface (list/info/validate/remove), and `IngestFiles` (single + batch, same
per-item outcome contract). What remains REST-only surfaces as a typed
`Unsupported` error on gRPC, never a silent drop:

- **`sessions.turn_stream`** — the SSE streaming turn has no streaming RPC.
- **The four bulk-upload session routes** (open/chunk/status/complete) and
  **`ingest.get_source`**.
- **`query.findings`** (QueryService has no findings RPC).
- **Chronology-rules** put/get on the runbooks plane.
- **`providers.list`** (the free `GET /v1/providers` disclosure) and
  **`health_ai`** (the live six-model default probe).
- **`providers.max_tokens` / `providers.replace_max_tokens`** (`GET`/`POST /v1/max-tokens`, the per-call output-token ceilings read and
  replaced as a whole — an operator setting beside the provider configs; no
  RPC exists).
- **The entire reports plane** (all nine — AdminService.Usage is declared
  but UNIMPLEMENTED, so the clients don't pretend).
- **The entire authoring plane** (no authoring RPCs exist).
- **Index builds** (no BuildIndex RPC).
- **`server_version()`** (`GET /version` is a REST meta route; use server
  reflection on gRPC).

**proto3 zero sentinels**: explicitly-zero `as_of_seq`/`limit`/`top_k`/
`fact_limit`/`budget_tokens`/`max_tokens`/counter `budget`/token `ttl_secs`
and `confidence`/`temperature` of 0.0 cannot ride the gRPC wire and are
rejected with a typed invalid-input error (REST carries them faithfully).
The same applies to an **explicit empty list** where REST and proto3
disagree about what "empty" means: `collections: []` on an ingest file
(REST: bind to nothing; proto3 empty = matcher auto-bind) and
`runbook_refs: []` on a token mint (REST: no runbook allowed; proto3 empty
= any runbook) are refused on gRPC with the typed invalid-input error.
Omit the field (`None`/`null`/`Option::None`) or use REST.

**gRPC ABORTED without details**: the server answers `ABORTED` for both a
head conflict (reason `head-conflict`) and a held run lock (reason
`run-locked`); the clients tell them apart by the `google.rpc.ErrorInfo`
detail in `grpc-status-details-bin`. When an intermediary strips that
detail, an `ABORTED` decodes as a head conflict with `expected`/`actual`
0/0 — a run-lock rejection is then indistinguishable from a head conflict
(the write loop re-reads the head and retries; a held lock keeps
conflicting until it clears). Identical in all four clients; the REST
problem+json body never has this ambiguity.

Server-side note: the server does not emit `Retry-After` today, so the
rate-limit hint (`retry_after`) is null on **both** transports — the
clients read it opportunistically (both the delta-seconds and HTTP-date
forms) and will surface it the moment the server or an intermediary sends
one.

Upload ceiling: `PUT /v1/sources` accepts up to **256 MiB** (the handler
buffers, so the cap is the memory guard) and the gRPC `PutSource` decoder is
raised to match; the chunk helpers frame at 1 MiB so no single message
approaches the gRPC limit. The file/batch/bulk ingest planes cap at **500
files per request** — the clients reject an over-cap list locally, before it
ships a body the server would refuse.

## Development

CI: [clients-ci.yml](../.github/workflows/clients-ci.yml) — lint/type/unit gates per
language, then the full `{rust, python, dotnet, java} × {rest, grpc}` conformance matrix
against a server built from the same commit. The Matrix clients run in
[matrix-ci.yml](../.github/workflows/matrix-ci.yml).

Two stdlib checks run first in CI and take a second locally, all from this directory:
`python3 check_compatibility.py` (`compatibility.json` matches every client manifest) and
`python3 check_license.py` (every manifest says Apache-2.0; every Ioka-authored source
file carries `SPDX-License-Identifier: Apache-2.0` on its first line — protoc output and
Gradle's wrapper are the named exemptions; every `LICENSE` copy is the canonical text and
every `NOTICE` copy matches the root one; nothing under `clients/` names the retired
proprietary identifier). The same script then scans each language's built package in its
CI job — wheel and sdist, nupkg, jar, and `cargo package --list` — for the license files,
the Apache-2.0 metadata, and anything that is not this tree's own: the proof that a
published package carries no server material.

To run conformance locally against a Munarium Server built from [server/](../server/) — REST
on 18080, gRPC on 15051, a static rw token `devtoken` and a mgmt token `devmgmt` for
the same tenant:

```bash

cd clients/rust && cargo run -p munarium-client-conformance -- \
  --rest http://127.0.0.1:18080 --grpc http://127.0.0.1:15051 \
  --token devtoken --mgmt-token devmgmt --smoke
cd clients/python && MUNARIUM_REST_URL=http://127.0.0.1:18080 \
  MUNARIUM_GRPC_URL=127.0.0.1:15051 MUNARIUM_TOKEN=devtoken \
  MUNARIUM_MGMT_TOKEN=devmgmt pytest conformance
cd clients/dotnet && MUNARIUM_REST_URL=http://127.0.0.1:18080 \
  MUNARIUM_GRPC_URL=http://127.0.0.1:15051 MUNARIUM_TOKEN=devtoken \
  MUNARIUM_MGMT_TOKEN=devmgmt dotnet test tests/Ioka.Munarium.Client.Conformance
cd clients/java && MUNARIUM_REST_URL=http://127.0.0.1:18080 \
  MUNARIUM_GRPC_URL=127.0.0.1:15051 MUNARIUM_TOKEN=devtoken \
  MUNARIUM_MGMT_TOKEN=devmgmt ./gradlew conformanceTest
```


## Trying it

Run a server from [server/](../server/) — `docker compose up --build` is the whole
evaluation — and point any client at it. Without a server you can still run every offline
unit test in every language here, and the conformance scenarios against a local server
build (above).

Questions go to GitHub Discussions, defects to Issues, and suspected
vulnerabilities to the private channel [SECURITY.md](../SECURITY.md) names; the support
boundary is [SUPPORT.md](../SUPPORT.md).
