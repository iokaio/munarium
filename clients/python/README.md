# munarium-client (Python)

Official Python client for munarium-server: the full ten-plane surface
(`commands`, `query`, `ingest`, `retrieval`, `runbooks`, `providers`,
`sessions`, `tokens`, `reports`, `authoring`), sync **and** async, both
transports, typed exceptions, the head-conflict write loop built in. See the
[clients front door](../README.md) for the invariants, the transport-gap
ledger, and guides.

Python ≥ 3.11 · fully typed (`py.typed`, mypy --strict clean).

## Install

```bash
pip install munarium-client
# from a checkout: pip install -e ".[dev]" inside python/
```

## Use

```python
from munarium_client import (
    AsyncMunariumClient, ClientOptions, HeadConflictError, MunariumClient,
)

# sync + REST
client = MunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))
# …or sync + gRPC / async + REST / async + gRPC
client = MunariumClient.grpc(ClientOptions("127.0.0.1:50051", token="devtoken", uid="user-1"))
aclient = AsyncMunariumClient.rest(
    ClientOptions("http://127.0.0.1:8080", token="devtoken", uid="user-1"))

v = client.commands.create_version()

# Disputed is SUCCESS — the governance record, not an error.
outcome = client.commands.propose_claim(v, subject="hero", key="eyes", value="blue")
if outcome.is_disputed:
    for f in outcome.findings:
        print(f.rule_id, f.message)

# The write loop: expected_head + fresh idempotency key per attempt.
outcome = client.propose_claim_with_retry(
    v, lambda head: {"subject": "hero", "key": "home", "value": "harbor"})

# One pin bounds all stores.
page = client.query.facts(v, as_of_seq=1)
```

`uid` is the acting end-user id (audit attribution), required by the server's
default posture (`MUNARIUM_REQUIRE_UID=true`) — omit it and every call draws the
typed `uid-required` error.

The platform surface stays Pythonic: `sessions.turn_stream(...)` is a plain
`Iterator` (async: an async generator) of `TurnProgress` events whose **last
item is the `TurnResult`** — typed errors raise during iteration, and a
stream that ends without a terminal event raises `TransportError`. If you
may leave the ASYNC stream early, wrap it in `contextlib.aclosing(...)` so
the pooled connection is released deterministically instead of whenever the
garbage collector finalizes the abandoned generator:

```python
from contextlib import aclosing
from munarium_client.models import TurnProgress

async with aclosing(aclient.sessions.turn_stream(sid, query="vacation policy")) as events:
    async for event in events:
        if isinstance(event, TurnProgress) and event.stage == "model":
            break
```

On gRPC `turn_stream` is REST-only: the sync plane raises `UnsupportedError`
when called, and `AsyncMunariumClient.grpc(...).sessions.turn_stream` is a real
async generator that raises it on the first iteration. When the 60 s SSE
idle watchdog fires, the `TransportError` says the turn may still be
executing server-side (the completion was paid) — read the transcript with
`sessions.get` before re-sending. Unary turns are deadline-exempt and never
auto-retried (they spend provider tokens a client abort cannot stop); bulk
upload sessions ride `ingest.bulk_open/bulk_chunk/bulk_complete`;
`tokens`/`reports`/`authoring` cover the management plane (mint with a
mgmt-role bearer).

`providers.max_tokens()` / `providers.replace_max_tokens(budgets)` read and
replace the tenant's per-call output-token budgets (`GET`/`POST
/v1/max-tokens`). `MaxTokensResponse` is the eight `MaxTokensBudgets` fields
flattened beside `source` (`tenant` | `environment`) and `updated_at`, and
it subclasses the budgets model, so a read result edited in place
round-trips into the replacement — only the eight budget fields go on the
wire. There is no partial update: a dict missing a field is refused before
it is sent, an out-of-range value is the server's `InvalidInputError`, the
replace needs the static **rw** role (`ForbiddenError` otherwise), and both
are REST-only (`UnsupportedError` on gRPC).

`sessions.turn`/`turn_stream` take an optional `research_profile=`: the
turn runs through a named evidence hierarchy and `TurnResult.hierarchy`
carries the decision — which layers ran, which refused, whether a
completeness claim was permissible at all. Omit it and nothing changes:
the request body grows no key and the response carries no `hierarchy`.
A streamed turn under a profile adds the `profile` / `layer_start` /
`layer_source` / `layer_complete` / `coverage` / `compose` stages after
the existing ones (`TurnProgress` declares only `stage`, so per-stage
fields ride as extras). On the operator side `reports.evidence(window=)`
shows which layer is quietly refusing — those turns still return 200, so
no error rate reveals them — and `reports.matrix()` reports Matrix's
reachability, keeping `configured=False` (never wired) distinct from
`circuit_open=True` (tripped).

Exceptions mirror the problem-slug registry (`HeadConflictError`,
`PolicyRejectionError` with findings + truncation markers, `RunLockedError`
— typed but NOT transient, pace it like a rate limit —
`RateLimitedError.retry_after` — populated only if the server sends a
`Retry-After` header, which it does not today). `UnsupportedError` marks the documented
gRPC gaps.

Command retry is deliberately narrow: on REST a command re-sends its SAME
idempotency key only after a connect-phase failure (`httpx.ConnectError` /
`ConnectTimeout` / `ProxyError` — the request never left) or the typed
`OverloadedError` (shed before executing); a gateway 502/504 is transient
for reads but never re-sent as a command, because it may still be executing
upstream. On gRPC commands re-send only on `OverloadedError` — never on a
transport failure. Two gRPC input rules beyond the proto3 zero sentinels:
`ingest()` mirrors REST `POST /v1/ingest` (a locally undecodable
`content_base64` raises `InvalidInputError`; a server-side per-item error on
that one file raises `UnexpectedError` with the text — the wire has no slug;
`ingest_batch` keeps per-item results), and an explicit `collections=[]` on
an ingest file or `runbook_refs=[]` on `tokens.mint` raises
`InvalidInputError` (proto3 cannot carry "explicitly empty"; pass `None` or
use REST). Both transports accept base64 with surrounding whitespace.

The async gRPC variant drives the thread-safe sync stubs via
`asyncio.to_thread` (documented implementation choice); async REST is native
httpx.

## Regenerating the gRPC stubs

The generated stubs under `src/munarium_client/_proto/` are committed and
CI-drift-checked. After a proto change:

```bash
python scripts/gen_protos.py
```

## Tests

```bash
pytest tests                                # offline unit tests
MUNARIUM_REST_URL=http://127.0.0.1:18080 MUNARIUM_GRPC_URL=127.0.0.1:15051 \
MUNARIUM_TOKEN=devtoken MUNARIUM_MGMT_TOKEN=devmgmt \
pytest conformance     # scenarios x {rest,grpc} x {sync,async} + smokes
                       # + the platform tests (skipped without the mgmt token)
```
