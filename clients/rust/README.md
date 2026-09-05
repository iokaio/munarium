# munarium-client (Rust)

Official Rust client for munarium-server: the full ten-plane surface
(`commands`, `query`, `ingest`, `retrieval`, `runbooks`, `providers`,
`sessions`, `tokens`, `reports`, `authoring`) on both transports. Async
(tokio), typed errors, the head-conflict write loop built in. See the
[clients front door](../README.md) for the invariants, the transport-gap
ledger, and guides.

## Install

```toml
[dependencies]
munarium-client = "1.0"
```

Feature flags: `rest` (reqwest + rustls) and `grpc` (tonic) — both on by
default; disable one to drop its dependency tree.

## Use

```rust
use munarium_client::{dto, ClaimOutcome, MunariumClient, MunariumClientOptions, WriteLoopOptions};

// REST (:8080 demo posture / :443 behind gateways) …
let client = MunariumClient::rest(
    MunariumClientOptions::new("http://127.0.0.1:8080").token("devtoken").uid("user-1"))?;
// … or direct gRPC (:50051; https:// enables TLS)
let client = MunariumClient::grpc(
    MunariumClientOptions::new("http://127.0.0.1:50051").token("devtoken").uid("user-1")).await?;

let v = client.commands.create_version(Default::default(), None).await?;

// The write loop: expected_head + fresh idempotency key per attempt.
let outcome = client.propose_claim_with_retry(&v.version_id, |_head| {
    dto::ProposeClaimRequest {
        claim_type: dto::ClaimTypeDto::Fact,
        subject: "hero".into(), key: "eyes".into(), value: "green".into(),
        expected_head: None, scope_path: None, provenance: None,
        supersedes_id: None, entity_id: None, evidence: None,
        confidence: None, shape_ref: None,
    }
}, WriteLoopOptions::default()).await?;

// Disputed is SUCCESS — the governance record, not an error.
if outcome.is_disputed() {
    for f in outcome.findings() { eprintln!("{}: {}", f.rule_id, f.message); }
}
```

`uid` is the acting end-user id (audit attribution), required by the server's
default posture (`MUNARIUM_REQUIRE_UID=true`) — omit it and every call draws the
typed `uid-required` error.

The platform surface is idiomatic Rust: `sessions.turn_stream(...)` returns
an `impl Stream<Item = Result<TurnStreamEvent>>` over the SSE turn (progress
events, then exactly one `Done`; no overall deadline, a 60 s idle watchdog
over the server's 15 s heartbeats), unary turns are deadline-exempt and never
auto-retried (they spend provider tokens a client abort cannot stop), bulk
upload sessions ride `ingest.bulk_open/bulk_chunk/bulk_complete`, and
`tokens`/`reports`/`authoring` cover the management plane (mint with a
`devmgmt`-role bearer).

The per-call output-token budgets ride the providers plane:
`providers.max_tokens()` reads the tenant's effective set and where it comes
from (`source: tenant | environment`), and
`providers.replace_max_tokens(&dto::MaxTokensBudgets { .. })` replaces the
WHOLE set — all eight fields, range-checked, static rw role — answering with
the same flattened shape, so `resp.budgets` is a ready-made replacement body.
Both are REST-only (`/v1/max-tokens`; gRPC answers the typed `Unsupported`).

Naming a `research_profile` on a turn runs it through an evidence hierarchy
and fills `TurnResponse.hierarchy` with the decision — which layers ran, which
refused, and whether a completeness claim was permissible at all — while the
SSE stream gains the `profile`/`layer_*`/`coverage`/`compose` stages. Leaving
it `None` is byte-identical to a pre-hierarchy turn in both directions, which
is why the field is optional on the wire rather than defaulted. On the
management side `reports.evidence(window)` aggregates how those layers
behaved and `reports.matrix()` reports whether the structured-evidence plane
is even wired — `configured: false` is not the same fact as a tripped
breaker, so read it first.

Errors are `MunariumError` variants keyed on the problem-slug registry;
`HeadConflict` carries `expected`/`actual`, `PolicyRejection` carries the
findings (+ truncation markers on gRPC), `RunLocked` is typed but NOT
transient (a run lock lasts minutes — pace like `RateLimited`), and
`RateLimited` carries `retry_after` when the server sends a `Retry-After`
header (it does not today, so the hint is null in practice). `Unsupported`
marks the documented gRPC gaps.

Retry classes are decided by `MunariumError::is_transient` (reads) and
`MunariumError::is_command_retry_safe` (commands): a command re-sends its
SAME idempotency key only for a `Transport` error with
`may_have_reached_server: false` (a REST connect-phase failure) or the typed
`Overloaded` (shed before executing). A gateway 502/504 (`Unexpected`) is
transient for reads but never re-sent as a command, and on gRPC no
transport failure is provably undelivered, so `rpc_command` re-sends only
on `Overloaded`. Two documented gRPC facts: `ingest(...)` mirrors REST
`POST /v1/ingest` (a locally undecodable base64 body is `InvalidInput`; a
server-side per-item error on that one file is `Unexpected` carrying the
text — the wire has no slug), and an explicit `collections: []` /
`runbook_refs: []` is refused as `InvalidInput` like the zero sentinels
(proto3 cannot carry "explicitly empty"; use `None` or REST).

## Examples

```
cargo run --example write_loop            # disputed-is-success + retry loop
cargo run --example pins                  # one pin bounds all stores
cargo run --example ingest_stream         # replayable chunked upload
cargo run --example retrieval_envelope    # search + render the envelope
cargo run --example runbook_approve       # run -> awaiting_approval -> done
cargo run --example session_turn          # streaming turn: progress -> done
cargo run --example bulk_upload           # manifest -> needed -> verify (re-run = 0 bytes)
```

All read `MUNARIUM_REST_URL` / `MUNARIUM_TOKEN` from the environment.

## Conformance

`munarium-client-conformance` runs the seven wire scenarios of
`SCENARIOS.md`,
written against the client API (the eighth scenario,
`gates.chronology-certain-only`, is kernel-only and has no wire form, so no
client carries it) on both transports, plus 15 plane smokes and
— given a mgmt-role token — the 10 platform smokes (uid contract, role
partition, sessions + SSE ordering, bulk lifecycle, reports + revoke,
authoring lifecycle, gRPC surface):

```
cargo run -p munarium-client-conformance -- \
  --rest http://127.0.0.1:18080 --grpc http://127.0.0.1:15051 \
  --token devtoken --mgmt-token devmgmt --smoke
```

## Formatting: use `-p`, not `--all`

This workspace path-depends on the server's two wire crates under
`../../server/src/`, and **cargo-fmt follows path dependencies**. So from
`clients/rust`:

```
cargo fmt -p munarium-client -p munarium-client-conformance          # this client
cargo fmt --all                                                       # ALSO rewrites the server's wire crates
```

The `--check` form is read-only and harmless either way, but the bare `--all`
is a write into the server tree. CI runs the package-scoped form for the same
reason.
