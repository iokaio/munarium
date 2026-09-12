# 3. Grading against an answer key

[Creating a laboratory, §4](../../server/docs/guides/creating-a-lab.md#4-write-the-answer-key-independently-of-model-output)
says what a key is: a case library written by a reviewer from the documents, recording the
caller, the required facts, the supporting passages and the forbidden assertions, kept outside
the corpus. [§10](../../server/docs/guides/creating-a-lab.md#10-score-evidence-correctness-and-usefulness-separately)
says what to score and why evidence, correctness and usefulness are separate dimensions. This
page turns those into a file and a script.

## The key as a file

[example/answer-key.json](example/answer-key.json) is the whole format. One runbook reference,
one list of cases, and optionally a `ledger` block for the governance check in
[04-memory-governance.md](04-memory-governance.md). Each case has an id, a question family, the
question, the caller's level and compartments, and an `expect` block using the graded kinds
below. JSON rather than YAML so that a grader with no dependencies can read it.

Dev-guide §21 step 8 introduced the vocabulary with three kinds; a lab needs a few more:

| Kind | What it checks | Needs a model |
|---|---|---|
| `cite_any: [paths]` | At least one listed document is among the turn's hits (`hits[].source_path`) | no |
| `cite_all: [paths]` | Every listed document is among the hits | no |
| `must_not_cite: [prefixes]` | No hit's path starts with a listed prefix. A violation is a **hard fail** regardless of anything else: it is access leakage | no |
| `conflict: {cite_all, cite_any, sides}` | Both sides' documents are retrieved (evidence), and each side's terms appear in the answer (text). An answer that quietly picks one side fails | evidence no, sides yes |
| `contains_all: [terms]` | Every term appears in the completion text | yes |
| `insufficient: true` | The completion declares that the evidence does not establish an answer. A confident answer fails. Under §17's fourth lesson insufficiency is a success state, and the negative control proves the assistant can reach it | yes |

Write the evidence expectations first. They are the ones that never need a provider, and they
locate a failure in the retrieval path before a prompt can hide it.

### The cases a first key must contain

The example's six cases are the minimum shape of a key rather than a suggestion of size:

| Case | Why it is there |
|---|---|
| A direct lookup with a qualifier (the revision B tolerance) | The ordinary case; if it fails, nothing else matters |
| A conflict between an older ticket and a newer bulletin | Both documents must be retrieved and both positions reported. This is the case that a helpful model averages away |
| An absence under a lexical collision (a customer's contract balance, where the customer's name *is* in the corpus) | Tests refusal when retrieval finds plausible hits, not refusal against an empty index |
| The same question from an unprivileged and a cleared caller | The unprivileged case must never see the restricted prefix; the cleared case must. Together they prove the clearance boundary in both directions |
| An enumeration (every pump-fault code) | A ranked sample can miss members of a set; the key states the full set |

Grow the key from the corpus's real threads, one deterministic kind per case, toward twenty and
then eighty cases for a corpus the size of a data room. Split it as §5 of the method describes:
development cases you tune against, validation cases at decision points, and a held-out set that
nobody tunes against and that is replenished when it stops being held out.

## The grader

[example/grade.py](example/grade.py) is dev-guide §21 step 8's "one page of client code", with
scoped callers added. For each case it:

1. mints a capability token for the case's caller with the `mgmt` token
   (`POST /v1/access-tokens`: uid, level, compartments, `query` scope, short TTL);
2. opens a fresh session on the key's runbook reference with that token, so the Server's own
   clearance filter decides which collections the case can see;
3. sends one turn, with `complete` on or off;
4. applies the deterministic checks to `hits[].source_path` and `completion.text`;
5. prints one scorecard row and writes the full turn response to `results.json`.

It exits `0` only when every grade that ran passed. A hard fail or any failed grade exits `1`. If
answer grades were requested and a turn came back without a completion, it exits `2` rather than
counting the missing grade as anything.

### Keyless first, always

```console
py example/grade.py --base-url http://127.0.0.1:8080 --mgmt-token <mgmt> --key example/answer-key.json
```

The evidence column is the measurement; the answer column reads `n/a` because no model ran, and
the grader never converts "did not run" into a pass. Read this scorecard before you spend
anything on a provider. If the conflict case does not retrieve both documents, no prompt will
report both positions, and the fix is in the shape or the retrieval block.

Then break it on purpose. Give the unprivileged case the cleared caller's clearance in a copy of
the key and run again: the restricted memo appears in the hits, `must_not_cite` fails, the row is
marked as a hard fail, and the exit code is `1`. A grader that has never failed has not been
tested.

### Then with a model, per tier

Apply a provider configuration ([example/provider-ollama.yaml](example/provider-ollama.yaml) for
a local model per the [Ollama guide](../../server/docs/guides/ollama.md), or one of the
[provider samples](../../server/runbooks/providers/) with a key supplied through the Server's
environment), make sure the runbook's `models` block names it, and run:

```console
py example/grade.py --base-url http://127.0.0.1:8080 --mgmt-token <mgmt> --key example/answer-key.json --complete --out results-fast.json
```

Run the key once per model tier the application will actually offer, using `--model-override`
only where the runbook's `allowOverrides` permits it, and keep each scorecard under its own name.
Success on the capable tier does not establish the quality of the fast fallback, and fast-tier
models tend to fail ceremony (naming the document, declaring insufficiency in the expected words)
rather than retrieval. When that happens the text-side checks tell you which, and the answer is
usually a prompt change and a new runbook version, not a bigger model.

A small local model will not pass every answer grade in the example. That is a measurement, not
a defect in the key: record which cases it failed and why, and compare with the tier you intend
to ship.

## Reading a scorecard

Three habits from the method, applied to the file the grader writes:

- **Trace a failure backward.** `results.json` holds the hits, the collections searched and
  skipped, the permitted collections, and the completion. A case fails at the earliest point the
  needed information went missing: not permitted, not indexed, not retrieved, retrieved but
  displaced, or retrieved but not reported. Each is a different change.
- **Keep hard fails separate from scores.** The access case is not a point on an average. One
  leak fails the candidate.
- **Report counts, not only rates**, and report the cases that could not run. Six cases passing
  is six cases; it says nothing about the seventh you have not written.

## Iterate

Each candidate changes one thing: a chunk boundary in the shape, `topK` or `candidateN` in the
runbook, a sentence in the completion prompt. Publish it as a new version, rebuild if the shape
changed, grade in fresh sessions with the same key, and keep the scorecard beside the previous
one. Write the expected effect before the run, naming the cases that should improve and the ones
that might regress; a candidate that improves the case you were looking at and regresses two
others is a finding, not a win.

Add every real failure to the development set once it is understood, and keep it there as a
regression case. Keep the held-out set held out.

Put the grader in CI against a disposable Server, so that a runbook edit which quietly harms
grounding fails a build rather than a customer. The example's compose file and scripts are
enough to do that in a workflow: up, apply, upload, run, approve, grade, down.

## Release, and run again before every build

When a candidate meets the acceptance conditions you wrote down before looking at the held-out
results, release **exactly** the documents that were measured:

1. Export the set with `mmctl author export` (or commit the files you applied) and deploy with
   `mmctl bundle apply`, which verifies every file hash and the manifest hash before posting.
   What reaches production is the validated set that left the rig, byte for byte.
2. Write the run record beside the release: image digest, shape and runbook references and
   their applied hashes, the corpus manifest, the index versions from `buildIndex`, the key
   version, every scorecard, and the decision with its exclusions.
3. Keep the previous documents and their index identities for a compatible rollback.

Then keep the rig recipe. A production index build over new documents, a Server upgrade, a model
change and a prompt change are each a reason to run the loop again, with the same key plus the
cases that reality has added since.

Next: [04-memory-governance.md](04-memory-governance.md).
