# Platform authority and decision records

Server provides an explicit `platform-v1` authority profile, independent of legacy
write and management credentials. Set `MUNARIUM_AUTHORITY_PROFILE=platform-v1`
and `MUNARIUM_PLATFORM_CONFIG_FILE` to an operator-owned JSON configuration. Its
`certificate_file`, `private_key_file` and `client_ca_file` configure direct mTLS
on both REST and gRPC. `peers` maps lowercase SHA-256 fingerprints of client leaf
certificates to `service`, `tenants` and `scopes` (`read`, `record`, `govern`).
Forwarding headers never establish peer identity.

Each `tenants` entry contains `authority` (deployment, tenant, audience, positive
epoch and enrolled bootstrap public keys) and an absolute `checkpoint_file` path.
A bootstrap key binds its issuer, authenticated presenter service and explicitly
enrolled human subjects. Run `munarium-server platform-enroll` once with this
configuration to create the initial checkpoint. This command refuses to replace
an existing checkpoint or initialize used authority. Enrollment grants no authority
to an ordinary writer. Signing keys are external operator inputs and never belong
in source, image layers, client configuration examples or logs.

`GET /v1/platform/{tenant}/authority` reads the current fenced state. `POST` to the
same path accepts an `attestation` compact JWS and an `artifact` object. Equivalent
named gRPC operations and all four SDKs use the same handlers and errors. The
artifact carries `schema_version=1`, `bindings`, `retire_bootstrap` and
`successor_keys`. The signed attestation binds deployment, tenant, audience, issuer,
human subject, action `install-governance`, exact artifact digest, expected revision
and head, epoch, nonce, and a validity interval of at most 300 seconds. The protected
header uses `alg=Ed25519`, enrolled `kid`, and `typ=munarium-authority+jws`.
See the [authority types](../src/munarium-core/src/platform_authority.rs) for exact
required fields and the [live test](../../clients/python/tests/test_platform_authority_live.py)
for a complete ceremony with newly generated disposable keys and certificates.

PostgreSQL commits the new authority state, immutable artifact/receipt and nonce
together. An exact retry returns its original receipt; changed content under the
same nonce conflicts. Retirement atomically installs successor keys and cannot
reintroduce a bootstrap key, including under a different key ID. The independently
retained checkpoint is written before database activation and reconciled afterward.
Missing, corrupt, locked or inconsistent checkpoint state fails closed. Protect and
retain this file independently of database backups; restoring both to an older
version cannot be detected by a local checkpoint alone. This profile supports one
Server replica. Memory mode is disposable and refuses a restart after activation
when its surviving checkpoint is ahead of its empty state.

Platform mode refuses legacy governing mutations, including indirect shape/runbook
application and index cutover. Ordinary version creation cannot install a governance
policy or transition; child versions can retain inherited policy. A PostgreSQL
database containing platform enrollment refuses legacy startup. Existing databases
without enrollment retain their legacy behavior. These APIs install platform
bindings; they do not translate arbitrary bindings into legacy Server configuration.

Local checks cover memory/PostgreSQL authority transactions, crash recovery and real
REST/gRPC mTLS requests. CI has a separate `platform-authority` job and database.
Separate-process Harness integration also passes against both stores. These local
checks do not establish human acceptance or production qualification.

## Decision records

The internal [platform module](../src/munarium-core/src/platform.rs) stores Stage 1
proposal, decision and refusal events and immutable replay bundles over the existing
`StorageBackend`. It uses the unchanged proposed
[hub candidate](../contract/platform-stage1/README.md). The platform profile exposes
`POST /v1/platform/{tenant}/records` through REST and the equivalent named gRPC/SDK
operation. Its closed body contains original signed `chain` and `action`: `append`
with canonical event bytes, `source-head`, `lookup` with operation ID, `archive`
with canonical replay bundle bytes, or `replay` with operation ID.

`EventLedger` requires a tenant-scoped store and a reserved version that ordinary writers
cannot access. `RecorderContext` is a privileged adapter result, not a deserializable
request identity. The service verifies the original assertion using Warden
verification source and current `identity:<recipient>` operator policy. It binds the
actual certificate presenter, tenant, recipient and `records:<tenant>` read/propose
scope. Stored audit metadata preserves verified origin, actor, kind and assertion
digest. PostgreSQL maps each external tenant to a reserved physical store/version;
ordinary legacy credentials cannot access it. The authority checkpoint remains
locked through admission and durable recording.

Events bind source/epoch/sequence/predecessor, tenant/operation/request, exact payload
and event digests. Same event ID and bytes return the original acknowledgement. Changed
content, conflicting operation bindings/results, sequence gaps and cross-tenant contexts
refuse. Expected-head appends serialize competing writers; contention is bounded and
must be handled as failure, never success. A replay bundle is archived only after matching
proposal/result records exist, is immutable per operation, and is checked on retrieval.
There is no global ordering claim across sources. The implementation scans a pinned
reserved ledger, so it is a correctness-oriented implementation, not a scale-qualified event service.

```console
cargo test --offline --locked -p munarium-core -p munarium-store-mem
cargo test --offline --locked -p munarium-store-pg --test platform -- --ignored
```

Run from `server/`. The explicitly ignored PostgreSQL test requires an isolated
`MUNARIUM_TEST_DATABASE_URL`; without it the selected test fails rather than passing
vacuously. Use the pgvector image pinned in [docker-compose.yml](../docker-compose.yml),
fictional credentials and a unique loopback port/container. The live test exercises
concurrent duplicate appends, reconnect persistence, changed-content conflict and tenant
isolation. Harness's Stage 1 composition also exercises actual Gate/Warden/Registry
against both MemStore and PgStore, including replay archive and recording outage.

The authority profile above supplies non-agent enrollment, retirement and governing
route admission. The event library alone does not establish their qualification.
Historical principal-key verification and network isolation are also distinct from
replay bundle integrity.
