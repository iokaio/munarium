# 4. Memory governance: what the ledger does with a contradiction

Retrieval grading asks whether the right evidence reached an answer. Memory governance asks
what the system does when the evidence disagrees with what it already holds. Munarium's ledger
is append-only: a fact that conflicts with accepted canon is not dropped, averaged or
overwritten. It is recorded as **disputed**, with a finding that names both values and both
claims, and it waits for a reviewer. A lab should exercise that path on its own corpus, because
the contradictions that matter are the ones the documents actually contain.

This page is optional on a first pass. Come back to it once the retrieval scorecard is green.

## The mechanics, on the example's conflict

The worked corpus contains one planted disagreement. Ticket T-1042 (2025) tells a technician to
perform step 4 of the revision A calibration; service bulletin SB-07 (2026) withdraws that step.
The helper deliberately proposes the ticket's reading before the bulletin's reading.
[example/lab.py](example/lab.py) `ledger` runs the sequence and prints every response:

```console
py example/lab.py ledger --base-url http://127.0.0.1:8080 --rw-token <rw> --key example/answer-key.json
```

1. **Create a lineage.** `POST /v1/versions` with an `Idempotency-Key`, as every core write
   requires. The response is a `version_id`.
2. **Propose the ticket's reading.** `POST /v1/versions/{id}/claims` with
   `{"claim_type": "fact", "subject": "hx200_rev_a_calibration", "key": "step_4", "value": "required"}`.
   Status `accepted`, sequence 1, no findings. The key discipline matters here: a folded subject,
   a dot-free key and a brief value are what make the next write collide instead of landing under
   a different spelling.
3. **Propose the bulletin's reading.** The same subject and key, value `withdrawn`. HTTP 200,
   sequence 2, status **`disputed`**, and one finding:

   ```text
   gate.ledger-conflict [block]: claim 'hx200_rev_a_calibration.step_4=withdrawn' conflicts with
   accepted canon 'hx200_rev_a_calibration.step_4=required' (use a correction to supersede)
   ```

   The finding's `detail` carries `canon_claim_id`, `canon_seq`, `canon_value` and
   `proposed_value`. Nothing was lost. The disagreement is now a recorded, reviewable event.
4. **The review slice.** `GET /v1/versions/{id}/facts?statuses=disputed` lists every disputed
   fact in the lineage. This query is the review queue; an application renders it, a reviewer
   works it.
5. **Correct through the front door.** The reviewer reads both documents and decides the
   bulletin governs. `POST …/claims` with `claim_type: correction`, the new value, and
   `supersedes_id` set to the id of the accepted claim from step 2. Sequence 3, status
   `accepted`, no findings.
6. **Read canon now, and as it stood.** `GET …/facts` answers `withdrawn` at the head.
   `GET …/facts?as_of_seq=1` answers `required`, with the response naming both `as_of_seq` and
   `head_seq`. Every query route accepts the pin, and it is how an application answers "what did
   we know when that memo went out".
7. **The findings persist.** `GET /v1/versions/{id}/findings` returns the gate finding with its
   sequence, so the audit does not depend on whoever read the write response.

## Grading governance

The example key's `ledger` block states the expectations, and `grade.py --ledger --rw-token <rw>`
runs the sequence and checks them:

| Expectation | Why |
|---|---|
| The first proposal and the correction are accepted | Both writes established canon |
| The second proposal lands `disputed`, not `accepted` and not rejected | The gate caught the contradiction and kept it |
| Exactly one disputed fact before review, naming the second claim | The review queue has the case, and only the case |
| Canon after the correction contains only the reviewed value | Supersession removed the old value from current canon |
| The pinned read returns only the first value at the first claim's sequence | The old canon remains readable after correction |
| A `gate.ledger-conflict` block finding appears in the write response and persists at the second claim's sequence | The audit survives beyond the write response |

"Disputed is a success status." A lab that grades governance is looking for the block finding;
its absence on a planted contradiction is the failure.

For your own corpus, plant the contradictions the key already names in the retrieval cases (the
conflict family in [03-grading.md](03-grading.md)) and extract their facts with the subject and
key discipline above. A contradiction the ledger did not catch is almost always a spelling: two
subjects for one entity, or a key with a dot in it. This helper submits explicit ledger claims
to a new lineage; it does not extract claims from the documents or attach the collection's
shape to that lineage. It exercises the ledger gates independently of the retrieval index.

## Where to go next

- [Dev-guide §18](../../server/docs/guides/dev-guide.md#18-beyond-rag-canonical-memory-over-the-corpus)
  for canonical memory over a corpus: the five claim verbs, anchors, promises and digests.
- [Dev-guide §21 step 6](../../server/docs/guides/dev-guide.md#step-6-the-red-flag-act-one-planted-conflict-caught)
  for the same sequence over a full data room, with the review queue as the application's
  product.
- [Chronology rules](../../server/docs/api/rest.md) (`POST /v1/chronology-rules`) to arm the
  sixth gate when the order of events is part of what your corpus asserts.
- [Evidence hierarchy](../../server/docs/guides/evidence-hierarchy.md) to place pinned ledger
  facts, documents and structured records in a declared trust order on a turn.

Back to the [overview](README.md), or run the whole loop in the [worked example](example/README.md).
