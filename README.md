# Munarium

**Governed memory for production AI systems**, and the structured-evidence plane that backs it with
records rather than recollection.

Three components, one repository, all under the Apache License 2.0:

| | What it is | Start here |
|---|---|---|
| **[server/](server/)** | The governed-memory service: an append-only fact ledger with governance in the write path, hybrid retrieval carrying a provenance envelope on every answer, declarative runbooks, and bring-your-own-key model providers. REST and gRPC, both speaking the Munarium Memory Protocol. | [server/README.md](server/README.md) |
| **[matrix/](matrix/)** | Munarium Matrix core: registers formal data sources, materializes governed record collections, executes verified query contracts, and seals the exact typed evidence an answer used. | [matrix/README.md](matrix/README.md) |
| **[clients/](clients/)** | The official client libraries — Rust, Python, .NET and Java for the Server, and .NET, Java and Python for Matrix — proven against the servers by the same conformance scenarios. | [clients/README.md](clients/README.md) |

## Try it

```bash
cd server && docker compose up --build     # postgres + pgvector, munarium-server
curl http://localhost:8080/healthz
```

That is the whole evaluation. There is no trial key, no clock, and no sales call.

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
