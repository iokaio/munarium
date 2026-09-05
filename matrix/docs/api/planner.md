# Conversational planners (Genie)

`POST /v1/datasources/{name}/planner/ask`.

Databricks AI/BI Genie is the one implementation today, and the Databricks
adapter that carries it is part of Munarium Matrix Enterprise, not of this
repository — the policy described here is. Everything about the
*policy* is vendor-neutral — the seam is
`SourceAdapter::planner_ask`, mirroring `semantic_execute`, and the deciding
code names no vendor — because the policy is the part that has to be the same
for whoever's planner arrives next.

## The rule

**A planner proposes; Matrix decides.**

Databricks itself asks users to review a trusted-asset match, because even a
*trusted* asset can be matched to the wrong question. So a planner's answer
carries no authority here. It is evidence about what a planner said, and every
step downstream treats it that way.

Concretely: **this route executes nothing.** It returns the SQL the allowlist
admitted, and the caller runs that through a contract — where the compiler's
allowlist walk, the budget, the effective identity and the seal live. A
planner path that ran its own SQL would be a second execution path with its own
limits, which is the shape every other surface in this system was built to
avoid.

## Two modes

| Mode | What it does | What it refuses |
| --- | --- | --- |
| `assist` (default) | Runs the allowlist over the proposal and returns what may run. | A trusted asset not on the allowlist; generated SQL where no `allowedTables` are declared; a message with no query at all. |
| `evaluation` | Records what the planner said, for scoring under the same verified suite everything else is scored under. **Admits nothing.** | Anything, unless `evaluationEnabled` is on. |

Evaluation admits nothing even for an allowlisted asset. Measuring a planner
and trusting it are different acts, and an evaluation that quietly admitted its
own subject would be one that changed what it measured.

Evaluation is **off by default**. Calling a model surface costs money and
produces output that must never be mistaken for a contract's, so it is opted
into rather than out of.

## Declaring one

Genie is Databricks-specific, so it lives in `spec.connection`, which is
adapter-owned by design. Putting it in the shared `DataSourceSpec` would ask
every other adapter to carry a field it can never use — and would move the
cross-tree contract for a vendor feature.

```yaml
apiVersion: munarium.ioka.io/v1
kind: DataSource
metadata: { name: crm-genie, version: 1 }
spec:
  adapter: databricks
  connection:
    host: <workspace>.azuredatabricks.net
    warehouseId: abc123
    catalog: main
    schema: crm
    auth: { oauthM2m: { clientId: sp-1, clientSecretRef: matrix-databricks } }
    allowHosts: [<workspace>.azuredatabricks.net]
    genie:
      spaceId: 01ef-1234
      trustedAssets: [asset-open-pipeline]     # exact match, never a prefix
      allowedTables: []                        # empty: no generated SQL
      evaluationEnabled: false
  credentialRef: matrix-databricks
  egress: { allowHosts: [<workspace>.azuredatabricks.net] }
  authorization: { strategy: source_native }
```

**The allowlist is required and never empty.** A block declaring neither
`trustedAssets` nor `allowedTables` is refused at apply time — planner-assist
with neither is "run whatever the model wrote", which is the one thing this
system exists not to do, and defaulting it to "everything" would make the safe
posture the one nobody configures.

`trustedAssets` matches **exactly**. A glob would be an allowlist that grows
whenever somebody names a new asset conveniently.

`allowedTables` being non-empty is what admits **generated** SQL at all. The
SQL compiler then does the real work; this check only stops a space that never
intended generated SQL from getting it by default.

A source may be asked only about the space it declares. A question addressed to
another `spaceId` is `genie_asset_not_allowed` — the egress reasoning, one
level up.

Two refusals arrive before anything is wired, so a typo never becomes a
connection attempt: `planner_mode_unknown` (the mode must be `assist` or
`evaluation`) and `question_required` (asking a model surface nothing costs
money and answers nothing). Both are class `invalid`, so both are **400**.

## It spends a budget unit

A planner question is metered against the source's `budgetPerHour`, exactly as
an execute is. It is not a free read: it reaches the vendor and bills a **model
call** there, which costs more per question than the statement an execute runs,
and `budgetPerHour` is the one ceiling this system has on what a source costs.

The unit is **released** when the refusal never left the process — an
unconfigured surface, a question addressed to another space, an asset the
allowlist turned down before anything was asked — and **settled** when the
planner was actually reached. Probe and introspect are deliberately *not*
metered; they are operator acts against the same connection an execute uses.
The line between them is that this one calls a model.

## `genie_plan_unpinned` is a label, not a failure

Reproducing a planner's answer needs the space's own configuration: its
instructions, its example SQL, its trusted assets. **No Databricks API returns
that.** So the pin carries what it can —

```json
{
  "space_id": "01ef-1234",
  "conversation_id": "01ef-conv",
  "message_id": "01ef-msg",
  "attachment_id": "01ef-att",
  "statement_id": "01ef-stmt",
  "query_hash": "sha256:…",
  "pinned": false,
  "trusted_asset_id": "asset-open-pipeline"
}
```

— and `pinned: false` says the rest. The response repeats it in words rather
than leaving it to be inferred:

> the plan behind this query is NOT pinned: sealed bytes are replayable, the
> decision that produced the query is not

That distinction is the whole point. A Genie answer is **not** deterministically
reproducible merely because it came from a configured space, and presenting one
beside a contract's answer without saying so would be the single most
misleading thing this surface could do. `pinned` is a field rather than a
constant because the day a vendor API exposes a space's configuration
fingerprint, it becomes true for spaces that expose one and the envelope's
shape does not change.

## What will happen in practice

**A planner's SQL frequently will not survive the contract path.** The
allowlist walk refuses `SELECT *`, subqueries, non-deterministic functions and
undeclared tables, and a generative surface writes all of those.

That is not a defect in this path. A query Matrix cannot verify is a query
Matrix will not seal, and reporting the refusal is more useful than sealing
something nobody can check.

## Status: the policy is proven; the vendor transport is not in this repository

The decision logic is exhaustively tested — every mode against every message
shape, including the invariant that an outcome never admits nothing without
saying why — and the decoder is pinned against the vendor API's *documented*
response shape rather than against its own serializer.

Three `planner.*` scenarios run the pure `decide()` in the offline tier on
every push: assist admits only a permitted trusted asset; evaluation records
and admits nothing; and the unpinned plan is a label an ADMITTED proposal still
carries. Those are the properties the policy exists for, and they are the half
that lives here.

The adapter that produces a `PlannerMessage` is **Munarium Matrix Enterprise**
and is not in this repository, so no end-to-end run against a vendor planner
happens here. This document does not imply one. What a core build guarantees is
narrower and worth stating plainly: whatever a planner proposes, nothing is
admitted that the spec does not permit, and an unpinned plan is labelled rather
than refused.
