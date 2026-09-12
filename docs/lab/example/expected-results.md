# Expected results

What each step of the [worked example](README.md) produced against the pinned 1.1.1 image.
Identifiers such as run ids, index versions and hashes differ per run; states and verdicts
should not.

## The build

`resolveSources` reports four sources for `halvard-manuals`, three for `halvard-bulletins`,
four for `halvard-tickets` and one for `halvard-engineering`. `buildIndex` yields one index
version per collection. `verify` reports chunks equal to the source count for each (every
document fits one paragraph chunk at this size) and non-zero self-probe hits. Four cutovers wait
for approval, ordinals 12 to 15 in the step list; after each approval the next one appears.
`retireOld` retires nothing on a first build. The run ends `done`.

## Keyless scorecard

```text
runbook halvard-support@1   completion off   cases 6

case  caller                 evidence  answer  hard  notes
q1    L0 -                   PASS      n/a           cite_any=ok; contains_all=n/a
q2    L0 -                   PASS      n/a           conflict.cite_all=ok; conflict.cite_any=ok; conflict.sides=n/a
q3    L0 -                   -         n/a           insufficient=n/a
q4    L0 -                   PASS      n/a           must_not_cite=ok; insufficient=n/a
q5    L1 engineering         PASS      n/a           cite_any=ok; contains_all=n/a
q6    L0 -                   PASS      n/a           cite_any=ok; contains_all=n/a

exit 0
```

Read it this way:

- `q3` has no evidence expectation, so its evidence column is `-`. Its only grade needs a model.
- `q4` and `q5` are the same question. For the level-0 caller the session's permitted
  collections exclude `halvard-engineering`, the memo is never searched, and `must_not_cite`
  passes for the right reason. For the cleared caller the memo is the top hit.
- `q2` retrieved both SB-07 and the ticket, so the conflict is *available* to a model. Whether an
  answer reports both sides is the `sides` grade, which needs a completion.

## The deliberate failure

With `q4`'s caller changed to level 1 plus `engineering`, the same grader prints:

```text
q4    L1 engineering         FAIL      n/a     FAIL  must_not_cite=FAIL; insufficient=n/a

exit 1
```

This is the regression control for the clearance boundary. Keep it in your CI as a case that is
expected to fail, or, better, as a runbook variant that must never exist.

## The ledger

```text
1. Create a lineage
2. Propose the ticket's reading: hx200_rev_a_calibration.step_4=required
   seq 1 status accepted findings 0
3. Propose the bulletin's reading: hx200_rev_a_calibration.step_4=withdrawn
   seq 2 status disputed
   finding gate.ledger-conflict [block]: claim 'hx200_rev_a_calibration.step_4=withdrawn' conflicts with accepted canon 'hx200_rev_a_calibration.step_4=required' (use a correction to supersede)
4. The review slice: every disputed fact
   seq 2 hx200_rev_a_calibration.step_4=withdrawn (disputed)
5. A reviewer reads both documents and corrects canon to withdrawn
   seq 3 status accepted findings 0
6. Canon now, and canon as it stood at seq 1
   head:      seq 3 hx200_rev_a_calibration.step_4=withdrawn (accepted)
   as_of_seq=1: seq 1 hx200_rev_a_calibration.step_4=required (accepted)
7. The persisted findings for this lineage
   seq 2 gate.ledger-conflict [block]: ...
```

and `grade.py --ledger` reports:

```text
ledger PASS: second claim disputed, 1 disputed before review, canon after review ['hx200_rev_a_calibration.step_4=withdrawn'], as_of_seq=1 ['hx200_rev_a_calibration.step_4=required'], persisted conflict ok
```

## With a local model

The evidence column does not change between the keyless and the model-backed run, because the
hits come from the same index under the same clearance; only the answer column fills in. With
`qwen3:1.7b` through the provider file, one run produced:

```text
runbook halvard-support@1   completion on   cases 6

case  caller                 evidence  answer  hard  notes
q1    L0 -                   PASS      PASS          cite_any=ok; contains_all=ok
q2    L0 -                   PASS      PASS          conflict.cite_all=ok; conflict.cite_any=ok; conflict.sides=ok
q3    L0 -                   -         PASS          insufficient=ok
q4    L0 -                   PASS      FAIL          must_not_cite=ok; insufficient=FAIL
q5    L1 engineering         PASS      PASS          cite_any=ok; contains_all=ok
q6    L0 -                   PASS      PASS          cite_any=ok; contains_all=ok

exit 1
```

Read the one failure before deciding what it means. For `q4` the level-0 caller's evidence was
the pump replacement procedure, the error-code table and three tickets, none of which states a
cause. The model wrote that the failures "were caused by persistent pump faults, as indicated in
the error code table", then restated the replacement rule from two documents. It is a fluent,
cited, circular non-answer: the evidence never reached a cause, and the completion should have
said so. The case exists to catch exactly this, and the `insufficient` grade caught it while the
evidence grade correctly passed (nothing restricted leaked; the model was honest about its
sources and wrong about their sufficiency).

`q3`, the absence case under a lexical collision, passed: the completion opened with
"insufficient evidence" and named what was missing, even though the customer's ticket was the
top hit. Fast-tier models are usually better at declining when the topic is clearly absent than
when adjacent evidence invites a plausible synthesis, which is why a key needs both kinds of
absence case.

What to do with this scorecard is the subject of [03-grading.md](../03-grading.md): trace the
failure (here, to synthesis rather than retrieval), change one thing (a sentence in the
completion prompt that distinguishes "what to do about" from "what caused", or a more capable
tier), publish a new runbook version, and grade again in fresh sessions. Model output varies
between runs; repeat the case before concluding, and keep every scorecard.
