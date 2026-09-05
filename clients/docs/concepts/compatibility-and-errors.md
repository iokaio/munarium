# Compatibility and errors

Two housekeeping questions come up in every integration: which server versions does this
client actually support, and how do I tell what went wrong when a call fails. Both have one
authoritative, machine-readable answer, and this page is the short prose explanation of each.

## Compatibility

**[`compatibility.json`](../../compatibility.json)**, at the root of this tree, is the
authoritative record of which server versions each client supports. The policy it encodes: a
client's minor version supports the current server minor and the one immediately before it, so
upgrading the server by one minor release never strands a client that hasn't caught up yet. A
wire-breaking change to the protocol itself bumps the contract's major version rather than a
client's own version number, and the server keeps serving both majors for one full minor
release, so a client can move across a breaking change on its own schedule rather than needing
to coordinate a simultaneous flag day with the server operator.

Client and server version numbers are not required to match, and a shared number on a given
release (as with this project's first one) is a coincidence of that release rather than a rule.
Check `compatibility.json`, not the version numbers themselves, to know whether a given client
release supports a given server release.

## Errors

Every client decodes a failure into the same small set of typed errors, regardless of which
transport carried it. The **problem slug** — a short, stable machine-readable name like
`head-conflict` or `evidence-forbidden` — is the one thing that means the same thing on both
transports and across every client language; human-readable message text is not, and code
should never key logic on it. On the HTTP transport, the slug rides as a standard structured
problem response; on gRPC, the same slug rides in the status's structured error details. A
client that receives a slug it doesn't specifically recognize still surfaces it as a typed
generic error carrying that slug, rather than failing to parse the response at all — a detail
that matters if the server ever adds a new failure type after this client version was released.

**`errors.md`**, part of the public contract bundle, is the complete registry:
every problem slug Munarium can emit, what it means, and which extra fields (if any) come
attached to it — the stale-head values on a head conflict, the gate findings on a policy
rejection, and so on. It's short enough to read end to end, and several of the concept pages in
this set link into specific rows of it (evidence's five refusal types, the token mismatch and
override-not-allowed errors, and the rest).

## Why one registry instead of per-language error types

An application that talks to Munarium from more than one language — a Python ingestion job and
a Rust-backed service, say — needs "the server refused this because the head moved" to mean the
same thing regardless of which client raised it. Keying every language's errors off one shared
slug registry, rather than letting each language invent its own error taxonomy, is what makes
that portable: the slug in a log line from one language is directly searchable in another's
documentation, and a bug report that crosses a language boundary doesn't need translating first.

## See also

- [Capability tokens](capability-tokens.md) — the uid-mismatch and revocation-related errors
  a token-authenticated call can draw.
- [Evidence](evidence.md) — the five citation-resolution refusals, each its own row in
  `errors.md`.
- [Conformance as specification](conformance-as-spec.md) — `ledger.append-head-conflict` is the
  scenario that specifically exercises the head-conflict error this page describes.
