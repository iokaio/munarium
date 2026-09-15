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
  iokaio/munarium:1.2.1
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

The bundled client lists its commands with `docker exec <container> /mmctl`
(usage exits with status 2). For an authenticated request, set `MUNARIUMCTL_URL`,
`MUNARIUMCTL_TOKEN`, and `MUNARIUMCTL_UID`, then run `/mmctl runbook list`.

## Versions and verification

`1.2.1` identifies one release. `1.2` and `latest` may advance; use a verified
digest for deployments. Candidate tags such as `1.2.1-rc.1` are evaluation
builds. Prior numeric release tags remain unchanged.

Published and verified on **2026-09-14**:

| Artifact | Identity |
|---|---|
| Server | `1.2.1`, `1.2`, `latest` on [Docker Hub](https://hub.docker.com/r/iokaio/munarium/tags) |
| OCI index | `sha256:8c937f91b5ab952fa080bdfbc748e041fffd5b69270f5ea4052b96afdebb2df7` |
| AMD64 manifest | `sha256:2515c419524974ca02db10a79631ac75bc451e3253047620b9cacbff69a51141` |
| ARM64 manifest | `sha256:b5f53e3d0f55598d7a99cbeaa0ce0e068bc40863bd2e50a0cd49ab641b71c760` |
| Image and acceptance-suite source | [`c638a8e56fff45cef358ff2f4a5b5ba57957ba59`](https://github.com/iokaio/munarium/commit/c638a8e56fff45cef358ff2f4a5b5ba57957ba59) |

Both architectures passed runtime, authentication, PostgreSQL write/read,
CLI and restart checks, plus real Ollama REST/gRPC completion, embeddings,
retrieval and persistence tests. These tests passed again on exact manifests
pulled from the public registry. AMD64 ran natively; ARM64 ran under emulation,
not on physical ARM64 hardware. Image audits, both-platform security scans and
all 11 main-branch source CI checks passed. The separate Matrix CI and client
checks also passed on this source. These are repeatable synthetic container
checks, not certification of every hosted model or customer installation.

A synthetic database rehearsal passed 1.2.0 → 1.2.1, then restored its pre-upgrade
backup and successfully restarted 1.2.0 with the original data/configuration.
**Rollback requires that database restore**; an older binary cannot open the
new migration 0034. See [upgrade and vocabulary controls](docs/guides/collection-vocabularies.md).
Automatic vocabulary generation is enabled by default and may use configured
paid providers for existing eligible collections after upgrade. Review generated
vocabularies for semantic accuracy and retain administrator editing controls.

Pin and verify the published image:

```console
docker pull iokaio/munarium@sha256:8c937f91b5ab952fa080bdfbc748e041fffd5b69270f5ea4052b96afdebb2df7
cosign verify --certificate-identity https://github.com/iokaio/munarium-int/.github/workflows/server-release.yml@refs/heads/main --certificate-oidc-issuer https://token.actions.githubusercontent.com docker.io/iokaio/munarium@sha256:8c937f91b5ab952fa080bdfbc748e041fffd5b69270f5ea4052b96afdebb2df7
```

The signature was independently verified, including its transparency-log claim,
and anonymous registry retrieval matched the exact certified index bytes.
The signing identity above is the identity in the public certificate; it does
not require access to the operator repository. SBOM and build provenance
attestations accompany the OCI index. Client libraries are published separately
on NuGet, Maven Central, PyPI and crates.io; see the
[client package links and installation guide](../clients/README.md#installation-and-publication).

The previous [1.2.0 release](https://github.com/iokaio/munarium/releases/tag/v1.2.0)
remains at index `sha256:b1ef684bdb4d432dcb3cf750d5cd51938821232f2850496557f3fa95bc213d51`.
Its historical qualification remains separate from this release.

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
  --build-arg SOURCE_REVISION=$revision --build-arg BUILD_VERSION=1.2.1 `
  --sbom=SELECT_CATALOGERS=+rust-cargo-lock-cataloger --provenance=mode=max `
  --output type=oci,dest=munarium.oci.tar ./server
```

Building ARM64 does not prove it runs: execute and test each platform before
publishing a multi-platform image.

The SBOM includes operating-system packages and the public source's locked Rust
dependencies. The source inventory includes build and development dependencies;
it is broader than the set linked into either executable.
