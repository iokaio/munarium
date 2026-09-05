# Munarium clients — release notes

## 1.0.0

The first public release of all seven clients: Rust, Python, .NET and Java for
Munarium Server, and Python, .NET and Java for Munarium Matrix.

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
