# munarium-matrix (Python)

The Python client for **Munarium Matrix**, the structured-evidence plane. It
speaks Matrix's REST API and it is deliberately small: Matrix's whole surface
is *registering assets, running the three modes, and reading what happened*.

```bash
pip install munarium-matrix
```

One runtime dependency, `httpx` — the same choice the server's Python client
made, for the same reasons: one library for sync and async, a timeout that is
not optional, and no transitive surprise.

## Use

```python
from munarium_matrix import MatrixClient

with MatrixClient("https://matrix.example", token="...", uid="ops@example.com") as mx:
    print(mx.version().lockstep_ok)  # does Matrix agree with its server?

    mx.apply(open("datasource.crm.yaml").read())
    mx.apply(open("contract.pipeline.yaml").read())

    outcome = mx.verify("open-pipeline-by-region")
    if outcome.failed:
        for q in outcome.questions:
            if not q.ok:
                print(q.question, q.failures)
        raise SystemExit(3)  # the exit discipline `mxctl` uses
```

Async is the same surface:

```python
from munarium_matrix import AsyncMatrixClient

async with AsyncMatrixClient("https://matrix.example", token="...") as mx:
    await mx.sync("crm")
```

### Refusals are typed

Matrix answers a refusal as RFC 9457 problem+json carrying a `refusal` object
with the **class** and the **code** — the closed vocabulary the whole system
rests on. They arrive as attributes, not prose:

```python
from munarium_matrix import MatrixError

try:
    mx.verify("open-pipeline-by-region")
except MatrixError as e:
    if e.retryable:  # unavailable | exhausted
        wait = e.retry_after  # seconds, when the service said
    elif e.code == "not_covered":  # the collection cannot answer it
        ...
```

`retryable` is a property and not a guess: `unavailable` and `exhausted` are
states of the world, and every other class is a statement about the request or
the assets, where repeating it changes nothing. Retrying a `denied` is
hammering a door that is locked on purpose.

### Lockstep

`version().lockstep_ok` is true only when the server reports `exact`. That is
the one state in which an evidence id minted by this Matrix is certain to
resolve on that server — which is what a citation like
`[evidence/<id>#r0003]` depends on.

## What this client deliberately does NOT do

Three absences, each of them a design decision rather than a missing feature:

* **No sealing.** A manifest is a statement about work the *sealer* did. An SDK
  offering `seal_evidence` would invite an application to assert provenance it
  cannot vouch for. Sealing is Matrix's own act; evidence is *read* through the
  **server's** client, resolving `[evidence/<id>#<row>]`.
* **No local validation.** `validate()` posts the YAML and returns Matrix's own
  findings. A client carrying its own copy of the rules would drift from the
  service that enforces them, and the drift would surface as an asset that
  validates here and is refused there.
* **No SQL.** Nothing on this surface takes a statement. Queries are
  pre-declared contracts and views, executed by name.

There is also no gRPC transport here. Matrix's gRPC plane serves `Execute`
alone, and `Execute` is service-to-service — the munarium-server calls it, not
an application. When that changes, this package grows a transport rather than a
second client.

## Surface

| Area | Methods |
| --- | --- |
| meta | `version`, `healthz`, `healthdata` |
| registry | `apply`, `validate`, `list_assets`, `get_yaml` |
| sources | `introspect`, `probe`, `sync` |
| contracts and views | `verify`, `verify_view` |
| reconcile | `reconcile`, `promotion_status`, `gate_history`, `promote`, `demote`, `rollback` |
| audit | `journal` |

`verify_view` takes either a metric view or a native data view and tries the
metric-view route first — the caller names the view, not the route it happens
to live on. The fallback fires on a 404 **or** a `not_covered` 422, because a
missing metric view loads through the runtime and comes back as the latter; a
different 422, such as `metric_view_changed`, is a real answer about a view
that exists and is not retried as something else.

`validate` returns the service's own `valid` flag beside its findings, and not
`not findings`: three codes are advisory, so a valid asset can carry them.

`sync` and `reconcile` return a `JobAccepted`: Matrix queues them, so the call
returns an id rather than an outcome. Poll `journal()` or the run route for the
terminal state.

## Versioning

This package targets Matrix `1.0.0`. A version bump on the wire surface bumps
them together.

## Tests

```bash
pip install -e ".[dev]"
pytest
```

The offline tier drives a stub transport and asserts the response *shapes* this
client claims to understand, which is what catches a field rename. Setting
`MUNARIUM_MATRIX_TEST_URL` adds a live round-trip against a real Matrix; with
it unset that test **says it skipped**, because a skip that prints nothing is
indistinguishable from a pass.
