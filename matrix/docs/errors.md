# The refusal registry

**G7: Matrix answers with a typed refusal instead of a degraded answer or a
generic connector error.** This is the vocabulary.

The normative definition is
[`contract/refusal.schema.json`](../contract/refusal.schema.json), which is
vendored into the server tree. This document explains it and lists what a
deployment can actually meet.

## Two fields, and only one of them is closed

```json
{
  "contract_version": "0.1.0",
  "class": "exhausted",
  "code": "budget_exceeded",
  "message": "source 'crm' has 0 of 2 unit(s) left this hour and this execution needs 1",
  "retry_after_seconds": 1800
}
```

**`class` is CLOSED and is what a consumer switches on.** Six values, and they
will not grow without a contract MAJOR.

**`code` is OPEN and is what an operator reads.** A consumer that meets a code
it does not know **must fall back to the class**. That rule is what lets Matrix
add a precise code — `cdc_slot_wrong_plugin`, say — without breaking a caller
written before it existed.

Getting this backwards is the mistake the design is shaped to prevent: a client
that switches on `code` breaks on every new adapter, and a client that only
logs `class` throws away everything an operator needs.

## The six classes

| Class | What it means | Retry? | HTTP |
|---|---|---|---|
| `not_covered` | This layer cannot answer this question **at all**. Not a state — a shape. | No | 422 |
| `unavailable` | The source or a dependency is down. | Yes | 503 |
| `denied` | Policy said no. | Not without a different principal | 403 |
| `incomplete` | Something came back, but it cannot support a completeness or exactness claim. The server may still show it, **labelled**. | Sometimes | 200 |
| `invalid` | The intent was malformed or contradicts the contract. A bug, not a state. | No | 400 |
| `exhausted` | A budget, row cap, byte cap or deadline stopped it. | Yes, later | 429 |

`incomplete` answering **200** is deliberate and is the one row worth pausing
on. An incomplete result is a real answer — rows that came back, honestly
labelled as not covering everything — and turning it into an error would throw
away evidence a reader can use, while turning it into a silent success would be
the exact failure G4 exists to prevent.

`retry_after_seconds` is present when the service can say *when*. A caller
pacing an `exhausted` refusal should not have to guess.

## The codes

Grouped by where they come from. Every one is reachable: the contract schema
requires that each code Matrix can emit is provoked by at least one conformance
scenario, because **an unreachable code is dead vocabulary**.

### Shape and policy

| Code | Class | Meaning, and what to do |
|---|---|---|
| `not_covered` | not_covered | The generic form. A contract, view or collection cannot answer this. |
| `contract_not_found` | not_covered | No asset by that name in this tenant's registry. Check `mxctl list`. |
| `adapter_not_available` | not_covered | This build carries no adapter for the kind the asset names. The asset is valid and the grammar accepts every kind; the implementation is not linked into this binary. The adapters for analytics platforms — `databricks`, `bigquery`, `snowflake`, `cube`, `dbt` — are Munarium Matrix Enterprise. No retry or reconfiguration changes it. |
| `policy_denied` | denied | The authorization class does not permit this read. |
| `policy_delegation_unavailable` | denied | The source resolved no authorization class to act as. |
| `too_many_classes` | invalid | A collection carries exactly one class; this asset asked for several. |
| `required_evidence_not_permitted` | denied | The session cannot see evidence the answer would need. |
| `egress_denied` | denied | The host is not in the source's allowlist. Default-deny is working. |
| `credential_unresolved` | invalid | `credentialRef` names a secret the deployment does not provide. |
| `asset_invalid` | invalid | The asset does not satisfy its own grammar. Validation says which rule. |
| `invalid_config` | invalid | The adapter's connection block is unusable — a missing host, a scheme where a bare name belongs. |
| `decision_required` | invalid | A promotion, demotion or rollback needs the operator's decision id. |

### Execution

| Code | Class | Meaning, and what to do |
|---|---|---|
| `source_unavailable` | unavailable | The source could not be reached, or a peer this role needs is unset. |
| `source_stale` | incomplete | The result is older than the freshness bound the profile set. |
| `deadline_exceeded` | exhausted | The intent's deadline passed. Raise it, or narrow the question. |
| `budget_exceeded` | exhausted | `budgetPerHour` is spent for this source. `retry_after_seconds` says when. |
| `rate_limited` | exhausted | The engine pushed back. |
| `result_too_large` | exhausted | Over `maxRows`/`maxBytes` before rendering. |
| `result_truncated` | incomplete | Rows came back, and they are not all of them. **Never equal to a complete result** — the logical hash differs by construction. |
| `partial_result` | incomplete | Some part of a multi-part read did not complete. |
| `statement_refused` | invalid | The compiler's allowlist walk refused the statement. |
| `schema_drift` | invalid | The source's schema is not the one the contract was verified against. Spends budget: the ENGINE rejected it, so the source was reached. |
| `snapshot_expired` | unavailable | The snapshot the read pinned is gone. Re-read. |
| `seal_failed` | invalid | munarium-server refused the evidence. The message carries its answer. |
| `result_not_identifiable` | invalid | The result declares neither key columns nor a total ordering, so it cannot be sealed at all. |

### Semantic views

| Code | Class | Meaning, and what to do |
|---|---|---|
| `metric_not_covered` | not_covered | A measure, dimension or filter outside the asset's closed lists — or an adapter with no semantic layer. |
| `metric_view_changed` | not_covered | The source's definition moved since it was verified. **Verify it again before it executes.** Spends budget: the definition was read to compare. |

### Materialization and the change feed

| Code | Class | Meaning, and what to do |
|---|---|---|
| `sync_not_covered` | not_covered | The adapter cannot materialize in the mode asked for. |
| `checkpoint_gap` | incomplete | The checkpoint is behind what the source can still replay. Resnapshot. |
| `cdc_checkpoint_gap` | incomplete | The Delta/WAL position is no longer available. The worker resnapshots rather than report coverage it lacks. |
| `cdc_slot_missing` | not_covered | No replication slot. The message carries the statement that creates one — Matrix will not create it, because a slot retains WAL until something consumes it and creating one implicitly would make Matrix the author of a full disk. |
| `cdc_slot_wrong_plugin` | invalid | The slot decodes with `test_decoding`, which **bypasses RLS and column privileges entirely**. Only `pgoutput` with a publication is accepted. |
| `cdc_role_lacks_replication` | denied | The principal cannot read the stream. |
| `cdc_publication_missing` | not_covered | No publication carries this table. |
| `cdc_publication_projection_mismatch` | invalid | The publication's column list does not match what the mapping projects. |
| `cdc_publication_bypasses_row_policy` | denied | The publication has no row filter on a row-secured table, so the feed would carry rows the reader cannot see. |
| `cdc_unchanged_toast` | incomplete | An unchanged TOASTed value is not in the stream; the record cannot be rendered whole. |
| `cdc_truncate_not_covered` | not_covered | `TRUNCATE` has no per-row representation. |
| `cdc_stream_malformed`, `cdc_unsupported_message` | unavailable / not_covered | The decoder met something it will not guess at. |

### Reconciliation

| Code | Class | Meaning, and what to do |
|---|---|---|
| `identity_ambiguous` | not_covered | Two rows in one scope resolve to one ledger subject. **Ambiguity never merges.** |
| `ledger_volume_exceeded` | exhausted | The pass would exceed the mapping's declared finding or proposal ceiling. Refused **before it writes anything** — a dry pass counts first. |
| `batch_unserializable` | invalid | The observation batch could not be rendered. |

### Adapter configuration and wiring

Raised before a source is reached — a deployment that is not wired, rather than
a source that misbehaved.

| Code | Class | Meaning, and what to do |
|---|---|---|
| `missing_credential` | invalid | The source declares no `credentialRef`, and there is no ambient credential to fall back to **by design**. |
| `databricks_host_unset`, `databricks_client_id_unset`, `databricks_auth_unknown` | invalid | **Enterprise adapter.** The Databricks connection block is incomplete or names an auth kind that build does not implement. A core build never reaches these: it refuses the adapter first, with `adapter_not_available`. |
| `landing_root_unset` | invalid | A `file` landing source with no root configured. |
| `missing_manifest` | not_covered | A landing export with no manifest — there is nothing to state coverage from. |
| `registry_corrupt` | invalid | Stored asset bytes do not parse. Something wrote to the registry underneath the service. |
| `wrong_kind` | invalid | The asset named is not of the kind the route expects. |
| `empty_statement` | invalid | A contract compiled to nothing. |
| `http_client` | unavailable | An outbound HTTP client could not be constructed. |

### Decoding and typing

| Code | Class | Meaning, and what to do |
|---|---|---|
| `unmapped_type` | not_covered | A source type canon@1 does not model. Refused **by name** rather than guessed — a value under a type nothing downstream can verify is worse than no value. |
| `decimal_without_scale` | invalid | An exact decimal with no declared scale has no canonical form. |
| `row_width_mismatch` | invalid | A row's cell count disagrees with the declared schema. |
| `decode_failed`, `response_unreadable` | unavailable | The engine's bytes could not be read as this build expects. |
| `transport` | unavailable | The request did not complete. |

### Engine state

| Code | Class | Meaning, and what to do |
|---|---|---|
| `statement_timeout` | unavailable | The statement did not finish inside the deadline. It is cancelled best-effort, because a warehouse keeps billing a statement nobody is waiting for. |
| `statement_unidentified` | unavailable | The engine reported a pending statement with no id, so it cannot be polled. |
| `still_running`, `unknown_state` | unavailable | The engine reported a state this build does not treat as terminal. |
| `snapshot_isolation_unavailable` | not_covered | SQL Server: a consistent view was asked for and the database does not offer one, so no snapshot marker may be claimed. |
| `token_malformed` | unavailable | An auth response did not carry a usable token. |

### Change feeds

| Code | Class | Meaning, and what to do |
|---|---|---|
| `cdf_not_enabled_or_supported` | not_covered | **Enterprise adapter.** The table has no change feed. Materializing by watermark instead is refused, because a watermark read cannot see a delete. |
| `delta_version_expired` | not_covered | **Enterprise adapter.** The pinned source version is past the platform's retention window. |
| `cdf_checkpoint_gap` | incomplete | The pinned version is no longer available; the worker resnapshots rather than report coverage it lacks. Emitted by the Postgres logical-replication path in this repository. |

### Promotion

| Code | Class | Meaning, and what to do |
|---|---|---|
| `promotion_gate_identity` | not_covered | Identity precision is below the configured minimum. The message carries both numbers and the run. |
| `promotion_gate_conformance` | not_covered | Value conformance is below the configured minimum, likewise. |
| `subject_template_unfillable` | invalid | A mapping's `subjectTemplate` has a placeholder the row cannot fill. |

### Conversational planners

| Code | Class | Meaning, and what to do |
|---|---|---|
| `genie_asset_not_allowed` | denied | The proposal named a trusted asset outside the allowlist, generated SQL where none is admitted, or a space this source does not declare. |
| `genie_plan_unpinned` | not_covered | **A label, not a failure.** The sealed bytes are replayable; the decision that produced the query is not, because no vendor API exposes a space's configuration. |
| `planner_mode_unknown` | invalid | Mode must be `assist` or `evaluation`. |
| `question_required` | invalid | Asking a model surface nothing costs money and answers nothing. |
| `genie_http`, `genie_malformed`, `genie_timeout`, `genie_unreachable`, `genie_failed` | unavailable | **Enterprise adapter.** The planner transport did not answer usefully. The policy half — `genie_asset_not_allowed`, `genie_plan_unpinned` — is core and is emitted by this repository. |

## Which refusals spend budget

A refusal that reached the source spends its unit; one that never left the
process does not. The predicate is `rest::source_was_touched`, and it is a
**policy** question rather than a formatting one:

- `invalid`, `not_covered` and `denied` are raised by the compiler, the binder
  or the policy check — before any statement exists — so the units go back.
- `unavailable`, `incomplete` and `exhausted` can only be known by *trying*, so
  the units are kept.
- Three codes override their class because the ENGINE answered:
  `schema_drift`, `deadline_exceeded` and `metric_view_changed`.

Refunding everything would let a caller hammer a source for free with requests
that always fail late. Keeping everything would charge for typos.

## Keeping this document honest

`munarium-matrix-core` carries a test that every code named in a
`Refusal` constructor or `Refusal::new` call appears in this file, and that
every code in this file exists in the source. A registry that drifts from the
vocabulary is worse than no registry, because it is read as authoritative.
