# 1. A disposable Server and a slice of the corpus

A lab measures a Server that exists for the measurement. It has its own database, its own
tokens, and a corpus you can describe completely. When the run is over you delete it, and the
record you kept is what remains.

## The rig

The Compose file in [example/compose.yaml](example/compose.yaml) is the persistent setup from
the [repository README](../../README.md#persistent-storage-with-postgresql) with four deliberate
differences:

| Difference | Why |
|---|---|
| The image is pinned by digest, not by tag | The record of a run names exact bytes. Copy the current digest from the [publication record](../../server/CONTAINER.md#versions-and-verification) when you start a new lab |
| Two static tokens, `rw` and `mgmt`, in one tenant | `rw` applies documents, runs and approves the build, and uploads. `mgmt` mints the scoped capability tokens each graded case runs under; it cannot approve a run, and `rw` cannot mint. Both are needed |
| Only `./corpus` and the three YAML assets are mounted, read-only, each named individually | The answer key sits beside them on the host and is not reachable from the container. That boundary is the whole point of the key |
| A Compose project name of its own (`-p halvard-lab`) and loopback-only ports | Two labs on one machine never share a volume, and nothing outside the machine can reach the rig |

Create `.env` from [example/.env.example](example/.env.example) with four long random values,
then:

```console
docker compose -p halvard-lab config --quiet
docker compose -p halvard-lab up -d
```

Wait for `GET /readyz` to answer 200, then prove the rig with one authenticated write and read
exactly as [Getting started, step 3](../../server/docs/guides/getting-started.md#3-make-an-authenticated-ledger-write)
does. A rig that cannot keep a fact is not a measurement instrument, and the check costs ten
seconds.

Record three things before loading anything: the image digest, the compose file as applied,
and the tenant name. They go in the run record described in [03-grading.md](03-grading.md).

### Using `mmctl` inside the container

The image is a distroless binary with no shell, so `mmctl` is invoked directly:

```console
docker compose -p halvard-lab exec -T server /mmctl runbook list
```

The compose file sets `MUNARIUMCTL_URL`, `MUNARIUMCTL_TOKEN` (the `rw` token) and
`MUNARIUMCTL_UID` on the server container, so every `exec` call is already authenticated. On
Windows, run these commands from PowerShell: Git Bash rewrites the container path `/mmctl` into a
Windows path before Docker sees it (setting `MSYS_NO_PATHCONV=1` is the alternative).

### Spending limits

A keyless lab spends nothing. Before you add a provider, set the per-call output ceilings the
runbook does not already fix (`GET`/`POST /v1/max-tokens`, see [token budgets](../../server/docs/tokenbudgets.md))
and give the provider configuration a `budgets` block. The example's
[provider-ollama.yaml](example/provider-ollama.yaml) shows the shape. Treat a provider key in a
lab the way [Managing keys and secrets](../../server/docs/guides/managing-key-and-secrets.md)
describes: separate from production, revocable, never in a YAML document.

## The corpus slice

[Loading corpora](../../server/docs/guides/loading-corpora.md#start-small) names three scales.
A lab starts with the first:

| Scale | What it is for |
|---|---|
| A slice: a dozen documents that reach every collection binding | Proves the binding contract and the grader, in seconds, on the all-PostgreSQL rig |
| A representative pilot, tens of megabytes | Where retrieval settings are tuned against real competition between documents |
| The full corpus | Reveals coverage gaps and operating cost that samples hide; needs object storage |

Choose the slice by what the key needs, not at random. The worked example's twelve documents
exist because each graded kind needs a case: two revisions of one procedure, a bulletin that
withdraws a step, a ticket that still recommends it, a customer name that collides with an
unrelated question, and one restricted memo. When you sample your own corpus, keep evidence
relationships whole: an amendment without its base document is a different test.

### The prefix layout is the access layout

A document's filename is its identity, its blob path, and the string collections bind by
literal prefix match. Levels and compartments attach to collections, and collections bind
prefixes. So the directory tree you upload *is* your collection topology and your clearance
topology. [Dev-guide §16, "Prefix design is access design"](../../server/docs/guides/dev-guide.md#16-designing-retrieval-for-a-corpus)
is the full treatment; the consequence for a lab is that you design the tree before you upload,
and you record it:

```text
halvard/manuals/       level 0
halvard/bulletins/     level 0
halvard/tickets/       level 0
halvard/engineering/   level 1, compartment `engineering`
```

Write a manifest first: filename, sha256, document family, revision, date, and whether it is
restricted. `lab.py ingest` writes one as it uploads; for a bulk session the server's own
completion report is the manifest of record.

### Uploading

The recommended path is a bulk session, run from the mounted directory:

```console
docker compose -p halvard-lab exec -T server /mmctl bulk upload --dir /corpus/halvard --prefix halvard/ --label halvard-slice
```

`--prefix` is prepended to each file's path relative to `--dir`, so the directory you name is the
one *below* the prefix. Naming `/corpus` here would store every file under `halvard/halvard/…`,
which binds to nothing; the runbook's `resolveSources` step reports that as a collection with no
sources rather than failing silently. The session is idempotent per document: rerunning it over
an already-loaded slice uploads zero bytes.

If the container cannot read the mount in your environment, the same upload runs from the host
through `POST /v1/ingest`:

```console
py example/lab.py ingest --base-url http://127.0.0.1:8080 --rw-token <rw> --corpus example/corpus
```

Either way, check what bound. A document that reports an empty `bound_to` did not match any
collection prefix, and the fix is in the runbook or the path, never in the grader.

### Check extraction before you measure retrieval

Text and Markdown are stored as they are; PDF and DOCX are extracted at index time, and a
scanned page with no text layer reads `empty`. After the first build, read each source back:

```console
py example/lab.py extraction --base-url http://127.0.0.1:8080 --rw-token <rw>
```

On Server 1.1.1 a text or Markdown source's record carries no extraction fields, and the helper
reports it as `text`. Where a status is reported, `empty` is the one to watch: that document is
in the index in name only, and no retrieval setting recovers it. Fix the document, or record the
limitation in the key as an unanswerable case.

## The answer key stays out

Three mechanisms keep the key outside the retrieval index, and a lab uses all of them:

1. The key lives beside the corpus directory, not inside it, and is not mounted. In the example,
   `answer-key.json` sits at the top of `example/` and only `example/corpus` reaches the
   container.
2. No runbook prefix covers the key's path. `mmctl runbook validate` raises
   `sources.prefix-mismatch` for a binding outside the declared root; the guided authoring
   set-check raises `set.answer-key-filename` when a binding looks like a key (`answer-key`,
   `ground_truth`, `seeded_findings` and similar segments).
3. The grader is the only reader. It sends questions and reads hits and text; it never sends the
   expected answer anywhere.

A system that can retrieve its own answer sheet will look excellent and teach you nothing about
the shape or the runbook.

## Teardown

```console
docker compose -p halvard-lab down -v
```

`down` alone keeps the database volume, which is useful between candidates. `down -v` deletes it.
Delete the rig when the run record is written, not before: the record names index versions and
run ids that only this database can still explain.

Next: [02-shape-runbook-index.md](02-shape-runbook-index.md).
