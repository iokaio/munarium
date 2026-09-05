# Mode C — reconcile

Observe a source, compare what it says against what the ledger already
believes, and report the disagreements. Then — only after a measured decision —
let the source write.

This is the mode with the sharpest edge in the system: it is the one that can
change canon. Everything about it is shaped to make that hard to do by
accident.

## Shadow first, always

A `ClaimMapping` declares `mode: shadow` or `mode: authoritative`, and
**declaring authoritative is not enough**. A mapping writes only when it is
*also* promoted, which is a separate, recorded operator decision.

```
mmctl matrix reconcile captable-holdings     # enqueues a pass
```

In shadow, a pass observes, compares, and files **findings**. Canon is left
byte-identical — asserted by a conformance scenario, not by intention.

## What a pass produces

| | |
|---|---|
| **observations** | typed values read from the source, sealed as an evidence artifact |
| **discrepancies** | a source value that disagrees with a current ledger claim |
| **ambiguous** | two rows in one scope resolving to one subject |
| **missing_in_source** | a subject canon names that the source does not have |
| **findings** | what the server records, each citing both evidence sides |

**`missing_in_source` fires under three conditions**, and each closes a way to
be confidently wrong: the read was COMPLETE (an incremental or truncated batch
says nothing about rows it did not return), the mapping has STANDING over the
subject, and NO row claims it at any confidence.

## Identity: declared, never computed

Aliases live in the mapping asset. Normalization folds case and whitespace
**and nothing else**.

That is a scar. An earlier experiment paid for the other choice and found "an
alias normalizer that turned a *similarity* into an equivalence class" — which
is the move that merges two people.

**Ambiguity is a tie, and a tie never merges.** An exact key match beside a
lower-confidence alias hint is a *ranking*, and producing a ranking is what a
resolver is for. Only a **contested** alias — two rows in one scope naming one
ledger subject — is ambiguous, and it is reported rather than resolved.

Contest is **scoped**: holding shares in two companies is ordinary; two rows on
*one* cap table resolving to one holder is the ambiguity.

## Promotion has two gates, checked at the decision

```
mxctl mappings promote captable-holdings --decision DEC-17 --reason "..."
```

| Gate | Default |
|---|---|
| identity precision | ≥ 0.95 |
| value conformance | ≥ 0.99 |

Both are measured against the **latest completed run** and checked **at the
moment of promotion** — not at reconcile time, where a slipping number would
silently turn writes off and on. A refusal names the gate and the numbers.

Promotion also requires: the asset declares `mode: authoritative`, it declares
at least one `authority:` scope, the latest run completed *with observations*,
and a decision id is present. A promotion that authorizes nothing is a trap for
the next reader.

## Precedence: documents outrank sources by default

`document_over_source` is the default authority rule, and it is the right
default: canon's subjects come from documents that a human wrote and a human
can read. A connector claim does not silently overwrite one.

A **rollback** claim carries the DOCUMENT's value, so it outranks the source
exactly as the original document claim did. Only `connector` is the connector's
own — a subtlety that cost a defect: a rollback counted as a connector claim
would be overwritten by the next promoted pass, undoing the operator's restore.

## Rollback is supersession, never deletion

```
mxctl mappings rollback captable-holdings --decision DEC-18
```

History is not rewritten. Rollback appends a **correction** whose value is the
prior one, and the superseded connector claim stays readable.

It groups by `(lineage, subject, property)` and files **one** correction per
chain, oldest prior restored. Undoing each link separately would leave two
current facts for one key — because `resolve_slice` returns every unsuperseded
claim, and `compare` would read whichever came first.

## Volume ceilings

`spec.limits.maxFindingsPerRun` / `maxProposalsPerRun`. A pass over its ceiling
is refused `ledger_volume_exceeded` **having written nothing**: a DRY pass
counts before the real one runs.

## What a good shadow run looks like

Findings that are *true* and few. On the committed T0 fixture the answer key is
seven findings, and the measured result is precision **1.000** / recall
**1.000**.

The bar is 1.0 deliberately: a deterministic typed comparison against a known
key has no sampling, no model and no ranking in it, so a lower bar would
license a defect.

## When it refuses

`identity_ambiguous`, `ledger_volume_exceeded`, `subject_template_unfillable`,
`promotion_gate_identity`, `promotion_gate_conformance`, `seal_failed` — see
[errors.md](../errors.md).
