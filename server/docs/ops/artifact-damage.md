# Artifact damage: isolation, fleet blocks and verified repair

This procedure covers what happens when a Datastore artifact's bytes no longer match its pin. It also covers how to repair the artifact and return it to service. The feature is opt-in and off by default. The specification, including the definitions it relies on, is in the [design page](../design/artifact-damage.md). Those definitions are proposals pending the ART1 and ART2 hub decisions.

## Enable it

`MUNARIUM_ARTIFACT_DAMAGE_MODE` applies to replicas in `shadow` or `datastore` retrieval mode:

| Value | Effect |
|---|---|
| `off` (default) | v1.4 behavior: a bad copy is quarantined in that process only. |
| `observe` | Records findings and honors existing blocks. Never commits a block or a clearance. |
| `enforce` | Also commits confirmed fleet blocks and verified clearances. |

1. Give every replica a distinct, stable instance id (`MUNARIUM_INSTANCE_ID`, or the pod `HOSTNAME`). Two replicas with the same id count as one verifier.
2. Upgrade every serving replica, then set `observe` everywhere. Drain or replace any v1.4 reader.
3. Switch every serving replica to `enforce`.

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
- **History full.** Logs report `history_full = true`. Findings, blocks and clearances still work. Review and resolve the incidents. Entries are never evicted.

List the state for a tenant:

```console
munarium-server artifact-damage list --tenant <tenant-id>
```

The command prints JSON with each pin's generation and state, each incident's object, expected digest, both confirming verifiers, and any clearance. It reads `MUNARIUM_DATABASE_URL` and the store settings like the server does.

## Repair and clear

A block is cleared only by a preauthorized repair workflow, after two serving replicas independently fetch the repaired bytes and both match the pin. No API route or console action clears a block.

1. **Withdraw if needed.** Reads of the blocked artifact already fail. To serve the scope from PostgreSQL meanwhile, set its rollout selector to `postgres`. Selecting `datastore` again is refused while the artifact is blocked.
2. **Repair the bytes** under your separate authority to change artifact storage. Restore the named object (`object_key` in the listing) under the artifact's prefix from a copy whose `sha256` equals the `expected_sha256`. Alternatively, republish the same artifact id from a verified build. Do not change the pin. A different artifact id is a new artifact, not a repair.
3. **Preauthorize the workflow** after the bytes are repaired:

   ```console
   munarium-server artifact-damage authorize-repair --tenant <tenant-id> \
       --incident <incident-id> --workflow <workflow-name> --actor <your-name> \
       [--reason "<ticket or change id>"]
   ```

   Verification fetches count only if they start after this command runs. Re-running with the same workflow returns the same attempt. A different workflow supersedes it.
4. **Wait for verification.** Each `observe` or `enforce` replica verifies the attempt once, in its readiness sweep (`MUNARIUM_DATASTORE_ROLLOUT_REFRESH_MS`, 15 s by default). Clearance needs two different `enforce` replicas. In a deployment declared single-replica, the replica verifies once and again after the single-replica interval, and its two matching runs clear the block. Without that declaration, a single-replica deployment cannot clear a block: run a second replica with a distinct instance id for the verification.
5. **Confirm** with `list`. The incident shows `cleared`, the attempt and workflow ids, both verifying replicas and `clear_assurance`. Replicas that had quarantined the artifact re-fetch and re-verify it before serving.

If a verifier finds a mismatch during verification, the attempt is invalidated. Repair the bytes again and authorize again. A block whose bytes cannot be repaired stays in force. Bind a rebuilt artifact through the normal promotion path instead.

## Roll back safely

Before you set `off` on any serving replica with an incident still `blocked`: first withdraw the affected scope (rollout selector to `postgres`), or make sure the artifact is not in the `serving` slot. An `off` reader cannot see the block. The tables are additive, so the block remains in force when you switch back. Returning to a v1.4.0 binary is by restoring the pre-upgrade backup ([rolling back to 1.4.0](../guides/server-1.5.md#rolling-back-to-140)); the restored database has no block, so withdraw or repair the affected artifact before v1.4.0 serves it.

## Limits

- Verifier independence relies on distinct instance ids, because replicas share one database credential.
- A declared single-replica deployment confirms with one replica's view. See the residual risk above.
- Storage object generations are not yet recorded.
- The release fence operates on whole search results.
- Multi-replica qualification on a real fleet is pending.

See the design page's [open questions](../design/artifact-damage.md#open-questions).
