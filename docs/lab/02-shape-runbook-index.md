# 2. Shape, runbook, index

The documents under test are two YAML files. A **shape** says how a class of sources is
represented: the fact schema, the supersession identity, the chunking and the indexing
parameters. A **runbook** says how an application uses collections governed by shapes: which
prefixes bind to which collection at which level, how retrieval is sized, what the completion
policy is, and the five steps that build and activate an index. Every change to either is a
new version measured against the same key. The question a lab asks is whether *these two
files* produce the evidence your application needs.

## Two starting points

**Copy the nearest sample.** [server/runbooks/README.md](../../server/runbooks/README.md) lists
thirteen application runbooks and the modelling decision each demonstrates, and the developer
guide's Appendix E cross-references them to the seven application patterns of §19. Pick the row
whose corpus is most like yours, copy its shape and runbook, and change the prefixes and names.
The example's [shape.yaml](example/shape.yaml) and [runbook.yaml](example/runbook.yaml) were
built this way from the getting-started starter and the `customer-support` sample.

**Or let the server interview you.** The guided authoring surface asks the §16 questions in the
order they are hard to revise and materialises a shape and runbook from the answers:

```console
docker compose -p halvard-lab exec -T server /mmctl author patterns
docker compose -p halvard-lab exec -T server /mmctl author new halvard-support --pattern ask-the-corpus
docker compose -p halvard-lab exec -T server /mmctl author answer <draft-id> -f /lab/answers.yaml
docker compose -p halvard-lab exec -T server /mmctl author export <draft-id> --out /tmp/out
```

`author answer` validates the whole set, including checks one document cannot make alone. One
of them, `set.answer-key-filename`, is worth triggering on purpose once: declare an area named
`ground_truth/` in the interview answers and watch the set-check refuse to let a key become a
collection. Drafts need the PostgreSQL store and the `rw` role, so they work on this rig.
[Dev-guide §21B](../../server/docs/guides/dev-guide.md#21b-creating-runbooks-and-shapes-the-guided-authoring-path)
walks the whole path with captured output.

Whichever start you choose, the files land in your application repository. Git is the source of
truth for what was tested; the Server holds what was applied, and the two are reconciled by hash
at release time.

## Validate before you apply

```console
docker compose -p halvard-lab exec -T server /mmctl runbook validate -f /lab/runbook.yaml
```

The deterministic findings are free and always run. `--suggest` adds a model review through the
rig's configured provider and costs a completion; leave it off until the deterministic findings
are clean. A runbook with `"valid": true` and an empty `findings` list is ready. A warning is
worth reading before you decide it is acceptable: `collections.uniform-access` on a public corpus
is honest modelling, while a prefix that does not end in `/` is almost always a mistake.

## Apply shapes first, then the runbook

A collection cannot bind to an unpublished shape, so the order is fixed:

```console
docker compose -p halvard-lab exec -T server /mmctl apply -f /lab/shape.yaml
docker compose -p halvard-lab exec -T server /mmctl apply -f /lab/provider-ollama.yaml   # only if the runbook names a provider
docker compose -p halvard-lab exec -T server /mmctl apply -f /lab/runbook.yaml
```

`apply` routes by the file's `kind:` line and returns the reference it registered
(`halvard-support-documents@1`, `halvard-support@1`) with the hash of the YAML as applied. Keep
those hashes: the release record compares them with the files in git.

Apply the runbook **before** uploading the corpus if you want `bound_to` populated on each
ingest response; a document uploaded earlier still binds at `resolveSources` time.

## Run, read, approve

```console
docker compose -p halvard-lab exec -T server /mmctl run halvard-support --watch
```

Every application runbook runs the same five steps once per collection: `resolveSources`,
`buildIndex`, `verify`, a gated `cutover`, and `retireOld`. `--watch` polls until the run stops at
the first `awaiting_approval` step and prints the exact `mmctl approve <run-id> <ordinal>` to
continue. Before approving, read the steps that ran:

| Step | What to check |
|---|---|
| `resolveSources` | `sources` equals the number of documents you expect in that collection. Zero means the prefix and the upload path disagree |
| `buildIndex` | An `index_version` was produced. Record it; the grader's hits carry it in every envelope |
| `verify` | `chunks` and `self_probe_hits` are non-zero. A collection with chunks but no self-probe hits is worth a look before it goes live |

Approve each cutover with the `rw` credential. In the example there are four gates, one per
collection, and `mmctl get run <run-id>` shows the next one after each approval. Approval is a
deployment decision, not a button to get past a failed build: a run that reached `failed` is
diagnosed, not approved.

When the run reports `done`, confirm the application sees what you built:

```console
docker compose -p halvard-lab exec -T server /mmctl runbook info halvard-support@1
```

## What a change costs

Every change to extraction, chunking or indexing in the shape, or to a binding in the runbook,
owes a rebuild: publish the new version, run it, approve it, and grade it in **fresh sessions**.
A session is pinned to the runbook version it was created with, so a grader that reuses sessions
after a change measures the old index. The example grader opens a new session for every case
for exactly this reason. Retrieval settings (`topK`, `candidateN`, `contextCharBudget`) and the
completion prompt live in the runbook and also require a new version; they do not require a
rebuild, but they do require new sessions.

[Dev-guide §21A](../../server/docs/guides/dev-guide.md#21a-operating-runbooks-publish-run-gate-evolve-retire)
covers publish, run, gate, evolve and retire from the operator's seat, including how retirement
is a double-pass soft removal rather than a delete.

Next: [03-grading.md](03-grading.md).
