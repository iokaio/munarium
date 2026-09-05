# The fact ledger

Munarium's memory is an append-only ledger of claims. Nothing is ever overwritten in place;
a value that changes is recorded as a new claim that **supersedes** the old one, and every
read resolves supersession as of some point — usually "now," but a client can ask for any
earlier point and get a consistent answer as of exactly then. That single idea — append,
never mutate; resolve supersession at read time — is what the rest of this page unpacks.

## Claims

A claim names a `subject`, a `key`, and a `value`: "the vacation policy's `notice_days` is 14,"
"the Q3 filing's `revenue` is 900000.50." Every claim also carries:

- a **claim type** — `fact` (new information), `update` (a value legitimately changed, such as
  a status transition), or `correction` (the earlier value was wrong);
- a **status** — `accepted` or `disputed`. A claim that conflicts with what the ledger already
  holds is not silently dropped and not silently accepted: it is recorded disputed, alongside
  the finding that explains why, so both sides of a disagreement stay visible;
- an optional `supersedes_id`, naming the exact claim it replaces;
- an optional **origin**, when the claim came from a connector rather than a model — the
  source system, the mapping version, the row key, and where in that source's own event
  stream the value was observed. A claim proposed without an origin reads back without one;
  a claim proposed with one carries it through every read, unchanged, forever.

Updates and corrections both supersede at the ledger level; the distinction is about what the
change *means* (a legitimate transition versus a fix), not about the mechanics of resolving it.

## Versions and lineage

Claims live inside a **version** — a node in a lineage tree, created with an optional parent.
A version's own claims plus everything inherited from its ancestors make up what a read against
it sees. A correction filed in a child version supersedes its parent's claim for every reader of
that child, while the parent line is untouched — the correction is scoped to where it was made,
not retroactive to every branch that happens to share history with it.

## Supersession is resolved at read time, not at write time

Nothing is deleted or edited when a correction lands. The ledger keeps both the original claim
and the correction, in order, and a read walks that order to decide what is current. This is
what makes point-in-time reads possible: the ledger doesn't need a separate history table,
because the append-only log already *is* the history.

## Pins: one `as_of_seq` bounds everything

Every version has a monotonically increasing sequence number — one claim, one tick. A read may
supply `as_of_seq` to see the ledger exactly as it stood after that tick, and the pin is not a
facts-only feature: it bounds every kind of state the ledger tracks together. A claim corrected
after the pin reads back as current at the pin. A milestone or obligation recorded after the pin
is invisible. An obligation fulfilled after the pin still reads as outstanding. And a composed
summary read under a pin is rebuilt fresh from the pinned facts — a summary computed for "now"
is never served under an earlier pin, because it could disclose something that hadn't happened
yet as of that pin.

This is the property the wire scenario `ledger.supersession-pin` locks in, and it's why a client
that needs an audit trail — "what did we know, and when did we know it" — doesn't need to build
one: the pin *is* the audit trail, for free, on every read.

One wire detail worth knowing: on the gRPC transport, an explicit pin of zero is indistinguishable
from "no pin was set" (protobuf has no way to tell "the caller wrote 0" from "the caller wrote
nothing" for a plain integer field). The clients treat an explicit zero pin as a mistake and
refuse it with a typed error rather than silently reading the head state instead of what was
asked for. Omit the pin entirely to read the head.

## Why this shape

A ledger that can only tell you the current value is adequate until someone asks *why* it
changed, or asks what it said last month, or two systems disagree about which fact is right.
Append-only claims with explicit supersession and origin make all three questions answerable
from data that was already being recorded anyway, rather than from a change log bolted on
afterward. It costs a small amount of read-time resolution work; it buys an audit trail, a
time-travel query, and a defensible answer to "where did this number come from" as intrinsic
properties of the storage model rather than as features someone has to remember to build.

## See also

- [Sessions and turns](sessions-and-turns.md) — how a retrieval turn's answer relates to the
  facts a caller can see.
- [Evidence](evidence.md) — a fact slice is one of the five things a turn can cite as evidence.
- The wire scenarios `ledger.append-head-conflict`, `ledger.supersession-pin`,
  `pins.one-pin-bounds-all-stores`, `gates.block-records-disputed`, and
  `ledger.origin-round-trips` in [Conformance as specification](conformance-as-spec.md) are this
  page's claims, each one turned into an assertion a client can run.
