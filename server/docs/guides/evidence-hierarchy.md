# The evidence hierarchy


A turn used to retrieve documents. It can now read a governed table, an exact
count, or a slice of the ledger's own facts as well — and, because those are
not the same kind of thing, it has to say which is which and what each one
can be used for.

## The governing invariant, first

**A turn that names no research profile is untouched.** Not "behaves
equivalently" — identical: the same retrieval call, the same response JSON
keys, the same SSE event sequence, the same bytes.

This is enforced by construction rather than by testing. `op_turn` branches on
the resolved profile, and the no-profile arm is the exact call that was there
before research profiles existed; `retrieve_documents` is the old inline retrieval body *moved*,
not rewritten (verified byte-for-byte against the previous commit, one line
differing: a `&doc` that became `doc` when the signature took a reference).
Four tests guard the wire: a legacy `TurnResponse` grows no `hierarchy` key, an
older client's `TurnRequest` round-trips without inventing `research_profile`,
the legacy `verify` SSE event still serializes to exactly its old bytes, and
the new stages are appended so every existing variant keeps its discriminant
index.

If you change anything in this area, that is the property to preserve.

## Three ideas

### A block is a closed set

`EvidenceBlock` has exactly five variants — `DocumentHits`, `CompleteTable`,
`Count`, `FactSlice`, `Refusal` — and consumers must handle all five. Adding a
sixth kind of evidence is a compile-checked event rather than a `_ => {}` arm
quietly ignoring it.

### A truncated block cannot support a completeness claim

`EvidenceBlock::supports_completeness()` is the whole of G4 in one predicate. A
truncated table and a refusal are the obvious noes.

**Document hits are also a no**, and that is the one worth stating: retrieval
returns the top-k it found, never a proof that nothing else exists. Treating a
good search as exhaustive is how a system says *"there are no other
contracts"* when it means *"I found three."*

### A refusal is a block, not an error

A layer that declines — policy, staleness, an unreachable source, an open
circuit — has still told the turn something, and often something the answer
must *disclose*. Providers return `Ok(Refusal)`, never `Err`, for anything the
caller should know about; `Err` is reserved for a bug. The refusal is composed
**into** the model's context, because an answer built without the register can
only say the register was not consulted if it was told.

## Declaring one

A runbook declares data views and profiles; see
[`runbooks/pipelines/diligence-hierarchy.yaml`](../../runbooks/pipelines/diligence-hierarchy.yaml)
for the worked file.

```yaml
dataViews:
  - name: revenue-register
    contract: revenue_by_region@2      # a PRE-DECLARED Matrix contract
    parameters:                        # its declared parameters, bound here
      as_of: { type: date, value: "2026-06-30" }
    accessLevel: 2

retrieval:
  defaultResearchProfile: diligence
  researchProfiles:
    - name: diligence
      layers:
        - name: register
          sources: [matrix:revenue-register]
          role: controlling
          requirement: optional
          preserveCompleteResult: true
          maxBytes: 6000
          deadlineMs: 4000
        - name: documents
          sources: [dataroom-contracts, dataroom-minutes]
          role: primary
        - name: ledger
          sources: [facts:memv-...]
          requirement: fallback
          role: supporting
```

**Layer order is the hierarchy.** Nothing declares rank; being first is what
makes the register outrank the documents when they disagree, and what gets it
the context budget first.

A source is a collection name, `matrix:<dataView>`, `facts:<version_id>`, or
`scope:<prefix>`. All of them resolve at **apply** time, so a turn cannot widen
its own reach by naming something new — and a layer's collections are further
intersected with the session's own access snapshot, which narrows and never
widens.

The model never writes SQL. It selects a pre-declared contract by name, so the
injection surface is not defended against; it does not exist.

## What fails at apply, and why it fails there

Every check in `validate_research` would otherwise fire mid-turn, in front of a
user, with money already spent.

| Refused at apply | Because |
|---|---|
| a layer naming an undeclared collection or data view | the runbook was never correct |
| a `required` + `preserveCompleteResult` layer whose `maxBytes` exceeds the context budget | **every turn it ever serves must refuse** — a contradiction in the document, caught once instead of once per paid call |
| `preserveCompleteResult` with no `maxBytes` | the check above becomes uncheckable, so the contradiction could only appear at turn time |
| a bare `facts` with no version | sessions carry no memory-version binding, so it could only ever refuse — a runbook that validates and then fails every turn is the vacuously-green trap in reverse |
| a profile of only `fallback` layers | nothing ever runs first |
| a `defaultResearchProfile` naming nothing | silently serving no hierarchy would be worse than saying so |

The same layer marked **optional** is fine in every budget case: it contributes
when it fits and stays silent when it does not. Only `required` turns a
too-large table into a guaranteed refusal.

`verifyDataViews` is a runbook **step**, not an apply-time check, because it
needs Matrix reachable and applying a runbook must not depend on a second
service being up. It is planned once per run, not once per collection. **A
runbook declaring data views with no `MUNARIUM_MATRIX_BASE_URL` fails it** —
skipping and reporting a pass is how a verification step becomes evidence of
something it never checked.

## Running one

`POST /v1/sessions/{id}/turns` with `research_profile: "diligence"`, or leave
it off and let `defaultResearchProfile` decide.

Layers run in order. A `fallback` layer runs only if nothing before it produced
evidence. Composition then fills the context in the same order, so the
highest-trust evidence goes in first, and a `preserveCompleteResult` layer is
taken **whole or dropped whole** — half a table is not a smaller true answer,
it is a false one, and a model shown nine of twelve rows will answer about
twelve.

The response carries an `EvidenceHierarchyDecision`: which profile, which
layers ran, what each produced, which refused and why, and whether any
completeness claim was permissible at all. It is deliberately about the
**decision**, not the content — no evidence rows appear in it — and it is
persisted to `session_turns.hierarchy` rather than to the interactions body,
because that capture is size-capped and it is exactly the large layered turns
whose bodies get summarized away.

Six SSE stages join the existing ones: `profile`, `layer_start`,
`layer_source`, `layer_complete`, `coverage`, `compose`. The existing `verify`
stage gained an optional `layer`.

### Two refusals

| Slug | Status | When |
|---|---|---|
| `unknown-research-profile` | 400 | the turn named a profile the pinned runbook does not declare. Fails closed: silently answering a different question than the one asked is the worse failure. |
| `required-evidence-unavailable` | 424 | a `required` layer produced nothing. **The detail names the layer, never its sources** — a caller who cannot see a source must not learn it exists from the shape of a refusal. |

## Typed assertions

A model given a sealed table will write *"revenue grew 12%"* without saying
which rows it subtracted, and no deterministic check can tell a correct
derivation from an invented one. An assertion makes the derivation *stateable*:

````
```assertions
[{"value": "900000.50", "unit": "EUR", "type": "value",
  "evidence_refs": ["evidence/ev-abc#r0001"]}]
```
````

Checked inside the existing `completion.verification` pass, on its existing
retry budget — a second loop would double what a bad answer costs, for a class
of error the one corrective re-ask already covers.

- Every `evidence_refs` entry must name a row the turn actually served.
- A **single**-ref assertion's `value` must appear verbatim in that row.

The value check stops at one reference deliberately. With two or more the value
is a derivation — a sum, a difference, a ratio — and is *supposed* not to
appear in any single row. Demanding that it did would fail every correct
aggregate, and a check that fires on correct work teaches people to switch it
off. Verifying the arithmetic needs derivation semantics, which are not built.

Values stay **text** end to end. `900000.5` and `900000.50` are the same number
and different sealed values; an `f64` anywhere in this path erases the exactness
the structured plane exists to provide. `unit` is separate for the same reason.

Row ids (`r0001`…) are assigned by position, and `served_evidence` numbers them
**exactly** as the context rendering labels them. If the two ever disagree the
checker rejects correct citations — a check that punishes the behaviour it
exists to encourage. There is a test for it.

## Operating it

`GET /v1/reports/matrix` — is the plane configured at all (distinct from
configured-and-failing), is the circuit open, what data views exist.

`GET /v1/reports/evidence?window=` — per layer: turns, refusals, refusal codes
by frequency, how often it could support a completeness claim, p50/p95.

`/admin/matrix` renders both, flagging any layer refusing on half its turns.

That flag is the point of the page. **A layer that refuses on most turns still
returns 200.** The answers get thinner, the runbook keeps claiming the layer,
and nothing else on any dashboard goes red.

Everything on that page is a **server-side** fact — how the plane behaved from
here. Matrix-side facts (sources and their posture, sync runs and checkpoints,
the budget ledger, the registry with its diffs, promotion gates, the journal)
live on **Matrix's own operator console**, and since 2026-08-30 this page links
to it — only when a URL is configured, because an `<a>` to nowhere reads as a
deployment that has one. **`MUNARIUM_MATRIX_ADMIN_URL`**
names where a *browser* reaches that console and is used verbatim; unset, the
link falls back to `<MUNARIUM_MATRIX_BASE_URL>/admin`. The two are distinct
because the base URL is the service-to-service address — on an internal
ingress or a cluster DNS name, a person cannot open it — and they coincide only
where one host serves both, as a single-host compose deployment does. Nothing is duplicated
between the two consoles, and no crate crosses the tree boundary in either
direction.

### The circuit breaker

Per instance, per provider, shared by every tenant. It trips after consecutive
failures and refuses immediately for a cool-off, so a Matrix outage costs one
timeout rather than one per turn.

A **4xx from Matrix does not trip it**: a governed refusal is Matrix working
correctly, and tripping on it would take out every other data view because one
of them is policed the way it should be.

Its metrics carry **no tenant label**. The breaker is per instance, so a
per-tenant series would report a fact that does not exist — and would let one
tenant's scrape reveal that another tenant's traffic had tripped it. A test
guards this, and it has been proven to fire.

## What a real Matrix changed

The provider was first written against a hand-made picture of Matrix's API and
was wrong on both sides. Pointing it at a running Matrix (2026-08-29, local, $0)
corrected four things, and the first is the one worth remembering:

**The turn's question never crosses into Matrix.** The intent's `semantic`
block is for bounded measure/dimension queries and its schema says in as many
words that no free-form expression crosses that boundary. Matrix executes a
**pre-declared contract with typed parameters**. That is why SQL injection is
structurally impossible here rather than defended against — and the first draft,
which posted `{"question": "..."}`, would have quietly given that up.

**A data view binds its contract's parameters**, in the runbook:

```yaml
dataViews:
  - name: revenue-register
    contract: revenue_by_region@2
    parameters:
      as_of: { type: date, value: "2026-06-30" }
```

Without this a contract with a required parameter is simply unreachable, which
is how the gap surfaced. Values are always **text**: a decimal round-tripped
through a JSON number reaches the source having lost the precision the contract
exists to keep. Binding at turn time was rejected — it would hand the caller a
knob on a query whose whole point is that it was declared in advance.

**Row ids come from the sealer.** `identity.row_id_rule` is `keys` for a keyed
result, so a row's id is its key (`EMEA`), not `r0003`. One function assigns
them for both the context rendering and the citation checker, because the model
cites the id it was *shown* and the checker resolves the id it was *given*.

**A rejected request is not an outage.** A 4xx with no typed refusal in the body
is now `source-request-rejected`, not `source-unavailable`: reporting a
malformed intent as an outage sends an operator to check Matrix's health when
the fault is in their own runbook.

The parser is now tested against the **contract's own committed examples**
rather than hand-written JSON, so it cannot drift from the contract again. Hand
fixtures are exactly how the first version came to agree with itself and be
wrong about the peer.

### Verified end to end

Server → Matrix → server, both built from this branch:

| Step | Result |
|---|---|
| Lockstep | `server lockstep confirmed server_version=1.0.0` |
| Intent accepted by Matrix's schema | yes (it was a 422 before this work) |
| Tenant check | a mismatched tenant refuses `policy_denied`, as designed |
| Typed refusals mapped into layer outcomes | `policy_denied`, `source_unavailable`, `not_covered` |
| An optional layer refusing | does not fail the turn; the document layer still answers |

**Closed 2026-08-29: a `complete_table` now flows end to end.** The blocker was
a Matrix-side defect, and not the fixture one it looked like: the production
compile scope was built from the contract's *result* columns, so `SUM(amount)`
was refused because `amount` is a source column. Query contracts now declare
`spec.reads`, and a statement that will not compile fails at apply rather than
at first execution. Measured with both services built from this branch:

```
kind: complete_table   evidence_id: ev-b097dc27...   truncated: false
columns: [region, pipeline_amount, opportunity_count]
EMEA -> ['EMEA', '2520000.50', '3']
derivations: [total_pipeline = 2520000.50 USD, scale 2]
```

and the sealed rows resolve back through `GET /v1/evidence/{id}/rows`.

## Known gaps

- **`[gap]` The fact-layer pin is per runbook, not per conversation.** Sessions
  carry no memory-version binding, so a layer names its version in the runbook.
  Two turns in one session cannot disagree, but two sessions on the same
  runbook read the same pinned version regardless of when they opened.
- **`[gap]` Conflict detection is narrow.** `disclosed_conflicts` counts
  disagreeing `Count` blocks and nothing else. Detecting that a table and a
  passage disagree is a model judgement, not a deterministic one, and guessing
  at it would manufacture disclosures nobody can check.
- **`[gap]` Derivation arithmetic is unverified.** See the single-ref rule
  above.
- ~~**`[gap]` No live `complete_table` end to end.**~~ **Closed** — the cause
  was Matrix's compile-scope wiring, not a fixture.
- **`[gap]` The document layer does not go through the `EvidenceProvider`
  trait.** `TurnResponse` carries `envelopes`, `collections_searched` and
  `skipped`, and `EvidenceBlock` has nowhere to put them, so a document layer
  forced through the trait would silently drop three response fields. A
  `DocumentProvider` was built first and deleted for exactly this reason:
  shipping it unused, so the module could claim three symmetrical
  implementations, would have made the architecture read more uniform than it
  is. The hierarchy runs documents through a closure over the real
  `retrieve_documents` instead. The asymmetry is real and deliberate.
