# Deploying and operating Munarium Datastore

Munarium Datastore serves retrieval from immutable search artifacts. PostgreSQL
continues to hold the ledger, collection and index metadata, artifact catalog,
bindings, jobs and rollout decisions. Raw documents remain in the configured
source store. Enabling Datastore does not replace either of those stores.

This guide covers the Server implementation shipped with `iokaio/munarium:1.0.0`:
configuration, local deployment, building and verifying artifacts, promotion,
serving, rollback and ongoing operation. For first-time corpus setup, complete
[getting started](getting-started.md) before following the local walkthrough.

## 1. Understand the storage and identity layers

| Layer | Contents | Operational treatment |
|---|---|---|
| PostgreSQL | Ledger, sources metadata, logical indexes, catalog, bindings and rollout state | Keep and back up; Datastore requires it |
| Source store | Original document bytes, selected by `MUNARIUM_SOURCE_STORE` | Preserve independently; `pg` keeps bytes in PostgreSQL |
| L2 artifact store | Sealed manifests and search-engine files | Durable storage, available to every serving replica |
| L1 local cache | Hydrated, verified artifact copies | Per-replica disk; can be rebuilt from L2 |
| L0 open-shard cache | Open search-engine handles | Per-process memory; repopulated after restart |
| Staging | Work in progress before sealing and publication | Builder disk with enough temporary capacity |

An `index_version_id` identifies a logical indexed corpus. An `artifact_id`
identifies the content of one physical realization, including its search engines.
An engine upgrade can produce another artifact without changing the logical
version or a session's version pin. An artifact hash grants no access: operations
still authenticate a tenant and name its logical version.

The binding slots are `staged`, `shadow` and `serving`. A verified candidate can
be bound to `staged` or `shadow`; only promotion changes `serving`. A collection's
active logical version and its retrieval engine selector are separate decisions.

## 2. Choose the process mode and scope selectors

| `MUNARIUM_RETRIEVAL_MODE` | User retrieval | Purpose |
|---|---|---|
| `postgres` | PostgreSQL | Baseline and ordinary deployment default |
| `mirror` | PostgreSQL | Build artifacts alongside the reference index without serving them |
| `shadow` | PostgreSQL | Sample candidate Datastore retrieval for comparison |
| `datastore` | Per collection/legacy-shape rollout selector | Serve selected scopes from verified artifacts; unselected scopes remain on PostgreSQL |

Use `mirror` to prepare an existing corpus, optionally measure `shadow`, then
start the process in `datastore` mode with selectors still set to `postgres`.
Prewarm and promote candidates before selecting Datastore for each scope.
Changing the environment variable alone does not move every collection.

Modes other than `postgres` require the PostgreSQL catalog and a local root.
Other missing capabilities can cause mirror/shadow to degrade to PostgreSQL;
Datastore serving refuses startup when it cannot support the requested mode.
An unknown mode logs a warning and falls back to PostgreSQL. Inspect the effective
mode and capabilities in `/admin/storage`, rather than accepting a healthy process
as proof that Datastore is enabled.

After a scope is selected for Datastore, a missing/unusable serving artifact
causes a refusal, not a silent query against PostgreSQL. `/readyz` reflects the
replica's ability to serve its required scopes; `/healthz` only shows liveness.

## 3. Complete environment reference

The following tables list all 22 `MUNARIUM_DATASTORE_*` variables read by the
current code, plus the associated retrieval, identity and lifetime settings.
Defaults apply to this implementation. Configure numeric values explicitly as
plain integers in the documented units: several parsers fall back to defaults
on malformed input. Environment changes require container recreation.

### Storage and engine selection

| Variable | Default | Meaning and constraints |
|---|---|---|
| `MUNARIUM_RETRIEVAL_MODE` | `postgres` | `postgres`, `mirror`, `shadow` or `datastore`; see mode table |
| `MUNARIUM_DATASTORE_LOCAL_ROOT` | Unset | Writable local base directory, required outside PostgreSQL mode. L1 is stored beneath `<root>/l1` |
| `MUNARIUM_DATASTORE_ARTIFACT_STORE` | Unset in capability detection | Set explicitly to `file`, `az`, `s3` or `gcs`. The factory defaults an omitted value to `file`, but capability detection treats omission as unavailable. `pg` is not implemented by the artifact factory |
| `MUNARIUM_DATASTORE_ARTIFACT_CONTAINER` | `indexes` | Separate artifact container/bucket for cloud storage; provision it and grant access |
| `MUNARIUM_DATASTORE_ARTIFACT_PREFIX` | `v1` | Object-key prefix for published artifacts; keep consistent across readers and builders |
| `MUNARIUM_DATASTORE_ARTIFACT_ROOT` | `<LOCAL_ROOT>/l2` | Durable directory for the `file` artifact store; explicit value allows a separate volume |
| `MUNARIUM_DATASTORE_STAGING_ROOT` | `<LOCAL_ROOT>/staging` | Build workspace; explicit value allows separate temporary storage |
| `MUNARIUM_DATASTORE_VECTOR_APPROX_THRESHOLD` | `4096` | Direct builds with vectors select DiskANN when the chunk count is **at least** this threshold and `vector-diskann` is compiled in. `off` forces exact vectors; numeric values are clamped to at least 1; malformed values warn and select exact. Does not change already sealed artifacts or the mirror build's physical plan |

The `file` artifact store is a useful single-machine choice. Keep L2 separate
from `<LOCAL_ROOT>/l1`; do not point durable artifacts at an eviction directory.
Separate sibling directories on a persistent volume are sufficient for the local
walkthrough. Keep enough free space for L2, L1 and staging together.

Cloud artifacts reuse the source-store configuration's account/region/endpoint
and credential mechanism, replacing its container/bucket with the artifact
container. Use matching cloud selectors, such as `MUNARIUM_SOURCE_STORE=s3` and
`MUNARIUM_DATASTORE_ARTIFACT_STORE=s3`. The factory derives the actual cloud client
from the **source-store configuration**; the artifact selector is not an
independent cloud credential configuration. A `pg`, `mem` or `file` source store
cannot supply that cloud client. There is no separate artifact account or
artifact-key environment variable. A source-container-scoped SAS may not permit
access to the artifact container; provision credentials covering the intended
operations on both without putting tokens in recorded URLs.

Use the [source-store configuration guide](source-stores.md) for all Azure, S3
and GCS endpoint/credential options and [Managing keys and secrets](managing-key-and-secrets.md#9-other-backend-secrets)
for secret references and rotation. Selecting `MUNARIUM_SOURCE_STORE=pg` remains
supported for **raw documents**, independently of the artifact backend restriction.

### Cache capacity, pins and retention

| Variable | Default | Meaning and constraints |
|---|---|---|
| `MUNARIUM_DATASTORE_L1_HIGH_WATERMARK` | `8589934592` (8 GiB) | L1 byte budget that triggers eviction; not a total-volume budget |
| `MUNARIUM_DATASTORE_L1_LOW_WATERMARK` | `6442450944` (6 GiB) | Target after eviction; must be strictly below the high watermark |
| `MUNARIUM_DATASTORE_L0_OPEN_SHARDS` | `8` | Maximum cached open shards; size for concurrent collection/version use and process memory |
| `MUNARIUM_DATASTORE_PIN_HORIZON` | Derived | Seconds a deactivated version remains serving-required; the derived minimum is `max(session_idle_ttl_secs, 1) + 21600` today |
| `MUNARIUM_DATASTORE_ALLOW_SHORT_PIN_HORIZON` | `false` | Only case-insensitive `true` bypasses the minimum-horizon check; accepts the possibility that a supported session outlives artifact retention |
| `MUNARIUM_DATASTORE_RETIRED_RETENTION` | Twice the **derived** horizon | Seconds; validation requires it to be at least the configured pin horizon. Raising an explicit horizon does not automatically raise this default |

There is no separately configurable runbook TTL or recovery-margin variable in
this code; the derivation uses the session TTL for both lifetimes and adds six
hours. `MUNARIUM_SESSION_IDLE_TTL_SECS=0` means sessions do not expire, so no
finite horizon fully covers them. Startup warns when that value is combined
with an omitted horizon. Choose a finite session policy for the tutorial and
review the effect on existing sessions before changing it on a live deployment.

The required set includes active versions and versions within the horizon from
**deactivation**, not from original build time. Never manually remove L2 bytes
while a supported pin may need them. Retention settings do not constitute a
complete object-store lifecycle policy or authorize deleting catalog rows.

### Building, reconciliation and readiness

| Variable | Default | Meaning and constraints |
|---|---|---|
| `MUNARIUM_DATASTORE_BUILDER` | Off | Case-insensitive `enabled` starts the durable build-job worker; it requires PostgreSQL. Does not enqueue work or select a serving engine |
| `MUNARIUM_DATASTORE_BUILDER_POLL_MS` | `5000` | Queue polling milliseconds, clamped to at least 250 |
| `MUNARIUM_DATASTORE_JOB_LEASE_SECS` | `600` | Claimed-job lease seconds, clamped to at least 30; the worker renews it during execution |
| `MUNARIUM_DATASTORE_RECONCILE_INTERVAL_SECS` | `60` | Interrupted sealed-build reconciliation interval, clamped to at least 30 seconds; also runs at startup |
| `MUNARIUM_DATASTORE_ROLLOUT_REFRESH_MS` | `15000` | Serving warmer/selector refresh interval, clamped to at least 1000 milliseconds |
| `MUNARIUM_DATASTORE_STARTUP_HYDRATE_TIMEOUT_MS` | `120000` | Startup hydration deadline used by the readiness warmer; does not permit serving incomplete scopes after expiry |

The reconciler handles interrupted build publication; it is not a general
database-to-cache synchronization or backup service. Synchronous rebuild/backfill
API calls do not require the queued-job worker, but they still require the
artifact and staging configuration.

### Shadow comparison

| Variable | Default | Meaning and constraints |
|---|---|---|
| `MUNARIUM_DATASTORE_SHADOW_SAMPLE_RATE` | `0` | One in N eligible turns sampled; 0 disables comparisons, 1 samples every eligible turn |
| `MUNARIUM_DATASTORE_SHADOW_MAX_CONCURRENT` | `2` | Maximum concurrent shadow comparisons; choose a positive capacity |
| `MUNARIUM_DATASTORE_QUERY_TIMEOUT_MS` | `5000` | Shadow candidate-query deadline, clamped to at least 1 millisecond; not a global timeout for all Datastore serving requests |

Shadow comparisons require `shadow` mode, usable artifacts and configured sampling.
Inspect the shadow counters in `/admin/storage`: completed comparisons, skipped
work, timeouts and dropped work matter when assessing coverage. Merely selecting
shadow mode with a zero sample rate produces no comparison evidence.

### Related Server settings

| Variable | Default | Datastore relevance |
|---|---|---|
| `MUNARIUM_STORE` | `memory` | Set `postgres` for catalog, bindings, jobs and rollout decisions |
| `MUNARIUM_DATABASE_URL` | Unset | Required PostgreSQL connection URI; keep credentials private |
| `MUNARIUM_SOURCE_STORE` | `az` with PostgreSQL; otherwise `mem` | Original document location and, for cloud artifacts, the client/credential source |
| `MUNARIUM_SESSION_IDLE_TTL_SECS` | `0` | Session expiry seconds; feeds the pin-horizon derivation |
| `MUNARIUM_INSTANCE_ID` | `HOSTNAME`, then `COMPUTERNAME`, then random fallback | Unique identity for each live replica/worker; do not share one across replicas |
| `MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID` | `local` | Environment scope of fleet snapshots/expectations; isolate independent deployments |
| `MUNARIUM_DEPLOYMENT_PLANE` | `rest` | Plane label in serving-node snapshots, matched by fleet expectations |
| `MUNARIUM_DEPLOYMENT_REVISION` | `local` | Revision label in serving-node snapshots; use the deployment's actual revision |

For connection pooling, authentication, TLS and other general Server options, use
the full [Server environment reference](../../README.md#configuration-env-vars).
Artifact envelope formats and compiled lexical/vector engines are binary
capabilities, not additional environment switches. Inspect the deployed image's
capabilities before promoting an artifact built by a different binary.

## 3A. Configure on-premises and cloud deployments

These profiles show concrete values for every setting in the reference above.
They are starting examples, not measured capacity recommendations. Choose one
profile, substitute your infrastructure and secret references, and retain the
build/verify/promote/rollout sequence below. Both start in `mirror` mode so that
installing the configuration does not immediately move user queries to Datastore.

### On-premises: Docker Compose with local durable artifacts

For a single Linux host or Docker Desktop in Linux container mode, merge this
service fragment into your existing Compose file. Preserve its authentication,
ports and database dependency configuration. `MUNARIUM_DATABASE_URL` below is a
private **host** environment variable or Compose interpolation value containing
the full connection URI. The service mapping explicitly passes it to the Server;
a host `.env` file alone would not do that. See
[Compose environment configuration](https://docs.docker.com/compose/how-tos/environment-variables/set-environment-variables/).

```yaml
services:
  server:
    image: iokaio/munarium:1.0.0
    environment:
      MUNARIUM_STORE: postgres
      MUNARIUM_DATABASE_URL: ${MUNARIUM_DATABASE_URL:?Supply the private database URI}
      MUNARIUM_SOURCE_STORE: pg
      MUNARIUM_RETRIEVAL_MODE: mirror
      MUNARIUM_DATASTORE_LOCAL_ROOT: /datastore/cache
      MUNARIUM_DATASTORE_ARTIFACT_STORE: file
      MUNARIUM_DATASTORE_ARTIFACT_ROOT: /datastore/artifacts
      MUNARIUM_DATASTORE_ARTIFACT_PREFIX: corpora/v1
      # Only used by cloud artifact backends; harmless but unused with file.
      MUNARIUM_DATASTORE_ARTIFACT_CONTAINER: indexes
      MUNARIUM_DATASTORE_STAGING_ROOT: /datastore/staging
      MUNARIUM_DATASTORE_L1_HIGH_WATERMARK: "8589934592"
      MUNARIUM_DATASTORE_L1_LOW_WATERMARK: "6442450944"
      MUNARIUM_DATASTORE_L0_OPEN_SHARDS: "16"
      MUNARIUM_SESSION_IDLE_TTL_SECS: "3600"
      MUNARIUM_DATASTORE_PIN_HORIZON: "43200"
      MUNARIUM_DATASTORE_ALLOW_SHORT_PIN_HORIZON: "false"
      MUNARIUM_DATASTORE_RETIRED_RETENTION: "86400"
      MUNARIUM_DATASTORE_BUILDER: enabled
      MUNARIUM_DATASTORE_BUILDER_POLL_MS: "5000"
      MUNARIUM_DATASTORE_JOB_LEASE_SECS: "600"
      MUNARIUM_DATASTORE_RECONCILE_INTERVAL_SECS: "60"
      MUNARIUM_DATASTORE_ROLLOUT_REFRESH_MS: "15000"
      MUNARIUM_DATASTORE_STARTUP_HYDRATE_TIMEOUT_MS: "120000"
      # Armed for a later change to shadow mode; inactive in mirror mode.
      MUNARIUM_DATASTORE_SHADOW_SAMPLE_RATE: "10"
      MUNARIUM_DATASTORE_SHADOW_MAX_CONCURRENT: "2"
      MUNARIUM_DATASTORE_QUERY_TIMEOUT_MS: "5000"
      MUNARIUM_DATASTORE_VECTOR_APPROX_THRESHOLD: "off"
      MUNARIUM_INSTANCE_ID: onprem-server-01
      MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID: onprem-lab
      MUNARIUM_DEPLOYMENT_PLANE: rest
      MUNARIUM_DEPLOYMENT_REVISION: datastore-r1
    volumes:
      - datastore-local:/datastore
volumes:
  datastore-local:
```

Use a fresh volume or explicitly prepare ownership for UID 65532. Adapt the
section 4 initialization helper to mount `datastore-local` at `/datastore`;
initialize before starting the Server. L1 will be `/datastore/cache/l1`, with
durable L2 at `/datastore/artifacts` and build workspace at `/datastore/staging`.
Back up the artifact directory. Reserve disk beyond the 8 GiB L1 watermark for
all durable artifacts and concurrent staging work.

This profile permits one-hour idle sessions, a twelve-hour pin horizon and
twenty-four-hour retention. The horizon exceeds the seven-hour derived minimum;
retention exceeds the explicitly configured horizon. Exact vectors provide a
baseline for later approximate-search comparison. The 1-in-10 sample rate only
takes effect after changing `MUNARIUM_RETRIEVAL_MODE` to `shadow` and recreating
the Server. Later use `datastore` mode for prewarming and selected-scope serving.
Keep `ALLOW_SHORT_PIN_HORIZON=false` for normal deployments in both environments.

Give another host a different `MUNARIUM_INSTANCE_ID`. This local-volume example
does not distribute L2 to another machine; for multiple on-premises hosts, use
shared object storage such as a provisioned S3-compatible service. Replace the
storage-related environment entries with this overlay, and mount the private
key file at the indicated container path:

```yaml
MUNARIUM_SOURCE_STORE: s3
MUNARIUM_S3_BUCKET: munarium-sources
MUNARIUM_S3_REGION: us-east-1
MUNARIUM_S3_ENDPOINT: https://minio.example.internal:9000
MUNARIUM_S3_FORCE_PATH_STYLE: "true"
MUNARIUM_S3_ACCESS_KEY_ID: munarium-lab-writer
MUNARIUM_S3_SECRET_KEY_REF: file:/run/secrets/s3-key
MUNARIUM_DATASTORE_ARTIFACT_STORE: s3
MUNARIUM_DATASTORE_ARTIFACT_CONTAINER: munarium-indexes
MUNARIUM_DATASTORE_ARTIFACT_PREFIX: onprem-lab/v1
```

Use the access-key ID actually issued by your object store. Provision both
buckets, ensure the HTTPS certificate is trusted by the container, and grant
the identity the required source/artifact operations. Remove
`MUNARIUM_DATASTORE_ARTIFACT_ROOT` with this overlay: L2 is now the artifact bucket.
Retain local cache and staging directories. On Compose, declare an `s3-key`
file-backed secret and grant it to `server`, as shown in
[Managing keys and secrets](managing-key-and-secrets.md#4-mount-an-ai-key-and-apply-its-provider-configuration).

### Cloud: Kubernetes configuration with S3 artifacts

The following ConfigMap is a concrete cloud profile. The bucket names are
placeholders you must replace with provisioned names. An already configured
workload identity supplies access through the ambient AWS credential chain;
neither the ConfigMap nor the service-account name grants those permissions.
Use matching S3 source and artifact stores, with distinct buckets and a shared
region. The cluster must also reach the external PostgreSQL service.

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: munarium-datastore
data:
  MUNARIUM_STORE: postgres
  MUNARIUM_SOURCE_STORE: s3
  MUNARIUM_S3_BUCKET: example-munarium-sources
  MUNARIUM_S3_REGION: us-east-1
  MUNARIUM_RETRIEVAL_MODE: mirror
  MUNARIUM_DATASTORE_LOCAL_ROOT: /datastore/cache
  MUNARIUM_DATASTORE_ARTIFACT_STORE: s3
  MUNARIUM_DATASTORE_ARTIFACT_CONTAINER: example-munarium-indexes
  MUNARIUM_DATASTORE_ARTIFACT_PREFIX: production/v1
  # ARTIFACT_ROOT is intentionally absent: only the file backend uses it.
  MUNARIUM_DATASTORE_STAGING_ROOT: /datastore/staging
  MUNARIUM_DATASTORE_L1_HIGH_WATERMARK: "34359738368"
  MUNARIUM_DATASTORE_L1_LOW_WATERMARK: "25769803776"
  MUNARIUM_DATASTORE_L0_OPEN_SHARDS: "32"
  MUNARIUM_SESSION_IDLE_TTL_SECS: "28800"
  MUNARIUM_DATASTORE_PIN_HORIZON: "86400"
  MUNARIUM_DATASTORE_ALLOW_SHORT_PIN_HORIZON: "false"
  MUNARIUM_DATASTORE_RETIRED_RETENTION: "172800"
  MUNARIUM_DATASTORE_BUILDER: enabled
  MUNARIUM_DATASTORE_BUILDER_POLL_MS: "2000"
  MUNARIUM_DATASTORE_JOB_LEASE_SECS: "1200"
  MUNARIUM_DATASTORE_RECONCILE_INTERVAL_SECS: "60"
  MUNARIUM_DATASTORE_ROLLOUT_REFRESH_MS: "5000"
  MUNARIUM_DATASTORE_STARTUP_HYDRATE_TIMEOUT_MS: "300000"
  MUNARIUM_DATASTORE_SHADOW_SAMPLE_RATE: "100"
  MUNARIUM_DATASTORE_SHADOW_MAX_CONCURRENT: "4"
  MUNARIUM_DATASTORE_QUERY_TIMEOUT_MS: "10000"
  MUNARIUM_DATASTORE_VECTOR_APPROX_THRESHOLD: "8192"
  MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID: production-us-east-1
  MUNARIUM_DEPLOYMENT_PLANE: rest
  MUNARIUM_DEPLOYMENT_REVISION: datastore-r1
```

The cloud values allow a 32 GiB L1 cache with a 24 GiB eviction target, eight-hour
idle sessions, a twenty-four-hour horizon and forty-eight-hour retention. The
derived minimum is fourteen hours, so both lifetime constraints hold. The
five-minute hydration deadline accommodates a larger cold cache; size it from
measured downloads rather than treating a larger deadline as a repair for failed
storage access. Direct builds may use approximate vectors at 8192 chunks;
validate recall before promoting those artifacts. Shadow mode samples 1 in 100
eligible turns, up to four concurrent comparisons with a ten-second deadline.

Inject this ConfigMap and private credentials into the Server container using
`envFrom` and `secretKeyRef`. Obtain a unique replica identity from the Pod name.
This is a **Deployment Pod-template fragment** to merge into an existing workload,
not a complete Deployment or an instruction to expose the service publicly:

```yaml
spec:
  template:
    spec:
      serviceAccountName: munarium-storage
      securityContext:
        runAsUser: 65532
        runAsGroup: 65532
        fsGroup: 65532
      containers:
        - name: server
          image: iokaio/munarium:1.0.0
          envFrom:
            - configMapRef:
                name: munarium-datastore
          env:
            - name: MUNARIUM_DATABASE_URL
              valueFrom:
                secretKeyRef:
                  name: munarium-runtime
                  key: database-url
            - name: MUNARIUM_AUTH_MODE
              value: static
            - name: MUNARIUM_STATIC_TOKENS
              valueFrom:
                secretKeyRef:
                  name: munarium-runtime
                  key: static-tokens
            - name: MUNARIUM_TOKEN_SECRET
              valueFrom:
                secretKeyRef:
                  name: munarium-runtime
                  key: token-secret
            - name: MUNARIUM_INSTANCE_ID
              valueFrom:
                fieldRef:
                  fieldPath: metadata.name
          volumeMounts:
            - name: datastore-cache
              mountPath: /datastore/cache
            - name: datastore-staging
              mountPath: /datastore/staging
      volumes:
        - name: datastore-cache
          emptyDir:
            sizeLimit: 48Gi
        - name: datastore-staging
          emptyDir:
            sizeLimit: 64Gi
```

Create `munarium-runtime` through your private secret-delivery process, in the
same namespace; its `database-url` value is a complete URI with the intended TLS
configuration, not a file path. Choose a workload identity supported by the cloud
credential chain or explicitly inject the S3 key pair through the supported
reference mechanism. Follow the Kubernetes documentation for
[environment injection](https://kubernetes.io/docs/tasks/inject-data-application/define-environment-variable-container/),
[Secret references](https://kubernetes.io/docs/tasks/inject-data-application/distribute-credentials-secure/) and
[Pod identity fields](https://kubernetes.io/docs/tasks/inject-data-application/environment-variable-expose-pod-information/).

The cache and staging volumes are disk-backed temporary storage. They are lost
when the Pod is removed; durable L2 remains in S3. Set node capacity and the
workload's ephemeral-storage requests/limits to cover both volumes plus logs and
container overhead: `sizeLimit` is not a reservation. Use suitable persistent
per-replica storage instead if repeated cold hydration is unacceptable. Retain
the deployment's readiness probes, resource limits, TLS/ingress and identity
configuration when merging this fragment.

For dedicated reader and builder deployments, omit `MUNARIUM_DATASTORE_BUILDER`
from the readers or override it to `disabled`; retain `enabled` on the builders.
Keep their database, environment ID and L2 namespace aligned. Do not put one
fixed instance ID into the shared ConfigMap. Set the actual plane/revision labels
per deployment and update fleet expectations before promotion. After ConfigMap
or environment-secret updates, roll out replacement Pods; running processes do
not automatically receive new environment values.

### Azure Blob and GCS alternatives

Keep the cloud profile's cache, lifetime, worker, readiness and shadow settings.
Replace **all** S3-specific entries and the source/artifact selectors with one of
the following maps; also replace the workload's credential delivery accordingly.

For Azure Blob on a platform exposing the managed-identity endpoint supported by
the Server (for example an appropriately configured Azure VM or Container Apps
deployment), the environment map is:

```yaml
MUNARIUM_SOURCE_STORE: az
MUNARIUM_AZURE_STORAGE_ACCOUNT: examplestorageaccount
MUNARIUM_AZURE_BLOB_CONTAINER: sources
MUNARIUM_BLOB_AUTH: managed_identity
MUNARIUM_DATASTORE_ARTIFACT_STORE: az
MUNARIUM_DATASTORE_ARTIFACT_CONTAINER: indexes
MUNARIUM_DATASTORE_ARTIFACT_PREFIX: production/v1
```

Provision both containers and assign the identity the necessary data-plane
access. For a user-assigned identity, also set `MUNARIUM_AZURE_CLIENT_ID` to its
client ID. Do not assume an arbitrary Kubernetes service account becomes an
Azure managed identity. Where ambient identity is unavailable, change
`MUNARIUM_BLOB_AUTH` to `sas` and set `MUNARIUM_BLOB_SAS_REF` to a delivered secret
reference such as `file:/run/secrets/blob-sas`; its authorization must cover both
source and artifact containers. The local cache/staging paths still require
writable container mounts even when L2 is Azure Blob.

For GCS with a deliberately supplied service-account JSON file:

```yaml
MUNARIUM_SOURCE_STORE: gcs
MUNARIUM_GCS_BUCKET: example-munarium-sources
MUNARIUM_GCS_CREDENTIALS_REF: file:/run/secrets/gcs-service-account.json
MUNARIUM_DATASTORE_ARTIFACT_STORE: gcs
MUNARIUM_DATASTORE_ARTIFACT_CONTAINER: example-munarium-indexes
MUNARIUM_DATASTORE_ARTIFACT_PREFIX: production/v1
```

Deliver the JSON as a readable secret mount; do not place its contents in the
ConfigMap. If the deployment instead supplies a supported ambient credential
chain, omit `MUNARIUM_GCS_CREDENTIALS_REF` and validate that identity's access to
both buckets. In either cloud alternative, omit `MUNARIUM_DATASTORE_ARTIFACT_ROOT`
because there is no local-file L2 directory.

These cloud maps are configuration examples checked against the Server's input
contract. They do not provision storage, identities or PostgreSQL, and have not
been exercised against live cloud resources as part of this guide. Before rollout,
verify credential access, artifact publication and hydration from every intended
replica, then repeat the query, cold-restart and rollback checks below.

## 4. Add local artifact storage to the Docker deployment

Start with the two-document collection and working PostgreSQL retrieval from
[getting started](getting-started.md). Keep its Compose project, PostgreSQL
volume, credentials and ports. Merge this fragment into that `compose.yaml`;
it shows **additions and replacements**, not a standalone deployment:

```yaml
services:
  datastore-init:
    image: pgvector/pgvector:pg16
    user: "0:0"
    profiles: [tools]
    entrypoint: ["/bin/sh", "-c"]
    command: ["chown 65532:65532 /datastore"]
    volumes:
      - datastore-data:/datastore
  server:
    environment:
      MUNARIUM_RETRIEVAL_MODE: mirror
      MUNARIUM_DATASTORE_LOCAL_ROOT: /var/lib/munarium-datastore
      MUNARIUM_DATASTORE_ARTIFACT_STORE: file
      MUNARIUM_DATASTORE_ARTIFACT_ROOT: /var/lib/munarium-datastore/l2
      MUNARIUM_DATASTORE_STAGING_ROOT: /var/lib/munarium-datastore/staging
      MUNARIUM_DATASTORE_BUILDER: enabled
      MUNARIUM_SESSION_IDLE_TTL_SECS: "3600"
      MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID: local-datastore
      MUNARIUM_DEPLOYMENT_PLANE: rest
      MUNARIUM_DEPLOYMENT_REVISION: local-guide
    volumes:
      - datastore-data:/var/lib/munarium-datastore
volumes:
  datastore-data:
```

This example's finite session TTL derives a seven-hour pin horizon and a
fourteen-hour retention default. The helper only changes ownership of the newly
created volume's root. Run it before the nonroot Server needs to create its
subdirectories; adapt permissions deliberately when using an existing populated
volume or Windows bind mount. The Server image has no shell for such repairs.

```powershell
docker compose config --quiet
if ($LASTEXITCODE -ne 0) { throw 'Invalid Compose configuration' }
docker compose run --rm --no-deps datastore-init
if ($LASTEXITCODE -ne 0) { throw 'Datastore volume initialization failed' }
docker compose up -d --no-deps --force-recreate server
if ($LASTEXITCODE -ne 0) { throw 'Server recreation failed' }
```

Wait for `/readyz` to return 200 and inspect `/admin/storage` using a `mgmt`
credential for your tenant. Confirm configured and effective `mirror` mode, a
usable catalog, artifact store, writable cache and compiled engines. For the
following API calls use the **rw** token from the local tutorial. If you changed
that token or the host port, update the values first.

```powershell
$base = 'http://127.0.0.1:18080'
$headers = @{ Authorization='Bearer devtoken'; 'X-Munarium-Uid'='datastore-operator' }
function Get-DatastoreJson([string]$Path) {
    Invoke-RestMethod "$base$Path" -Headers $headers
}
function Send-DatastoreJson([string]$Method, [string]$Path, $Body) {
    Invoke-RestMethod "$base$Path" -Method $Method -Headers $headers `
        -ContentType application/json -Body ($Body | ConvertTo-Json -Depth 10)
}
$collections = Get-DatastoreJson '/v1/collections'
$collection = @($collections.collections | Where-Object name -eq 'starter-documents')
if ($collection.Count -ne 1 -or -not $collection[0].active_index) {
    throw 'Complete the getting-started corpus build and approval first'
}
$collectionId = $collection[0].id
$activeIndex = $collection[0].active_index
```

The `active_index` is a logical index version. Do not use a ledger `version_id`,
run ID, artifact hash or collection name in its place.

## 5. Backfill and verify existing indexes

For the small tutorial collection, synchronously backfill every serving-required
version. Existing PostgreSQL indexes stay active throughout this step:

```powershell
$backfill = Send-DatastoreJson Post '/v1/index-artifacts/backfill' @{collection_id=$collectionId}
$backfill.versions | Format-Table index_version_id, reason, outcome, error
if (-not $backfill.complete -or -not @($backfill.versions).Count) {
    throw 'Backfill incomplete; inspect per-version outcomes before continuing'
}
$requiredVersions = @($backfill.versions.index_version_id)
foreach ($indexId in $requiredVersions) {
    $verified = Send-DatastoreJson Post "/v1/index-artifacts/$indexId/verify" @{}
    if (-not @($verified.results).Count -or @($verified.results | Where-Object { -not $_.verified }).Count) {
        throw "Artifact verification failed for $indexId"
    }
    Get-DatastoreJson "/v1/index-artifacts/$indexId"
}
```

`published`, `converged` and `already_built` report successful artifact outcomes;
`deferred` means another node holds the build, so poll/retry and inspect final
state. An HTTP success alone does not mean every required version is complete.
Builds may fill an empty `staged` slot, but an existing candidate is not silently
replaced. Status shows artifact states and slot generations; inspect them before
choosing the candidate to serve.

For large collections use durable jobs instead of holding a synchronous request
open. `MUNARIUM_DATASTORE_BUILDER=enabled` must be set on a PostgreSQL-connected
worker with access to staging and the same L2 storage. Enqueue, retain the job ID,
poll its state and inspect its result. In particular, a backfill job can execute
successfully while its result reports `complete: false`.

| Operation | API and input | CLI equivalent |
|---|---|---|
| Backfill required versions | `POST /v1/index-build-jobs`, `kind: backfill`, `collection_id` | `mmctl datastore jobs enqueue backfill <collection-id>` |
| Rebuild a logical version | Same route, `kind: rebuild`, `index_version_id` | `mmctl datastore jobs enqueue rebuild <index-version-id>` |
| Direct collection build | Same route, `kind: direct`, `collection_id`, optional `max_chars`, `watermark_seq` | `mmctl datastore jobs enqueue direct <collection-id> --max-chars N --watermark N` |
| Inspect/list jobs | `GET /v1/index-build-jobs/{job_id}` or `GET /v1/index-build-jobs` | `mmctl datastore jobs get <job-id>` or `list` |
| Cancel | `POST /v1/index-build-jobs/{job_id}/cancel` | `mmctl datastore jobs cancel <job-id>` |

All enqueue requests also accept an optional `correlation_id`. Job states are
`pending`, `running`, `succeeded`, `failed`, `cancelled` and `superseded`. Inspect
`attempts`, `claimed_by`, `result` and `error`. Lapsed leases are retried subject
to the implementation's attempt ceiling; cancellation is not an artifact
deletion or a serving rollback.

For scripted CLI polling, `datastore jobs get` returns exit 0 for `succeeded`,
4 for `failed`/`cancelled`, and 3 otherwise, including `superseded`. Read the
state to avoid polling a superseded job forever. Synchronous `verify` and
`backfill` use exit 3 for failed verification or incomplete coverage.

Direct builds create a candidate logical version and artifact; do not assume a
completed job activated it. The current direct-job defaults are `max_chars=400`
and `watermark_seq=0`. Supply values appropriate to the intended build and inspect
the returned `index_version_id`, `committed` and `expected_active`. Verify shape
and retrieval behavior before production use; direct building is not a substitute
for the runbook's approval and acceptance process.

## 6. Prewarm, promote and select Datastore serving

Change the Server's `MUNARIUM_RETRIEVAL_MODE` to `datastore` in Compose and recreate
the Server. Keep all other Datastore settings and volumes. With no scope selected
for Datastore yet, existing traffic continues to use PostgreSQL. The Datastore
warmer runs in this mode and can prewarm staged candidates while that remains true.

Read and change selectors with their current generation. A missing row is a first
creation; an existing row requires compare-and-swap. This helper accepts only a
404 as a missing selector, preserving other errors:

```powershell
function Set-CollectionEngine([string]$Engine, [bool]$Prewarm) {
    $current = $null
    try { $current = Get-DatastoreJson "/v1/retrieval-rollout/collection/$collectionId" }
    catch { if ([int]$_.Exception.Response.StatusCode -ne 404) { throw } }
    $request = @{
        scope_kind='collection'; scope_id=$collectionId; serving=$Engine
        prewarm_staged=$Prewarm; reason='datastore guide validation'
    }
    if ($null -ne $current -and $current.generation -gt 0) {
        $request.expected_generation = $current.generation
    }
    Send-DatastoreJson Put '/v1/retrieval-rollout' $request
}
Set-CollectionEngine postgres $true
```

Allow at least one warmer sweep and inspect `/admin/storage` for readiness and
fleet observations. `prewarm_staged` is a background request, not proof that files
are already open. If status has no staged candidate, bind the reviewed verified
artifact with `POST /v1/index-artifacts/{index_version_id}/bind`, supplying
`slot: staged`, `artifact_id` and, when replacing a slot, its `expected_generation`.
Use `shadow` instead of `staged` for a shadow candidate. Never directly bind the
`serving` slot.

Promote each inspected staged candidate using the generations just read:

```powershell
foreach ($indexId in $requiredVersions) {
    $status = Get-DatastoreJson "/v1/index-artifacts/$indexId"
    $staged = @($status.bindings | Where-Object slot -eq 'staged')
    if ($staged.Count -ne 1) { throw "Inspect and bind a staged candidate for $indexId" }
    $serving = @($status.bindings | Where-Object slot -eq 'serving')
    $servingGeneration = if ($serving.Count) { $serving[0].generation } else { 0 }
    Send-DatastoreJson Post "/v1/index-artifacts/$indexId/promote" @{
        expected_staged_generation=$staged[0].generation
        expected_serving_generation=$servingGeneration
        reason='verified local datastore candidate'
    }
}
Set-CollectionEngine datastore $false
```

A stale generation is a reason to re-read and review what changed, not to blindly
retry with whatever number now succeeds. The rollout to Datastore checks that
every serving-required logical version has a verified serving binding. PostgreSQL
rollback does not require that completeness check.

The selector API accepts `scope_kind`, `scope_id`, `serving`, `prewarm_staged`,
`expected_generation`, `reason` and `required_versions_policy`. Scope kinds are
`collection` and legacy `shape`. The only currently settable required-version
policy is `active_pinned_and_horizon`; omit it to use that default. Do not weaken
the policy to bypass a missing historical artifact. Repeat the procedure for
each corpus scope; there is no single whole-corpus selector.

Promotion and logical activation differ. This walkthrough keeps the previously
active logical index, so no activation call is needed. For a **new** logical
version in a Datastore-routed collection, build, verify and promote it first,
then `POST /v1/collections/{id}/activate-index` with `index_version_id` and
`expected_active`. The latter is the active logical version you read before the
change, or null for no active version. Check `activated: true`; a mismatch can
return HTTP success with `activated: false` and leave the pointer unchanged.

## 7. Prove serving, recreation and rollback

Wait for readiness after rollout, then repeat a known query through the actual
application endpoint. For the starter corpus:

```powershell
$session = Invoke-RestMethod "$base/v1/runbooks/starter@1/sessions" -Method Post -Headers $headers
$query = @{query='Who handles invoice questions?'; complete=$false}
$answer = Send-DatastoreJson Post "/v1/sessions/$($session.session_id)/turns" $query
if (-not @($answer.hits | Where-Object {
    $_.source_path -eq 'starter/billing.txt' -and $_.text -match 'billing team'
}).Count) { throw 'Expected evidence missing' }
$answer.envelopes | ConvertTo-Json -Depth 12
```

Inspect the retrieval envelopes as well as the hits. The envelope reports the
logical `index_version`; it does not itself identify the physical artifact or
engine. Verify that index's serving binding, the collection selector and the
effective mode of the replica handling the request together. A selector row
alone is not proof that a process is running the correct mode. Compare evidence coverage, ranking,
citations and latency against the PostgreSQL baseline on your real corpus.
This lexical starter query needs no AI provider key or paid model request.

Recreate only the Server, retain both volumes, wait for readiness and repeat the
query. Also read the ledger fact created in getting started. Verify every serving
replica after a multi-replica restart; a load-balanced request samples only one.
Exercise existing sessions as well as fresh sessions to test retained version pins;
use each existing session's original tenant and `X-Munarium-Uid` owner.

Rollback the collection using the same authenticated management path:

```powershell
Set-CollectionEngine postgres $false
```

Verify the selector, readiness and the query again. Keep PostgreSQL indexes and
source data available so this recovery path remains usable. A broken Datastore
replica may have left public ingress; retain an authorized internal route to
the API for rollback. If necessary, change the process mode to `postgres` and
recreate it to regain service, then update the scope selectors before attempting
Datastore again. Authentication and generation checks still apply to rollback.

Do not delete a volume or edit immutable artifacts to fix an integrity failure.
Rebuild a candidate from authoritative data, verify and promote it, or keep that
scope on PostgreSQL while diagnosing the cause.

## 8. Fleet gates, operations and capacity

All serving replicas need access to the same durable catalog and L2 objects.
Give each replica its own L1/L0 caches and unique instance identity. Cloud L2
supports separate replicas more naturally than a Docker named volume confined
to one host. Give builders sufficient staging disk and permission to publish;
readers need permission to hydrate and open artifacts.

Promotion consults the environment's `retrieval_plane_expectations`: plane,
deployment revision, required mode, minimum fresh ready nodes and minimum nodes
with the staged artifact open. The gate uses node and residency snapshots;
nodes older than 120 seconds are stale in this implementation. Set accurate
`MUNARIUM_DEPLOYMENT_ENVIRONMENT_ID`, `MUNARIUM_DEPLOYMENT_PLANE` and
`MUNARIUM_DEPLOYMENT_REVISION` on the serving fleet.

No expectation rows means promotion has no declared fleet gate and proceeds on
catalog checks alone. That is the local single-replica walkthrough's posture,
not proof of a production fleet rollout. Environment expectations are deployment
automation/DBA-managed state; this Server exposes no public expectation-write
API or `mmctl` command. Coordinate those records with deployment automation and
the [store schema/implementation](../../src/munarium-store-pg/src/rollout.rs).
Do not remove expectations merely to make a blocked promotion succeed.

Observe `/admin/storage` for effective mode, capabilities, fleet discrepancies,
cache and shadow information. Use `/readyz` for admission and the internal ops
listener's `/metrics` for build/query monitoring. Rehearse disk pressure, missing
L2 access, corrupt artifact refusal, cold restart, stale fleet and rollback in an
isolated deployment. Avoid destructive fault injection against the only copy of
an artifact or a live production volume.

Size disk for durable artifacts, hydrated active/historical versions and peak
concurrent build workspace. L1 watermarks do not cap L2 or staging growth. Size
open-shard capacity from the working set and memory/file-handle measurements;
increasing it blindly can trade disk thrashing for memory pressure. Changing the
approximate-vector threshold only affects eligible future direct builds. Validate
recall and ranking on held-out queries before promoting their physical artifacts.

Back up PostgreSQL and durable source/artifact storage with a coordinated recovery
plan. A database restore can reference unavailable L2 objects; resolve those by
rebuilding or rolling scopes back, then verify serving-required completeness and
readiness. L1 and L0 are caches, not substitutes for backups. Follow the
[backup/restore runbook](../ops/backup-restore.md) for recovery sequencing.

## 9. Troubleshooting

| Symptom | What to check |
|---|---|
| Configured mirror/shadow, effective PostgreSQL | Artifact-store setting, catalog, writable local root and compiled engines; inspect degradation reason |
| Artifact backend reports `pg` unsupported | Use `file`, `az`, `s3` or `gcs`; source-store `pg` is a different setting |
| Cloud artifact creation fails with PostgreSQL sources | Cloud artifact client comes from cloud source configuration; use local file artifacts or configure matching cloud source/artifact stores |
| Permission denied on local files | UID 65532 ownership/access for the cache, staging and artifact directories |
| Job stays pending | Builder enabled on a worker with database and storage access; inspect worker logs and lease state |
| Backfill responds successfully but is incomplete | Check every version outcome; retry deferred work and fix actual failed builds |
| Promotion rejected | Staged/serving generations, verified candidate, fleet revision/mode/readiness and staged-open residency |
| Rollout rejected | Every active/within-horizon version needs a verified serving binding; an empty scope cannot be Datastore-served |
| New logical index fails activation | Promote its artifact first; check `expected_active` and the returned `activated` flag |
| `/readyz` fails after rollout/restart | Required artifact hydration, verification, compatible engine and disk/L2 availability; inspect each replica |
| Startup rejects retention | Retention must cover the configured horizon; increasing the horizon leaves the derived retention default unchanged |
| Shadow produces no evidence | Nonzero sample rate, shadow mode, available candidate bindings and counters for skipped/dropped/timed-out comparisons |
| Query succeeds but has wrong ranking | Check actual engine/envelope, candidate bindings and fusion/recall against baseline; rollback until explained |

Keep an acceptance record with image digest, logical versions, artifact IDs,
generations, rollout rows, fleet coverage, quality measurements and rollback
results. Store no passwords or provider keys in that record.

The local walkthrough was exercised against the published `1.0.0` image with the
two-document starter corpus: all seven PowerShell blocks, file-volume permissions,
mirror backfill/verification, prewarming, promotion, Datastore retrieval, Server
recreation, old and new sessions, retained ledger data, PostgreSQL rollback and
a queued backfill with `complete: true` passed. No model calls were made. This
does not validate cloud L2 permissions, approximate-vector recall, a fleet with
declared promotion expectations, or a direct-build rollout; test those separately
for the deployment that uses them.

## 10. Related Datastore documentation

These are the existing Datastore references and the adjacent deployment guides:

| Document | Datastore coverage |
|---|---|
| [Root README: derived indexes](../../../README.md#other-document-stores-and-derived-indexes) | Relationship to ledger/document storage and quick deployment overview |
| [Server environment reference](../../README.md#configuration-env-vars) | Full Server configuration, including Datastore settings |
| [Architecture §5.2](../architecture.md#52-the-datastore-tier-immutable-search-artifacts) | Engine boundary, immutable artifacts and no-fallback serving |
| [Developer guide §8A](dev-guide.md#8a-the-datastore-plane-derived-indexes-beside-postgresql) | Datastore architecture, lifecycle, API behavior, readiness and development evidence |
| [Developer guide §4](dev-guide.md#4-workspace-tour-the-crates-and-their-boundaries) | Crate map and retrieval coordinator; see the Datastore discussion in that chapter |
| [Developer guide §10](dev-guide.md#10-ci-and-the-path-to-production) | CI boundaries, Docker mounts and deployment considerations |
| [Operator CLI](../ops/mmctl.md) | Exact `mmctl datastore` commands, arguments and generation handling |
| [REST reference](../api/rest.md) | Datastore-plane route table, auth roles, DTO fields and REST-only transport |
| [OpenAPI reference](../api/openapi.json) | Generated request/response schemas for artifacts, jobs, selectors and activation |
| [Error reference](../api/errors.md) | `datastore-unavailable` and other request/refusal contracts |
| [Deployment runbook](../ops/deployment-runbook.md) | Image rollout and Datastore-to-PostgreSQL rollback |
| [Backup/restore runbook](../ops/backup-restore.md) | Recovering database state and independently stored artifacts |
| [Artifact contract](../../contract/datastore/README.md) | Canonicalization, manifests, identity vectors and compatibility rules |
| [Getting started](getting-started.md) | The PostgreSQL corpus baseline used by this walkthrough |
| [Source stores](source-stores.md) | Source/cloud configuration and artifact-client credential prerequisites |
| [Managing keys and secrets](managing-key-and-secrets.md) | Database and cloud credential injection/rotation |
| [Creating a lab](creating-a-lab.md) | Corpus-specific evaluation and acceptance for candidate retrieval behavior |

Implementation details can be checked against
[capability resolution](../../src/munarium-retrieval/src/capabilities.rs),
[configuration validation](../../src/munarium-retrieval/src/config.rs),
[artifact builds and promotion](../../src/munarium-server/src/datastore_builds.rs),
[serving and rollout](../../src/munarium-server/src/datastore_serving.rs) and
[durable jobs](../../src/munarium-server/src/datastore_jobs.rs).
