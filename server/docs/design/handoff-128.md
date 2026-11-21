# ClearView handoff to the 128 GB workstation

This addendum records where to resume after moving to the Windows workstation
with 128 GB RAM, a 48 GB GPU and Docker Desktop Kubernetes on WSL2. Start by
transferring the three branch tips, establishing the local cluster, and selecting
and proving a GPU-capable Lens image. Server-only rehearsals can run while the
Lens and cross-repository reviews proceed. Use the [implementation ledger](implementation-ledger.md#21-november-local-qualification-rehearsals)
for the measured results and the [cluster run order](cluster-qualification-plan.md#run-order)
for the release sequence.

## Move the source

| Checkout | Starting point |
|---|---|
| `munarium` | `feat/clearview`, at the commit that adds this document |
| `munarium-lens` | `feat/clearview`, `83dbc07e550cc1a55775e4621a317c9dbec4f656`. Adds verified offline import and the pool bearer candidate. |
| `munarium-platform` | `feat/clearview`, `18ee95ac9fdb2939521548c61f1338ae65b61162`. Adds proposed ADR 0030 for Lens service bearer admission. |
| `munarium-matrix` | Matrix [PR #4](https://github.com/iokaio/munarium-matrix/pull/4) at `79aa8c573011e8095c171dc532b01f19a6ecda63`. All nine checks passed on this head. The publisher review and cutover are pending. |

Transfer the first three branch tips through the approved Git transport. For an
offline move, run the following commands separately in each source checkout,
using a distinct bundle path outside the repository. In the destination
checkout, inspect `git status --short --branch` before fetching. Fast-forward
the corresponding branch from the bundle, then verify its full SHA. If the
destination diverged, use a separate checkout to preserve its work.

```powershell
# Source checkout
git bundle create <bundle-path> refs/heads/feat/clearview
git bundle verify <bundle-path>

# Destination checkout on feat/clearview
git fetch <bundle-path> refs/heads/feat/clearview
git merge --ff-only FETCH_HEAD
git rev-parse HEAD
```

Keep ignored scratch data, local environment files, kubeconfigs and credentials
on their source workstation. Recreate disposable credentials on the new host.
Fetch or transfer model bundles independently and verify them against their
published pins. Retain raw receipts in the approved evidence location and put
only shareable summaries in Git.

## Re-establish the baseline

1. Record the date, operator, hardware, available disk, Windows and WSL2 versions,
   Docker and Kubernetes versions, GPU driver and actual GPU memory. Inspect
   `nvidia-smi` and `kubectl --context docker-desktop get nodes`. Confirm that
   the eventual Lens pod can enumerate its assigned GPU and run inference on it.
   Size WSL2, persistent storage and builder staging using the
   [workstation sizing](cluster-qualification-plan.md#sizing-within-128-gb).
2. Create a disposable L3 namespace and database cell. Use the
   [cluster setup](cluster-qualification-plan.md#steps) and pin CNPG, its
   PostgreSQL operand and the object store. The local kind rehearsal used
   CNPG 1.25.1 and PostgreSQL 16 with pgvector. Use server-side apply for
   that operator manifest so the Pooler CRD fits.
3. Resolve the object-store input early. The pinned MinIO image failed to pull
   during rehearsal. SeaweedFS at
   `sha256:4b42c212543a0acd4c0e675215ecdcaf62a186ef98def76c7e12701871222d15`
   passed live S3 source, generation and artifact repair tests. Have the
   release owner pin the store used for L3 and prove the CNPG base backup,
   WAL archive, source and artifact consistency point, and restore on that
   store. [COMPAT-T3 procedure](cluster-qualification-plan.md#plan-compat-t3)
4. Use the published Server v1.4.0 image
   `iokaio/munarium@sha256:800a7b19cd3e2b82a3fb8f0b33e86e3f21fc3ea71ac12a81d6a9a18a816f556a`,
   the chart from release commit `25012c22087fbaa8ce29ddd68d2880d6e9499b1c`,
   and the published 1.4.0 client packages. The previous local Linux candidate
   was built from `e8a0076` before the modern MCP fix. Rebuild Server and
   `mmctl` from the transferred Server commit. Record the actual image and
   chart digests for every run. [Release baseline](v14-baseline.md)

From the Server repository root, run the Helm and compatibility regression
suites. From `server/`, run the complete format preflight and the documented
gates for every feature selected for the candidate.

```powershell
py scripts/test_helm_chart.py
py -m unittest discover -s server/tools/compat
py -m unittest discover -s scripts -p test_lens_workspace.py
Push-Location server
py tools/gate_catalog.py runner.regression catalog.regression catalog.equivalence format
Pop-Location
```

The local [compatibility harness](compat-local-receipts.md#harness) and
[official MCP SDK driver](../ops/mcp-client-qualification.md#rehearse-with-the-official-typescript-sdks)
can catch packaging and configuration mistakes before the cluster run. On
the new machine, collect release receipts against the frozen image digests.

## Close the pre-freeze work

- **Lens runtime and transport.** The starting Lens build uses Candle CPU.
  Select and record a GPU runtime, base image, driver and library pins, device
  allocation, precision and resource limits. Build a workload that verifies
  device selection and measures one and multiple workers on the 48 GB GPU.
  Preserve the CPU reference and compare vectors, stable rerank order, scores
  and content-free failures before the load run. Use the two published Qwen3
  bundles at model repository revision
  `cb8d6d3f4da7b0f5eacdbde4a61b5000f9d57cb1` and their individual manifest
  digests. Reuse the offline importer and mount the verified bundles read-only.
  Qualify pool bearer provisioning, TLS, readiness, rotation and the real
  Server probe under the reviewed IR-1/SC2 profile. Ensure the evaluation
  driver can supply the approved credential and exercise the deployed endpoint.
  [Lens model pins](https://github.com/iokaio/munarium-lens/blob/5c4a35f46c401a107e04ff3227af5e4cbe2332a0/docs/design/model-runtime.md#published-bundles)
- **Contract and Matrix cutover.** Review and export the ClearView shared
  contracts. The C2/MCP allowance review bundle is already prepared at hub
  `042973e3e13f1be635adf46d905f9de7944738ef`. Review proposed ADR 0030 and pin
  the exported Lens profile. Once the Matrix publisher is reviewed and
  merged, regenerate and verify the Server consumer pin with
  `server/tools/matrix_pin.py` and update the matching CI checkout through the
  required baseline procedure. Run the installed-peer, released-SDK,
  packaged CLI and backup/restore cases. Get the matching per-path receipts
  and owning review before removing transferred sources. [Matrix switch](matrix-switch-receipts.md#m1-t2-procedure-installed-peer-and-clients)
- **Corpora and load.** Freeze held-out judgments and thresholds before the
  measured run. Prepare the public LENS-D2 Git inventory with
  `scripts/lens_workspace.py freeze` and `verify`. Reclassify unresolved
  entries at the new pins and reconcile extracted files and blob IDs against
  ingestion. Obtain restricted-root processing authorization before freezing
  that root in L3R. Prepare the arrival-rate workload, mixed-version overlay,
  and owned evidence collection described in the qualification plan. Keep
  per-file citation and rollback outcomes separate from held-out scoring.
  [Frozen evaluation](../guides/frozen-evaluation.md) · [Workspace inventory](repository-handoffs.md#workspace-inventory)

## Run W0 and close the release

1. Freeze the Server, Lens and Matrix revisions and artifact digests, chart
   inputs, client packages, model and corpus manifests, and test topology.
   Record the owner and actual limits for each receipt. Install v1.4, seed the
   fixture and take the matching database and object-store backups **before**
   any v1.5 migration. Exercise the released clients on the seeded baseline.
2. Upgrade and start COMPAT-T1–T5, M1 and SCALE-1–3. Run Server-only resize
   and builder drain with work in flight. Exercise LOGIN1 cutover and
   ART1/ART2 isolation and repair on the same candidate for REL1.
3. Run LENS-D1 and I3 on the approved serving profile, then LENS-D2 for all
   16 roots with fresh sessions and resolved citations. Use the enrolled
   collections to qualify SC1 and SC2 under load. Capture measured latency,
   throughput, memory, provider arrivals, placement and denied dispatches.
   Run A5 with both official client revisions, durable recovery and generation
   accounting. Run the `platform-v1` case in a fresh namespace.
4. Run rollback cases last. Complete the chart, console and artifact rollbacks
   and replay prior citations with Lens unavailable and the approved index
   and policy restored. Record the direct-start `VersionMissing` refusal,
   then restore the v1.4 database backup and matching object state and start
   v1.4.0. Check that the saved reads match and the v1.5-only writes are
   absent. Tear down the disposable environment after retaining its receipts.

Use one receipt per acceptance ID and environment. Capture expected and
observed outcomes and coverage gaps. The release owner closes the gate after
reviewing the exact candidate receipts. Local rehearsal results remain useful
for debugging and comparison. [Receipt format](cluster-qualification-plan.md#receipt-format) · [Release evidence](README.md#release-evidence)
