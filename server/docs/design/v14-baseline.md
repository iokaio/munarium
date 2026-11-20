# ClearView P0-T1: the frozen v1.4 baseline

This page completes [P0-T1](README.md#p0-t1). It records the released artifacts,
fixtures, expected results, accountable components and rollback boundaries that
every ClearView compatibility receipt compares against. It uses only tracked
repository records: the [container record](../../CONTAINER.md#versions-and-verification),
[`clients/compatibility.json`](../../../clients/compatibility.json), the
[client publication record](../../../clients/README.md#installation-and-publication),
the Server and client changelogs, the [1.4 release guide](../guides/server-1.4.md)
and the migration files. Responsible parties are roles; people are assigned in
the release-owned record. Review dates are placeholders for the release owner.

Prepared 20 November 2026 on `clearview/compat`, based on `feat/clearview` at
`4a67227`. Local receipts that use this baseline are in
[compat-local-receipts.md](compat-local-receipts.md).

## Released artifacts

| Artifact | Identity | Source of record |
|---|---|---|
| Server and `mmctl` source | `v1.4.0` at `25012c22087fbaa8ce29ddd68d2880d6e9499b1c` (9 October 2026) | [Container record](../../CONTAINER.md#last-published-image-140) |
| Server image, OCI index | `docker.io/iokaio/munarium@sha256:800a7b19cd3e2b82a3fb8f0b33e86e3f21fc3ea71ac12a81d6a9a18a816f556a`, tags `1.4.0` (immutable), `1.4`, `latest` | Container record |
| Server image, platform manifests | AMD64 `sha256:80891867b353c83025669d131b2ef7bce208a7e8f1d677f1ecb091b6e690b7c6`; ARM64 `sha256:778981d8f0894f0b7d522c65f902fa93c36dfc87f4994333305b27fd22bc6897` | Container record |
| Image signature | Keyless; certificate identity `https://github.com/iokaio/munarium-int/.github/workflows/server-release.yml@refs/heads/release/server-1.4.0`, issuer `https://token.actions.githubusercontent.com` | Container record (the `cosign verify` command) |
| CLI | `/mmctl` inside the Server image; no separate package | [mmctl guide](../ops/mmctl.md) |
| Rust client | `munarium-client` 1.4.0 on crates.io | `compatibility.json`, client publication record |
| Rust wire crates | `munarium-proto` 1.4.0 and `munarium-api-types` 1.4.0 on crates.io (tag `server-crates-v1.4.0`) | Client publication record |
| Python client | `munarium-client` 1.4.0 on PyPI | `compatibility.json` |
| .NET client | `Ioka.Munarium.Client` 1.4.0 on NuGet | `compatibility.json` |
| Java client | `io.ioka.munarium:munarium-client` 1.4.0 on Maven Central | `compatibility.json` |
| Client source | Tag `clients-v1.4.0`, the same commit as the Server | Client publication record |
| SDK compatibility | MMP contract major 1; every 1.4.0 client declares Server `1.4` and `1.3` (N/N-1) | `compatibility.json` |
| Helm chart at `v1.4.0` | `deploy/helm/munarium/Chart.yaml` version `0.3.0`, `appVersion: "1.3.0"` at the tag. The branch chart is `0.5.0`, `appVersion: "1.4.0"` | `git show v1.4.0:server/deploy/helm/munarium/Chart.yaml`; [chart README](../../deploy/helm/munarium/README.md) |
| Matrix | Released and maintained in `iokaio/munarium-matrix`; the Server consumer pin is a candidate (M1 step 3) | [Implementation ledger](implementation-ledger.md#phase-m-matrix-isolation) |

The image record states that the signature, certificate identity and transparency
log claim were verified at publication. A receipt that pulls the image records its
own verification separately; the local receipts did not run `cosign`.

## Fixtures and expected results

| Fixture | What it freezes | Expected v1.4 result | Checked by |
|---|---|---|---|
| [Phase 1 job and console fixtures](../../tests/fixtures/clearview-phase1/README.md) (`ff65ad5`) | Seven build-job rows with their exact v1.4 DTOs; twelve console escaping cases and five failed-read cases | Each row's `v14` DTO and list order; each `v14` escaping output | `datastore_jobs::tests::v14_*`, `v14_job_fixtures`, `charts::tests`, `dashboard::monitoring::tests` |
| [Local compatibility harness](compat-local-receipts.md#harness) seed and replay cases | A v1.4 seed (ledger, shapes, an Ollama provider configuration, a runbook with a text and a PDF collection, approved index, session, capability tokens) and 127 replay cases over REST and the native bridge plus 29 typed gRPC calls | The v1.4.0 binary's own transcript on a copy of the same database; comparison rules in the harness | `compat_local.py`, `grpc_driver.py` ([receipts](compat-local-receipts.md)) |
| [MMP scenarios](../../conformance/SCENARIOS.md) | Seven wire scenarios and one kernel scenario | Every scenario passes on both transports | `mmp-conformance`; the four client conformance suites |
| Client conformance suites at `clients-v1.4.0` | Scenarios plus plane and platform smokes per language | Pass, with the documented chronology skip | Each client's `conformance` target |
| [Helm render test](../../../scripts/test_helm_chart.py) | Eight render cases, including a byte comparison of the legacy render with the v1.4.0 chart | Identical legacy render | `py scripts/test_helm_chart.py` |
| [Wire inventory](../wire-compatibility.md) | Integer carriers, unknown-field and presence policy | As documented | Offline boundary fixtures named there |

## Accountable components

| Area | Accountable role | Baseline it owns |
|---|---|---|
| Release record, P0-T1, final receipts | Server release engineering | This page; candidate and baseline digests in each receipt |
| REST, typed gRPC, native bridge, SSE ([COMPAT-T1](README.md#compat-t1)) | Server API maintainers | Replay cases and wire policy |
| CLI, SDKs, configuration ([COMPAT-T2](README.md#compat-t2)) | Client maintainers; Server CLI maintainers; chart owner | Published packages above, `mmctl` exit codes, chart render |
| Stored state and rollback ([COMPAT-T3](README.md#compat-t3)) | Server storage maintainers | Migration boundaries below |
| Optional work and cost ([COMPAT-T4](README.md#compat-t4)) | Server retrieval and provider maintainers | Default-off settings and outbound call counts |
| Console ([COMPAT-T5](README.md#compat-t5)) | Console owner | Phase 1 console fixtures, URLs and actions |
| Matrix consumer | Matrix release maintainers | Matrix pin and `mmctl matrix` verbs |
| MCP native baseline ([A5-T2](mcp-analysis.md#a5-t2)) | Server MCP lead | Native results with MCP disabled |

## Rollback boundaries by migration

`munarium-store-pg` runs the embedded migrations at startup without
`ignore_missing`. A binary therefore refuses to start against a database that
has a migration it does not know (`MigrateError::VersionMissing`). Every row
below is additive, but each one is a binary-rollback boundary for every older
release.

| Migrations | Introduced by | Effect | Binary rollback |
|---|---|---|---|
| 0034 | 1.2.1 | Collection governance | Older binaries cannot open; restore the pre-upgrade backup ([container record](../../CONTAINER.md#previous-image-121)) |
| 0035–0040 | 1.3.0 | Budget evidence, governance profiles, accounting, recovery, retention | Older binaries cannot open; restore before policy activation ([1.3 guide](../guides/server-1.3.md)) |
| 0041–0042 | 1.4.0 | Platform authority and protected record mapping | Image-only downgrade to 1.3.0 unsupported ([1.4 guide](../guides/server-1.4.md#database-and-configuration-upgrade)) |
| 0043 | ClearView R4 | Publication fencing: three new tables | **v1.4.0 refuses to start (VersionMissing).** Drain fenced builders and resolve pending publication intents before any v1.4 builder returns ([Datastore guide](../guides/datastore.md#mixed-version-builders-drain-before-switching)) |
| 0044 | ClearView R6 | Per-version job outcomes: two new tables | As for 0043 |
| 0045 | ClearView R8 | Tenant turns: one table, a sequence and a trigger on job insert that also covers v1.4 writers | As for 0043; disable `tenant_fair` before a v1.4 builder returns |
| 0046 | ClearView R9 | Nullable progress columns on `index_build_jobs`, an event table and triggers | As for 0043 |
| 0047 | ClearView R5/C2 | Nullable `error_code` column with a syntax check | As for 0043; a v1.4 builder writing `error` alone is read as a generic failure |
| 0050 | ClearView U10 | Console sessions table, unread in `legacy` mode | As for 0043 |
| 0060 | ClearView R10 | Artifact damage tables, unread with the default `off` | As for 0043 |
| 0070 | ClearView Phase 5 | MCP store tables, unwritten with MCP unset | As for 0043 |
| 0080 | ClearView IR-1–IR-4 | Inference stage tables, empty without an enrolled collection | As for 0043 |
| 0082 | ClearView ART1/ART2 amendment | `block_assurance` and `clear_assurance` on artifact damage incidents; existing rows are `independent`, and the independence CHECKs keep their condition for them | As for 0043 |
| 0083 | ClearView ART1/ART2 amendment | Enrolled verifier public keys, empty until an operator enrolls one | As for 0043 |
| 0084 | ClearView IR-1 | `text_changed_at` on `provider_configs`; existing rows take the migration time, so an earlier inference probe must be repeated | As for 0043 |
| 0085 | ClearView §5.3 | MCP operation key tombstones, empty until the key janitor prunes a key | As for 0043 |
| 0086 | ClearView B2 follow-up | Replaces the build-job event trigger function so its prune skips event rows a concurrent prune holds; no row is touched | As for 0043 |

In short, once any of 0043–0086 is applied, the v1.4.0 image cannot serve that
database. The local [COMPAT-T3 negative control](compat-local-receipts.md#compat-t3)
reproduces this. By maintainer decision of 20 November 2026 the rollback route
is a **pre-upgrade backup restore**: take a backup before upgrading and restore
it into v1.4.0 to roll back; data written by v1.5 after the upgrade is lost. See
[rolling back to 1.4.0](../guides/server-1.5.md#rolling-back-to-140) and the
[cluster plan](cluster-qualification-plan.md#plan-compat-t3).

## Pending contracts and measurements

| Item | Accountable role | Affected phase | Next review |
|---|---|---|---|
| Hub ADRs 0020–0028 (M1, C1, C2, I1+I2, SC2, A1+A4, A2+A8+A9, A6, ART1+ART2), accepted 20 November 2026: contract export | Hub maintainers with each component owner | M, 1, 2, 5, 6, 8, SCALE | `<YYYY-MM-DD>` |
| Matrix consumer switch (M1 step 3) and M1-T2 installed-peer receipt | Matrix release maintainers | M | `<YYYY-MM-DD>` |
| Lens model runtime decision and I3 measurements | Lens maintainers | L, 6 | `<YYYY-MM-DD>` |
| B4 PDF containment measurements | Server extraction maintainers | 3 | `<YYYY-MM-DD>` |
| LOGIN1 cutover preflight and rollback revocation (manual today) | Console owner | 7 | `<YYYY-MM-DD>` |
| Release-candidate digest and chart revision for every receipt | Server release engineering | Release | `<YYYY-MM-DD>` |
| Acceptance of L3 or local receipts as environments of record | Release owner | Release | `<YYYY-MM-DD>` |
