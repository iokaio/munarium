# Sessions and turns

A **session** pins one runbook version and takes retrieval **turns** against the collections
that runbook grants — filtered to what the calling token's clearance actually permits, so the
session's own creation response is a least-privilege echo: it tells the caller exactly which
collections it can see, not the runbook's full nominal set.

## A turn is a paid, non-retried action

A turn spends provider tokens the moment it runs a completion step, and it runs **exactly
once** — the client libraries never auto-retry a turn on your behalf, because retrying a paid
action silently is how a transient network blip turns into a double charge. A turn is also
**deadline-exempt**: once the server has accepted it, a client-side timeout or cancellation
cannot un-spend the completion that's already in flight. If a call times out on your side, the
turn may still be executing and may still land in the transcript; the way to find out is to
read the session back, not to assume failure and resend.

## What a turn returns

A turn's result names the collections it actually searched, the collections it was permitted
to search but skipped (because they had no active index, for instance), and carries a
provenance record per collection — so an answer's sourcing can always be traced back to which
collections contributed and which did not, per the access boundary each collection sits behind.
When the runbook's completion step ran, the result also carries the completion itself: which
provider and model actually served it, and the answer text.

## Model overrides: honored or refused, never silently downgraded

A caller may request a specific provider, model, or tier for a turn's completion step. The
runbook's own policy decides whether that's allowed. If it isn't, the client raises a typed
refusal — the turn is never silently served on the default model instead, because a caller who
asked for a specific model and got a different one without being told has been misled about
what answered them. When an override *is* honored, the completion echoes exactly what served
it, so nothing about which model answered is ever left to guesswork.

## The streaming turn

The same turn can run over a streaming transport, which narrates the stages a turn passes
through as they happen — the collections being searched, the results being merged, the model
call, the completion, and any verification passes — before delivering the same final result a
non-streaming call would have returned. Two invariants hold regardless of language: the stream
ends with **exactly one terminal event** (a successful result or a typed error, never neither),
and progress events are forward-compatible — a client build may see a stage name it doesn't
recognize from a newer server and should treat it as informational rather than fail on it.
Streaming is available over the HTTP transport; it is not part of the gRPC surface, and a
gRPC client that asks for it receives a typed "not supported here" refusal rather than a
confusing low-level error.

## Verification, when the runbook asks for it

A runbook may declare a verification pass over a turn's completion. When it does, the result
carries which checks ran and which violations, if any, remained after the retry budget was
spent — an empty list means the answer passed; a non-empty list means it stands unverified, and
what to do with that is a decision the caller makes, not the server. Whatever the turn spent
across every completion attempt, verification retries included, is summed into one honest total
rather than reported as just the cost of the last attempt.

## The transcript

Every turn a session takes is stored against it, and reading the session back returns the whole
turn-by-turn record along with the session's current state (open, closed, or expired). A turn
against a closed session is refused with a typed error — closing is a deliberate, final action,
and it's idempotent: closing an already-closed session simply echoes its state back rather than
raising an error about something that was already true.

## See also

- [Runbooks and access](runbooks-and-access.md) — what a runbook actually grants a session, and
  what "model tier" and `allowOverrides` mean at the runbook level.
- [Evidence](evidence.md) — a turn's completion can cite sealed evidence rows alongside document
  hits; both ride the same citation syntax.
- [Capability tokens](capability-tokens.md) — the clearance a session's permitted-collections
  echo is computed from.
