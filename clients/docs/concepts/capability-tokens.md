# Capability tokens

Munarium separates two kinds of credential, and most of the confusion a new integration runs
into traces back to conflating them.

## Static tokens authenticate services; capability tokens authenticate people

A **static token** authenticates the service account your application runs as. It carries a
role — read-write, read-only, or management — and that role is a hard boundary: a management
token cannot touch the data plane at all, so a token leaked from an admin tool can't be used to
read anyone's data, and a data-plane token can't mint or revoke credentials or pull reports.

A **capability token** — a short-lived, signed JSON Web Token — authenticates an actual end
user of your application. Your application mints one, through the management plane, every time
it needs to let a specific person act against Munarium; Munarium never sees that person
directly. This is the trust handoff at the center of the whole model: *you* authenticated the
user by whatever means your application uses, and minting the token is you telling Munarium
what that already-authenticated person is allowed to do.

## What a capability token carries

- an **access level** and a set of **compartments** — the clearance that determines which
  collections a session opened with this token can see (see
  [Runbooks and access](runbooks-and-access.md));
- one or more **scopes** — `query` to open sessions and search, `ingest` to feed the ingest
  plane — so a token minted for a read-only assistant can't also be used to write new sources;
- an optional **runbook allowlist**, confining the token to specific named runbooks rather than
  everything its access level would otherwise reach;
- a **time-to-live**, capped at a fixed ceiling regardless of what's requested, so a token that
  leaks has a bounded window rather than an indefinite one.

The signed token material is returned exactly once, at mint time, and is never stored on the
server in a form that could be read back — only its metadata (a unique id, the user identifier,
its scopes, its expiry) enters the audit trail. Treat the token the way you'd treat any other
secret handed to a specific user for a specific window of time.

## The uid contract

Every capability token names a subject — the identifier of the user it was minted for. When a
client uses that token, it must also declare the same identifier as its own `uid`, and the two
must match exactly. This isn't a courtesy header: it's the identity every action taken with
that token is attributed to in the audit trail, and the server checks that the token's own
claim about who it belongs to agrees with what the caller says. A mismatch is refused outright,
because letting them disagree would mean the audit trail could be told a lie about who did
what.

## Revocation is real, but ask before assuming it bites

A management-role token can list issued capability tokens and revoke one by its id. Revocation
adds the token to a deny-list that's checked on every subsequent use — but only when the server
is actually running with that check turned on, and the revocation response tells the caller
whether it was. This is not the client hedging: it's the honest answer to "did that revocation
actually do anything," because a system that claimed revocation always works when it might not
be enabled would be worse than one that tells you plainly.

## Why two credential kinds instead of one

A single credential type would have to be either too powerful for routine per-user requests
(a service credential handed to every end user) or too weak for administrative operations (a
narrowly-scoped user token asked to mint other tokens). Splitting them means a compromised
end-user token — the one an application mints constantly, for every session — can never be
used to mint more tokens, read audit reports, or touch another user's data outside its own
declared clearance; the blast radius of a leak is exactly the one user's own access, for the
remainder of that token's short lifetime.

## See also

- [Runbooks and access](runbooks-and-access.md) — what access level and compartments actually
  gate: the collections a session opened with this token can see.
- [Sessions and turns](sessions-and-turns.md) — how a session's permitted-collections response
  reflects the token's own clearance.
