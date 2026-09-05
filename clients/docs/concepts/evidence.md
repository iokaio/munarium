# Evidence

A structured answer from Munarium can cite a row, not just a document: `[evidence/<id>#<row>]`.
The `<id>` names a **sealed evidence artifact** — a typed result executed against a governed
source and fixed in place with two hashes, one over the logical result and one over the stored
bytes — and `<row>` names a row inside it. This page is about reading that citation back, and
about a design choice that runs through the whole evidence surface: **no client can create
one.**

## Sealing is deliberately absent from every client

A manifest is a statement about work the sealer did: it executed this exact statement, against
this exact source, and got exactly these bytes. An SDK that let an application "seal evidence"
would let that application assert provenance it has no way to actually vouch for — it would be
signing someone else's homework. What an application legitimately needs is the other direction:
an answer cites a row, and the application resolves that citation to show a reader what the
number was actually computed from. Every client's evidence surface is built for exactly that,
and only that.

## Reading a citation

Two operations, both scoped to your tenant, both refused unless your capability token's
clearance dominates the class the manifest declares, and both privately audited — a resolution
records that a read happened, never what was read:

- **get the manifest** — the artifact's metadata: what kind of result it is, what source and
  query or semantic definition produced it, whether the read was complete or truncated, the two
  hashes, and its retention state;
- **read rows** — the sealed rows themselves, in the order they were sealed, paged.

A manifest that resolves successfully means the artifact is committed; a pending one and a
purged one each answer with their own distinct refusal rather than either looking like "not
found." A purged artifact's *metadata* survives its own purge, so a citation into deleted data
still resolves to "this is what that was," rather than to nothing at all — and a legal hold
blocks deletion (and only deletion; a held artifact is still readable).

## Completeness is a property of the read, not an assumption

Before quoting a total from a resolved manifest, check whether that read was marked complete or
truncated. A truncated result supports "at least this many," never "exactly this many" — the
same discipline the fact ledger applies to point-in-time reads applies here: a system that
can't see everything says so, rather than letting a partial answer be read as a total one.

## The five kinds of evidence a turn can cite

A turn's completion can draw on more than retrieved documents, and because those kinds of
evidence aren't interchangeable, the response says which kind backed which part of the answer.
There are exactly five: document hits, a complete table read, a count, a slice of the fact
ledger itself, and a refusal. Consumers are expected to handle all five as distinct cases, not
to treat "some evidence was found" as one bucket.

The one that surprises people: **document hits can never support a completeness claim.**
Retrieval returns the best matches it found, never a proof that nothing else exists — treating
a good search result as exhaustive is how a system ends up implying "there are no other
contracts" when the honest statement is "I found three." A refusal, by contrast, is not an
error to be hidden from the answer — a layer that declined to answer (a stale source, a policy
boundary, an unreachable connector) has told the turn something, and an honest answer has to be
able to say "this part of the register wasn't consulted" rather than silently proceeding as if
it didn't exist.

## Typed assertions: making a derivation checkable

When a completion draws a numeric or textual claim from sealed evidence, it can attach a typed
assertion naming exactly which cited row or rows the value came from. A single-referenced
assertion is checked verbatim: the stated value must appear, unchanged, in that one row. An
assertion citing two or more rows is a derivation — a sum, a difference, a ratio — and is
*expected* not to appear verbatim in any single row, so it isn't held to the same literal check.
Values in this path are compared as text, never as floating-point numbers: `900000.5` and
`900000.50` are the same number and different sealed values, and collapsing that distinction
anywhere in the path would erase the exactness the whole evidence surface exists to preserve.

## See also

- [Sessions and turns](sessions-and-turns.md) — a turn's completion is where an evidence
  citation shows up in the first place.
- [The fact ledger](fact-ledger.md) — a fact-slice citation draws directly on ledger claims,
  under the same supersession-and-pin rules as any other read.
- [Compatibility and errors](compatibility-and-errors.md) — the typed refusals a citation
  resolution can draw (not found, forbidden, pending, expired, on hold) are part of the same
  error vocabulary every other call uses.
