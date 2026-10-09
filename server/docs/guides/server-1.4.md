# Server 1.4: release and upgrade preparation

**1.4.0 is an unreleased source candidate**, containing Server and `mmctl`.
The last published image remains **1.3.0**. This guide prepares review and
qualification; it does not announce a tag, signed image, package publication,
production deployment or human acceptance of the platform contracts.
See the [publication record](../../CONTAINER.md#versions-and-verification).

## Changes since 1.3.0

The baseline is tag `v1.3.0`, source
`eaa04ac6da25cb332b674c6535013a19b87fa0e7`. The merged changes below run through
`7d4a4d8`, followed by release preparation, Haiku compatibility and the
OpenRouter/session-answer fixes described below.

| Change | Behavior and boundary |
|---|---|
| [#71](https://github.com/iokaio/munarium/pull/71), [#72](https://github.com/iokaio/munarium/pull/72) | Record the published 1.3.0 image and its qualification; align SDK source versions and document that release. Those historical results do not qualify 1.4.0. |
| [#73](https://github.com/iokaio/munarium/pull/73) | Describe Server client families for the separate client publishing repository. Source preparation does not publish packages. |
| [#74](https://github.com/iokaio/munarium/pull/74) | Remove the extracted Matrix source and clients from this checkout. Matrix remains a separate service and repository, consumed through its wire contract. |
| [#75](https://github.com/iokaio/munarium/pull/75) | Add platform authority, authenticated service transport, protected decision records/replay, SDK transport support and migrations 0041–0042. |
| [#76](https://github.com/iokaio/munarium/pull/76) | Add governed Stage 2 action archives and lifecycle events with exact durable acknowledgements and bounded historical recovery. |
| [#77](https://github.com/iokaio/munarium/pull/77) | Add the Server activation participant and atomic custody of participant receipts, Council transition archives and audit events. |
| 1.4.0 preparation | Add Haiku 5.5 sampling compatibility; align source versions, generated API metadata and release/upgrade documentation. |
| Provider and session compatibility | Add explicit OpenRouter reasoning controls and unwrap echoed model-only repair envelopes before deterministic answer checks, including streamed turns. |

### Claude Haiku 5.5

Select the fixed model ID explicitly in an Anthropic ProviderConfig:

```yaml
apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata: { name: claude-fast }
spec:
  provider: anthropic
  credentialRef: { env: MUNARIUM_SECRET_ANTHROPIC }
  models:
    fast: claude-haiku-5-5
```

Haiku 5.5 rejects the deterministic temperature previously sent by internal
tasks such as query expansion. The adapter now omits temperature for the exact
`claude-haiku-5-5` ID, including ordinary and structured completion requests.
It preserves the output ceiling, structured schema and request identity rules.
Legacy Haiku 4.5 and unrelated models keep their existing sampling behavior;
the built-in Fast tier is not automatically switched by installing this release.

Thinking and effort use Anthropic defaults. The existing
`spec.anthropic.models` control block remains limited to its documented models;
this fix does not add explicit Haiku effort/thinking controls. Reassess token
ceilings and quality with representative prompts, since thinking can consume
the output budget. See Anthropic's
[migration guide](https://platform.claude.com/docs/en/models/haiku-5-5/migration-guide)
and [Server provider configuration](managing-key-and-secrets.md).

Apply the configuration through the authenticated provider API or `mmctl apply`.
A runbook must permit that provider override. Verify `/v1/providers` reports
the intended Fast model, then use a fresh session for a bounded question that
exercises query expansion, retrieval and completion. Model availability and
answer quality require actual provider evidence; fixture tests do not prove them.

### OpenRouter and session answers

`spec.openrouterReasoning` accepts an explicit `enabled` boolean per exact model
ID. Omission preserves provider defaults. Configured requests require downstream
parameter support and retain any `openrouterProvider` routing restrictions and
the existing output ceiling. Upgrade all replicas before applying the additive
configuration; older binaries ignore it. Qualify the requested control for each
model before use. See [provider configuration](managing-key-and-secrets.md#openrouter-reasoning).

Some models echo the model-only evidence envelope supplied with a corrective
prompt. Session completion now extracts the answer from that reserved envelope
before citation, quotation and assertion checks. This shared path covers normal
turns and SSE turns. Model-declared verification is ignored; malformed reserved
envelopes fail, while ordinary JSON answers retain their requested format.
Provider token-exhaustion handling runs first, preserving bounded retries and
usage progress events. These deterministic checks do not establish factual
correctness or live model quality.

### Munarium Governance Platform

The opt-in `platform-v1` profile requires independently enrolled authority and
direct mTLS on REST and gRPC. Current certificate identity, tenant and scope
govern admission; forwarding headers do not establish identity. Governance
attestations bind the exact artifact and expected authority state, including
epoch, nonce and validity. Retirement atomically installs successors and cannot
restore bootstrap authority under another key ID.

The retained checkpoint fences database activation and recovery. Platform mode
refuses legacy governing writes and protects its reserved ledger from ordinary
credentials. Unenrolled databases retain legacy behavior. Once enrolled, a
database refuses legacy startup. See [Stage 1 authority](../platform-stage1.md).

Stage 2 records bind action artifacts, lifecycle prerequisites, logical producer,
authenticated recorder, generation and stream continuity. Exact retries return
the original acknowledgement; changed immutable identities conflict. Recovery
requires separately ratified permits for exact historical events and original
claims. A record or acknowledgement grants no dispatch authority.

The activation participant independently fetches current Council ratification,
Gate pause/head and Registry receipt/head over mTLS. It checks the root authority
again after callbacks, then atomically records the participant receipt, canonical
transition archive and accountability event. Outgoing participant HTTP uses
HTTP/1.1; native gRPC still uses HTTP/2. Head responses retain
`cell_resumed:false` and `execution_enabled:false`.
See [Stage 2 records and activation](../platform-stage2.md).

These implementations consume **proposed** candidate contracts. A release of
the implementation does not accept those contracts, certify remote evidence,
enable target effects, or qualify production scale. The platform profile supports
one Server replica. The protected-ledger scans prioritize correctness rather
than demonstrated scale. Snapshot restore, cross-participant reconciliation,
outbox delivery and execution qualification remain separate obligations.

## API and SDK compatibility

MMP remains major 1; existing `/v1` and `/v1.2` paths remain. Platform authority,
records and activation use named REST/gRPC operations with the same handlers.
SDK source packages and their version-handshake targets are prepared at
**1.4.0**, declaring the N/N-1 range **1.4/1.3**, pending qualification.
That range does not make new platform operations available on a 1.3 server.
Platform callers also need the enrolled authenticated transport/profile.

OpenAPI comes from the Server binary; `scripts/generate_server_api.py` derives
the named RPC/SDK catalog. Do not hand-edit generated or vendored contracts.
Publish Rust wire crates `munarium-proto` and `munarium-api-types` before the
Rust client. Image and SDK publication are independent approval/release actions.
See [compatibility](../../../clients/compatibility.json),
[REST](../api/rest.md) and [gRPC](../api/grpc-reference.md).

## Database and configuration upgrade

| Migration after the 1.3.0 schema | State |
|---|---|
| 0041 | Platform authority, immutable receipts and authority transition guards |
| 0042 | Immutable mapping from enrolled tenant to its protected record ledger |

1. Retain and rehearse a consistent backup of PostgreSQL, sources, derived
   artifacts, configuration and existing governance/recovery records. Platform
   checkpoints and signing-key custody require independent protection.
2. Rehearse 1.3.0 to the exact 1.4.0 candidate on a disposable copy. Migrations
   are additive; never reset the schema or change an applied migration.
3. Drain work and upgrade all relevant readers, writers and workers before
   enabling new policy or relying on new platform operations. Review provider
   budgets before paid probes; automatic vocabulary generation can also spend.
4. For Haiku, apply only the intended model mapping while preserving credentials,
   budgets and other tiers. Check model disclosure and a fresh completing turn.
5. Keep platform adoption explicit. Configure enrolled peers, human governance
   authority and independent checkpoints, then run the documented enrollment
   ceremony. Installing the image is not enrollment or execution authorization.
6. Verify health, readiness, `/version`, authentication on both transports,
   scoped retrieval and persistence after restart. Platform installations also
   need current-authority refusals, exact retry and participant reconciliation.

An image-only downgrade to 1.3.0 is unsupported once migrations 0041–0042 exist.
Do not delete migration history. A restore must also reconcile independently
retained authority and participant records before serving. Restoring both the
database and checkpoint to old state cannot be detected by the checkpoint alone.
Prefer a compatible roll-forward fix after authority activation. No 1.4.0
backup/restore rehearsal or production rollback certification is claimed here.

## Testing, documentation and CI

Run from the named directory and retain exact source identity and outcomes.
The [validation guide](validation.md) explains receipts and failed versus
unavailable coverage. Use isolated databases, fictional credentials and loopback
bindings; preserve required assertions and clean up only owned resources.

| Stage | Entry point and evidence |
|---|---|
| Provider regression, `server/` | `cargo test --locked -p munarium-providers` and `cargo clippy --locked -p munarium-providers --all-targets -- -D warnings`; captured requests verify Haiku sampling omission, legacy sampling, schemas and output budgets. Paid tests returning early without keys are not live-provider evidence. |
| Mandatory preflight, `server/` | `python tools/gate_catalog.py runner.regression catalog.regression catalog.equivalence format`; inspect all selected outcomes, not only formatting. |
| Server tiers, `server/` | `./test.ps1 -Postgres -BlackBox -Platform -Cluster`; or the broader `./gates.ps1` with its documented tools. Includes PostgreSQL, REST/gRPC, platform and cluster conformance. |
| Platform authority/actions | Follow the separate `platform-authority` job in the [Server workflow](../../../.github/workflows/server-ci.yml): memory tests, explicitly selected `--ignored` PostgreSQL authority/action tests, Server API tests and live Python mTLS tests on both stores. The general platform conformance tier is not a substitute for this job. |
| Generated documentation | Export `cargo run -p munarium-server -- openapi` to `server/docs/api/openapi.json`; run `python scripts/generate_server_api.py` from root, then its `--check`. Run Server `docs_coverage` and regenerate/check the gRPC reference if its source changes. |
| Version and repository checks, root | `python scripts/check_release_versions.py`, `python clients/check_compatibility.py`, `python check_license.py`, `python scripts/docs_linkcheck.py`, `python scripts/private_material_scan.py`, and `git diff --check`. Ignored-file findings remain findings and must not be suppressed. |
| SDKs | Run each affected language's formatting, lint, build and tests from [CONTRIBUTING.md](../../../CONTRIBUTING.md); retain four-language hosted conformance evidence on the exact PR head. |

The PR records commands, results and unavailable checks on its actual head.
Existing automatic workflows remain enabled: Server lint/test, independent
platform authority, embedded datastore, dependency and Terraform checks;
four-language clients/conformance; repository hygiene and DCO. CI permissions,
signing/release controls and contract candidates are unchanged by this preparation.
After any fix or rebase, inspect the new head's complete checks. A previously
green commit does not cover a different head.

## Build and release checklist

1. Review the feature-branch diff and PR, including generated metadata, release
   scope, disclosure and human DCO. Resolve conversations and obtain required
   human review. Refresh the base and verify all checks on the exact head.
2. After a separately authorized merge, bind the release-owning workflow to the
   resulting source SHA. Build Linux AMD64 and ARM64 candidates with explicit
   source/version labels, dependency SBOM and provenance. Use the
   [container build procedure](../../CONTAINER.md#building-from-source).
3. Before scheduling qualification, record each environment's owner,
   availability, cost authority and expiry. Audit release inputs in the owning
   workflow. Qualify exact artifacts for image/license/security checks,
   REST/gRPC authentication, PostgreSQL upgrade and restore, retrieval,
   persistence, CLI behavior and bounded model calls. Label emulated ARM64
   separately from physical ARM64. Preserve failed and unavailable evidence.
4. Qualify the platform profile separately, including independent checkpoint
   recovery and cross-service participant reconciliation. Prior component or
   Harness results are historical evidence, not proof for a new image set.
5. With release authorization and completed required evidence, sign and promote
   the tested index without rebuilding; verify anonymous pulls, architecture
   digests, signature identity, provenance and SBOM. Record the tag and source.
6. Only then update `release-versions.json`, installation/Helm defaults and the
   published container record together. Preserve old immutable tags. Publish
   SDK packages separately in dependency order after their qualification.
7. Deploy an approved digest to the intended environment, retaining rollback
   inputs and configuration. Observe health, version, admission, scoped model
   completion and persisted authority. Record the deployed revision and limits.

The earlier private demo Haiku rollout used a patched 1.3-based image. Its model
smoke does not establish that this 1.4.0 source, new migrations or Governance
Platform additions have been deployed or release-qualified. No public 1.4.0
image/tag, two-architecture acceptance, restore rehearsal or platform production
qualification is asserted by this source-preparation PR.
