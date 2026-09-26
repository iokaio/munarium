# Server 1.3: release preparation and upgrade

This checkout prepares **1.3.0** of `iokaio/munarium`, containing Server and
`mmctl`. Publication and image qualification are pending. The last published
image remains 1.2.1; its digests and test results in the
[container record](../../CONTAINER.md#versions-and-verification) do not qualify
1.3.0. Matrix remains 1.0.0 and client packages retain their independent versions.

## Changes since the 1.2.1 image

The review starts at image source `c638a8e56fff45cef358ff2f4a5b5ba57957ba59`
and covers merged main through `f5212c8` (PR #69). The following groups account
for PRs #29–39 and #42–69, plus the CI changes in #40 and `6b39a36`.
Historical measurements remain tied to their recorded source and environment.

| Area and changes | Behavior in 1.3 |
|---|---|
| Release and SDK publication (#29–39) | Recorded 1.2.1 image identities, published client packages, Rust wire-crate packaging and manual client publication. These are separate from the new container release. |
| Usage evidence (#44, #47, #48, #64) | Retain original reservations and observed/estimated/unknown usage; reconcile late evidence without rewriting history or changing its original accounting day. Estimates are not provider invoices. |
| Provider admission and diagnostics (#65, #66) | Opt-in `dailyTotalTokens` reserves shared config capacity for each HTTP attempt, including retries. Managed diagnostics restrict paid probes to management callers and capped tenant configurations. Credential aliases are explicit public labels. |
| Claude and completion behavior (#69) | Exact-model effort/thinking controls preserve structured output and output ceilings. A larger completion retry requires explicit token exhaustion; empty non-truncated responses do not trigger it. Incomplete final responses fail. Revised bundled prompts have new runbook versions. |
| Durable recovery (#52–54, #64, #67) | Runbook checkpoints and transitions commit atomically; critical task failures affect readiness. Opt-in PostgreSQL `guarded-v1` claims prevent blind command re-execution after ambiguous outcomes. This does not guarantee exactly-once remote effects. |
| Governance and evidence (#49, #55, #56, #64) | Durable immutable comparison profiles, explicit history transitions and retained findings; model evidence is framed as untrusted input. Matrix execution receipts retain outcome identity. |
| Source retention (#56, #68) | Tenant-wide stable-path denial, holds, and retryable PostgreSQL original-byte cleanup. Derived data, audit history, external copies and backups require their own retention procedures. |
| Monetary accounting (#60) | Optional immutable tariffs, per-attempt liability and append-only observations, with coverage-aware currency reports. No automatic prices, whole-account billing claim or monetary admission limit. |
| Storage and wire integrity (#46, #49, #51, #57, #58) | Explicit JSON persistence checks, deterministic dependencies, sparse retrieval work diagnostics, exact integer handling and fail-closed unknown values, plus Linux filesystem qualification. |
| Embedded library and panic handling (#61, #62) | Supported pinned-source datastore API, Rust 1.92 minimum, four feature sets and isolated consumer checks. Invalid artifacts and serialization/startup failures produce errors; production panic shortcuts are denied with documented exceptions. |
| Validation and documentation (#40, #42, #43, #45, #46, #50, #59, #63, `6b39a36`) | Source-bound test receipts distinguish failed and unavailable coverage; frozen evaluations preserve inputs and outcomes. Shared local/CI gates retain automatic suites. Reviewed history and implementation guidance accompany the code. |

See the [changelog](../../CHANGELOG.md), [token accounting](../tokenbudgets.md),
[provider configuration](managing-key-and-secrets.md),
[monetary accounting](monetary-accounting.md), [wire compatibility](../wire-compatibility.md),
[embedded support](../embedded-support.md) and [validation guide](validation.md).

## API and client compatibility

MMP remains major 1. Existing `/v1` and `/v1.2` paths keep their names; the
Server version does not introduce a `/v1.3` prefix. Additive management methods
cover usage reconciliation, monetary accounting, provider diagnostics, command
recovery and source retention. Consult the current [REST](../api/rest.md) and
[native gRPC](../api/grpc-reference.md) references for authority and payloads.

The committed generated client surface describes this source tree. Published
1.1.1 SDK packages predate the new methods. The source
[client compatibility record](../../../clients/compatibility.json) and all four
version-handshake constants target 1.3.0, with the N/N-1 range 1.3/1.2. These
are release qualification targets; published packages retain their historical
metadata. Run all four language conformance suites before publication; a version
constant alone does not qualify either transport or the support range.
Checkout Rust clients use the 1.3.0 wire crates; publishing those crates
and a new SDK package is a separate release action.

## Database and configuration upgrade

Server startup applies migrations after the 1.2.1 baseline (0034):

| Migration | Retained state |
|---|---|
| 0035 | Original token reservations and usage evidence |
| 0036 | Governance profiles, revisions and transition protections |
| 0037 | Monetary prices, physical attempts and observations |
| 0038 | Append-only token adjustments and estimator/evidence revisions |
| 0039 | Tenant command-recovery activation and durable command claims |
| 0040 | Source denial/hold/cleanup journal and write guards |

1. Back up PostgreSQL, source bytes and derived artifacts together with their
   configuration, active pins, policies and accounting records. Rehearse on a
   disposable copy before an operational upgrade.
2. Drain work and upgrade all readers, writers and workers before relying on
   the new policies. Additive DDL does not make mixed-version behavior safe:
   older binaries ignore provider controls and can bypass recovery/retention.
3. Governance stays legacy by default. Follow the
   [governance upgrade](../ops/governance-policy-upgrade.md) to adopt profiles,
   transition existing histories, review findings and rebuild affected collections.
   Installing 0036 alone does not change historical comparison semantics.
4. Enable `guarded-v1` only after all command writers are upgraded. Activation
   is one-way. Resolve `command-unresolved` by inspecting authoritative effects,
   never by automatically issuing a new key. See
   [command recovery](../ops/command-recovery.md).
5. Review `dailyTotalTokens`, `MUNARIUM_MANAGED_PROVIDER_DIAGNOSTICS` and exact-model
   Claude settings explicitly. Legacy behavior remains where opt-in settings are
   omitted. Managed probes can spend tokens; readiness probes are separate.
   Automatic vocabulary generation remains default-on and may call a paid provider.
6. Use [source retention](../ops/source-retention.md) deliberately: denial has no
   undeny operation; completed cleanup means only PostgreSQL original bytes were
   removed. Export all journal pages for independent restore reconciliation.
7. Reload revised runbooks and use fresh sessions to evaluate them. An existing
   session retains its pinned version. Repository checks do not establish live
   model quality; compare grounding, completeness, truncation, usage and latency.

An image-only downgrade to 1.2.1 is not supported: its migrator does not know
0035–0040. Do not delete migration history. A pre-upgrade backup is necessary
for a legacy rollback but is not sufficient after policy activation or external
effects: it may lose source denials, command claims and accounting evidence.
Prefer a compatible roll-forward fix. Keep restored systems isolated until
authoritative recovery, retention and revocation records are reconciled using
the [backup/restore procedure](../ops/backup-restore.md).

## Build and release checklist

The target version is `1.3.0`, the eventual immutable Git tag is `v1.3.0`, and
the candidate image tag is `1.3.0-rc.1`. Only after qualification should the same
verified image index receive `1.3.0`, `1.3` and `latest`. Leave the existing
`1.2` and immutable earlier tags unchanged.

- Use the reviewed merged source commit. Check the workspace version, Docker
  label, `/version`, OpenAPI version and generated API inventory all report
  1.3.0. Record the exact source revision, toolchain, build features and builder.
- Run the [component gates](validation.md), including PostgreSQL, native
  REST/gRPC, client conformance, feature combinations and migration checks.
  Preserve missing coverage as `not_run`; historical receipts are not new results.
- Build both `linux/amd64` and `linux/arm64` using the pinned Dockerfile and
  locked dependencies. The [container build command](../../CONTAINER.md#building-from-source)
  exports an OCI archive with SBOM and provenance without publishing it.
- Test each exact platform artifact for version/labels, nonroot startup, auth,
  readiness, persistent writes/reads, CLI, retrieval and restart behavior.
  Record native versus emulated execution. Run controlled provider tests and
  separately authorized real-model checks; neither substitutes for the other.
- Rehearse 1.2.1 → 1.3.0 on a database copy, including migrations, policy
  activation, unresolved commands, source holds/denials and restore reconciliation.
  Security-scan both manifests and review dependency/license notices.
- In the authorized release process, publish the candidate, pull and qualify
  its exact registry manifests, then promote that same index digest. Verify the
  source Git tag points to the tested commit and preserve signing/attestations.
- Record the 1.3.0 OCI index and platform digests, signing identity, date,
  source SHA and qualification/rollback outcomes in `CONTAINER.md`. Mark the
  changelog and installation guidance published only when this evidence exists.

This preparation does not publish an image, create a release tag, deploy a
service, or qualify a production backup restoration.
