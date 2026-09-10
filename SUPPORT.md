# Support

Munarium is open source under the Apache License 2.0 ([LICENSE](LICENSE)). **The license includes
no support from Ioka LLC**, and nothing in this repository is a support commitment.

## What is available to everyone

- **Questions** — GitHub Discussions on this repository.
- **Defects** — GitHub Issues. Say which component (server, matrix, or a client), its version, and
  the smallest reproduction you can manage. The conformance suites are the fastest way to show a
  behavior that differs from the documented one, and Matrix's refusal registry says what each
  refusal code means.
- **Vulnerabilities** — the private channel in [SECURITY.md](SECURITY.md), never an issue.
- **Compatibility** — [clients/compatibility.json](clients/compatibility.json) records which
  server and Matrix versions each client release supports.

Issues are read and triaged by a small team. There is no response-time commitment here, and a
defect may be closed as "recorded, not scheduled" — a truthful answer rather than a dismissal.

## What is not

A production support relationship. If you need one — a response target, a supported-version window,
long-term support branches, certified deployment architectures, upgrade tooling,
or the evidence adapters for Databricks, BigQuery, Snowflake, Cube and dbt —
that is **Munarium Enterprise**, a separate proprietary distribution sold by subscription. It is not
open source and this license grants no right to it.

Commercial enquiries go to **info@ioka.io**.

## Running it yourself

Everything needed to operate Munarium without Ioka is in this repository: the deployment runbooks,
the clustering guide, backup and restore, troubleshooting, the adapter support matrix, and the
conformance suites that tell you whether your deployment behaves. That is deliberate. The open
edition is a complete product, not a trial.

The open edition can be run from the public Server image or built from source.
Its source-document backends include PostgreSQL, Azure Blob, S3, GCS and local
files; these are part of the open-source implementation.

Release publishing is operated outside this repository. A local source build
does not reproduce Ioka's signing identity. Published images can be checked
against the digest and signing instructions for that specific release where
those instructions are available. See the [publication record](server/CONTAINER.md#versions-and-verification)
for current artifacts and gaps, and each component's release notes for limitations.
