# Matrix switch receipts (ClearView M1 step 3)

This page records the Server side of [M1](repository-handoffs.md#m1) step 3, "switch the consumer without changing deployment", and the procedure for the receipts that still need an installed Matrix peer. The acceptance criteria are [M1-T1](repository-handoffs.md#m1-t1) and [M1-T2](repository-handoffs.md#m1-t2). Where this page and the handoff page disagree, the handoff page wins. Step 4, removing the superseded Server sources, has not started. [Transfer order and rollback](repository-handoffs.md#transfer-order-and-rollback) · [Matrix transfer inventory](https://github.com/iokaio/munarium-matrix/blob/79aa8c573011e8095c171dc532b01f19a6ecda63/docs/clearview-transfer.md)

## What the Server consumes

The kernel's `munarium_core::evidence::CONTRACT_VERSION` keeps its v1.4 name. Its value now comes from the generated `server/src/munarium-core/src/matrix_pin.rs` instead of a compile-time read of `server/contract/matrix/VERSION`. `server/tools/matrix_pin.py` generates that file and `server/contract/matrix-consumer/matrix-consumer.json`, the Matrix publisher's `--consumer-out` export, from one publisher run. [Contract directory guide](../../contract/README.md#the-matrix-consumer-pin-clearview-m1-step-3)

| Pinned value | Value |
|---|---|
| Wire contract version | `0.1.0` |
| Locked bundle digest | `2f35a515db4501a0320f51335e3d212796f757f06af6f309f8b3f639305ced96`, the same digest as the vendored v1.4 lock |
| Verified source commit (from the export) | `643863fcb4433c8236f3ab22ca812f17c354f34c` |
| Publisher revision | `79aa8c573011e8095c171dc532b01f19a6ecda63`, Matrix `feat/clearview` |
| Publisher review status | **Candidate: not reviewed or merged.** Re-pin to the reviewed, merged revision before step 4. |
| Export sha256 / `profile_digest` | `ca6f0099…3eca` / `52721cdd…782d`, in full in `matrix_pin.rs` |

Three checks tie the pin together. They do not replace the existing vendored-copy gate:

- `munarium-core`'s `matrix_pin` test verifies the compiled constants against the tracked export. It covers the file hash, the wire version and digest, the verified source and an independent recomputation of `profile_digest`. Negative controls show that a changed byte, route, version or digest is refused. It runs in the ordinary `cargo test --workspace` gate with no Matrix checkout.
- `munarium-api-types`' `matrix_contract` test checks that the export names the vendored bundle's version and digest over 17 locked files. The lock test ties each of those files to the digest. Negative controls on a scratch copy show that a tampered, missing or unlisted vector is refused.
- `tools/matrix_pin.py --check` needs the Matrix checkout at the pinned revision with a clean `contract/` tree. It runs the publisher's `--self-test`, then cuts the bundle and checks it with `--verify`. It checks the vendored copy with `--check` and compares the 17 locked files byte for byte. It then exports the profile, runs `--consumer-check`, recomputes the digest and compares both generated files with the tracked ones. It refuses a checkout at another revision.

The export and the vendored v1.4 bundle are byte-identical in every locked file. A fresh cut at `79aa8c5` differs from the vendored copy only in the lock's `source_commit`, which is `unknown` in the vendored copy.

## M1-T1 results, 19 November 2026

These local results are rehearsal evidence from an unreviewed candidate. They are not a release receipt.

| Check | Result |
|---|---|
| Reproduce the cut at Matrix `79aa8c5` (`--self-test`, `--out`, `--verify`, `--check` against the vendored copy, `--consumer-out`, `--consumer-check`) | Passed. 17 files, contract `0.1.0`, digest `2f35a515…ced96`. `diff -r` shows only `source_commit`. |
| Reject a tampered, missing or unlisted vector | Passed. The publisher's self-test, `matrix_pin.py --check` against a modified or renamed vendored vector, and the new Rust negative controls all refused. |
| Reject a tampered export or a wrong publisher revision | Passed. `matrix_pin.py --check` and the `matrix_pin` test refused an edited route. `--check` refused a checkout of `2203173`. |
| Standalone build and tests from declared inputs | Passed on Windows with no sibling path read by any build or test: `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked -p munarium-api-types -p munarium-core` (including 10 `matrix_contract` and 7 `matrix_pin` tests), `cargo test --locked -p munarium-server --bin munarium-server` (330 passed, 8 ignored; 16 `evidence_providers::`) and `docs_coverage` (7). `check_matrix_contract.py` passed against the Matrix checkout. Cargo used the local registry cache, and the run did not disable the network. |
| Container image from `server/` | Passed. `docker build` with only `server/` as the context produced local image ID `sha256:bfdb2a78…365e`, with no sibling checkout in the context. The Server started with authentication disabled, and `/mmctl` printed the v1.4 `mmctl matrix` usage. The report route needs PostgreSQL and is covered by M1-T2. The build fetched base images and crates from the network. |

Still open for M1-T1:

- Repeat the build with external network disabled after `cargo fetch --locked` and from the packaged source distribution.
- Re-pin to the reviewed, merged publisher revision.

## M1-T2 procedure: installed peer and clients

M1-T2 needs a running Matrix peer. It was not run for this change. The procedure uses only disposable local resources: a dedicated Compose project name, loopback ports, test tokens and a disposable database volume that is removed at the end. It needs no paid resource.

**Inputs to record.** Server v1.4.0 image by digest, as `<baseline-digest>`. The candidate image built from this branch, as `<candidate-digest>`, with the Server commit. The Matrix image built from the reviewed publisher revision, with its commit. The four released v1.4 SDK versions from `clients/release.json`. The chart revision. The host and Docker versions.

1. **Start an isolated stack.** From a Matrix checkout at the pinned revision, run `docker compose -p m1t2 up -d postgres munarium-matrix`. Seed one data view with the Matrix operator guide's quickstart. Record its name as `<view>`.
2. **Baseline, unconfigured.** Start the v1.4.0 image on the same network and database with `MUNARIUM_MATRIX_BASE_URL`, `MUNARIUM_MATRIX_ADMIN_URL` and `MUNARIUM_MATRIX_TOKEN` unset. Record `GET /v1/reports/matrix`, gRPC `MatrixReport`, `/admin/matrix`, `/metrics` lines matching `munarium_matrix_provider_requests_total` and a runbook declaring `<view>`. That runbook must fail its verify step, not pass vacuously.
3. **Baseline, configured.** Restart the same image with the three variables set to the peer. Save a runbook with a `matrix:<view>` reference and run one structured read. Save the answer's citations and a `matrix.` finding filter result. Also run a governed refusal, an unauthorized read and a peer outage (`docker compose -p m1t2 stop munarium-matrix`). Record the report, typed refusal, breaker state, per-user audit rows and metric `outcome` labels for each case.
4. **Clients.** Against the configured baseline, run the report and evidence calls from the Rust, Python, .NET and Java SDKs at their released v1.4 versions. Save each response.
5. **Upgrade.** Stop the baseline. Start `<candidate-digest>` with the same database, environment and tokens. Repeat steps 2 to 4 without reseeding. Compare each saved response and metric series. Differences other than timestamps and IDs fail the receipt. The `contract_version` emitted on sealed evidence must stay `0.1.0`.
6. **Rollback.** Stop the candidate. Start `<baseline-digest>` with the same database and original configuration. Repeat steps 3 and 4. Check that the runbooks, evidence references and citations written by the candidate still resolve.
7. **Helm.** Render the chart with `helm template` twice: once with `matrix.baseUrl` and `matrix.adminUrl` empty, and once with both set. Diff the rendered Server environment against the Compose inputs used above. Run the full install only in the environment the [cluster qualification plan](cluster-qualification-plan.md) authorizes.
8. **CLI (M1-T3 inputs).** With `MUNARIUMCTL_MATRIX_URL`, `MUNARIUMCTL_MATRIX_TOKEN` and `MUNARIUMCTL_UID` set, run Matrix's `tools/check_mmctl_matrix.py` against `/mmctl` in both images and against `mmctl` built from this branch.
9. **Clean up.** Run `docker compose -p m1t2 down -v`. Remove only the containers, volume and images this run created.

Record one receipt per environment with the fields in [Receipt format](cluster-qualification-plan.md#receipt-format). Include both image digests, the pinned publisher revision and every step's result. Record unavailable steps as unavailable.

## Server CI pin check

Under the maintainer's decision of 20 November 2026 (CI additions), Server CI runs `matrix_pin.py --check` against the current candidate pin rather than waiting for the Matrix review:

- The `contract.matrix` gate in `server/tools/gate-catalog.json` runs `tools/check_matrix_contract.py` and then `tools/matrix_pin.py --check`. `server/gates.ps1` selects the same gate.
- The `lint-test` job keeps its `checkout Matrix contract source` step at `2203173`, the audited PR #2 extraction, as the input of `check_matrix_contract.py`. That revision predates the consumer export, so it cannot serve the pin check. A second step, `checkout pinned Matrix consumer publisher`, checks out `79aa8c573011e8095c171dc532b01f19a6ecda63` at `munarium-matrix-pin`. The job sets `MUNARIUM_MATRIX_PIN_ROOT` to that path. `matrix_pin.py` reads that variable strictly and never falls back to another checkout.
- `server/tools/gate-ci-baseline.json` records the catalog command and the audited orchestration hash. `server/tools/test_gate_catalog.py` asserts both commands and requires the pinned checkout's ref to equal `PUBLISHER_REVISION`. Its negative controls reject removing, reordering or weakening either command, disabling or moving the pinned checkout, and advancing the audited ref. A regression test shows that `--check` refuses a clean checkout at another revision.

When the pin moves to the reviewed, merged publisher revision, update the pinned checkout's `ref` and comment in the same commit. Then update the baseline under the Server CI procedure in `AGENTS.md`. Step 4 replaces `check_matrix_contract.py` and the audited checkout.

## Step 4 prerequisites

Do not start step 4 until these are complete:

- Re-pin to the reviewed, merged publisher revision.
- Move the CI pin check above to that revision.
- Pass M1-T2 and M1-T3 receipts.
- Record each owning reviewer's confirmation in the Matrix inventory.

Step 4 then switches `evidence_providers.rs`'s three embedded examples and the `matrix_contract` schema tests to the pinned external cut in an explicit interoperability job. It deletes `server/contract/matrix/` and the Matrix-only checker. It also updates `.dockerignore`, `check_license.py` and the contract guide, and coordinates Matrix's `matrix-server-contract.yml`. That workflow currently runs `publish.py --check` against `server/contract/matrix` and the `evidence_providers::` tests from Server sources.
