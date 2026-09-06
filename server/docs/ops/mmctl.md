# mmctl — the operator CLI

`mmctl` is deliberately thin: every operation is exactly one REST call, so
anything mmctl does, CI can do with `curl`. That is the point — it exists
so that shapes, provider configs, and runbooks are **files you review in Git
and apply**, not state you click together. It ships inside the same ~29 MB
container as the server (source: `src/munarium-cli/src/main.rs`; std-arg-parsed,
no subcommand framework).

## Environment

| Var | Default | Purpose |
|---|---|---|
| `MUNARIUMCTL_URL` | `http://localhost:8080` | server base URL |
| `MUNARIUMCTL_TOKEN` | unset | bearer token; when unset **no** Authorization header is sent (works only against `MUNARIUM_AUTH_MODE=disabled`) |
| `MUNARIUMCTL_UID` | `mmctl` | the uid contract — sent as `x-munarium-uid` on every call |

Every request also carries a fresh `idempotency-key` (a new UUID per
request), and the HTTP client allows up to 600 s per call — runbook
operations can be slow by design.

## Commands

Everything below is the complete surface; there are no hidden flags.

```text
mmctl apply -f <file.yaml>                # kind-sniffed: Shape | ProviderConfig | Runbook | ChronologyRules
mmctl run <runbook> [--version-id V] [--watch]
mmctl approve <run-id> <step-ordinal>
mmctl get run <run-id>
mmctl runbook list
mmctl runbook info <name[@version]>
mmctl runbook validate -f <file.yaml> [--suggest]
mmctl author patterns | pattern <id>              # the dev-guide §19 catalog
mmctl author new <name> [--pattern <id>] [--seed]
mmctl author list | show <draft-id>
mmctl author answer <draft-id> -f <answers.yaml> [--no-materialize]
mmctl author validate <draft-id>
mmctl author assist <draft-id> [--description D] [--instructions I] [--provider P] [--model M] [--tier T]
mmctl author export <draft-id> --out <dir>
mmctl bundle apply -f <bundle.json> [--dir <dir>]  # hash-verified prod deploy
mmctl token issue <uid> <level> <scopes,csv> [compartments,csv]
mmctl matrix version | apply -f <yaml> | validate -f <yaml> | introspect <source> | probe <source>
mmctl matrix verify <contract> | verify-view <view> | sync <source> | reconcile <mapping> | journal [--limit N]
```

**`apply -f <file.yaml>`** — reads the file, routes by its `kind:` line
(`Shape` → `POST /v1/shapes`, `ProviderConfig` → `/v1/providers`, `Runbook` →
`/v1/runbooks`), and posts the YAML as-is. Any other kind is refused.

**`run <runbook> [--version-id V] [--watch]`** — starts a run
(`POST /v1/runbooks/{name}/runs`). `--version-id` pins the memory version.
`--watch` polls the run every 2 s, printing each state, until it leaves
`running`; when it stops at `awaiting_approval` it prints the exact
`mmctl approve` command to continue.

**`approve <run-id> <step-ordinal>`** — approves a gated step (typically the
`cutover`). The ordinal is the step's position in the runbook's `steps:`.

**`get run <run-id>`** — the run's current state and step history.

**`runbook list | info <name[@version]>`** — the registered runbooks
(`GET /v1/runbooks`, `GET /v1/runbooks/{name}`); `info` accepts a
`name@version` pin.

**`runbook validate -f <file.yaml> [--suggest]`** — server-side validation of
a local file *before* applying it: deterministic findings always;
`--suggest` adds AI review via the deployed environment's BYOK keys.

**`token issue <uid> <level> <scopes,csv> [compartments,csv]`** — mints a
short-lived capability JWT via `POST /v1/access-tokens`. Requires a
`mgmt`-role token in `MUNARIUMCTL_TOKEN`; `level` must parse as an integer,
scopes are `query`/`ingest` comma-separated, compartments optional.
Example: `mmctl token issue alice 2 query,ingest eng,field`.

**`author ...`** (2026-08-19) — the guided authoring loop over
`/v1/authoring/*` (rw role; drafts need the postgres store). `new` starts
a draft, optionally from a §19 pattern (`--seed` copies the pattern's
exemplar documents in, renamed); `answer -f` reads a YAML file of
interview answers (flat map keyed by question id — `show` prints the
questions with guidance) and re-materializes + validates the set
(`--no-materialize` stores the answers without regenerating documents —
the flag for seeded or assist-edited drafts, where re-materialization
would replace those documents);
`assist` runs the BYOK drafting pass (degrades to `assist_note` when no
provider is configured); `export --out <dir>` writes `shapes/*.yaml`,
`runbooks/*.yaml`, and `bundle.json`, re-reads what landed on disk, and
verifies every sha256 plus the manifest before reporting success.

**`bundle apply -f <bundle.json> [--dir <dir>]`** — the production deploy
path. Verifies each file hash and the manifest hash (with `--dir`, the
git-reviewed files on disk are the source and must still match), dies on
any drift ("bundle content drifted since export"), then POSTs each file
in `apply_order` — shapes first — through the same kind-sniffed routes
`apply` uses. No new server surface: CI can do the identical thing with
curl.

**`matrix …`** — one CLI for GitOps across both trees. Every subcommand is one REST call to **Munarium Matrix's** own API,
forwarded verbatim: `version` → `GET /version`; `apply -f` / `validate -f` →
`POST /v1/assets` / `/v1/assets/validate` (the YAML posted as `text/yaml`,
kind-sniffed by Matrix — DataSource, QueryContract, RecordCollection,
ObservationMapping); `introspect` / `probe` / `sync` → `POST
/v1/datasources/{name}/introspect|probe|sync`; `verify` → `POST
/v1/contracts/{name}/verify`; `verify-view` → `POST /v1/metricviews/{name}/verify`, else `/v1/dataviews/{name}/verify`
on a 404 (a metric view's or a native data view's questions, recording the
definition fingerprint); `reconcile` → `POST /v1/mappings/{name}/run`;
`journal` → `GET /v1/journal?limit=N` (default 50). It reads
`MUNARIUMCTL_MATRIX_URL` (default `http://localhost:8180`),
`MUNARIUMCTL_MATRIX_TOKEN` (a Matrix static token or JWT, sent as a bearer)
and the shared `MUNARIUMCTL_UID`. **Nothing is validated locally**: ground
rule 1 forbids a `server/` crate from depending on a `matrix/` crate, so the
plan's "reuse mxctl's validators via the Rust client" would have been exactly
that edge. Matrix validates on apply and answers with the same findings
`mxctl` prints, so the passthrough loses nothing — it just does not pretend to
know the grammar. `verify` exits **3** when any verified question fails, the
same discipline as `mxctl verify`, so CI can tell a broken contract from a
broken command.

All output is the server's JSON response, pretty-printed — there is no
table formatting to scrape around.

### `mmctl datastore …` — the derived-index tier (2026-08-30/31)

For deployment prerequisites and a complete build-to-rollout walkthrough, see
the [Datastore guide](../guides/datastore.md).

Operator commands over the datastore plane's REST routes ([../api/rest.md](../api/rest.md),
"Datastore plane"). Every command names a LOGICAL version or collection;
artifact ids appear only in output.

```text
mmctl datastore status  <index-version-id>              # artifacts + bindings with generations
mmctl datastore verify  <index-version-id>              # re-verify against stored bytes; exit 3 on a failed component
mmctl datastore rebuild <index-version-id>
mmctl datastore backfill <collection-id>                # every serving-required version; exit 3 while incomplete
mmctl datastore bind    <index-version-id> <staged|shadow> <artifact-id> [--expect <generation>] [--reason <text>]
mmctl datastore promote <index-version-id> --staged <generation> [--serving <generation>] [--reason <text>]
mmctl datastore rollout get <collection|shape> <id>
mmctl datastore rollout set <collection|shape> <id> <postgres|datastore> [--prewarm] [--expect <generation>] [--reason <text>]
mmctl datastore jobs enqueue <backfill|rebuild|direct> <target> [--max-chars N] [--watermark N]
mmctl datastore jobs get <job-id> | list | cancel <job-id>
```

The generations `bind` and `promote` expect are the ones `status` printed:
every change is a compare-and-swap against what you read. `rollout set
… datastore` is gated on the scope's serving-required completeness;
`… postgres` — the rollback — never is. A whole corpus moves by running `rollout set … datastore` (or `… postgres`
to roll back) over each of its scopes — a loop worth scripting in your own
deployment tooling, driving these same routes. The per-call token budgets (`/v1/max-tokens`,
[../tokenbudgets.md](../tokenbudgets.md)) have no `mmctl` verb yet; use the
two-line `curl` in that page.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | the call succeeded |
| `1` | anything failed — unreadable file, connection error, or a non-2xx response (the problem+json `detail` is printed to stderr as `mmctl: <status>: <detail>`) |
| `2` | usage error — unknown/missing command; the full usage text goes to stderr |
| `3` | A Matrix verification failed; Datastore verification found a failed component; Datastore backfill is incomplete; or `datastore jobs get` reports a state other than `succeeded`, `failed` or `cancelled` (inspect the returned state, including `superseded`) |
| `4` | `datastore jobs get` reports `failed` or `cancelled` |

## See also

- [../../runbooks/README.md](../../runbooks/README.md) — the sample shapes
  and runbooks, and the apply-shapes-first order.
- [../guides/platform-features.md](../guides/platform-features.md) — the
  worked walkthrough these commands appear in.
- [../security-posture.md](../security-posture.md) — what a capability token
  is (and is not); why `token issue` needs the mgmt role.
