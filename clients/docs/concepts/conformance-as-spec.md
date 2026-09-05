# Conformance as specification

Munarium doesn't have a separate specification document that the server and the four clients
were each independently written to satisfy. It has **`contract/conformance/SCENARIOS.md`**
— seven scenarios, stated once, in prose, with a fixed setup, a fixed sequence of steps, and an
exact expected outcome for each — and every client implements every one of them, against a live
server, as its own conformance suite. The suite is not a smoke test that happens to also
document behavior; it *is* the specification, made executable.

## Why prose plus a real implementation, rather than a formal spec

A formal protocol document can drift from what the software actually does the moment either
side changes without the other noticing. A scenario that's actually run, in every language,
against a real server, can't drift silently that way: if a client's implementation of
`ledger.supersession-pin` and the server's behavior ever disagreed, the scenario would fail,
not quietly pass while describing something neither side does anymore. The prose in
`SCENARIOS.md` is there so a reader — porting the suite to a fifth language, or just trying to
understand what's actually guaranteed — has one text to check an implementation against,
instead of reverse-engineering the guarantee from someone else's Rust, Python, or Java.

## The seven scenarios, and what each one proves

| Scenario | What it proves |
|---|---|
| `ledger.append-head-conflict` | A stale expected-head is refused, and refusal never advances the ledger's clock. |
| `ledger.supersession-pin` | A correction filed in a child version supersedes at head while an earlier pin still reads the original. |
| `pins.one-pin-bounds-all-stores` | One pin bounds facts, milestones, obligations, and counters together — not just the fact ledger. |
| `gates.block-records-disputed` | A claim that conflicts with canon is recorded disputed, with the finding that explains why, and canon itself is unchanged. |
| `composer.budget-degradation` | Under a token budget, a composed context degrades its own summaries before it ever drops a fact. |
| `digests.rebuilt-under-pin` | A summary computed for "now" is never served under a pin taken before it existed. |
| `ledger.origin-round-trips` | A connector-sourced claim's origin survives a fresh read, byte for byte; a claim proposed without one reads back without one. |

Each row here is one page of this concept set, turned into a checkable assertion:
`ledger.supersession-pin` and `pins.one-pin-bounds-all-stores` are
[The fact ledger](fact-ledger.md)'s claims; `composer.budget-degradation` and
`digests.rebuilt-under-pin` are the same page's pin-and-budget guarantees applied to composed
context rather than raw facts; `gates.block-records-disputed` is the disputed-not-dropped
behavior [Runbooks and access](runbooks-and-access.md) describes for shape violations, applied
to plain ledger conflicts.

An eighth scenario, `gates.chronology-certain-only`, exists in the same document but is marked
as a **kernel scenario**: it exercises a pure evaluation over declarative rules with no request
made to any server, so it has nothing for a client to port and stays part of the server's own
in-process suite rather than any client's.

## Reading the scenarios yourself

`SCENARIOS.md` states, for each scenario, its setup, its steps in order, and exactly what a
conforming implementation must observe at each checkpoint. It's short enough to read end to
end, and it's the right place to start if you're evaluating whether a use case Munarium's API
supports actually behaves the way this concept set describes — the scenarios are the ground
truth this whole directory was written to explain, not the other way around.

## See also

- [The fact ledger](fact-ledger.md), [Runbooks and access](runbooks-and-access.md) — the
  concepts these scenarios check.
- Each language's guides under [`../guides/`](../guides/) show the scenario's steps as real,
  idiomatic calls in that language.
