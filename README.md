# Munarium

**Governed memory for production AI systems**, and the structured-evidence plane that backs it with
records rather than recollection.

Three components, one repository, all under the Apache License 2.0:

| | What it is | Start here |
|---|---|---|
| **[server/](server/)** | The governed-memory service: an append-only fact ledger with governance in the write path, hybrid retrieval carrying a provenance envelope on every answer, declarative runbooks, and bring-your-own-key model providers. REST and gRPC, both speaking the Munarium Memory Protocol. | [server/README.md](server/README.md) |
| **[matrix/](matrix/)** | Munarium Matrix core: registers formal data sources, materializes governed record collections, executes verified query contracts, and seals the exact typed evidence an answer used. | [matrix/README.md](matrix/README.md) |
| **[clients/](clients/)** | The official client libraries — Rust, Python, .NET and Java for the Server, and .NET, Java and Python for Matrix — proven against the servers by the same conformance scenarios. | [clients/README.md](clients/README.md) |

## Guides

- **[Getting started](server/docs/guides/getting-started.md)** — deploy the published
  image, write through the API, and build your first searchable corpus with a shape and runbook.
- **[Creating a lab](server/docs/guides/creating-a-lab.md)** — design experiments, build
  answer keys, and improve shapes and runbooks for your corpora and application use cases.
  This tutorial focuses on approach and technique without code examples.
- **[Managing keys and secrets](server/docs/guides/managing-key-and-secrets.md)** — configure
  AI providers, Docker secrets and PostgreSQL access; verify, rotate and revoke credentials.

## Run with Docker

The public [iokaio/munarium image on Docker Hub](https://hub.docker.com/r/iokaio/munarium)
contains **Munarium Server and the `/mmctl` client**. It supports `linux/amd64` and
`linux/arm64`; Docker selects the platform automatically. On Windows, use Docker Desktop
in Linux container mode. Matrix is deployed separately; see [Matrix setup](matrix/README.md).

```console
docker pull iokaio/munarium:1.0.0
```

`1.0.0` is the immutable release tag. `1.0` tracks the current 1.0 release, and `latest`
tracks the current stable release. To pin the exact 1.0.0 build, use:

```text
docker.io/iokaio/munarium@sha256:9f5cd5dec2f52cef26aabce625ace1390164e4930c93b5cc0d2177806b498d4c
```

The image includes license notices, SBOM and build provenance attestations, and a signature.
The [1.0.0 release notes](https://github.com/iokaio/munarium/releases/tag/v1.0.0) include
signature verification instructions. There is no trial key or time limit.

### Quick evaluation in memory

Run this single-line command in PowerShell or a Unix shell:

```console
docker run --rm --name munarium-evaluation -p 127.0.0.1:8080:8080 -p 127.0.0.1:50051:50051 -e MUNARIUM_STORE=memory -e MUNARIUM_SOURCE_STORE=mem -e MUNARIUM_AUTH_MODE=static -e MUNARIUM_STATIC_TOKENS=evaluation-token:evaluation:rw iokaio/munarium:1.0.0
```

Open `http://localhost:8080/admin` for the dashboard or `http://localhost:8080/docs` for
the API documentation. `/healthz` checks liveness; `/readyz` checks readiness. REST uses
port **8080**, gRPC uses **50051**, and the internal operations/metrics listener uses
**9090**. The examples publish only the first two, on loopback.

Authenticated `/v1` requests require `Authorization: Bearer evaluation-token` and
`X-Munarium-Uid: evaluator`. The example token is for local evaluation only. Stopping
this container loses its in-memory data; persistent storage and the full retrieval/runbook
workflow use PostgreSQL.

### Persistent storage with PostgreSQL

Munarium separates the ledger from raw document storage:

| Setting | What it controls |
|---|---|
| `MUNARIUM_STORE=postgres` | Persists the ledger, configuration and retrieval metadata in PostgreSQL. `memory` is temporary, per-process storage. |
| `MUNARIUM_DATABASE_URL` | Connection URI for an existing PostgreSQL database, such as `postgres://user:password@host:5432/munarium`. Required with `postgres`. |
| `MUNARIUM_SOURCE_STORE=pg` | Stores ingested document bytes in that same database. Set this explicitly for an all-PostgreSQL deployment; the default with the PostgreSQL ledger is Azure Blob (`az`). |
| `MUNARIUM_RETRIEVAL_MODE=postgres` | Uses PostgreSQL retrieval, the default. The optional derived-index datastore has additional configuration described below. |

For a persistent local installation, create an empty directory with these two files.
No source checkout or compilation is required. In `.env`, fill in three different random
values. Use a long alphanumeric database password so it can be embedded in the connection
URI without percent-encoding; the capability signing secret must be at least 32 bytes.
Keep this file private and outside version control.

```dotenv
POSTGRES_PASSWORD=
MUNARIUM_API_TOKEN=
MUNARIUM_TOKEN_SECRET=
```

Save the following as `compose.yaml`. PostgreSQL 16 and pgvector are supplied by the
database image. The database is reachable only on the Compose network, and its data lives
in a named Docker volume.

```yaml
services:
  postgres:
    image: pgvector/pgvector:pg16
    restart: unless-stopped
    environment:
      POSTGRES_USER: munarium
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:?Set POSTGRES_PASSWORD in .env}
      POSTGRES_DB: munarium
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U munarium -d munarium"]
      interval: 3s
      timeout: 3s
      retries: 20

  server:
    image: iokaio/munarium:1.0.0
    restart: unless-stopped
    depends_on:
      postgres:
        condition: service_healthy
    environment:
      MUNARIUM_STORE: postgres
      MUNARIUM_DATABASE_URL: "postgres://munarium:${POSTGRES_PASSWORD:?Set POSTGRES_PASSWORD in .env}@postgres:5432/munarium"
      MUNARIUM_SOURCE_STORE: pg
      MUNARIUM_RETRIEVAL_MODE: postgres
      MUNARIUM_AUTH_MODE: static
      MUNARIUM_STATIC_TOKENS: "${MUNARIUM_API_TOKEN:?Set MUNARIUM_API_TOKEN in .env}:local:rw"
      MUNARIUM_TOKEN_SECRET: ${MUNARIUM_TOKEN_SECRET:?Set MUNARIUM_TOKEN_SECRET in .env}
    ports:
      - "127.0.0.1:8080:8080"
      - "127.0.0.1:50051:50051"

volumes:
  pgdata:
```

Start it from that directory:

```console
docker compose pull
docker compose up -d
docker compose ps
docker compose logs server
```

The Server applies its embedded database migrations at startup, including enabling the
`vector` extension. Wait for `http://localhost:8080/readyz` to return HTTP 200 before
sending requests. Use your `MUNARIUM_API_TOKEN` as the bearer token and provide an
`X-Munarium-Uid` identifying the caller. Static token registrations have the form
`token:tenant:role`; the example grants read/write access to tenant `local`. The signing
secret enables capability tokens; it is separate from the API bearer token.

`docker compose down` removes the containers while retaining `pgdata`; running
`docker compose up -d` again reuses the database. **`docker compose down -v` also deletes
the database volume.** Keep backups independently of that volume; see
[backup and restore](server/docs/ops/backup-restore.md). With these settings, the Server
needs no data volume of its own. Changing `POSTGRES_PASSWORD` in `.env` does not change
the password in an already initialized PostgreSQL volume; update the database role and
connection settings together.

### Connect to an existing PostgreSQL server

Use PostgreSQL 16 with pgvector installed and an existing database. Set
`MUNARIUM_DATABASE_URL` on the Server to its connection URI, keep
`MUNARIUM_STORE=postgres`, and select a document store explicitly. The connecting role
must be able to apply schema migrations; arrange for `CREATE EXTENSION vector` through
the database administrator if the application role cannot enable it.

In the Compose example, remove the `postgres` service, the Server's `depends_on` block,
and the `pgdata` volume declaration when using an external database. Replace the Server's
connection setting with:

```yaml
      MUNARIUM_DATABASE_URL: ${MUNARIUM_DATABASE_URL:?Set MUNARIUM_DATABASE_URL in .env}
```

Add the full URI to `.env`. A PostgreSQL service on the Windows host can be reached from
Docker Desktop using [`host.docker.internal`](https://docs.docker.com/desktop/features/networking/networking-how-tos/#connect-a-container-to-a-service-on-the-host), for example
`postgres://user:password@host.docker.internal:5432/munarium`. `localhost` inside the Server
container refers to that container. The database must accept connections from Docker's
network. Percent-encode reserved characters in URI usernames/passwords. For remote
databases, configure TLS according to the database provider, including certificate
validation (for example, `sslmode=verify-full` with the appropriate trusted CA).

For production, supply credentials through your deployment platform's secret facilities,
pin tested image digests, retain database backups, and terminate TLS at your ingress.
The loopback bindings above are intended for local use.

### Other document stores and derived indexes

Keep `MUNARIUM_STORE=postgres` for the persistent ledger while choosing where document
bytes live with `MUNARIUM_SOURCE_STORE`:

| Value | Storage and required configuration |
|---|---|
| `pg` | The same PostgreSQL database; useful for a self-contained deployment. |
| `file` | A persistent directory specified by `MUNARIUM_FILE_ROOT`, such as `/data/sources`. Mount storage there and grant the container's nonroot UID **65532** write access. Back up this directory as well as PostgreSQL. |
| `az` | Azure Blob Storage; requires `MUNARIUM_AZURE_STORAGE_ACCOUNT` and an identity or configured SAS credential. |
| `s3` | S3 or a compatible service; requires `MUNARIUM_S3_BUCKET`, region/endpoint settings and credentials. |
| `gcs` | Google Cloud Storage; requires `MUNARIUM_GCS_BUCKET` and credentials. |
| `mem` | Temporary document bytes, lost on restart; unsuitable for persistent documents. |

See the [source-store guide](server/docs/guides/source-stores.md) for authentication,
bucket/container settings and local filesystem configuration. Changing the storage
environment variable does not migrate existing documents between backends.

The optional **derived-index datastore** is separate from both the ledger and the document
store. Leave `MUNARIUM_RETRIEVAL_MODE=postgres` for the setup above. Other retrieval modes
require PostgreSQL plus a writable `MUNARIUM_DATASTORE_LOCAL_ROOT`, artifact storage and
index build/rollout configuration. Persist any local artifact storage and make mounted
directories writable by UID 65532. See the
[configuration reference](server/README.md#configuration-env-vars) and
[Server documentation](server/docs/README.md) before enabling those modes.

For more container details, see [server/CONTAINER.md](server/CONTAINER.md). To build from
source instead, run `cd server` followed by `docker compose up --build` using the repository's
development Compose file.

## The invariants

Enforced by the conformance suites, which are the executable specification and the record worth
reading:

- the ledger is **append-only with supersession** — a correction is a new row, never an update;
- governance is a property of the **command path**, so a blocked claim is recorded `disputed`
  rather than dropped;
- one `as_of_seq` pin bounds facts, anchors, promises, counters and entities **together**, and
  digests rebuild deterministically under a pin;
- every retrieval answer carries a **provenance envelope**;
- Matrix **refuses rather than assumes**: an adapter declares what it can do, and a combination it
  cannot serve is a typed refusal, never a best-effort answer.

## About this repository

Munarium begins here, at version 1.0.0. Its design was worked out over an extended period of
private research and development — experiments, measurements, superseded designs, and the
operational records of the environments they ran in — and that history is deliberately not carried
into this repository.

It is omitted because it documents how the design was reached rather than how the software behaves,
and it would give an evaluator, an operator or a contributor nothing they need. What that work
produced is here in full: the implementations, their conformance suites, their API documentation
and their deployment assets.

**Version 1.0 is a compatibility and support commitment, not a claim that every planned capability
is finished.** It commits to additive-only migrations, a stable wire contract under the N/N−1
policy, a stable `MUNARIUM_*` configuration contract, and Matrix's adapter interface as public API
under semantic versioning. What it does not yet cover is published in each component's release
notes, in the same voice its support matrix already uses:
[server/CHANGELOG.md](server/CHANGELOG.md), [matrix/CHANGELOG.md](matrix/CHANGELOG.md) and
[clients/CHANGELOG.md](clients/CHANGELOG.md).

## Licensing

Apache-2.0 throughout ([LICENSE](LICENSE), [NOTICE](NOTICE)). The names are not part of that grant:
[TRADEMARK.md](TRADEMARK.md) says what you may do without asking, which is most things.

**Munarium Enterprise** is a separate, proprietary distribution built from this software —
certified builds, supported deployment architectures, upgrade tooling, long-term support, and the
evidence adapters for Databricks, BigQuery, Snowflake, Cube and dbt. It is not open source and
nothing here grants any right to it. See [SUPPORT.md](SUPPORT.md).

## Contributing, support, security

Signed-off pull requests, no CLA ([CONTRIBUTING.md](CONTRIBUTING.md)). Questions go to Discussions,
defects to Issues, and suspected vulnerabilities to the private channel [SECURITY.md](SECURITY.md)
names — never a public issue. What is and is not supported: [SUPPORT.md](SUPPORT.md). Conduct:
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
