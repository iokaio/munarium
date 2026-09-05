# The gRPC data plane — `matrix.v1`

One service, one RPC, on its own port. Everything else — registry, journal,
reports, ops — is REST ([rest.md](rest.md)).

| | |
|---|---|
| Port | **50151** (`MUNARIUM_MATRIX_GRPC_ADDR`; `disabled` turns it off) |
| Roles | served by `all` and `query`; `control` and `sync` answer `UNIMPLEMENTED` |
| Transport | h2c from the container; TLS is the ingress's job — an `http2`-capable ingress or Gateway in front of the container terminates it |
| Auth | `authorization: Bearer <token>` metadata, the same static or capability token REST takes |
| Extras | `grpc.health.v1.Health` and server reflection on the same listener — and health's descriptor is registered WITH reflection, without which the `Health/Check` line below answers "target server does not expose service" against a server that is serving it (found on a real cluster, 2026-08-30) |
| Proto | [`proto/matrix/v1/matrix.proto`](../../proto/matrix/v1/matrix.proto) |

## `MatrixQuery.Execute(ExecuteRequest) → stream ExecuteEvent`

The request names a contract and carries a `QueryIntent` — the same intent
`POST /v1/contracts/{name}/execute` takes, as a message instead of JSON. The
stream carries progress stages, then exactly one terminal event:

```text
Progress{stage: "authenticated"}
Progress{stage: "loading"}      the contract is read from the registry
Progress{stage: "wiring"}       the source adapter and the server client open
Progress{stage: "budget"}       a unit is reserved BEFORE the source is touched
Progress{stage: "executing"}
Progress{stage: "sealed"}       only on success
EvidenceBlock | Refusal         terminal; the call then completes OK
```

**A refusal is an answer.** `not_covered`, `policy_denied`, `budget_exceeded`
and the rest arrive as a `Refusal` message with the contract's closed `class`
and open `code`, and the call ends with status OK — exactly as REST puts the
same refusal in a 200 body. Status codes mean the transport or the caller's
own mistake:

| Status | When |
|---|---|
| `UNAUTHENTICATED` | no bearer, or one the server does not know |
| `PERMISSION_DENIED` | a token whose role cannot execute (`ro`, `mgmt`) |
| `INVALID_ARGUMENT` | no intent, no contract name, or an intent that does not convert — an `UNSPECIFIED` enum is an error, never a default |
| `UNIMPLEMENTED` | a role that does not serve the query plane |
| `DEADLINE_EXCEEDED` / `CANCELLED` | the client's `grpc-timeout` elapsed before the response opened; an intent whose own `deadline_at` has passed is instead a `deadline_exceeded` **refusal on the stream** |

**Cancellation is native.** Drop the stream and the execution task is aborted
at its next await. A budget unit reserved for it stays reserved until the
sweep reclaims it — spent, not refunded, which is the safe direction.

**Same path as REST.** Both planes run `execute.rs`; the journal record names
the plane in `via`. The block a stream carries cites the same sealed evidence
id REST would, because sealing is idempotent by logical hash — the `grpc`
conformance tier asserts exactly that.

## One contract

`matrix.proto` mirrors `matrix/contract/*.schema.json` field for field. Open
JSON values are `google.protobuf.Value`; the evidence manifest is a
`google.protobuf.Struct`, because its JSON schema is the normative one and a
hand-maintained proto mirror of it would be a second contract. The drift check
is a test that round-trips every committed contract example through the
messages and back.

## Trying it

```powershell
grpcurl -plaintext localhost:50151 list      # matrix.v1.MatrixQuery + grpc.health.v1.Health
grpcurl -plaintext localhost:50151 grpc.health.v1.Health/Check
grpcurl -plaintext -H 'authorization: Bearer mxdev' \
  -d '{"contract":"open-pipeline-by-region","intent":{"kind":"INTENT_KIND_STRUCTURED_QUERY","contract":"open-pipeline-by-region","parameters":{"as_of":{"type":"date","value":"2026-06-30"}},"authorization":{"tenant":"tenant-default","access_level":0},"limits":{"max_rows":500,"max_bytes":1048576},"seal":{"required":true}}}' \
  localhost:50151 matrix.v1.MatrixQuery/Execute
```

## Semantic intents

`Execute` carries the contract's `QueryIntent`, and its `kind` selects the
path: `structured_query` names a query contract in `contract`; `semantic`
names a **metric view** in the same field and carries the `semantic` block
(`provider`, `measures`, `dimensions`, `filters`). The stream is identical —
progress stages, then one block or one refusal — and so is the evidence: a
`[evidence/<id>#<row>]` citation into a metric-view result resolves like any
other. There is no second service and no second RPC; a metric view is a
different way of choosing the statement, not a different plane.
