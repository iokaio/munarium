# Artifact damage: isolation, fleet blocks and verified repair

This procedure covers what happens when a Datastore artifact's bytes no longer match its pin. It also covers how to repair the artifact and return it to service. The feature is opt-in and off by default. The specification, including the definitions it relies on, is in the [design page](../design/artifact-damage.md). Those definitions are proposals pending the ART1 and ART2 hub decisions.

## Enable it

`MUNARIUM_ARTIFACT_DAMAGE_MODE` applies to replicas in `shadow` or `datastore` retrieval mode:

| Value | Effect |
|---|---|
| `off` (default) | v1.4 behavior: a bad copy is quarantined in that process only. |
| `observe` | Records findings and honors existing blocks. Reports, without refusing, a build whose result is a blocked artifact. Never commits a block or a clearance. |
| `enforce` | Also commits confirmed fleet blocks and verified clearances, and refuses to publish a blocked artifact. |

The building replica reads the same variable for publication, whatever its retrieval mode (see [Builds of a blocked artifact](#builds-of-a-blocked-artifact)).

1. Give every serving replica its own verifier key and enroll it (see [Verifier keys](#verifier-keys)). Two replicas that share a key count as one verifier, and a replica without an enrolled key only isolates its own copy.
2. Upgrade every serving replica, then set `observe` everywhere. Drain or replace any v1.4 reader.
3. Switch every serving replica to `enforce`.

### Verifier keys

A fleet block or a clearance counts only observations signed by enrolled, unrevoked verifier keys. Each replica needs its own key.

1. **Generate** a key where the replica's secrets are prepared, never in a tracked directory:

   ```console
   munarium-server artifact-damage generate-verifier-key --out /secure/path/verifier.key
   ```

   The command refuses to overwrite a file, creates it owner-only on Unix, and prints only the `fingerprint` and `public_key`. The file holds the private key. Deliver it like any other secret, for example as a Kubernetes secret mounted read-only, and never log it or commit it.
2. **Configure** the replica with `MUNARIUM_ARTIFACT_VERIFIER_KEY_FILE=/path/to/verifier.key`. An unreadable or malformed file stops Datastore infrastructure from arming. With no file, the replica logs that its observations will not count. The startup line `durable artifact damage handling enabled` shows its `verifier` fingerprint.
3. **Enroll** the public key with the deployment's database credential:

   ```console
   munarium-server artifact-damage enroll-verifier --public-key <public_key> \
       --label <replica name> --actor <your-name>
   ```

   `verifier-public-key --key-file <path>` prints the public key and fingerprint of an existing key file. `list-verifiers` shows every enrolled key and whether it is revoked.
4. **Rotate** by generating and enrolling a new key, switching the replica to it, and then revoking the old fingerprint. Observations made with the old key after its revocation count for nothing.
5. **Revoke** a key that is lost, copied or retired:

   ```console
   munarium-server artifact-damage revoke-verifier --fingerprint <fingerprint> \
       --actor <your-name> [--reason "<ticket>"]
   ```

   Revocation is permanent and takes effect at once. An unconfirmed finding or first repair match made with the key is never confirmed, and a revoked key cannot be enrolled again. Committed blocks and clearances keep the fingerprints they recorded.

### Single-replica deployments

With one serving replica, the default rule can never block or clear, because it needs two replicas. To let one replica block and clear, declare it:

| Setting | Value |
|---|---|
| `MUNARIUM_ARTIFACT_DAMAGE_REPLICAS` | `multi` (default) or `single` |
| `MUNARIUM_ARTIFACT_DAMAGE_SINGLE_INTERVAL_SECS` | Spacing between the replica's two runs, 60 to 3600. Default 300. |

The replica then blocks after two of its own runs, with distinct fetches at least the interval apart, both see the same mismatch. It clears after two such runs both match the repaired bytes. Every incident records `block_assurance` and `clear_assurance` (`independent` or `single_replica`). The startup log line `durable artifact damage handling enabled` shows `replicas` and the interval.

`single` is refused, and Datastore infrastructure does not arm, when `MUNARIUM_REPLICA_COUNT` is above 1. Do not declare `single` for a deployment that runs more than one serving replica.

Know the remaining risk: a `single_replica` decision is one replica's view. A replica whose verification is systematically wrong, for example from a faulty build, storage client or endpoint, can both block a healthy artifact and clear a damaged one. Review `single_replica` incidents with that in mind, and add a second replica with a distinct instance id when the risk matters. [Single-replica assurance](../design/artifact-damage.md#single-replica-assurance)

An unknown value stops Datastore infrastructure from arming, and a `datastore` replica stays unready. Check the startup log for `MUNARIUM_ARTIFACT_DAMAGE_MODE`.

## What you will see

- **Local isolation.** One replica logs `artifact integrity failure verified against the authoritative store`. Its reads of that artifact return `datastore-unavailable` (`integrity (quarantined)`). Other replicas keep serving.
- **Fleet block.** After a second replica independently observes the same mismatch (or, in a declared single-replica deployment, after the replica's own second run at least the interval later), every replica refuses the artifact with `datastore-unavailable`. The refusal names `blocked by integrity incident inc-…`. The warmer records the residency as `blocked`. Replica readiness does not change, and other scopes keep serving.
- **Unauthenticated observations.** History entries with decision `unauthenticated` come from a replica without a key, with an unenrolled or revoked key, or with a signature that does not verify. They count for nothing. Check the replica's key file and `list-verifiers`.
- **History full.** Logs report `history_full = true`. The observation history holds at most 1,024 entries per tenant and never shrinks: entries are never evicted or pruned, and resolving or clearing an incident does not free any. Once it is full, new observations are not added to the history, but they are still decided: findings, blocks and clearances keep working, because candidates, incidents and repair attempts carry their own evidence, and `artifact-damage list` still shows them. What is lost is the per-observation trail of later matches, unavailable fetches and unauthenticated observations, so treat a full history as a signal to review what filled it (for example a replica without an enrolled key, or a store that keeps failing), not as something that resolving incidents will reset.

- **Storage generations.** On an object store (S3, Azure, GCS), every authoritative observation records the stored object's generation: `version:<id>` when the store reports a version id, else `etag:<etag>`. The local-file store records none. Observations of different generations never combine toward a block or a clearance, and an absent generation never combines with a known one. Run every replica against the same artifact store backend, or their observations will not confirm each other. [Storage generations](../design/artifact-damage.md#storage-generations)

List the state for a tenant:

```console
munarium-server artifact-damage list --tenant <tenant-id>
```

The command prints JSON with each pin's generation and state, each incident's object, expected digest, `storage_generation`, both confirming verifiers, and any clearance with its `clear_storage_generation`. It reads `MUNARIUM_DATABASE_URL` and the store settings like the server does.

## Repair and clear

A block is cleared only by a preauthorized repair workflow, after two serving replicas independently fetch the repaired bytes and both match the pin. No API route or console action clears a block.

1. **Withdraw if needed.** Reads of the blocked artifact already fail. To serve the scope from PostgreSQL meanwhile, set its rollout selector to `postgres`. Selecting `datastore` again is refused while the artifact is blocked.
2. **Repair the bytes** under your separate authority to change artifact storage. Restore the named object (`object_key` in the listing) under the artifact's prefix from a copy whose `sha256` equals the `expected_sha256`, for example from a verified build's staged copy of the same artifact id. A rebuild job does not do this: in `enforce` it refuses a blocked artifact (see below). Do not change the pin. A different artifact id is a new artifact, not a repair. On an object store, the restored object has a new storage generation, and the verifications bind to it.
3. **Preauthorize the workflow** after the bytes are repaired:

   ```console
   munarium-server artifact-damage authorize-repair --tenant <tenant-id> \
       --incident <incident-id> --workflow <workflow-name> --actor <your-name> \
       [--reason "<ticket or change id>"]
   ```

   Verification fetches count only if they start after this command runs. Re-running with the same workflow returns the same attempt. A different workflow supersedes it.
4. **Wait for verification.** Each `observe` or `enforce` replica verifies the attempt once, in its readiness sweep (`MUNARIUM_DATASTORE_ROLLOUT_REFRESH_MS`, 15 s by default). Clearance needs two different `enforce` replicas. In a deployment declared single-replica, the replica verifies once and again after the single-replica interval, and its two matching runs clear the block. Without that declaration, a single-replica deployment cannot clear a block: run a second replica with a distinct instance id for the verification.
5. **Confirm** with `list`. The incident shows `cleared`, the attempt and workflow ids, both verifying replicas and `clear_assurance`. Replicas that had quarantined the artifact re-fetch and re-verify it before serving.

If a verifier finds a mismatch during verification, the attempt is invalidated. Repair the bytes again and authorize again. If the object is rewritten during verification, its generation changes and the first match is replaced. With two replicas the attempt can then stall, because the first replica does not verify the same attempt twice. Authorize a new workflow to start a fresh attempt. A block whose bytes cannot be repaired stays in force. Bind a rebuilt artifact through the normal promotion path instead.

## Builds of a blocked artifact

A rebuild of unchanged content produces the same artifact id it produced before. When that artifact is blocked, the building replica's `MUNARIUM_ARTIFACT_DAMAGE_MODE` decides:

| Mode | Effect |
|---|---|
| `off` | Unchanged. The build reports `already_built` or `converged`. |
| `observe` | The build succeeds as before. The replica logs `a build's result is a fleet-blocked artifact` with the version, artifact and incident ids, and counts `munarium_artifact_publication_blocked_total{action="published"}`. Serving still refuses the artifact. |
| `enforce` | The build is refused before anything is bound or uploaded. A build job fails with `error_code` [`artifact_blocked`](../api/errors.md#job-failure-artifact_blocked), a synchronous rebuild returns [`artifact-blocked`](../api/errors.md#problem-artifact-blocked) (409), and a backfill reports that version as `failed`. Retrying does not help: the refusal holds until the block is cleared. The log line `publication refused: the build's result is a fleet-blocked artifact` names the ids. Counts `action="refused"`. |

An unavailable integrity check also refuses the build in `enforce`, with `datastore_unavailable` (`datastore-unavailable`), because it can pass on a later attempt. Clear the block through the repair procedure above; the same rebuild then succeeds. [Publication of a blocked pin](../design/artifact-damage.md#publication-of-a-blocked-pin)

## Roll back safely

Before you set `off` on any serving replica with an incident still `blocked`: first withdraw the affected scope (rollout selector to `postgres`), or make sure the artifact is not in the `serving` slot. An `off` reader cannot see the block. The tables are additive, so the block remains in force when you switch back. Returning to a v1.4.0 binary is by restoring the pre-upgrade backup ([rolling back to 1.4.0](../guides/server-1.5.md#rolling-back-to-140)); the restored database has no block, so withdraw or repair the affected artifact before v1.4.0 serves it.

## Limits

- Verifier independence relies on each replica having its own enrolled key. Replicas share one database credential, so whoever holds it can also enroll keys.
- A declared single-replica deployment confirms with one replica's view. See the residual risk above.
- Storage generations are recorded only where the store reports them. The local-file store reports none.
- The publication check runs when a build takes its result; a block committed afterwards is enforced by readers, not by the build.
- The release fence operates on whole search results.
- Multi-replica qualification on a real fleet is pending.

See the design page's [open questions](../design/artifact-damage.md#open-questions).
