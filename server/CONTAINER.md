# Munarium Server container

`docker.io/iokaio/munarium` packages the Apache-2.0 Munarium Server and its
`/mmctl` command-line client. Source: [iokaio/munarium](https://github.com/iokaio/munarium).
The image runs as a nonroot user. Server platforms are `linux/amd64` and
`linux/arm64`; Windows hosts use Docker Desktop in Linux container mode.

## Quick start

For an isolated, temporary evaluation from PowerShell:

```powershell
docker run --rm --name munarium-evaluation `
  -p 127.0.0.1:8080:8080 -p 127.0.0.1:50051:50051 `
  -e MUNARIUM_STORE=memory -e MUNARIUM_AUTH_MODE=static `
  -e MUNARIUM_STATIC_TOKENS=evaluation-token:evaluation:rw `
  iokaio/munarium:1.0.0
```

Open `http://127.0.0.1:8080/admin` or `/docs`. Check `/healthz`, `/readyz`,
and `/version`. Authenticated `/v1` requests need both
`Authorization: Bearer evaluation-token` and `X-Munarium-Uid: evaluator`.
The example credential is public and intended only for loopback evaluation.
Memory storage disappears when the container stops.

## Persistent deployment

Use PostgreSQL 16 with the pgvector extension. Configure secrets through your
deployment platform, and retain database storage independently of the Server:

| Setting | Purpose |
|---|---|
| `MUNARIUM_STORE=postgres` | Persistent ledger and configuration |
| `MUNARIUM_DATABASE_URL` | PostgreSQL connection URI, with TLS where appropriate |
| `MUNARIUM_SOURCE_STORE=pg` | Store source document bytes in PostgreSQL |
| `MUNARIUM_AUTH_MODE=static` | Require explicitly configured bearer credentials |
| `MUNARIUM_STATIC_TOKENS` | Comma-separated `token:tenant:role` registrations |
| `MUNARIUM_TOKEN_SECRET` | Private signing secret for capability tokens |

An example production container configuration is:

```yaml
services:
  server:
    image: docker.io/iokaio/munarium@sha256:<verified-release-digest>
    restart: unless-stopped
    ports:
      - "127.0.0.1:8080:8080"
      - "127.0.0.1:50051:50051"
    environment:
      MUNARIUM_STORE: postgres
      MUNARIUM_SOURCE_STORE: pg
      MUNARIUM_DATABASE_URL: ${MUNARIUM_DATABASE_URL:?Set a database connection URI}
      MUNARIUM_AUTH_MODE: static
      MUNARIUM_STATIC_TOKENS: ${MUNARIUM_STATIC_TOKENS:?Set private credentials}
      MUNARIUM_TOKEN_SECRET: ${MUNARIUM_TOKEN_SECRET:?Set a private signing secret}
```

Provide a reachable PostgreSQL service, backups, and TLS ingress for remote
clients. Do not publish the operations listener to the Internet. With the `pg`
source store, persistence lives in PostgreSQL; the Server requires no data volume.
Other source stores and datastore modes have their own storage requirements.
See [Server configuration](https://github.com/iokaio/munarium/tree/main/server/docs).

| Container port | Interface |
|---|---|
| 8080 | REST, documentation, administration |
| 50051 | gRPC, requiring HTTP/2 through proxies |
| 9090 | Operations and metrics |

Run the bundled client with `docker exec <container> /mmctl --help`.

## Versions and verification

`1.0.0` identifies one release. `1.0` and `latest` may advance; use a verified
digest for deployments. Candidate tags such as `1.0.0-rc.1` are evaluation
builds and are not stable releases. Published source revisions are recorded in
OCI labels, alongside SBOM and build provenance attestations. Release notes
provide the certified digest and signing identity for verification.

The image includes `LICENSE`, `NOTICE`, and dependency notices under
`/usr/share/licenses/munarium/`. Third-party components retain their own
licenses. [License](https://github.com/iokaio/munarium/blob/main/LICENSE) and
[support policy](https://github.com/iokaio/munarium/blob/main/SUPPORT.md).

## Building from source

Build from a clean, recorded source commit. The pinned compiler cross-compiles
both binaries and verifies their architecture and static linkage. A Buildx
builder using the `docker-container` driver supports the OCI export and attestations:

```powershell
docker buildx create --name munarium-builder --driver docker-container
$revision = git rev-parse HEAD
docker buildx build --builder munarium-builder --platform linux/amd64,linux/arm64 `
  --build-arg SOURCE_REVISION=$revision --sbom=true --provenance=mode=max `
  --output type=oci,dest=munarium.oci.tar ./server
```

Building ARM64 does not prove it runs: execute and test each platform before
publishing a multi-platform image.
