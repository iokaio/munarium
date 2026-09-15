# Munarium clients — release notes

## 1.1.1 — first registry releases, crate README

- First public releases on 2026-09-15: Server clients 1.1.0 and Matrix clients
  1.0.0 to PyPI, NuGet, Maven Central and crates.io, with the Server wire crates
  `munarium-proto` and `munarium-api-types` 1.2.1 on crates.io.
- Ship the Rust README inside the `munarium-client` crate (1.1.0 on crates.io
  renders none) and make its links absolute so they resolve on crates.io.
- Server client packages move to 1.1.1 together; no API change. Supported Server
  minors remain 1.2 and 1.1. Matrix clients remain 1.0.0.

## 1.1.0 source update — Server 1.2.1 alignment

- Target Server 1.2.1 in all four SDK constants and the compatibility record;
  package versions remain 1.1.0, with supported Server minors 1.2 and 1.1.
- Extend `ServerApiClient` to 121 named REST/gRPC operations, including
  collection queries, governance reads/replacement and publication authorization.
- Derive the generated API inventory version from OpenAPI and check SDK target
  constants against the compatibility record.
- Update Rust dependencies and notices for the patched rustls 0.23.45.

## 1.1.0 — Server 1.2 complete API

- Add `ServerApiClient` in Rust, Python, .NET and Java with a named method for
  every documented Server REST operation over REST and native gRPC, including
  collection vocabulary management, answer composition and streaming turns.
- Generate the method catalog and RPC contract from OpenAPI, with a CI drift
  check. Preserve null/presence, 64-bit integer values, typed errors and send-once
  behavior; route both transports through the same authorized Server handlers.
- Target Server 1.2.0 and retain the existing typed planes' Server 1.1 baseline.
  Matrix clients remain independent and unchanged. Package versions describe
  source; they do not assert registry publication.

See the [Server 1.2 guide](docs/guides/server-1.2.md).

## Unreleased — Server 1.1.1 alignment

- Target Server 1.1.1 in the compatibility record and retain the Server 1.0 minor baseline. Client package versions remain 1.0.0; MMP major remains 1. Matrix client compatibility is unchanged.
- Raise the Rust wire-crate dependency requirements to 1.1.1, matching the source checkout and lockfile.
- Document Ollama configuration, named health checks, optional local credentials, and the Server 1.1.1 rule that session model overrides apply to both query expansion and completion. Preserve the existing transport gaps and send-once behavior for provider calls and session turns.
- Add a disposable Server 1.1.1 qualification stack and an opt-in Python sync/async REST/gRPC regression suite for Ollama health, completion, embeddings, invocation provenance, caching, override routing, and rejection before provider calls. Check actual model and usage fields on streaming progress.
- Validate Server target/minor ranges separately from Matrix compatibility and test that stale or mixed service records fail the check.

See the [alignment guide](docs/guides/server-1.1.1.md) for the compatibility boundary and validation procedure.

## 1.0.0

The first public source release of all seven clients: Rust, Python, .NET and
Java for Munarium Server, and Python, .NET and Java for Munarium Matrix.
This version number does not imply registry publication; see
[installation and publication](README.md#installation-and-publication).

**What 1.0 commits to.** Each Clients minor release supports the current Server
(or Matrix) minor and the one before it. A breaking MMP wire change bumps
`mmp_contract_major`. Clients version independently of Server: a shared version
number on a given release is a coincidence of that release, not a rule. See
[compatibility.json](compatibility.json).

### Accepted limitations

- **Index builds are REST-only.** The gRPC clients return a typed
  `Unsupported` error rather than pretending otherwise.
- **The Matrix clients expose no gRPC transport.** Matrix's gRPC plane serves
  `MatrixQuery/Execute` alone, and that call is service-to-service: the server
  makes it while answering a turn, carrying an authorization snapshot an
  application does not hold. Generating stubs for it would put a large
  transitive dependency on every consumer's classpath to expose a call none of
  them may make.
- **Publishing the Rust client to crates.io has a prerequisite.**
  `munarium-client` path-depends on `munarium-api-types` and `munarium-proto`,
  which are server crates; both must reach crates.io first.
- **No dependency-vulnerability gate yet.** The clients workflow checks
  licenses, notices, formatting, types and tests, and builds and scans each
  artifact. It has no equivalent of `cargo deny` for the Python, .NET and Java
  dependency graphs.
- **The conformance suites need a running server.** They skip cleanly without
  `MUNARIUM_REST_URL` / `MUNARIUM_GRPC_URL` / `MUNARIUM_TOKEN`, and the
  platform smokes additionally need a `mgmt`-role token.
