# Source stores: where document bytes live, and how to run each backend

Every ingested document has exactly one home for its raw bytes, selected by
`MUNARIUM_SOURCE_STORE`. This guide is the operational companion to
[loading-corpora.md](loading-corpora.md) (which explains what a "source" *is*
and the filename-is-identity rule): how each backend authenticates, how to
stand one up locally, and what fails closed when you misconfigure it.

## Why an object store, and what changed

Postgres `BYTEA` was never going to carry the 580 MB insurance corpus or the
514 MB LOC harvest — documents belong in object storage, with Postgres holding
the metadata row. The seam is the `SourceStore` trait in `munarium-core`; behind
it, one adapter (`munarium-store-objects`, built on Apache Arrow's
[`object_store`](https://docs.rs/object_store) crate, v0.14) serves Azure
Blob, S3, GCS, and the local filesystem. It **replaced** the hand-rolled
`munarium-store-az` crate — with a test pinning that Azure recorded URIs stayed
byte-identical across the swap, so no existing row changed meaning. The
in-memory backend stays `munarium-store-mem`; the Postgres fallback lives in the
pg retrieval crate (`source_blobs` table).

Whatever the backend, every source row records `storage_backend` and a
**credential-free** `blob_uri` (also test-enforced — a SAS token can never
leak into a recorded URI), so `GET /v1/sources/{id}` always tells you where
the bytes went without telling anyone how to get in.

## The backend matrix

| Backend | When | Env vars | Credential |
|---|---|---|---|
| `az` | deployed default (pg store) | `MUNARIUM_AZURE_STORAGE_ACCOUNT` (**required**, fails closed), `MUNARIUM_AZURE_BLOB_CONTAINER` (default `sources`), `MUNARIUM_BLOB_AUTH` (`managed_identity` default \| `sas`), `MUNARIUM_AZURE_CLIENT_ID` (user-assigned identity; unset = system-assigned), `MUNARIUM_BLOB_SAS_REF` (required under `sas`), `MUNARIUM_AZURE_BLOB_ENDPOINT` (Azurite/sovereign override) | managed identity — no secret exists |
| `s3` | AWS, or any S3-compatible (MinIO, R2) | `MUNARIUM_S3_BUCKET` (**required**, fails closed), `MUNARIUM_S3_REGION`, `MUNARIUM_S3_ENDPOINT`, `MUNARIUM_S3_FORCE_PATH_STYLE` (default `true` iff endpoint set), `MUNARIUM_S3_ACCESS_KEY_ID` + `MUNARIUM_S3_SECRET_KEY_REF` (static pair, both-or-neither) | ambient AWS chain, or the static pair |
| `gcs` | Google Cloud Storage | `MUNARIUM_GCS_BUCKET` (**required**, fails closed), `MUNARIUM_GCS_CREDENTIALS_REF` (env-var name or `file:/path` yielding key JSON) | `GOOGLE_APPLICATION_CREDENTIALS` / metadata server, or the ref |
| `file` | single-node, air-gapped, tests | `MUNARIUM_FILE_ROOT` (**required** — no silent temp-dir default; created if absent) | filesystem permissions |
| `pg` | offline/CI fallback (the dev compose profile) | none beyond `MUNARIUM_DATABASE_URL` | the database credential |
| `mem` | in-process tests (memory store default) | none | none |

The env-var semantics above are copied from the
[README env table](../../README.md#configuration-env-vars) — that table is the
contract; this one is the tour.

## Local dev with MinIO, end to end

The compose file ships an S3-compatible target behind a profile, so the normal
dev loop and CI never start it:

```powershell
cd server
docker compose --profile s3 up -d minio

# Create the bucket once (mc ships inside the MinIO image):
docker compose exec minio mc alias set local http://127.0.0.1:9000 minioadmin minioadmin
docker compose exec minio mc mb --ignore-existing local/sources
```

Run the server against it — static credentials, with the secret as a **ref**
(an env-var *name*), never inline:

```powershell
$env:MUNARIUM_SOURCE_STORE = 's3'
$env:MUNARIUM_S3_BUCKET = 'sources'
$env:MUNARIUM_S3_ENDPOINT = 'http://127.0.0.1:9000'   # http allowed for loopback tooling
$env:MUNARIUM_S3_ACCESS_KEY_ID = 'minioadmin'
$env:MINIO_SECRET = 'minioadmin'
$env:MUNARIUM_S3_SECRET_KEY_REF = 'MINIO_SECRET'      # the NAME of the var, not the value
cargo run -p munarium-server
```

Path-style addressing defaults on when an endpoint is set (MinIO needs it;
AWS prefers virtual-hosted), so there is nothing else to flip.

The gated integration test proves the round trip — put/exists/get/delete plus
the credential-free-URI assertion — and skips vacuously when the endpoint var
is unset, the same contract as the pg-gated tests:

```powershell
$env:MUNARIUM_TEST_S3_ENDPOINT = 'http://127.0.0.1:9000'
cargo test -p munarium-store-objects --test s3_integration
```

## S3, GCS, and file in production

**The posture is ambient-first.** On AWS, leave the static pair unset and the
adapter uses the standard chain — env vars, web identity/IRSA on EKS, the
instance profile on EC2. On GCP, `GOOGLE_APPLICATION_CREDENTIALS` or the
metadata server. The static escape hatches exist for off-cloud tooling
(`MUNARIUM_S3_ACCESS_KEY_ID` + `MUNARIUM_S3_SECRET_KEY_REF`;
`MUNARIUM_GCS_CREDENTIALS_REF`) and every secret goes through the same ref seam
the BYOK provider keys use: an env-var name, or `file:/path` for a mounted
secret — never the material in configuration.

**Everything fails closed.** No backend has a guessable default: `s3` without
a bucket, `gcs` without a bucket, `file` without a root, and `az` without an
account all refuse to start rather than invent a location. Half a static S3
credential — a key id without `MUNARIUM_S3_SECRET_KEY_REF`, or the reverse — is
refused too, because a half-configured credential is a misconfiguration, not
a fallback to ambient.

`file` is honest single-node storage: bytes land under
`MUNARIUM_FILE_ROOT/{tenant}/{logical path}`. It is the right answer for
air-gapped evaluation and the wrong answer the moment a second replica exists.

## Azure specifics

- **Managed identity is the default and there is no storage secret.** The
  example AKS module creates its storage account with
  `shared_access_key_enabled = false`, so a key cannot leak because none
  exists; do the same. The adapter builds its Azure client with
  `from_env()` deliberately: on Container Apps / App Service the credential
  endpoint is `IDENTITY_ENDPOINT` + `IDENTITY_HEADER` (read per request —
  the header rotates), which `from_env()` picks up and a plain builder does
  not; VMs and AKS nodes without those variables fall back to classic IMDS.
- **SAS-by-ref** (`MUNARIUM_BLOB_AUTH=sas` + `MUNARIUM_BLOB_SAS_REF`) exists for
  off-Azure tooling that must reach blob; point it at a separate, CI-facing
  account rather than the production one. The SAS never appears in recorded
  URIs.
- **Azurite** and sovereign clouds: `MUNARIUM_AZURE_BLOB_ENDPOINT` overrides the
  endpoint; everything else is unchanged.
- **Operators checking blobs need data-plane RBAC of their own.** Verifying
  that a document landed at its logical path
  (`az storage blob exists --auth-mode login`) runs as *the operator*, and
  control-plane Owner/Contributor does not grant blob reads. Storage Blob
  Data **Reader** on the source account is the minimum.

## Deleting data

There is no delete API, deliberately. Physical removal of index data is the
DBA runbook, and its ["And the blobs"
section](../ops/index-deletion-runbook.md#6-and-the-blobs) covers the part
this guide's backends make real: dropping partitions and `sources` rows never
touches the document bytes, and each backend has its own sanctioned cleanup
path when a change ticket genuinely requires document destruction.

## See also

- [loading-corpora.md](loading-corpora.md) — what a source is, the
  filename-is-identity rule, and the per-corpus loading tiers.
- [README env table](../../README.md#configuration-env-vars) — the normative
  variable-by-variable contract.
- [security-posture.md](../security-posture.md) — the credential posture in
  the context of the whole trust model.
