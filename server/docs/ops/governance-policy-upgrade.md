# Upgrade to durable governance policies

This guide applies to the first release containing migration `0036_governance_profiles.sql`
(see [release notes](../../CHANGELOG.md)). Installing the release preserves legacy
comparison by default. To gain exact-value governance, explicitly adopt a profile
on new memory versions and migrate existing histories as described below.

## What is versioned

A memory version's `metadata.governance_policy` is an immutable profile. Schema 1
contains named subject/key comparison bindings; omitted keys use `legacy-text-v1`.
`exact-string-v1` compares the original strings: case, all whitespace and Unicode
normalization matter. Comparison never rewrites a stored value. Bindings currently
require nonempty, unpadded, dot-free subject and key components to avoid ambiguity
in the kernel's dotted claim identities. Duplicate bindings are errors.

Server computes `governance-v1:<sha256>` over the UTF-8 JSON serialization of the
typed profile (field order `schema_version`, `values`; each value's fields
`subject`, `key`, `comparison`; bindings sorted by subject/key). Store the returned
revision rather than implementing an independent JSON canonicalizer. Unknown
schema versions, algorithms, or profile fields fail closed.

Claims and anchors retain their original `version_id`, which resolves their
original profile. PostgreSQL also stamps a `governance_revision` projection column;
historical NULLs mean the named implicit legacy policy. New ledger events retain
original subject/key/value and profile identity; normalized text alone is not an
exact-value replay format. For pre-feature events, retain the original claims and
anchors in the database backup: original whitespace cannot be recovered by
rebuilding claims solely from old normalized event strings. Collection index
rebuilds do not require rebuilding that authoritative history.
Export the version metadata and governance findings
alongside claims, anchors and events. A bare legacy Claim DTO is not a complete
policy-aware export; its public shape is unchanged for client compatibility.

Read `GET /v1/versions/{id}/findings?rule_prefix=governance.` to obtain profile
receipts and assessments. Use `rule_id=governance.policy-revision` or
`rule_id=governance.command-evaluation` to narrow the view; increase `limit` as
needed and check completeness before treating an export as complete. The existing
native gRPC findings operation returns the same records. The `governance.` finding
namespace is server-owned and cannot be filed through the external findings API.

Governed command evaluations record the snapshot head, profile revision, findings,
candidate text and shape content hashes, plus the chronology rules used. The receipt
and appended claims commit together. Its stored sequence is the batch's final
sequence; `claim_count` identifies the contiguous batch ending there. Keep the
referenced shape YAML revisions available for later full replay. Raw storage
appends, such as runbook checkpoint events, retain their policy identity but do not
claim to have passed the command gates. Text-only checks retain their existing
best-effort findings behavior and are not durable claim assessments.

## Upgrade and adopt

1. Back up PostgreSQL and retained source/artifact stores using the
   [backup procedure](backup-restore.md). Record active memory versions, index
   versions, publication revisions, collection access settings and session pins.
   Retain original sources; rebuilding cannot recover deleted source bytes.
2. Pause ingestion and other writers during activation. Upgrade every server,
   worker and direct database writer, then apply the additive migration through
   normal Server startup. No ledger reset or historical value backfill is needed.
   PostgreSQL guards reject old writers on governed versions and reject changes
   to stored profile/transition metadata. These are compatibility guards, not a
   substitute for restricting direct database access.
3. Create a new root memory version with the profile below, or follow the
   existing-history transition procedure. The profile is selected on version
   creation by an authorized writer, not by a claim's evidence or shape label.
   A child without an explicit profile inherits its parent's profile. Carry forward
   other required version metadata explicitly, especially `chronology_rules`:
   governance-profile inheritance does not automatically inherit unrelated settings.
   Retain the referenced rule/shape assets and check that all intended gates remain
   armed on the new version.
4. Review the transition assessment and resolve any newly material conflicts
   through ordinary corrections and anchor policy. The migration does not promote
   disputed claims, change accepted claims, release anchors, or silently supersede
   facts. An accepted historical value remains historical canon until explicitly
   corrected; a correction still cannot override a locked anchor.
5. Route producers and runbooks to the new memory version (`mmctl run <runbook>
   --version-id <new-version> --watch`). Keep ancestor writers paused during the
   transition review and retire their write targets after cutover. Memory lineage
   is live inheritance: later ancestor writes are visible in descendants. The
   assessment covers only its recorded head; it is not a freeze of the ancestor
   and must not be represented as covering later writes.
6. Rebuild affected collections and validate fresh sessions as below. Resume
   ingestion after the new policy, indexes and producers agree on the target.

Root creation through `POST /v1/versions` (or native `CreateVersion` metadata):

```json
{
  "metadata": {
    "governance_policy": {
      "schema_version": 1,
      "values": [
        {"subject": "file", "key": "path", "comparison": "exact-string-v1"}
      ]
    }
  }
}
```

For existing history, read the parent's head and policy revision, then create a
child. For a pre-feature parent with no profile receipt, the implicit legacy
revision is `governance-v1:9066f416785a6ebfb43db32a871bbdbb7af4a0b24e83a2196468436a772f0f54`. Substitute the actual parent ID and head below:

```json
{
  "parent_version_id": "<parent-version>",
  "metadata": {
    "governance_policy": {
      "schema_version": 1,
      "values": [
        {"subject": "file", "key": "path", "comparison": "exact-string-v1"}
      ]
    },
    "governance_transition": {
      "from_revision": "<parent-profile-revision>",
      "expected_head": 123,
      "reason": "Preserve case-sensitive file paths"
    }
  }
}
```

A stale head fails with `head-conflict`; reread and review before retrying.
A missing transition, incorrect old revision, empty reason, unknown policy, or
caller-supplied assessment/revision fails with `invalid-input`. The store computes
and commits the new profile and assessment under the same lineage lock. The
assessment checks current accepted facts against peer canon and locked anchors
under the new comparison policy; it lists claim IDs and original statuses. It is
not a replay of shape validation, superseded/disputed history, external evidence,
or original-time decisions. Future assessments can add those explicitly versioned
kinds without relabeling this one. Subsequent commands evaluate inherited values
under the explicitly selected child policy, preserving the original decisions.

## Rebuild collections to obtain the full benefit

Installing this migration alone does not require an index format conversion.
Policy adoption changes ledger evaluation; it does not automatically re-extract
sources, re-propose claims, regenerate vocabularies, or rebuild collection indexes.
Rebuild every affected collection whose extraction, accepted-claim inputs, shapes,
or generated vocabulary changed. For a small deployment, rebuilding all collections
after reviewing the migrated claims is the simplest way to cover stale derived data.
Record the new memory version/profile and resulting logical index IDs in the
upgrade record; index identity does not itself prove claim-policy adoption.

1. Retain the previous active indexes and their source bytes. Apply reviewed
   shape/runbook revisions and re-run extraction or claim proposals when their
   outputs need reevaluation. Do not resubmit historical claims without a deliberate
   correction/import strategy: a rebuild must not duplicate authoritative history.
2. Run each collection's existing build/verify/approval runbook against the new
   memory version. Use `mmctl run <runbook> --version-id <new-version> --watch`,
   inspect verification results, then approve the reported cutover step with
   `mmctl approve <run-id> <step-ordinal>`. Follow the runbook's actual ordinals.
   For direct datastore collection builds, enqueue
   `mmctl datastore jobs enqueue direct <collection-id> --max-chars <N> --watermark <seq>`
   and inspect the job's result; the watermark is an explicit provenance pin,
   not a command to migrate claims. See the [Datastore guide](../guides/datastore.md).
3. Verify each new logical index and, where datastore serving is selected, its
   artifacts. Follow build → verify → staged binding → fleet-gated promotion →
   logical activation. `mmctl datastore rebuild <index-version-id>` repairs that
   version's derived artifact; it is not a substitute for creating a new logical
   index from changed inputs. Preserve compare-and-swap generation checks and
   approval requirements. See [operator commands](mmctl.md).
4. If the collection uses publication governance, update the publication's index
   pin through its revision-checked governance API after activation. Preserve source
   identities, hashes, dates and withdrawal tombstones. This publication governance
   is distinct from the memory-version profile described here; see
   [collection governance](../guides/collection-vocabularies.md#collection-queries-and-publication-governance-121).
5. Open fresh sessions. Check exact case/whitespace conflicts, accepted/disputed
   outcomes, citations, collection authorization, and representative retrieval
   answers. Old sessions may intentionally remain pinned to old indexes. Keep old
   artifacts for the configured pin horizon and retention period; retire them only
   through the [index deletion procedure](index-deletion-runbook.md).

## Rollback and future policies

A rollback must keep a policy-aware reader and writer. Do not drop the migration,
strip revision labels, or run an old binary against governed history. To restore
legacy comparison for future commands, create another explicit child transition
to the schema-1 profile with `values: []`, review its assessment, and reroute writes.
Retrieval can return to a retained verified index using the existing approval and
activation controls, independently of that ledger transition.

Future releases can introduce new profile schema versions and algorithm IDs for
precision, evidence requirements, conflict precedence or temporal rules. Old
readers must reject unsupported profiles, never ignore new obligations. New
assessments reference original claim IDs, original outcomes and the newly selected
revision. Keep retrieval configuration in its own revisioned profile and always
apply current authorization/revocation: historical policy never restores access.
