# Using the platform features: users, tokens, collections, runbook applications, sessions

**Audience:** two people. The **operator** (publishes shapes, authors and runs
runbooks, watches reports) and the **API-management-layer integrator** (the
service in front of munarium-server that authenticates humans, mints capability
tokens, and forwards end-user calls). If you are building a chat UI or agent
on top, you talk to the API manager — not to munarium-server directly.

Everything below is copy-pasteable against a local dev rig. Companion
references: [../api/rest.md](../api/rest.md) (routes),
[../api/errors.md](../api/errors.md) (problem slugs),
[../security-posture.md](../security-posture.md) (why auth works this way),
[../ops/index-deletion-runbook.md](../ops/index-deletion-runbook.md) (the
only way index data is ever physically deleted).

---

## 1. The five ideas in two minutes

1. **uid** — every API call names the end user it acts for
   (`X-Munarium-Uid` header). munarium-server does not authenticate users — the API
   manager does — but every log line, interaction record, and report is
   uid-attributed.
2. **Capability tokens** — the API manager exchanges its `mgmt` credential
   for a short-lived JWT carrying an **access level** (integer, higher sees
   more), optional **compartments** (need-to-know tags), and **scopes**
   (`query` and/or `ingest`). munarium-server verifies these locally; nothing
   else about identity lives here.
3. **Collections** — indexes are separate, compartmentalized data
   collections. Each carries an `access_level` + `compartments` requirement;
   physically each is its own PostgreSQL partition with its own vector/text
   indexes. **No API can delete one.**
4. **Runbooks (v2)** — a runbook is a versioned *retrieval application*: the
   collections it spans, which uploaded files feed each collection, retrieval
   knobs, default models per task, and an optional RAG completion step. The
   same 5-step pipeline (`resolveSources → buildIndex → verify → cutover →
   retireOld`) now runs once per collection, with per-collection approval
   gates.
5. **Sessions** — a multiturn conversation over one runbook. The session pins
   the runbook version and snapshots the token's clearance at creation; every
   turn searches only the collections that clearance permits. One runbook
   serves every clearance level simultaneously — users simply see different
   slices.

## 2. Server setup (operator)

```bash
MUNARIUM_STORE=postgres
MUNARIUM_DATABASE_URL=postgres://munarium:munarium-dev@localhost:5433/munarium
MUNARIUM_AUTH_MODE=static
#                          ┌ control plane      ┌ management plane (the API manager)
MUNARIUM_STATIC_TOKENS="opstoken:acme:rw,mgmttoken:acme:mgmt"
MUNARIUM_TOKEN_SECRET="a-random-secret-of-at-least-32-bytes!!"   # enables capability tokens
# optional:
MUNARIUM_TOKEN_TTL_SECS=3600            # default token lifetime (cap 24 h)
MUNARIUM_REQUIRE_UID=true               # the default; false only for single-user dev rigs
MUNARIUM_TOKEN_REVOCATION_CHECK=false   # true = deny-list lookup on every JWT verify
# optional, 2026-09-02 — the per-call output-token ceilings (docs/tokenbudgets.md);
# unset = the built-ins, set = must parse and sit in range or the server refuses to start:
MUNARIUM_MAX_TOKENS_TURN_COMPLETION=2048   # a session turn's answer (the retry pays 4x)
MUNARIUM_MAX_TOKENS_QUERY_EXPANSION=256    # the modelQueryExpansion call
MUNARIUM_MAX_TOKENS_COMPLETE_DEFAULT=1024  # /v1/providers/{name}/complete without max_tokens
MUNARIUM_MAX_TOKENS_HEALTHAI_PROBE=512     # each /healthai probe
#   … and HIERARCHY_CLASSIFIER=32, HIERARCHY_INTENT=480, RUNBOOK_ADVISORY=2048, AUTHORING_ASSIST=8192
```

Roles are deliberately partitioned: `rw` runs the control plane but cannot
mint tokens; `mgmt` mints tokens and reads reports but cannot write the
ledger; `ro` reads. A leaked credential is bounded to its plane.

Every example below sends the uid header. For operator calls that is the
*operator's* id — operators are users too, and the audit trail records them:

```bash
H_OPS=(-H "Authorization: Bearer opstoken" -H "X-Munarium-Uid: casey.ops")
H_MGMT=(-H "Authorization: Bearer mgmttoken" -H "X-Munarium-Uid: api-manager")
```

Send the header always. The server allows exactly one substitution: when the
bearer *is* a capability JWT, a missing header falls back to that token's
`sub` (the header is redundant there — it can only agree or be rejected as
`uid-mismatch`). With a static token and no header the call is always
`400 uid-required` under the default `MUNARIUM_REQUIRE_UID=true`.

## 3. Author the retrieval application (operator)

### 3.1 Publish a shape, then the runbook

```bash
curl -X POST localhost:8080/v1/shapes "${H_OPS[@]}" --data-binary @- <<'EOF'
apiVersion: munarium.ioka.io/v1
kind: Shape
metadata: { name: docs, version: 1 }
spec:
  fact:
    schema: { type: object }
EOF
```

`field-support.yaml` — a two-clearance retrieval application:

```yaml
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: field-support, version: 1 }
spec:
  collections:
    - name: public-docs            # visible to everyone (level 0)
      shape: docs@1
      accessLevel: 0
      sources: { filenamePrefix: "public/", mediaTypes: [text/plain, text/markdown] }
    - name: internal-eng           # level 2 AND the 'eng' compartment
      shape: docs@1
      accessLevel: 2
      compartments: [eng]
      sources: { filenamePrefix: "eng/" }
  retrieval: { topK: 8, rrfK: 60, candidateN: 50 }
  models:
    default: { provider: default, tier: capable }       # fallback for the tasks below
    tasks:
      completion: { provider: default, tier: capable }  # session-turn RAG answers
      validation: { tier: fast }                        # AI runbook review
    allowOverrides: [default]      # which providers a caller may request instead
  completion:
    promptTemplate: |
      Answer using only the context.
      {context}

      Q: {query}
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
```

Key spec decisions:

- **Source bindings are declarative.** `filenamePrefix` and `mediaTypes` AND
  together when both are present; an explicit `contentHashes:` list ORs in.
  Every `resolveSources` step (and every ingest, §5) re-evaluates them, so
  newly uploaded files flow to the right collections automatically.
- **`models:`** sets the default provider/model per task level. Two levels are
  resolved today: `completion` (session-turn RAG, §6) and `validation` (the AI
  review pass, §3.2). A third level, `embedding`, is a legal key that validates
  cleanly but is **not consumed yet** — index builds always use the built-in
  deterministic `local-hash@1` embedder, so declaring it changes nothing.
  `tier: fast|capable|frontier` resolves through the provider config's tier map
  (falling back to a built-in per-family table); an explicit `model:` pins one.
  `allowOverrides` governs API-level overrides: `false` (the default when
  omitted), `true`, or a provider allowlist.
- **`completion:`** is optional. Without it the runbook is retrieval-only
  and turns return hits + provenance; clients compose their own answers.
  Its `maxTokens` (validated 256..=16,384) is the one place a runbook
  overrides the per-call output ceiling the server otherwise takes from
  its token budgets (§7): declare it when a reasoning-always-on model
  serves the runbook, since hidden reasoning spends the same ceiling.
- **`execution:`** (optional, default `{ order: stepMajor }`) decides how the
  plan is flattened across collections, which is really a decision about how
  much work one HTTP request carries. Only `cutover` can pause a run, so
  step-major executes resolveSources + buildIndex + verify for *every*
  collection before the first gate — fine for a 13-collection data room
  (3.9 s), impossible for a 530 MB archive, where the request times out and
  the disconnect wedges the in-flight step. Declare
  `execution: { order: collectionMajor }` to walk each collection through all
  its steps in turn: the first request stops at collection 1's cutover and
  each approval builds exactly one more collection. Balance such collections
  by BYTES, not document count — build cost tracks chunks.

### 3.2 Validate before you apply

```bash
curl -X POST "localhost:8080/v1/runbooks/validate" "${H_OPS[@]}" --data-binary @field-support.yaml
# → {"valid": true, "findings": []}
```

Deterministic findings catch step-order mistakes (`cutover` before
`buildIndex` is an error), duplicate collection names, bad retrieval ranges,
unknown model task levels, uniform access levels (info: your
compartmentalization does nothing), and more. Add `?suggest=true` for an
AI review pass through the runbook's `validation` model — advisory only,
and it degrades to a `suggest_note` when no provider key is configured:

```bash
curl -X POST "localhost:8080/v1/runbooks/validate?suggest=true" "${H_OPS[@]}" --data-binary @field-support.yaml
```

Or from the CLI: `mmctl runbook validate -f field-support.yaml --suggest`.

### 3.3 Apply, run, approve

```bash
curl -X POST localhost:8080/v1/runbooks "${H_OPS[@]}" --data-binary @field-support.yaml
# → {"runbook_ref": "field-support@1"}   (collections + bindings materialize now)

curl -X POST localhost:8080/v1/runbooks/field-support/runs "${H_OPS[@]}"
# → {"run_id": "run-…", "state": "awaiting_approval"}
```

v2 runs execute every step **once per collection** — `GET /v1/runs/{id}`
shows rows like `buildIndex:public-docs`, `cutover:internal-eng`. Each
collection's cutover pauses for its own approval:

```bash
curl localhost:8080/v1/runs/$RUN "${H_OPS[@]}"          # find the awaiting ordinal
curl -X POST localhost:8080/v1/runs/$RUN/steps/6/approve "${H_OPS[@]}"   # cutover:public-docs
curl -X POST localhost:8080/v1/runs/$RUN/steps/7/approve "${H_OPS[@]}"   # cutover:internal-eng
```

(`mmctl run field-support --watch` then `mmctl approve <run> <ordinal>`
does the same.)

### 3.4 Inspect what you built

```bash
curl localhost:8080/v1/runbooks "${H_OPS[@]}"            # all runbooks + access requirements
curl localhost:8080/v1/runbooks/field-support "${H_OPS[@]}"   # collections, levels, active indexes, versions
curl localhost:8080/v1/collections "${H_OPS[@]}"         # collections directly
```

`GET /v1/runbooks/{name}` is the endpoint the plan calls "runbook info": it
answers *which indexes does this runbook reach, and what clearance does each
require* — plus every hosted version of the name.

## 4. Mint capability tokens (API manager)

The manager authenticates a human however it likes (SSO/MFA), decides their
level/compartments from its own directory, then exchanges its `mgmt`
credential for a short-lived token:

```bash
curl -X POST localhost:8080/v1/access-tokens "${H_MGMT[@]}" -H "Content-Type: application/json" -d '{
  "uid": "alice",
  "access_level": 0,
  "scopes": ["query"]
}'
# → {"token": "eyJ…", "jti": "tok-…", "expires_at": "2026-08-11T01:11:13Z"}

curl -X POST localhost:8080/v1/access-tokens "${H_MGMT[@]}" -H "Content-Type: application/json" -d '{
  "uid": "bob",
  "access_level": 2,
  "compartments": ["eng"],
  "scopes": ["query", "ingest"],
  "runbook_refs": ["field-support"],
  "ttl_secs": 1800
}'
```

- `scopes`: `query` = sessions/turns; `ingest` = file upload. A token may
  carry both.
- `runbook_refs` (optional): restricts the token to named runbooks (by NAME,
  so one token spans versions).
- The token is never stored server-side; the issuance is audited in
  `access_tokens` (visible via `GET /v1/access-tokens`, §7). The audit row is
  written on the postgres store only — minting still works under
  `MUNARIUM_STORE=memory`, but nothing records it, and every reporting route
  below requires postgres.

The manager forwards the token as the bearer **and** asserts the uid on
every call. The two must agree — a stolen token replayed under a different
uid is rejected (`403 uid-mismatch`).

## 5. Ingest files (ingest-scoped token)

One at a time or in batch; content is base64; binding is explicit
(`collections: [...]`) or automatic via the runbooks' declarative matchers:

```bash
curl -X POST localhost:8080/v1/ingest/batch \
  -H "Authorization: Bearer $INGEST_TOKEN" -H "X-Munarium-Uid: bob" \
  -H "Content-Type: application/json" -d '{
  "files": [
    {"filename": "public/faq.md", "media_type": "text/markdown",
     "content_base64": "'"$(base64 -w0 faq.md)"'"},
    {"filename": "eng/runway.md", "media_type": "text/markdown",
     "content_base64": "'"$(base64 -w0 runway.md)"'"}
  ]}'
# → per-file results: {"sha256": …, "existed": false, "bound_to": ["public-docs"]}, …
```

Rules worth knowing:

- **Clearance applies to writes too.** Binding into `internal-eng` requires
  a token that could *read* it (level 2 + `eng`). A level-0 ingest token
  explicitly targeting it gets `403 forbidden` on `POST /v1/ingest`; on
  `/v1/ingest/batch` the same refusal comes back as that file's `error` field
  inside a 200 response (see the batch bullet below). Clearance is checked for
  every target *before* any bind commits, so a forbidden target never leaves a
  partial binding behind.
- Upload is content-addressed and idempotent (`existed: true` on replay);
  an optional `sha256` field is verified before commit.
- Batches (≤ 500 files) report per-item results — one bad file never fails
  the batch, so check each result's `error`, not just the HTTP status.
- New files become searchable after the next `buildIndex`+`cutover` run
  (re-run the runbook; indexes are immutable versions, never mutated in
  place).

## 6. Sessions and turns (query-scoped token)

```bash
# New session — pins field-support@<latest non-removed> and snapshots clearance:
curl -X POST localhost:8080/v1/runbooks/field-support/sessions \
  -H "Authorization: Bearer $ALICE_TOKEN" -H "X-Munarium-Uid: alice"
# → {"session_id": "ses-…", "runbook_ref": "field-support@1",
#    "permitted_collections": ["public-docs"]}          ← alice is level 0

# Follow-on turns reuse the session id:
curl -X POST localhost:8080/v1/sessions/$SES/turns \
  -H "Authorization: Bearer $ALICE_TOKEN" -H "X-Munarium-Uid: alice" \
  -H "Content-Type: application/json" -d '{"query": "vacation policy"}'
```

A turn response carries score-merged `hits` (each tagged with its source
collection), one **ProvenanceEnvelope per collection** searched,
`collections_searched`, and `skipped` (permitted collections that have no
active index yet — never silently dropped).

The same query from bob (level 2 + `eng`) on the same runbook returns
`public-docs` *and* `internal-eng` hits. That is the whole
compartmentalization model at work — author one runbook, serve every
clearance.

**RAG completion** — when the runbook declares `completion:`, ask for it:

```json
{"query": "vacation policy", "complete": true}
```

The turn then also returns `completion: {provider, model, text, tokens,
was_override}` — retrieval context interpolated into the runbook's
`promptTemplate` and sent to the model resolved by the chain
*request override → `models.tasks.completion` → `models.default` → tenant
default provider*.

**Model overrides** — callers may request a different provider/model *if the
runbook allows it*:

```json
{"query": "…", "complete": true,
 "model_override": {"provider": "default", "tier": "fast"}}
```

Anything outside `models.allowOverrides` → `403 override-not-allowed`. Every
turn records which model actually served it and whether it was an override,
so the cost report (§7) can split native vs overridden spend.

Session history: `GET /v1/sessions/{id}` (own uid, or any control/mgmt
token).

## 7. Monitor and govern (mgmt token)

```bash
# Who is using what:
curl "localhost:8080/v1/reports/usage?group_by=uid" "${H_MGMT[@]}"
#   group_by = uid | session | runbook | collection; from=/to= RFC 3339 bounds

# The uid-attributed audit trail (add &bodies=true for captured payloads):
curl "localhost:8080/v1/reports/audit?uid=bob&limit=50" "${H_MGMT[@]}"

# Model spend per provider/model, native vs overridden turns:
curl "localhost:8080/v1/reports/cost" "${H_MGMT[@]}"

# Today's daily token caps beside their ceilings (2026-09-01):
curl "localhost:8080/v1/reports/budgets" "${H_MGMT[@]}"

# The per-call output-token ceilings (2026-09-02): read by any role;
# replaced as a WHOLE by the rw role — all eight fields, no partial update:
curl "localhost:8080/v1/max-tokens" "${H_MGMT[@]}"
curl -X POST "localhost:8080/v1/max-tokens" "${H_OPS[@]}" -H 'Content-Type: application/json' -d '{
  "turn_completion": 4096, "query_expansion": 256, "complete_default": 1024,
  "healthai_probe": 512, "hierarchy_classifier": 32, "hierarchy_intent": 480,
  "runbook_advisory": 2048, "authoring_assist": 8192 }'

# Token issuance audit + revocation:
curl "localhost:8080/v1/access-tokens?uid=bob&active=true" "${H_MGMT[@]}"
curl -X POST "localhost:8080/v1/access-tokens/$JTI/revoke" "${H_MGMT[@]}"
```

The max-tokens answer carries `source` (`environment` while the container's
`MUNARIUM_MAX_TOKENS_*` defaults apply, `tenant` after a replacement) and
`updated_at`, and a GET body round-trips into a POST; a missing or
out-of-range field is 400 `invalid-input`, a non-rw POST 403. A runbook's own
`completion.maxTokens` (§3.1) still wins over both. Reference:
[../tokenbudgets.md](../tokenbudgets.md).

Revocation is a deny-list entry; it is *enforced* at verify time only when
the server runs `MUNARIUM_TOKEN_REVOCATION_CHECK=true` (the revoke response
tells you which mode is active). With the check off, a revoked token dies at
its `exp` — that is the documented "deliberately light" posture.

## 8. Upgrade, version, remove

- **In-place upgrade:** re-apply the SAME `name@version` — yaml is replaced,
  collections/bindings re-sync. Ongoing sessions keep their pinned ref and the
  clearance snapshot taken at creation, but each turn re-reads the *current*
  yaml of that ref: changed retrieval knobs, prompt template, model defaults,
  and collection set take effect mid-conversation, and a newly added collection
  becomes searchable to any live session whose snapshot clears it (a session
  can never gain reach beyond its snapshotted level/compartments). Publish a
  new version instead when a running conversation must not shift under it.
- **Side-by-side version:** apply `metadata.version: 2`. Both versions serve
  simultaneously; give v2 different collection names if it needs separate
  indexes/sources. Bare-name session creation resolves to the **latest
  non-removed** version; pin `name@version` to target one explicitly.
- **Removal is double-pass and soft:**

```bash
curl -X POST "localhost:8080/v1/runbooks/field-support@1/remove-request" "${H_OPS[@]}"
# → {"removal_id": "rm-…", "expires_at": "…"}        (15-minute window)
curl -X POST "localhost:8080/v1/runbooks/field-support@1/remove-confirm" "${H_OPS[@]}" \
  -H "Content-Type: application/json" -d '{"removal_id": "rm-…"}'
```

  Removal targets an **exact** `name@version`, requires the matching
  `removal_id` inside the TTL (`409 removal-not-confirmed` otherwise), and
  only hides the runbook: sessions get `410 runbook-removed`, lists omit it
  (`?include_removed=true` shows it), and the yaml, run history, collections,
  and index data are all retained. **No API deletes a collection or its active
  index data.** The one deletion any API performs is the `retireOld` step,
  which reclaims chunk rows for *inactive* index versions beyond
  `keep_versions` (their manifests stay resolvable, so past provenance
  envelopes remain verifiable). Physically removing a collection is a manual
  DBA operation:
  [../ops/index-deletion-runbook.md](../ops/index-deletion-runbook.md).

## 9. Error slugs you will actually see

| Slug | When | Fix |
|---|---|---|
| `uid-required` 400 | no `X-Munarium-Uid` **and** the bearer is not a capability JWT (a JWT's `sub` fills in for the header) | the API manager must assert the uid on every call |
| `uid-mismatch` 403 | header uid ≠ JWT `sub` | forward the token only with its own user's uid |
| `token-expired` 401 | JWT past `exp` | mint a new token |
| `token-revoked` 401 | jti on the deny-list (check enabled) | mint a new token |
| `scope-missing` 403 | query token on ingest, or vice versa | mint with the right `scopes` |
| `override-not-allowed` 403 | `model_override` outside `allowOverrides` | drop the override or open the policy |
| `removal-not-confirmed` 409 | confirm without/with wrong/late `removal_id` | request again, confirm within 15 min |
| `runbook-removed` 410 | exact ref was removed | use a live version |
| `forbidden` 403 | clearance below a collection's requirement | expected — that's compartmentalization working |

Full registry: [../api/errors.md](../api/errors.md).
