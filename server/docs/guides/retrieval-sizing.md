# Sizing a new corpus's retrieval

Written after three corpora shipped behind a chat front end on the engine
defaults and began refusing questions their own documents answered.

Read this **before** writing a runbook's `retrieval:` block, next to
[loading-corpora.md](loading-corpora.md). Those are the two halves of standing
a corpus up: getting the documents in, and sizing the search over them. The
per-knob reference lives in the dev-guide (the retrieval-knobs section that
follows §13.5); this guide is the prescription — what to set, computed from
your corpus, and why.

## The defaults are the narrow case

`RetrievalSpec::default()` is `topK: 10`, `candidateN: 50`,
`minimumShouldMatch: 1`, no `collectionSelection`, no `fusion` (so an
unweighted two-leg merge), and `completion.contextCharBudget` defaults to
16,000 characters. Those values exist to keep older runbooks byte-identical
when new knobs land. **They are a compatibility floor, not a recommendation.**

What that costs, measured:

- **`topK: 10` starves a multi-collection corpus.** The merge is a global
  fusion over the pooled candidates, so with `collections >= topK` it cannot
  give any collection more than one hit on average. Found live on the
  due-diligence page — a clean-team turn asking about change-of-control
  provisions was served one commercial document out of ninety-six — and the
  engine half was fixed (dev-guide §13.5 entry 17). The runbook half is
  `topK`, and it is yours to set.
- **An unweighted merge spends about a third of the context on noise.** The
  only embedder is `local-hash@1`, 256-dim feature hashing. Its global rank-1s
  are the shortest chunk sharing any token. Measured on the revolution corpus,
  the unweighted merge returned a 6/7/6 mix of narratives, *fragments* and
  letterbooks — **7 of 20 slots to text like `0 1424 215/3`** (§13.5 entry 18).
- **The 16,000-character default silently truncates.** Hits past the budget
  are retrieved, scored, and **reported in the response** — they just never
  reach the model. A `topK: 20` turn over 1,500-character chunks serves ten
  hits and reports twenty, and nothing in the response says so.

## Do the arithmetic before you write the block

Count your collections as `C`. Read your shape's `chunking.max_chars` as `S`.

| Knob | Rule | Why |
|---|---|---|
| `topK` | at least 20, and comfortably above `C` | at `C >= topK` the merge cannot represent every collection |
| `candidateN` | at least 10 x `topK` | the per-leg pool the fusion ranks; below `topK` the validator warns |
| `completion.contextCharBudget` | at least **`topK x (S + 60)`** | the 60 is the real per-hit overhead of the `[{collection}/{chunk_id}] {text}` entry format and its blank line |
| `completion.maxTokens` | raise past the 2,048 default (1,024 until 2026-09-02) when a reasoning-always-on model serves the runbook | hidden reasoning spends this same budget before any visible text; the truncation retry pays one 4x re-ask, so the effective ceiling is 4x the value (2026-09-01) |

The budget row is the one people skip, and skipping it makes the `topK` rise a
partial or total no-op. Worked: 20 hits over 1,500-character chunks needs
31,200; leave the default and the model sees ten hits — the same ten it saw
before you changed anything, while the response reports twenty. **A retrieval
change you cannot see in the answer is usually this.**

The bound is an upper bound. **Measure whether it binds:** run one battery
turn before and after declaring the budget and compare `input_tokens`.
Identical counts mean the hits already fit (chunks run under `max_chars` —
true of the data room and support corpora on 2026-08-30); a jump means it was
binding (patents' 1,500-character chunks: ~4k → ~8k). Declare it either way,
so a corpus change cannot make it binding silently.

**Enumerable sets set a floor of their own.** If a question can only be
answered by reading a whole set — every quarterly sheet, every contract, the
most recent of N postmortems — `topK` must exceed N with headroom, and the
budget with it. Then read §"What sizing cannot do" before relying on it.

**The output side has a ceiling too (2026-09-01).** The turn's completion
budget defaults to 2,048 tokens with one truncation-aware retry at 4x — sized
for models whose output is the answer. (It was 1,024 when the measurement
below was taken; on 2026-09-02 every per-call output budget was doubled after
z-ai/glm-5.2, the capable tier, returned no text at 1,024 + 4,096 on an
advisory question. Since the same day the default is a SETTING, not a
constant: `MUNARIUM_MAX_TOKENS_TURN_COMPLETION` on the container, or the
tenant's whole-object replacement through `POST /v1/max-tokens` — see
[../tokenbudgets.md](../tokenbudgets.md); a runbook's `completion.maxTokens`
still wins over both.) A reasoning-always-on model spends
hidden reasoning from the same budget: z-ai/glm-5.3, behind the frontier
tier, measured ~5k reasoning tokens on a hard revolution question and
returned EMPTY answer text even after the retry (out = 1,024 + 4,096, no
visible words, two runs of three). Declare `completion.maxTokens` (validated
256..=16,384; history-revolution carries 4096 → 16,384 on the retry) when any
tier a runbook serves runs such a model. It is a ceiling, not spend —
non-reasoning tiers generate what they generate — but the spending-cap
reserve estimates against it, so oversizing inflates transient holds.

## The block to start from

```yaml
  retrieval:
    topK: 20                 # >= 20 and > your collection count
    rrfK: 60
    candidateN: 200          # >= 10 x topK
    # Declaring collectionSelection is the ONLY thing that raises each
    # collection's deep candidate pool above topK (sessions_api.rs:
    # params.top_k = max(top_k, candidatePoolPerCollection)). Without it the
    # merge picks topK hits out of C x topK candidates. Selection spends the
    # deep search; it never excludes -- an unselected collection keeps its
    # probe pool in the merge.
    collectionSelection:
      maxCollections: 8      # ~2/3 of C; set it EQUAL to C when C is small
      probeCandidateN: 50
      candidatePoolPerCollection: 100
      phraseBoost: 3.0
    # local-hash@1 is a bag-of-words embedder: down-weight its leg until a
    # real embedder is indexed. The evidence leg carries the selection probe's
    # ranking into the merge as a prior and REQUIRES collectionSelection --
    # the validator warns without it.
    fusion:
      lexicalWeight: 1.0
      vectorWeight: 0.3
      collectionEvidenceWeight: 1.0
      unselectedPoolWeight: 1.0
  completion:
    contextCharBudget: 31200   # topK x (chunk max_chars + 60)
```

Two notes on `maxCollections`. Above about eight collections, selecting
roughly two thirds of them concentrates the deep search where the evidence is.
At four or five collections, **set it equal to `C`**: every collection can
matter to a single question, and what you are buying is the 100-candidate deep
pool and the evidence leg, not exclusion.

Declare `collectionSelection` whenever `C > 1`. It costs one bounded probe
round (`searchConcurrency`, default 4, in flight) and it is the only lever
that lifts the per-collection pool off `topK`.

## What sizing cannot do

Three question classes failed on every setting tried in the experiments
behind this guide. Two of the three were then closed the same day — by an engine change and a
structured path, exactly not by sizing — and the third was measured to be
indifferent to the knob this guide controls:

- **Number forms** — *closed in the engine* (dev-guide §13.5 entry 25).
  `US4436097` in the corpus is one Postgres token; `4,436,097` in a question
  is three, and `4436097` is a fourth. No knob reaches this and
  `modelQueryExpansion` forbids numbers. Since 2026-08-30 the engine
  normalizes identifier-shaped numbers into the permitted collections' own
  observed forms (always on, vocabulary-free), so chips may be written the
  way a person asks; both retired number-form chips measured 2/2 on both
  tiers and are back on their page.
- **Enumeration wider than a turn** — *closed by the register path, not by
  width* (dev-guide §13.5 entry 26). Members of a set tie on term density,
  so which twelve of twenty-six sheets arrive is arbitrary; `topK: 40`
  served 21 and 6 of them on two questions. The answer is a Matrix data
  view over a typed register with a research profile
  ([evidence-hierarchy.md](evidence-hierarchy.md)) — the turn then reads
  ALL the rows, labeled COMPLETE, and cites them individually. When you
  bind one, **declare the view's `accessLevel`/`compartments` to match the
  register's class**: the turn asks Matrix at the intersection of session
  and view, and an undeclared view asks at level 0 and is refused before
  Matrix journals anything.
- **The per-collection flood.** `collectionSelection` lifts each
  collection's pool from `topK` to `candidatePoolPerCollection`, and on a
  five-collection corpus the densest collection took 20 of 20. Measured
  2026-08-30: shrinking the pool from 100 to 40 changed *nothing* for the
  flooded question — still 20 of 20, both tiers — because the flood is a
  ranking property (one collection uses every word of the question), not a
  pool-size one. Keep the pool modest (2×`topK` is still a fine default,
  and 40 lost nothing anywhere), but do not expect it to restore
  diversity; a per-collection quota in the merge would be an engine
  change, and it has not been approved.

## What not to copy from history-revolution

That runbook is the most heavily tuned in the tree, and three of its settings
are **specific to a many-shard corpus of tens of thousands of documents**.
Copying them into a small one makes things worse, quietly:

- **`minimumShouldMatch: 2`** requires a chunk to hold two of the query's
  lexemes before it can enter the lexical candidate pool. It exists to stop an
  OR query scanning most of a newspaper shard. A few-hundred-document corpus
  has no such scan cost, and the knob can only *remove* candidates — fatally
  where questions are keyed to one distinctive token (a patent serial, a CVE
  id, an incident number): the chunk holding the serial and little else is
  exactly what it drops.
- **`stopTermFraction`** learns per-shard stop words from corpus frequencies.
  On a small corpus there is nothing to learn and candidates to lose.
- **`contentDemotions`** there targets `**Text:** none (metadata record)`, an
  artifact of Library of Congress catalog records. Write a demotion only for a
  marker your own corpus actually carries — and give it `exceptCollections`
  for any collection where the marked record *is* the content.

## What costs money

`modelQueryExpansion` adds a fast-tier model call to **every turn**. It earns
its place on corpora with vocabulary drift — archaic spelling, OCR noise, a
subject the sources name differently than a user would. Add it on a
measurement, never speculatively, and pair it with
`models.tasks.query_expansion: { provider: default, tier: fast }` and
`queryExpansionWeight` (below about 0.7 a record's header out-ranks the body
passage that actually answers).

## A decline is a retrieval signal, not a prompt bug

Every corpus prompt in this tree ends with some form of *answer only from the
served text; if it does not answer, say so*. That rule is load-bearing, and it
has a consequence worth stating plainly:

**A capable model obeys it. A fast model often does not.** Nothing in the turn
pipeline varies by tier — `resolve_model` picks a model id and nothing else;
context budget, retrieval, round count and the corrective retry are identical
— so when a page refuses more on the capable tier than the fast one, the
capable model is *reporting your retrieval accurately* while the weaker one
answers from whatever fragments it was handed.

So: **treat "the documents do not support an answer" as a retrieval
measurement.** Read the turn's `hits` and which collections they came from
before touching the prompt. Softening the grounding rule to make a chip pass
trades a false refusal for a fabrication — and it will also make the
out-of-corpus control chip answer, which is the failure that control exists to
catch. That has happened here once already.

## Earn the chips

A chat front end's example questions ("chips") are a claim that the system
answers those questions. Make the claim true before shipping it:

1. Write a battery — a script in your own repository that drives
   `POST /v1/sessions/{id}/turns` — with the chips copied verbatim from the
   page and an expected class per question (`answers` / `declines`).
2. Include **one deliberately out-of-corpus question**, ideally adjacent to
   one the corpus does answer. Without it a battery cannot tell a working
   system from one that answers everything.
3. Run it on **both tiers** — `-Tier capable` and `-Tier fast`. They fail
   differently, and only one of them is telling you the truth about retrieval.
4. Record the result somewhere committed. A number that lives only in a
   terminal is not a measurement anyone else can check.

## Pre-flight

```powershell
mmctl runbook validate -f runbooks/applications/<name>.yaml --suggest
mmctl apply -f runbooks/applications/<name>.yaml
```

Changing only `retrieval:`, `completion:` or `models:` is **query-time — no
reindex, no run, no cutover.** Collections resolve by name, independent of the
runbook version, so nothing has to be rebuilt. Two things to know:

- **A session pins its `runbook_ref` when it is created**, so an already-open
  browser tab keeps the old configuration. Verify in a fresh session.
- **Applying is a deploy step of its own.** Runbooks live only in the
  `runbooks` table; rolling an image applies nothing. Script it: read the rw
  token (mgmt is the wrong role), validate server-side, apply, read back, and
  refuse to downgrade a version. When `apply` answers *shape not found*,
  apply the committed shape first and retry — a fresh database has an empty
  registry.
- If a front end of yours republishes runbook YAML (as a download, say),
  **apply first, then redeploy the front end.** Applying is what changes the
  server's behaviour — a code deploy does not, because runbooks live in the
  `runbooks` table and nothing reads them from the image's filesystem. Do it
  in that order and the worst case is a page offering a slightly older file
  than the behaviour it describes; do it backwards and the page hands
  visitors a runbook describing tuning the live server is not running.

The whole-tree test `cargo test -p munarium-runbooks --test sample_runbooks`
parses and validates every committed runbook. Run it with `-- --nocapture`:
cargo hides `println!` for a passing test, so the validator's warnings are
invisible otherwise.

## Worked example: the 2026-08-29 regression

Three corpora were promoted into a chat front end with one line changed each
(`allowOverrides`), inheriting every default above. What that produced:

| | advisory | patents | intel |
|---|---|---|---|
| collections (`C`) | 14 | 5 | 4 |
| chunk `max_chars` (`S`) | 1200 | 1500 | 1000 |
| shipped `topK` | 10 | 10 | 10 |
| `C >= topK`? | **yes** | no | no |
| vector leg | 1.0 (noise) | 1.0 | 1.0 |
| per-collection pool | 10 | 10 | 10 |

Advisory is the clearest case: the front end ran it at clearance level 3 with
every compartment, so all fourteen collections are permitted on every turn and
`topK: 10` could not represent them. Patents and intel were starved a
different way — with no `collectionSelection` the merge chose ten hits from
five (or four) pools of ten, for questions that need a target draft, an office
action, a prior-art patent and an assessment at once, or two vendors' reports
side by side.

The fix was this guide's arithmetic and nothing else — and it fixed **one of
the seven** failing chips (advisory's net-worth question, at `topK: 40`).
Intel was already answering; the other six fail for the reasons in "What
sizing cannot do". Measured on both tiers, final harness: advisory 4/6,
patents 3/6, intel 6/6, support 3–4/6, dataroom 6/6, every out-of-corpus
control still declining. The experiment behind these numbers also found three
defects in its own check script, which had been reporting GREEN over wrong
answers — one more reason the out-of-corpus control question is not optional.
