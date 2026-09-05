# Runbooks and access

Munarium doesn't grant access to a document store or a database; it grants access to a
**runbook** — a declarative description of a set of collections, how they're indexed, and what
a session over them is allowed to do. This is the unit of access the whole system is built
around: everything a session can search, and everything it can ask a model to do with what it
finds, traces back to the runbook it was created against.

## Shapes: what a claim must look like to be accepted

A **shape** is a schema gate for claims — a declarative description of what a valid claim
looks like for a given kind of fact. Applying a shape is itself recorded as a claim in the
ledger, so the lineage explains when and why the rules for accepting a claim changed. A claim
that violates the shape currently in force is not accepted quietly and it isn't dropped: it
draws a block-severity finding and is recorded disputed, exactly like any other claim the
governance layer rejects.

## Runbooks: checkpointed step machines

A **runbook** describes a pipeline — resolving sources, building an index, verifying it,
cutting traffic over, retiring what came before — as an ordered sequence of steps. Running a
runbook is itself an auditable process: every step transition, every retry, and every approval
is recorded against the ledger version the run names, so "what happened during this reindex"
is answerable the same way "what changed in this fact" is. A step can require human approval
before it proceeds; a run that reaches such a step pauses there until someone with the right
role approves it, and the approval itself is an evented, attributable action.

## Collections, clearance, and compartments

A runbook names the collections a session over it may search. Which of those collections a
*particular* session can actually see is a second, narrower filter: the calling token's access
level and compartments (see [Capability tokens](capability-tokens.md)) are checked against each
collection's own posture, and the session's creation response tells the caller exactly which
collections survived that intersection — never the runbook's full nominal list, and never a
silent expansion beyond what the token actually grants. A collection a token cannot see is
invisible to that session in every sense: not searched, not named in results, not counted
against retrieval budgets.

## Model tiers and overrides

A runbook declares which model tier serves its completions by default, and whether a session
may ask for something else. When overrides are allowed, a caller may request a specific
provider, model, or tier for one turn; when they aren't, that same request is refused rather
than quietly served on the default — see [Sessions and turns](sessions-and-turns.md) for what
that refusal looks like and why a silent downgrade is never the alternative. This is a
governance decision made once, at the runbook level, rather than something every caller has to
reason about per request.

## Why access lives here, and not on the collections themselves

Attaching permissions directly to a collection would mean every application that wants a
different slice of the same corpus needs its own copy of that corpus, re-permissioned. Binding
access to the runbook instead means the same underlying collections can be exposed through
several different runbooks, each with its own model policy and its own approval requirements,
without duplicating a single document. The runbook is the seam between "what data exists" and
"what may be done with it, by whom, under what model policy" — which is also why it's the right
place to look first when deciding whether a new application needs a new runbook or can reuse an
existing one.

## See also

- [Capability tokens](capability-tokens.md) — access level, compartments, and the
  `runbook_refs` allowlist that can confine a token to specific runbooks by name.
- [Sessions and turns](sessions-and-turns.md) — the permitted-collections echo and the
  model-override refusal, from the caller's side.
