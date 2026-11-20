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

An unknown value stops Datastore infrastructure from arming, and a `datastore` replica stays unready. Check the startup log for `MUNARIUM_ARTIFACT_DAMAGE_MODE`.

## What you will see

- **Local isolation.** One replica logs `artifact integrity failure verified against the authoritative store`. Its reads of that artifact return `datastore-unavailable` (`integrity (quarantined)`). Other replicas keep serving.
- **Fleet block.** After a second replica independently observes the same mismatch, every replica refuses the artifact with `datastore-unavailable`. The refusal names `blocked by integrity incident inc-…`. The warmer records the residency as `blocked`. Replica readiness does not change, and other scopes keep serving.
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
4. **Wait for verification.** Each `observe` or `enforce` replica verifies the attempt once, in its readiness sweep (`MUNARIUM_DATASTORE_ROLLOUT_REFRESH_MS`, 15 s by default). Clearance needs two different `enforce` replicas. A single-replica deployment cannot clear a block. Run a second replica with a distinct instance id for the verification.
5. **Confirm** with `list`. The incident shows `cleared`, the attempt and workflow ids, and both verifying replicas. Replicas that had quarantined the artifact re-fetch and re-verify it before serving.

If a verifier finds a mismatch during verification, the attempt is invalidated. Repair the bytes again and authorize again. A block whose bytes cannot be repaired stays in force. Bind a rebuilt artifact through the normal promotion path instead.

## Roll back safely

Before you set `off` on any serving replica, or roll back to a v1.4 binary, with an incident still `blocked`: first withdraw the affected scope (rollout selector to `postgres`), or make sure the artifact is not in the `serving` slot. A v1.4 or `off` reader cannot see the block. The tables are additive, so the block remains in force when you upgrade again.

## Limits

- Verifier independence relies on distinct instance ids, because replicas share one database credential.
- Storage object generations are not yet recorded.
- The release fence operates on whole search results.
- Multi-replica qualification on a real fleet is pending.

See the design page's [open questions](../design/artifact-damage.md#open-questions).
