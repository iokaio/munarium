# MMP conformance scenarios: the contract text

The Munarium Protocol's behaviour is defined by eight scenarios. The server's
`mmp-conformance` crate is the reference implementation: it runs them in-process against
every storage backend and black-box over both wire planes (REST and gRPC), and a
storage backend or a client is conforming when it passes them. This document states each
scenario in prose, with its setup, its steps and what must be observed, so that the
client-language ports (Rust, Python, .NET, Java) are checked against one text rather than
against each other. The names are the ones the suites print, so results are comparable
across languages.

Seven scenarios are **wire scenarios**: they use only the commands and query planes and
every client port carries them. One, `gates.chronology-certain-only`, is a **kernel
scenario**: it exercises the chronology gate over declarative rules with no API surface,
and only the in-process suite runs it. The kernel scenario is listed last, marked as such.

## Vocabulary

- **Version.** A node in a ledger lineage, created with an optional parent. Reads on a
  version see its whole lineage.
- **Claim.** `subject`, `key`, `value`; a `claim_type` of `fact`, `update` or
  `correction`; a `status` of `accepted` or `disputed`; an optional `supersedes_id`, an
  optional `scope_path`, and an optional `origin` (the S-4.1 connector origin: `kind`,
  `source_id`, `mapping_version`, `row_key`, and optional `event_position`, `observed_at`,
  `evidence_id`).
- **Seq.** The version's monotonic clock. Every write that a read can observe advances
  it: a claim, a promise registration, an anchor lock, a counter record. `head` is the
  current seq.
- **Pin.** A read `as_of_seq = N` sees the ledger exactly as it stood after seq N: facts
  with supersession resolved *as of N*, and anchors, promises and counters stamped at or
  before N. A fulfillment recorded after the pin reads as not yet having happened.
- **Expected head.** A write may carry `expected_head`; if the version's head has moved
  past it, the write is refused with a head conflict and nothing changes.
- **Facts.** The current claims of a version under supersession (`query.facts`), optionally
  filtered by status.
- **Gates.** Deterministic checks the command path runs before accepting a claim. A
  `block`-severity finding does not drop the claim: the claim is recorded with status
  `disputed`, and the finding is returned with the write.
- **Compose.** `query.compose_context(version, scope?, budget_tokens?, as_of_seq?)` renders
  the context a model would receive: digest rungs, then an "Accepted facts" section, then
  anchors, promises and budget directives, with an `estimated_tokens` count and the
  section list. Under a token budget the composer degrades digests tier by tier before it
  trims a single fact.
- **Digest.** A stored summary of a scope at a tier, with the seq it was built from. Stored
  digests describe the head; a pinned read must rebuild from the pinned facts and never
  serve a stored rung.

Wire form, as the Python port spells it (the other ports are the same operations in their
own idiom): `commands.create_version(parent?)`, `commands.propose_claim(version, subject,
key, value, claim_type?, supersedes_id?, scope_path?, origin?, expected_head?)` returning
the stored claim, its `is_disputed` flag and the gate `findings`; `commands.open_promise`,
`commands.fulfill_promise`, `commands.lock_anchor`, `commands.record_counts`,
`commands.upsert_digest`; `query.head`, `query.get_claim`, `query.facts(version,
as_of_seq?, statuses?)`, `query.anchors`, `query.counters`, `query.promises` (each with
`as_of_seq?`), `query.compose_context`.

## 1. `ledger.append-head-conflict`

**Purpose.** Optimistic concurrency: a stale `expected_head` is refused and leaves the
ledger untouched.

**Setup.** A fresh version.

**Steps and expectations.**

1. Propose `hero.eyes = green` with `expected_head = 0`. The stored claim has `seq = 1`.
2. Propose `hero.home = harbor` with the now-stale `expected_head = 0`. The write is
   refused with a **head conflict** error (REST: the `head-conflict` problem; gRPC: the
   typed status the errors registry maps it to).
3. `query.head` is still **1**: a refused write does not advance the clock.

## 2. `ledger.supersession-pin`

**Purpose.** A correction in a child version supersedes across the lineage at the head,
and a pin taken before the correction still reads the original as current.

**Setup.** Version `v1` with `hero.eyes = green` (seq 1) and `hero.home = harbor` (seq 2).
Version `v2` with parent `v1`.

**Steps and expectations.**

1. In `v2`, propose `hero.eyes = blue` with `claim_type = correction` and `supersedes_id`
   naming the original `green` claim.
2. `query.facts(v2)` at head contains exactly one `eyes` fact, and its value is **blue**.
3. `query.facts(v2, as_of_seq = 2)` contains exactly one `eyes` fact, and its value is
   **green**: a claim superseded after the pin is current at the pin.

## 3. `pins.one-pin-bounds-all-stores`

**Purpose.** One pin bounds every store, not only the fact ledger: anchors, counters and
promises stamped after the pin are invisible, and a fulfillment after the pin reads as
still open.

**Setup.** A fresh version, then six writes in this order, each advancing seq:

| seq | write |
|---|---|
| 1 | claim `hero.eyes = green` |
| 2 | promise `reveal` registered (`kind = setup`, "open the letter", origin scope `ch1`, due scope `ch3`) |
| 3 | anchor `hero.eyes = green` locked at scope `ch1` |
| 4 | counter `flashback` at scope `ch1`, count 1, budget 2 |
| 5 | claim `hero.home = harbor` |
| 6 | promise `reveal` fulfilled |

**Expectations.**

- At `as_of_seq = 1`: anchors, counters and promises are all **empty**.
- At `as_of_seq = 2`: anchors and counters are **empty**; promises hold exactly **one**,
  and its status is **open** (the fulfillment at seq 6 is after the pin).
- At head: anchors are non-empty and the promise's status is **fulfilled**.

## 4. `gates.block-records-disputed`

**Purpose.** The command path is the governance path. A claim that conflicts with canon
draws a `block` finding from the ledger-conflict gate, is recorded with status `disputed`
rather than dropped, and canon is unchanged.

**Setup.** A fresh version with `hero.eyes = green` accepted.

**Steps and expectations.**

1. Propose `hero.eyes = blue` as a plain fact at scope `ch2` (no `supersedes_id`).
2. The write **succeeds**: the response marks the claim **disputed** and its `findings`
   contain a finding with `rule_id = gate.ledger-conflict` and `severity = block`.
3. `query.facts(version)` (accepted facts) holds exactly one `eyes` fact, value **green**.
4. `query.facts(version, statuses = [disputed])` contains the `blue` claim.

In-process, the reference implementation reaches the same state by loading the snapshot,
running the gates on the candidate, and appending the blocked claim with status
`disputed`; over the wire, `propose_claim` does all three.

## 5. `composer.budget-degradation`

**Purpose.** Under a token budget the composer degrades digests before it trims facts.

**Setup.** A fresh version with twenty facts `hero.k1 … hero.k20`, each with the value
`value-<i> with prose attached`; `k1..k10` at scope `book.ch1`, `k11..k20` at
`book.ch2`.

**Steps and expectations.**

1. Compose for scope `book.ch1` with no budget; note `estimated_tokens` (call it `full`).
2. Compose for scope `book.ch1` with `budget_tokens = full − 20`.
3. The degraded context's `estimated_tokens` is **at or under** the budget.
4. Its "Accepted facts" section still has **20** lines: every fact survived; the budget
   was met by degrading digests.

The reference implementation composes in-process through the kernel; the Python, .NET and
Java ports use the server's `compose_context` and assert the same two properties.

## 6. `digests.rebuilt-under-pin`

**Purpose.** A stored digest describes the head and is never served under a pin; a pinned
compose rebuilds from the pinned facts.

**Setup.** A fresh version with `hero.eyes = green` (seq 1) and `hero.home = harbor`
(seq 2), both at scope `ch1`.

**Steps and expectations.**

1. Store a head-shaped tier-0 digest for scope `ch1` whose content mentions `home`
   (built from seq 2). In-process the reference builds it with the kernel's ladder; over
   the wire the port upserts it explicitly.
2. Compose (or load the snapshot) at `as_of_seq = 1`.
3. No tier-0 digest served under the pin mentions **home**: the stored rung was not used.

## 7. `ledger.origin-round-trips`

**Purpose.** A connector claim's origin (S-4.1) survives the round trip on every backend
and both planes, and a claim proposed without one reads back without one.

**Setup.** A fresh version. An origin: `kind = connector`, `source_id = crm`,
`mapping_version = captable-holdings@1`, `row_key = holder_id=43`,
`event_position = lsn/0/1A2B`, `observed_at = 2026-08-28T09:15:00Z`,
`evidence_id = ev-batch-0001`.

**Steps and expectations.**

1. Propose `shareholder.43.shares = 90500` with that origin. The stored claim in the
   write's response carries the origin back, field for field.
2. `query.get_claim(id)`, a fresh read rather than the write's echo, returns the same
   origin.
3. Propose `shareholder.43.class = A` with no origin. The stored claim's origin is
   **absent**: the field is optional in fact, not only in type.

## 8. `gates.chronology-certain-only` (kernel scenario)

**Purpose.** The chronology gate fires only on certain violations: an uncertain date
never produces an order violation, and a certainly-late response fires the deadline rule
with the complete event chain.

**Setup.** A version with `case.filing_date = 2020-01-01` and
`hero.birth_date = circa 1950`. Rules: an order rule `birth_date` before `death_date`
(warn) and a deadline rule `response_date` within 30 days of `filing_date` (warn).

**Steps and expectations.**

1. Check a candidate `hero.death_date = 1940` against the snapshot and rules. No finding
   with `rule_id = gate.chronology-order` is produced: "circa 1950" is uncertain, so the
   order cannot be certainly violated.
2. Check a candidate `case.response_date = 2020-06-01`. A finding with
   `rule_id = gate.chronology-deadline` is produced, and its `detail.chain` has exactly
   **2** entries: the complete event chain from filing to response.

This scenario has no wire surface: chronology rules are a declarative asset the kernel
evaluates client-side of the ledger, never an API call. It is covered by the in-process
suite and is not part of a client port.
