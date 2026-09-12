# Worked example: the Halvard Instruments support assistant

A fictional equipment maker, Halvard Instruments, supports the HX-200 bench analyser in two
revisions. Its support assistant answers from manuals, service bulletins and closed tickets,
and one engineering memo is restricted. This directory is the complete lab for that
application: a disposable Server, twelve documents, a shape, a runbook, an answer key and two
scripts. The sequence below takes about fifteen minutes and needs no provider key; the last
step adds a local model.

Every command was run against the pinned 1.1.1 image while this page was written. The files:

| File | What it is |
|---|---|
| [compose.yaml](compose.yaml) | The rig: PostgreSQL with pgvector, the Server by digest, two static tokens, read-only mounts of the corpus and the three YAML assets |
| [.env.example](.env.example) | The four secrets to fill in and the host port |
| [corpus/halvard/](corpus/halvard/) | Twelve short Markdown documents under four prefixes |
| [shape.yaml](shape.yaml) | `halvard-support-documents@1`: fact schema, supersession identity, paragraph chunking |
| [runbook.yaml](runbook.yaml) | `halvard-support@1`: four collections (three at level 0, `engineering` at level 1 with a compartment), retrieval sizing, a cite-or-insufficient completion policy, five steps |
| [provider-ollama.yaml](provider-ollama.yaml) | Optional local model provider for the answer grades |
| [answer-key.json](answer-key.json) | Six graded cases and one ledger expectation. Never uploaded, never mounted |
| [lab.py](lab.py) | `ingest` (REST upload fallback), `extraction` (per-source status), `ledger` (the governance sequence) |
| [grade.py](grade.py) | The grader: a scoped caller and a fresh session per case, deterministic checks, a scorecard, `results.json` |
| [expected-results.md](expected-results.md) | What a passing run looks like |

## 1. Bring up the rig

```console
cd docs/lab/example
cp .env.example .env            # fill in four long random values; keep the file private
docker compose -p halvard-lab config --quiet
docker compose -p halvard-lab up -d
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8080/readyz     # 200 when ready
```

`LAB_HOST_PORT` in `.env` moves the host port if 8080 is taken; every URL below changes with it.

## 2. Apply the shape and the runbook

Run the container commands from PowerShell on Windows (Git Bash rewrites `/mmctl`).

```console
docker compose -p halvard-lab exec -T server /mmctl apply -f /lab/shape.yaml
docker compose -p halvard-lab exec -T server /mmctl apply -f /lab/provider-ollama.yaml
docker compose -p halvard-lab exec -T server /mmctl runbook validate -f /lab/runbook.yaml
docker compose -p halvard-lab exec -T server /mmctl apply -f /lab/runbook.yaml
```

The shape answers with `halvard-support-documents@1` and the hash of the YAML as applied. The
provider is applied even for a keyless run because the runbook's `models` block names it;
nothing calls it until a turn asks for a completion. Validation should report `"valid": true`
with no findings. The runbook answers with `halvard-support@1`.

## 3. Upload the slice

```console
docker compose -p halvard-lab exec -T server /mmctl bulk upload --dir /corpus/halvard --prefix halvard/ --label halvard-slice
```

The report ends with `"status": "completed"`, twelve stored. Note the directory: `--prefix` is
prepended to each path relative to `--dir`, so `/corpus/halvard` with prefix `halvard/` yields
`halvard/manuals/…`. If the mount is unreadable in your environment, the same upload from the host is:

```console
py lab.py ingest --base-url http://127.0.0.1:8080 --rw-token <rw>
```

## 4. Build and approve the index

```console
docker compose -p halvard-lab exec -T server /mmctl run halvard-support --watch
```

The run resolves four collections (4, 3, 4 and 1 sources), builds four index versions,
verifies each (chunks and self-probe hits), then stops at the first cutover and prints the
approve command. Approve each of the four gates in turn, checking the run between them:

```console
docker compose -p halvard-lab exec -T server /mmctl approve <run-id> <ordinal>
docker compose -p halvard-lab exec -T server /mmctl get run <run-id>
```

The run reports `done` when all four collections are active. With the index built, the
extraction check reads every source back. It needs the manifest `lab.py ingest` writes, so run
the ingest first; over an already-loaded slice it uploads nothing and reports every document as
`existed`:

```console
py lab.py ingest --base-url http://127.0.0.1:8080 --rw-token <rw>
py lab.py extraction --base-url http://127.0.0.1:8080 --rw-token <rw>
```

Every row reads `text`: Markdown sources carry no extraction fields on 1.1.1. A PDF or DOCX
source would show its status and method here, and `empty` would be the row to act on.

## 5. Grade without a model

```console
py grade.py --base-url http://127.0.0.1:8080 --mgmt-token <mgmt> --key answer-key.json --out results-keyless.json
```

Every evidence grade passes and the answer column reads `n/a`; the exit code is `0`. See
[expected-results.md](expected-results.md) for the scorecard. `results-keyless.json` holds each
case's hits, permitted collections and collections searched.

Now prove the grader fails closed. Copy the key, change case `q4`'s caller to level 1 with the
`engineering` compartment, and grade with the copy: the restricted memo appears in the hits,
`must_not_cite` fails, the row is marked as a hard fail, and the exit code is `1`.

## 6. Walk the ledger

```console
py lab.py ledger --base-url http://127.0.0.1:8080 --rw-token <rw>
py grade.py --base-url http://127.0.0.1:8080 --mgmt-token <mgmt> --rw-token <rw> --key answer-key.json --ledger --out results-ledger.json
```

The first prints the seven-step sequence from [04-memory-governance.md](../04-memory-governance.md):
accepted, disputed with `gate.ledger-conflict`, the disputed slice, the correction, canon at the
head and at sequence 1, the persisted finding. The second checks the key's `ledger` expectations
and reports `ledger PASS`.

## 7. Grade with a local model (optional)

Start Ollama on the host and pull the small model the provider file names:

```console
ollama pull qwen3:1.7b
```

(or `docker run -d --name lab-ollama -p 127.0.0.1:11434:11434 ollama/ollama` followed by
`docker exec lab-ollama ollama pull qwen3:1.7b`). The provider file points at
`host.docker.internal:11434`, which Docker Desktop resolves to the host. Then:

```console
py grade.py --base-url http://127.0.0.1:8080 --mgmt-token <mgmt> --key answer-key.json --complete --out results-fast.json
```

The answer column now carries grades. Read [expected-results.md](expected-results.md) for what
a small model does and does not pass, and why that is the measurement rather than a defect.

## 8. Tear down

```console
docker compose -p halvard-lab down -v
docker rm -f lab-ollama          # if you started one
```

## What this example does not show

- **Scale.** Twelve documents fit in twelve chunks. Competition between documents, the reason
  retrieval sizing exists, only appears on a pilot of real size.
- **Repetition.** Each case ran once. The method asks for a declared repeat policy before a number
  is trusted.
- **A key written by a domain reviewer.** These cases were written with the corpus; yours are
  written from it, by someone who did not author the runbook.
- **A held-out set.** Six cases are all development cases.
- **Model tiers.** One small local model. The tiers you ship are the ones to grade.
- **Sizing recommendations.** The chunk size, `topK` and `candidateN` are teaching values for
  short documents. [Retrieval sizing](../../../server/docs/guides/retrieval-sizing.md) is the
  arithmetic to do for your corpus.
