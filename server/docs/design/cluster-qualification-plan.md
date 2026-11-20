# ClearView cluster qualification plan

Prepared **19 November 2026** on branch `feat/clearview` at `adc94cd`. This page prepares the cluster-dependent ClearView receipts so they can be scheduled efficiently later. It provisions nothing and authorizes nothing.

**Status: deferred.** By maintainer decision of 19 November 2026, paid Azure or other cluster runs wait until the entire ClearView plan is code complete, reviewed and tuned. No environment is booked, no owner, budget or reservation has been supplied, and no price on this page has been verified. Every cost line is a blank to be filled from a dated, verified quote. [Ledger row "Prepare qualification environments"](implementation-ledger.md#phase-0--v14-baseline-and-shared-decisions) · [Qualification environments](README.md#qualification-environments) · [Release evidence](README.md#release-evidence)

The design pages own the acceptance criteria. This plan only arranges them into runs. Where the two disagree, the linked acceptance ID wins and this page must be corrected.

## Contents

- [Rules that apply to every run](#rules-that-apply-to-every-run)
- [First choice: a local workstation cluster](#first-choice-a-local-workstation-cluster)
- [Pre-flight checklist before any spend](#pre-flight-checklist-before-any-spend)
- [Environment profiles](#environment-profiles)
- [Receipt format](#receipt-format)
- [Cost-estimate template](#cost-estimate-template)
- Receipts: [SCALE-1-T1](#plan-scale-1-t1) · [SCALE-3-T1](#plan-scale-3-t1) · [SCALE-2-T1](#plan-scale-2-t1) · [SC1-T1](#plan-sc1-t1) · [SC1-T2 and SC1-T3](#plan-sc1-t2-t3) · [SC2](#plan-sc2) · [COMPAT-T2](#plan-compat-t2) · [COMPAT-T3](#plan-compat-t3) · [LOGIN1](#plan-login1) · [ART1 and ART2](#plan-art) · [LENS-D1 hardware, I3 and LENS-D2](#plan-lens) · [Multi-replica MCP cases](#plan-mcp)
- [Minimum-spend ordering](#minimum-spend-ordering)
- [Tooling inventory](#tooling-inventory)
- [Decisions that block scheduling](#decisions-that-block-scheduling)

## Rules that apply to every run

- **Exact candidate.** Every receipt is collected on the frozen v1.5 release candidate (image digest, chart revision, Lens image digest where applicable). A receipt from an earlier build is rehearsal evidence only. A candidate change re-opens every receipt it could affect; [REL1](README.md#rel1) additionally needs LOGIN1 and ART1/ART2 on the *same* candidate.
- **One receipt per acceptance ID and environment.** Runs that share a window still produce separate receipts. A shared failure is recorded in each affected receipt.
- **Public repository.** No real subscription IDs, tenant IDs, hostnames, resource names, IP addresses, tokens or corpus text appear in this page, in receipts committed here, or in PR text. Use the placeholders below. Private values live in ignored local files (`*.tfvars`, `server/*.local.toml`) or in the release-owned record. Receipts that contain restricted material stay inside the authorized environment and are referenced opaquely.
- **Disposable and isolated.** Each window uses its own resource group, namespace, database and storage container, created for that window and destroyed at its end. Test credentials only. Never point a run at an existing environment.
- **Cost cap is binding.** When the running total reaches the recorded cap, including retries, stop, collect what exists, tear down, and record the remaining receipts as `not_run` with reason `cost_cap`.

Placeholders used below: `<qual-prefix>` (resource name prefix), `<region>`, `<registry>`, `<candidate-digest>`, `<baseline-digest>` (published v1.4.0 image), `<lens-digest>`, `<gpu-vm-size>`, `<ns>` (namespace, default `munarium`).

## First choice: a local workstation cluster

Before any paid Azure run, attempt every scaling and compatibility receipt on **a maintainer workstation (Windows, 128 GB RAM, Docker Desktop Kubernetes)**: the single-node Kubernetes cluster built into Docker Desktop, on the WSL2 backend. This is profile **L3** below. Azure windows are then scheduled only for what L3 cannot prove, as listed in the [minimum-spend ordering](#minimum-spend-ordering). L3 costs no cloud spend, so it needs no cost cap, but it still uses the candidate artifacts, disposable namespaces and test credentials, and it is torn down after each run.

### What one Docker Desktop node can prove

**Full** means every pass condition can be exercised on L3; the receipt is complete local-cluster evidence. **Partial** means the logic can be exercised but some condition needs more than one node, managed storage, Azure identity or GPU hardware. **Azure** means L3 can only rehearse the procedure.

| Receipt | On L3 | What one node cannot prove |
|---|---|---|
| [SCALE-1-T1](#plan-scale-1-t1) | **Full** for renders, effective counts, startup guards, admissions per window, surge with `maxSurge` 0 and default, `platform-v1` non-overlap, two upgrades and rollback | Workload identity with Azure Blob (`az` store with managed identity). Azurite exercises the `az` code path with SAS, not the identity exchange |
| [SCALE-2-T1](#plan-scale-2-t1) | **Full** for builder group, endpoint exclusion, drain with work in flight, fencing, reclaim and staging space on the local disk | Staging and object transfer on managed disks and Blob under realistic latency |
| [SCALE-3-T1](#plan-scale-3-t1) | **Full** for the comparison of operator-visible counts with SQL and fleet ground truth, failed-read handling and resize | Capacity numbers representative of a production node class. Local numbers are labelled local and are never a capacity claim |
| [SC1-T1](#plan-sc1-t1) | **Full** for pod-level resize 1 → 2 → 1, readiness, drain and v1.4 client replay under load | Node drain and node failure (draining the only node stops everything), failure domains, load generator isolated from measured CPU |
| [SC1-T2 and SC1-T3](#plan-sc1-t2-t3) | **Partial**: per-stage resize, worker replacement, incompatible profile, last-member removal, I1 and I2 outcomes with Lens CPU replicas or the Lens protocol stub | The qualified GPU profile, its latency and capacity |
| [SC2](#plan-sc2) | **Partial**: shared versus dedicated routing, refusals, budget and cache isolation with two labelled Lens deployments | Dedicated node pools and placement on separate hardware, location rules tied to real infrastructure, loss of a dedicated node |
| [COMPAT-T2](#plan-compat-t2) | **Full** for v1.4 chart upgrade with original values, client and CLI replay in both directions, Matrix-enabled values, replica-count migration and rollback | Workload identity and the managed `az` store |
| [COMPAT-T3](#plan-compat-t3) | **Full** for the selected rollback route, including mixed replicas, `VersionMissing` negative control, and a CNPG backup to MinIO for route B | Managed storage durability and restore timing on cloud storage |
| [LOGIN1](#plan-login1) | **Full** with a local TLS termination (self-signed certificate on a local gateway or port-forward) | The production gateway and load balancer TLS path |
| [ART1 and ART2](#plan-art) | **Full** with MinIO or Azurite as the authoritative store and two verifiers holding separate test credentials | Verifier identities issued by Azure workload identity |
| [I3, LENS-D1 hardware and LENS-D2](#plan-lens) | **Azure** (rehearsal only): manifest freeze, accounting, query generation, citation resolution and rollback on a small public subset | GPU hardware, quality and latency thresholds, the full 16-root run |
| [Multi-replica MCP cases](#plan-mcp) | **Full** | — |

### Sizing within 128 GB

Leave about 16 GB for Windows and Docker Desktop itself. Set the WSL2 VM limits in `%UserProfile%\.wslconfig` (on the WSL2 backend, Docker Desktop's own resource sliders do not apply), then restart WSL with `wsl --shutdown` and restart Docker Desktop:

```ini
[wsl2]
memory=112GB
processors=<physical cores minus 2>
swap=16GB
```

Raise Docker Desktop's virtual disk limit (Settings → Resources → Advanced) so images, PostgreSQL volumes, MinIO data and builder staging fit; 200 GB or more is a reasonable starting point for the corpus sizes used in local runs. Record the actual limits in every receipt.

| Component | Pods | Memory limit each | Total | Notes |
|---|---|---|---|---|
| Kubernetes system, CNPG operator | — | — | 6 GB | Control plane, CoreDNS, storage provisioner, operator |
| PostgreSQL (CNPG cell) | 2 | 8 GB | 16 GB | `cell.instances: 2`; one instance is acceptable for receipts that do not exercise replication. Set `max_connections` for the peak pod count |
| Server serving replicas | 3 | 4 GB | 12 GB | Room for 1 → 2 → 3 resizes |
| Server surge pod or separate builder | 1–2 | 4 GB | 8 GB | Builder staging on a local `emptyDir` or volume with a size limit |
| Object store: MinIO (S3) and, when testing the `az` path, Azurite | 1–2 | 4 GB | 6 GB | Both are supported: `s3` with `MUNARIUM_S3_ENDPOINT` for MinIO, `az` with `MUNARIUM_AZURE_BLOB_ENDPOINT` and SAS auth for Azurite ([source stores](../guides/source-stores.md)); Datastore artifacts use `MUNARIUM_DATASTORE_ARTIFACT_STORE` ([Datastore guide](../guides/datastore.md#3-complete-environment-reference)) |
| Lens CPU replicas or protocol stub | up to 3 | 8 GB | 24 GB | Only if the selected Lens profile has a CPU mode; otherwise the stub needs far less |
| Load generator | 1 | 4 GB | 4 GB | Give it a CPU limit and record it; it shares the host with the measured pods |
| Prometheus and kube-state-metrics | 2 | — | 4 GB | |
| Matrix peer (COMPAT-T2 Matrix pass) | 1–2 | 2 GB | 4 GB | |
| **Planned total** | | | **about 84 GB** | Leaves about 28 GB of the 112 GB VM as headroom for page cache and spikes |

Set CPU requests and limits so the sum of limits stays below the WSL2 processor count, and keep the load generator's limit fixed across runs so local comparisons stay consistent.

### Steps

1. Apply the WSL2 limits above and restart Docker Desktop.
2. Enable Kubernetes: Docker Desktop Settings → Kubernetes → Enable Kubernetes → Apply. If the installed version offers a choice of provisioner (single-node kubeadm or a kind-based cluster with several nodes), record the choice; several kind nodes on one host still share one machine's failure domain and disk.
3. Select the context: `kubectl config use-context docker-desktop`, then `kubectl get nodes`.
4. Build the candidate image locally with `.\build.ps1 -Image -ImageTag <tag>` from `server/` ([deployment runbook](../ops/deployment-runbook.md#2-build-and-push-the-image)). The single-node kubeadm cluster shares Docker's image store, so a non-`latest` tag needs no push; a kind-based cluster needs the image loaded into its nodes.
5. Install the CNPG operator at a pinned release (the chart README records 1.25.1 for the kind install) with `kubectl apply --server-side -f <pinned CNPG release manifest>`.
6. Install MinIO (and Azurite when needed) in a separate namespace, and create the source and artifact buckets.
7. Write a local values file outside tracked paths (for example under the ignored `server/scratch/`). It sets `image.repository` and `image.tag` to the local build, `gateway.enabled: false`, `directGrpc.enabled: false`, test-only `staticTokens`, `cell.storage` and the receipt's replica settings. Install with the production profile where the receipt calls for it:

   ```powershell
   helm install munarium deploy/helm/munarium -n <ns> --create-namespace `
     -f deploy/helm/munarium/values-production.yaml -f <local values file>
   ```

   Source-store, builder and admission settings the chart does not expose go through the same post-render overlay as on AKS (see [what the chart provides](#what-the-example-module-and-chart-provide-today)).
8. Reach the planes with `kubectl port-forward` per pod for per-replica checks, and through the Service for load.
9. After the run: `helm uninstall`, delete the namespaces created for it, and remove their volumes. Do not reset the Docker Desktop cluster if it hosts anything else.

### Labelling local evidence

A receipt recorded on L3 uses `environment: L3-docker-desktop` and `evidence_class: local-cluster`, and records the Docker Desktop and Kubernetes versions, provisioner, WSL2 memory and processor limits, and component limits from the sizing table. It is distinct from `evidence_class: release-candidate-cluster`, which is collected on the recorded qualification environment. An L3 receipt run on the exact candidate can satisfy an acceptance ID only where the release owner has recorded that L3 is an accepted environment of record for that ID; otherwise it is rehearsal evidence that fills the hour estimates and de-risks the Azure window. Performance numbers from L3 are always labelled local and never support a capacity claim.

## Pre-flight checklist before any spend

Complete and record every item in the release-owned record (not in this repository) before creating any paid resource. This implements the scheduling requirement in [Qualification environments](README.md#qualification-environments) and the [1.4 release checklist](../guides/server-1.4.md#build-and-release-checklist).

| # | Item | Done when |
|---|---|---|
| 1 | **Code complete.** Every prerequisite ledger row named by the receipts in this window is implemented, reviewed and merged to the qualification branch. | The window's receipt list has no "prerequisite open" entry. |
| 2 | **Candidate frozen.** Server image digest, chart version, Lens image digest and model artifact digests, Matrix peer revision, client package versions and fixture revisions. | All digests recorded; `/version` of a test pull matches. |
| 3 | **Local cluster run passed.** Each receipt ran on L3 (the [workstation cluster](#first-choice-a-local-workstation-cluster)) or its local rehearsal ran to completion on a pre-candidate or candidate build, with its timings recorded. | Rehearsal receipts exist; hour estimates below are filled from them. |
| 4 | **Named owner** for the window and for each receipt, plus a second person who can stop and tear down. | Names recorded privately. |
| 5 | **Cost authority and cap.** Approver, maximum spend for the window **including retries**, and the contingency multiplier used (for example one full retry of the longest receipt). | Signed approval; cap entered in the cost template. |
| 6 | **Availability probe.** Dated check that the region offers every VM size in the topology (including `<gpu-vm-size>`) with sufficient quota, that the images pull, and that the Matrix peer and real MCP clients (if used) start. | Probe date and result recorded; quota increase requests closed. |
| 7 | **Reservation and access expiry.** Start, end and hard deadline for the window, credential and reservation expiry, and the cleanup deadline. | All dates recorded; calendar holds for owner and teardown person. |
| 8 | **Data authority.** For Lens workspace runs, the per-root access scope and processing location authorization, including the separate authorization for the restricted root. | [LENS-D2-T1](repository-handoffs.md#lens-d2-t1) freeze approved. |
| 9 | **Teardown proven.** `terraform destroy` (or resource-group deletion) and namespace deletion have been rehearsed on the local equivalent, and a cost alert or budget on the resource group is configured. | Teardown command and alert threshold recorded. |
| 10 | **Evidence destination.** Where receipts, metric exports and logs go, who can read them, and how restricted material is separated. | Location recorded; redaction reviewer named. |

A missing item means the window is not scheduled. An item that changes after approval (for example a new candidate digest) requires re-approval of items 2, 3 and 5.

## Environment profiles

Every receipt names one of these profiles. Local profiles cost nothing beyond a workstation and are used for rehearsal.

| Profile | Purpose | Topology |
|---|---|---|
| **L1 · compose cluster** | Two-replica Server rehearsals without Kubernetes | `docker compose --profile cluster up` from `server/`: one pgvector PostgreSQL, `munarium-server` and `munarium-server-b`, round-robin Envoy on `:8443`. Add `--profile s3` for MinIO as an authoritative object store. [Cluster guide](../ops/clustering.md#how-to-run-it) |
| **L2 · kind or k3d** | Chart, rollout, drain, mixed-image and Helm upgrade/rollback rehearsals | One control-plane and three workers (k3d agents), CNPG operator, the chart with `gateway.enabled=false` and `directGrpc.enabled=false` as in the chart's [validated kind install](../../deploy/helm/munarium/README.md). Images loaded locally (`kind load docker-image` or `k3d image import`). MinIO in-cluster for the S3 source and artifact store. |
| **L3 · workstation Docker Desktop Kubernetes** | First choice for every Kubernetes receipt; see [first choice](#first-choice-a-local-workstation-cluster) | A maintainer workstation (Windows, 128 GB RAM, Docker Desktop Kubernetes on WSL2), sized as in that section. Every "Local rehearsal" below that names L2 runs on L3 as well. |
| **C1 · Server-only AKS** | Hash-only Server fleet: load, resize, drain, upgrade, rollback, login, artifact block | Derived from the [example AKS module](../../deploy/terraform/example-aks/README.md). See below. |
| **C2 · Server + Lens AKS** | Combined fleet with enrolled collections, placement, workspace | C1 plus a GPU node pool for Lens. See below. |
| **C2R · restricted** | The restricted workspace root only | A separate, authorized environment with a dedicated Lens pool, chosen by the corpus owner. Its details stay inside that boundary. |

### C1 · Server-only topology (smallest that proves the C1 receipts)

| Component | Count and size | Notes |
|---|---|---|
| AKS control plane | 1, `Free` tier as in the example module | Confirm the tier is acceptable for a qualification record; an SLA tier is not needed for test evidence. |
| System pool | 1 × the module default system size | Unchanged from the module. |
| Server pool (`role=server`) | 3 × the module default user size | Two serving replicas on separate nodes plus room for a surge pod, a third replica, or a separate builder group (SCALE-2). Pin with node labels; one Server pod per node during capacity comparisons. |
| Database | CNPG cell, `cell.instances: 2`, on the Server pool or a dedicated `role=db` node if contention distorts SCALE-3-T1 | `max_connections` must exceed the pool math for the peak fleet including surge. Default CNPG value is 100. [Pool math](../ops/clustering.md#pool-math) |
| Object storage | 1 storage account, `sources` container (from the module), plus an artifact container for Datastore L2 | Workload identity, no keys, as in the module. Datastore artifact storage is not wired by the chart or module: to be written. |
| Load generator pool (`role=loadgen`) | 1 node, size chosen so the generator never exceeds about half its CPU at peak | Keeps generator CPU out of the measured nodes, per [measuring performance](../guides/measuring-performance.md#increase-load-without-hiding-overload). |
| Monitoring | Prometheus (or Azure Managed Prometheus) scraping `:9090` via the chart's annotations, plus kube-state-metrics | Needed for `kube_deployment_status_replicas_ready` comparisons. Not in the module: to be written. |
| Provider target | In-cluster counting provider stub | Counts physical provider arrivals per minute window for SCALE-1-T1. To be written. No paid model calls. |

### C2 · Server + Lens topology (smallest that proves the C2 receipts)

| Component | Count and size | Notes |
|---|---|---|
| Everything in C1 | as above | The Server pool can shrink to 2 nodes if SCALE-2 is not rerun in this window. |
| GPU pool (`role=lens`, tainted) | 2 nodes baseline, 3 at peak, `<gpu-vm-size>` from the qualified [Lens hardware profile](repository-handoffs.md#lens-d1-t3) | SC1-T2 needs one→two→one per stage with the other stage held at one, so the peak is 3 Lens workers (2 + 1). If the profile allows embedding and reranking workers to co-locate on one device, record that and use fewer nodes. Scale the pool manually; autoscaling stays off. |
| Dedicated pool for SC2 | 1 of the GPU nodes relabelled `pool=dedicated` for the SC2 run | Reuses C2 hardware; no extra node unless a separate location is part of the policy under test. |
| Model artifact storage | 1 container for verified Lens model assets | Imported and digest-checked, never fetched at serve time. |

The workspace run (LENS-D2) uses C2 with the GPU pool sized from the I3 pilot. Its node count is a forecast, not a fixed number; see [Lens capacity and workspace](#plan-lens).

### What the example module and chart provide today

The [example AKS module](../../deploy/terraform/example-aks/README.md) is authored and syntax-checked but has never been applied end to end; expect a shakedown on first apply and budget an hour for it. It provides a resource group, AKS with a system and one user pool, workload identity, a key-less storage account with a `sources` container, CNPG and Envoy Gateway. For these runs it lacks, and these are **to be written** as an overlay or qualification module (not changes to the example's defaults):

- extra node pools with labels and taints (`server`, `loadgen`, `lens` GPU, optional `db`);
- a Datastore artifact container and its role assignment;
- a CNPG backup target (WAL archive to object storage), needed for any COMPAT-T3 route that restores a backup;
- Prometheus and kube-state-metrics;
- a budget alert on the resource group.

The chart lacks: `nodeSelector`/`affinity`/`tolerations`, `terminationGracePeriodSeconds` and a `preStop` sleep (the [cluster guide](../ops/clustering.md#how-to-run-it) recommends both), an `extraEnv` hook (so `MUNARIUM_TOKEN_SECRET`, `MUNARIUM_DATASTORE_BUILDER` and the admission settings need a post-render patch), a separate builder group (SCALE-2), a second Deployment for held mixed-version replicas, and Lens. Until those land, use a post-renderer overlay kept with the qualification tooling.

## Receipt format

Each receipt follows the [release evidence format](README.md#release-evidence) and the outcome vocabulary of the [validation guide](../guides/validation.md#outcomes-and-receipts). Use this skeleton, one file per acceptance ID and environment.

```yaml
receipt_format: clearview-cluster-1
acceptance_id: SCALE-1-T1            # exactly as in the owning design
environment: C1                      # profile from this page, plus window ID
window: W1-<date>
evidence_class: release-candidate-cluster   # or local-cluster (L1, L2, L3)
outcome: passed | failed | not_run
not_run_reason: null                 # missing_environment | prerequisite_open | cost_cap | ...
candidate:
  server_image: <candidate-digest>
  chart_version: 0.5.x
  lens_image: <lens-digest>          # or null
baseline:
  server_image: <baseline-digest>    # published v1.4.0
  chart_version: <v1.4.0 chart>
configuration:                        # non-secret values only
  values_files: [values-production.yaml, <private values: referenced, not copied>]
  overrides: []                      # every post-render or env override
topology: {nodes: ..., replicas: ..., lens_workers: ..., db: ..., object_store: ...}
fixture_revision: <git sha of fixtures, corpus manifest or inventory>
expected: [...]                      # copied from the acceptance criteria
observed: [...]                      # one entry per expected item, with evidence path
coverage_gaps: [...]                 # anything not exercised, with reason
owner: <role>                         # named privately
evidence_location: <opaque reference>
started_at / finished_at: <UTC>
cost_actual: <from the cost template>
```

Attach raw metric exports, `kubectl` snapshots and logs to the evidence location, not to the repository. Redact before sharing ([validation guide](../guides/validation.md#outcomes-and-receipts)).

## Cost-estimate template

Every receipt below carries a cost table in this form. **No unit price has been verified.** Fill each blank from a dated quote or the provider's price list for `<region>` on the probe date, and record that date.

| Resource | Quantity | Hours | Unit price (per hour or per unit) | Subtotal |
|---|---|---|---|---|
| Example: Server node `<vm-size>` | 3 | ___ | ___ | qty × hours × price |

Window total = sum of receipts' subtotals for resources that are *not* shared + one copy of each shared resource for the window's wall-clock hours. Then:

| Line | Value |
|---|---|
| Sum of resource subtotals | ___ |
| Storage, transactions and egress (per GB or per 10k operations) | ___ |
| Retry contingency (multiplier × longest receipt, agreed in pre-flight item 5) | ___ |
| **Cap requested** | ___ |
| Price source and date | ___ |

Hours come from the local rehearsal timings plus the provisioning and teardown times observed in rehearsal; never from this page.

<a id="plan-scale-1-t1"></a>

## SCALE-1-T1 · Helm replica counts and admissions

**Purpose.** Prove the chart's `providerReplicaCount` renders and behaves as designed on a real cluster: legacy upgrade unchanged, production profile divides provider limits, startup guards fire, surge and rollback behave as documented. [SCALE-1-T1](scale-out.md#scale-1-t1) · [Helm upgrade and rollback](scale-out.md#helm-installation-upgrade-and-rollback) · [COMPAT-T2](README.md#compat-t2)

**Prerequisites.** Ledger row SCALE-1 (code ready at `4d4d653`). The release candidate image. The counting provider stub (to be written). The `platform-v1` case needs the platform profile settings used by the existing `-Platform` tier.

**Topology.** C1 with 2 serving replicas; a third Server node for the surge and builder-divisor cases. No Lens. PostgreSQL ledger and source store (`pg`), then `az` for the shared-storage case.

**Procedure.**

1. Render checks on the exact chart: `py scripts/test_helm_chart.py` from the repository root (exists, 8 cases). Record the output in the receipt.
2. Install the **v1.4.0 chart** with v1.4.0 image and its original values. Record the rendered Deployment (`helm get manifest`).
3. Configure a provider with a finite RPM (for example 60) pointing at the counting stub. Drive an even load above the limit from the load generator for at least three full minute windows. Record per-pod admissions (`munarium_provider_calls_total`) and stub arrivals per window. Expected: each pod admits the full limit (divisor 1).
4. **Upgrade with the key omitted** (`helm upgrade --reuse-values --set image.tag=<candidate>` with the candidate chart). Verify the Deployment env has no `MUNARIUM_REPLICA_COUNT`, `munarium_replica_count` reports 1 on both pods, and admissions match step 3.
5. Apply an external override (`kubectl set env deployment/munarium-server MUNARIUM_REPLICA_COUNT=2`), upgrade again with the key omitted, and verify the override survives.
6. **Migrate to the production profile**: remove the override and upgrade with `-f deploy/helm/munarium/values-production.yaml` as one change. Verify count 2 on both pods, 30 admitted per pod per window at 60 RPM, and the console Health page shows 2.
7. **Explicit integer and mismatch**: upgrade with `providerReplicaCount: 3` while `replicas: 2`. Verify divisor 3 rendered, and record that no warning is produced (the live-count warning is not implemented; ledger row SCALE-3).
8. **Surge**: with the profile and default strategy, trigger a rollout under load. Record the peak live pods and aggregate admissions per window (expected up to 1.5× the limit). Repeat with `strategy.rollingUpdate.maxSurge: 0, maxUnavailable: 1` and verify the aggregate stays within the limit.
9. **Startup guards**: set `providerReplicaCount: 2` with a memory ledger or memory source store via override and verify the pod refuses to start with the documented error. Repeat with `file` source store and verify the warning.
10. **Total connections**: at steady state and during surge, record `munarium_db_pool_max_connections` summed across pods against PostgreSQL `max_connections`.
11. **`platform-v1`**: `replicas: 1`, `providerReplicaCount: 1`, `strategy: {type: Recreate}`. Verify count 2 is refused at startup even with shared storage. Roll the image and verify from pod timestamps that the old pod terminated before the new one started.
12. **Upgrade twice and roll back**: `helm rollback` to the v1.4.0 chart revision. Verify the env entry is gone, both pods return to divisor 1, admissions return to step-3 values, and drain or limit provider traffic during the transition as documented.

**Observations.** Per step: rendered env, `munarium_replica_count` per pod, ready and spec replicas, per-pod and stub-observed admissions per minute window, startup log lines, pool sums, rollout timestamps, Helm revision history.

**Pass/fail.** Every row of the [operator action table](scale-out.md#helm-installation-upgrade-and-rollback) matches its expected result. Any admission total above the documented bound outside the surge case, any startup that should have been refused, or any overlap in `platform-v1` fails.

**Teardown.** `helm uninstall`, delete namespace. Shared C1 window teardown at the end.

**Local rehearsal.** All steps except Azure storage on L2 (kind/k3d) with the stub; steps 1–9 and 11–12 are fully meaningful there. Cloud adds only the `az` store and real-node rollout timing. On L3 this receipt is Full except the identity path; see [decisions](#decisions-that-block-scheduling).

| Resource | Quantity | Hours | Unit price | Subtotal |
|---|---|---|---|---|
| Server node | 3 | ___ | ___ | ___ |
| System node | 1 | ___ | ___ | ___ |
| Load generator node | 1 | ___ | ___ | ___ |
| CNPG volumes (`cell.storage` × instances) | 2 | ___ | ___ | ___ |
| Blob storage and transactions | — | — | ___ | ___ |

<a id="plan-scale-3-t1"></a>

## SCALE-3-T1 · Diagnostics against real bottlenecks

**Purpose.** Drive a known build-queue and DB-pool bottleneck, resize, and compare operator-visible counts with actual fleet and database observations. [SCALE-3-T1](scale-out.md#scale-3-t1) · [Replica, pool and queue diagnostics](../ops/clustering.md#replica-pool-and-queue-diagnostics)

**Prerequisites.** Ledger row SCALE-3 (code ready at `273ac32`). For a realistic queue, ledger rows R4 (implemented), R6, R8 and R9 (planned) should be complete so the run measures the final claim path. Prometheus and kube-state-metrics in C1.

**Topology.** C1: 2 serving replicas, `MUNARIUM_DB_MAX_CONNS` lowered (for example to the floor of 2) for the pool case, one builder enabled. Synthetic corpus from the repository's existing generators (for example the 400-document benchmark loader in [measuring performance](../guides/measuring-performance.md#11-use-the-repository-benchmark-for-the-question-it-answers)) scaled up as needed.

**Procedure.**

1. Baseline: idle fleet. Record `/metrics` on each pod, `GET /v1/reports/scale` via each pod (port-forward per pod), console Health page, `kubectl get deploy`, and `SELECT state, count(*) FROM index_build_jobs GROUP BY state`.
2. **Queue bottleneck**: enqueue N build jobs (N ≫ builders) for a fixed corpus. Every minute, record `munarium_index_build_jobs{state}`, `munarium_index_build_oldest_pending_seconds`, the report backlog, and the SQL count. Expected: `max` across pods equals the SQL count within one scrape interval; oldest pending age grows.
3. Resize builders from 1 to 2 (on the same group until SCALE-2 exists). Verify `running` reaches 2 and the oldest pending age slope changes.
4. **Pool bottleneck**: with the lowered pool, drive concurrent queries. Record `munarium_db_pool_connections - _idle` against `_max_connections`, `munarium_admission_slots_in_use`, `munarium_admission_shed_total`, and `pg_stat_activity` per application name.
5. Resize serving replicas 2 → 3 and back. Compare `munarium_replica_count` (configured), `kube_deployment_status_replicas_ready`, `count(munarium_build_info)` and the console statement that membership is unknown.
6. **Failed read**: revoke the DB role's `SELECT` on `index_build_jobs` (or block the DB briefly with a network policy) and verify `munarium_index_build_queue_up 0`, counts omitted, report and console show `unavailable`, never 0.

**Observations.** Time series for every metric listed, the SQL ground truth at the same instants, resize timestamps.

**Pass/fail.** Each operator-visible count matches the ground truth within one scrape interval at steady state; the failed read is never shown as 0; labels contain no tenant, path or query content. Any mismatch beyond the interval fails.

**Teardown.** Restore pool and role settings; namespace deletion at window end.

**Local rehearsal.** Steps 1–6 on L2 (or L1 for steps 1–4 and 6). Cloud is needed only for representative capacity numbers and real-node resize timing.

| Resource | Quantity | Hours | Unit price | Subtotal |
|---|---|---|---|---|
| Server node | 3 | ___ | ___ | ___ |
| Load generator node | 1 | ___ | ___ | ___ |
| Monitoring (managed or in-cluster storage) | 1 | ___ | ___ | ___ |

<a id="plan-scale-2-t1"></a>

## SCALE-2-T1 · Add and drain a separate builder

**Purpose.** Add and drain a builder in its own group with work in flight; verify fencing and reclaim, continued foreground service and sufficient staging space. [SCALE-2-T1](scale-out.md#scale-2-t1)

**Prerequisites.** Ledger row SCALE-2 (planned: chart builder group outside the gateway selector), R4 (implemented), R6 cooperative cancellation and R9 fenced progress (planned). Chart support for a builder-only group and the `auto` divisor counting it.

**Topology.** C1: 2 serving replicas, builder group 0 → 1 → 2 → 1 on the third Server node (one builder pod per node where the staging disk is measured). Object store for artifacts.

**Procedure.**

1. With 2 serving and 0 separate builders, verify the serving group builds (or not) as configured and record the divisor (2).
2. Add 1 builder. Verify the divisor becomes 3 on all processes (`auto`), the builder is not in the Service endpoints (`kubectl get endpoints munarium-server`), and it claims jobs.
3. Enqueue long builds. While a job is running, drain the builder (`kubectl delete pod` with grace, then scale to 0). Verify the lease lapses, another builder reclaims under a new generation, the retired holder writes nothing after reclaim (inspect `index_build_attempts`, `index_build_publication_intents` with the queries in the [datastore guide](../guides/datastore.md#mixed-version-builders-drain-before-switching)), and the job completes once.
4. Kill a builder (`--grace-period=0 --force`) mid-publication and verify the reconciler finishes or fails the intent without binding a retired generation.
5. Throughout, run foreground queries from the load generator and record p95 latency and refusals.
6. Record staging disk use per builder at peak and compare with the node's ephemeral storage.

**Observations.** Job, attempt and intent rows before and after each drain; endpoints; divisor per pod; foreground latency and refusals; staging disk.

**Pass/fail.** No write by a retired generation; every job completes exactly once or fails with a classified error; foreground SLO and refusals stay within the pre-declared bound; staging never exhausts the disk.

**Teardown.** Scale builder group to 0, cancel outstanding jobs, namespace deletion at window end.

**Local rehearsal.** Fully on L2 (kind) once the chart group exists; fencing behavior is already covered by PostgreSQL tests. Cloud adds real staging disk and object-store transfer under load.

| Resource | Quantity | Hours | Unit price | Subtotal |
|---|---|---|---|---|
| Server node | 3 | ___ | ___ | ___ |
| Load generator node | 1 | ___ | ___ | ___ |
| Artifact storage, transactions | — | — | ___ | ___ |

<a id="plan-sc1-t1"></a>

## SC1-T1 · Server resize with Lens disabled

**Purpose.** Resize Server 1 → 2 → 1 ready replicas under sustained traffic with v1.4 clients replaying. [SC1](scale-out.md#sc1) · [SC1-T1](scale-out.md#sc1-t1) · [Upgrade baseline](scale-out.md#qualification-and-rollback)

**Prerequisites.** Ledger rows SCALE-1 and SCALE-3 (code ready), R7 (implemented, opt-in), SC1. A load profile and v1.4 client replay driver (to be written). The chart `preStop` and termination grace (to be written, or post-render).

**Topology.** C1, Lens disabled, production profile, 1 → 2 → 1 serving replicas. Released v1.4 clients (Rust, Python, .NET, Java and `mmctl`) run from the load generator node.

**Procedure.**

1. Start at 1 replica. Run the fixed foreground mix (queries, ledger writes, report reads) at a constant arrival rate, plus the v1.4 client replay in a loop.
2. Scale to 2 (`kubectl scale` or Helm `replicas: 2`). Record when the new pod becomes ready, when it receives traffic, and requests served per pod.
3. Hold for a steady-state interval. Record successful operations per second, p95/p99, refusals, DB connections.
4. Scale to 1. Record that the retiring pod's `/readyz` flips to `draining`, in-flight requests complete within `MUNARIUM_SHUTDOWN_GRACE_SECS`, and no client sees a connection reset outside the documented bound.
5. Compare v1.4 client results (stable fields, refusals, citations, index identities) with the pre-resize baseline.

**Observations.** Per-pod request counts, readiness transitions, client error log, latency histograms, DB connections, provider arrivals (should be zero with Lens disabled and no provider in the mix).

**Pass/fail.** Both replicas serve after scale-up; the retiring replica stops admitting, drains, and the survivor serves correct results; v1.4 client results unchanged; no unaccounted errors.

**Teardown.** Return to the window's baseline replica count.

**Local rehearsal.** L2 for the full sequence; L1 for the client replay. Cloud supplies the sustained-load numbers on representative nodes.

| Resource | Quantity | Hours | Unit price | Subtotal |
|---|---|---|---|---|
| Server node | 2 | ___ | ___ | ___ |
| Load generator node | 1 | ___ | ___ | ___ |

<a id="plan-sc1-t2-t3"></a>

## SC1-T2 and SC1-T3 · Combined Server and Lens fleet

**Purpose.** Vary Server with Lens steady, then vary embedding and reranking workers separately with Server steady; then replace a worker mid-request, try an incompatible profile and remove the last ready member of a stage. [SC1-T2](scale-out.md#sc1-t2) · [SC1-T3](scale-out.md#sc1-t3) · [I1](airk-analysis.md#i1) · [I2](airk-analysis.md#i2)

**Prerequisites.** Ledger rows IR-0 to IR-4, I1, I2 (planned), LENS-1 and LENS-2 (planned: working image and qualified profile), SC1. Enrolled collections with approved semantic indexes (reuse the LENS-D2-T2 builds of public roots when run in the same window). The Lens chart or manifests (to be written in Lens).

**Topology.** C2: 2 serving Server replicas; Lens embedding and reranking pools at 1 each, then one→two→one per stage (peak 3 workers). Tenant budgets configured with known allowances.

**Procedure.**

1. **Upgrade baseline.** With Lens disabled, run the v1.4 replay and record results. Enable the qualified profile and enroll the qualification collections one change at a time.
2. **Server varies (Lens 1/1).** Server 2 → 3 → 2 under the foreground mix plus one enrolled rebuild.
3. **Embedding varies (Server 2, rerank 1).** 1 → 2 → 1. Record requested and ready counts, observed work per worker, queue wait vs inference time, query latency, refusals, budget reservations.
4. **Reranking varies (Server 2, embed 1).** Same as step 3.
5. **Limiting-tier comparison** (supporting, not a pass condition): 1S/1L, 2S/1L, 1S/2L, 2S/2L per stage on the same corpus and offered load, as in [qualification and rollback](scale-out.md#qualification-and-rollback).
6. **SC1-T3.** Delete a worker pod mid-build and mid-query. Start a worker with a mismatched profile and verify it never enters rotation. Scale a stage to 0 and verify the configured I1 outcome (`require` refusal, or disclosed fallback under `prefer`), I2 accounting with zero outbound calls after denied admission, and SC2 placement. Restore capacity.
7. **Restore.** Scale to the qualified fixed counts, restore each collection's prior approved index and policy, replay baseline queries and resolve citations.

**Observations.** Per transition: requested/ready per stage, per-worker request counts, in-flight outcomes, p95/p99, refusals by reason, reservations and reconciled usage per attempt, citation resolution results.

**Pass/fail.** As in the SC1-T2 and SC1-T3 rows: every transition shows the listed observations; serving pins and citations survive drains; I1, I2 and SC2 hold through failure and recovery; baseline replay matches after restore.

**Teardown.** Scale GPU pool to 0 immediately after the last C2 receipt; it is the dominant cost.

**Local rehearsal.** The control flow (routing, readiness, drain, refusal, accounting) can be rehearsed on L1 or L2 with a Lens protocol stub that returns deterministic embeddings and permutations (to be written; the Lens CPU scaffold currently returns typed 503 only). Capacity numbers require the GPU profile.

| Resource | Quantity | Hours | Unit price | Subtotal |
|---|---|---|---|---|
| Server node | 2–3 | ___ | ___ | ___ |
| GPU node `<gpu-vm-size>` | 2 baseline, 3 peak | ___ | ___ | ___ |
| Load generator node | 1 | ___ | ___ | ___ |
| Model and artifact storage | — | — | ___ | ___ |

<a id="plan-sc2"></a>

## SC2 · Shared and dedicated Lens pools

**Purpose.** Route approved tenants through a shared worker, refuse forbidden placement, and handle loss of a dedicated pool. [SC2](repository-handoffs.md#sc2) · [SC2-T1](repository-handoffs.md#sc2-t1) · [SC2-T2](repository-handoffs.md#sc2-t2) · [SC2-T3](repository-handoffs.md#sc2-t3)

**Prerequisites.** Ledger row SC2 (planned; hub record pending for the shared placement contract), I1, I2, LENS-1. Three synthetic tenants: two approved for sharing, one dedicated-only. A per-worker request recorder (Lens access log without content, or a sidecar counter).

**Topology.** C2 GPU pool split: 1 worker `pool=shared`, 1 worker `pool=dedicated`. Server 2 replicas. Runs in the same window as SC1-T2/T3, after them.

**Procedure.**

1. **SC2-T1.** Submit identical synthetic input from both approved tenants. With caching enabled, show independent cache entries and expiry per tenant retention; disable caching and show no entry stored. Exhaust one tenant's budget and show zero inference calls for its denied request while the other continues.
2. **SC2-T2.** Remove approval; set incompatible location and retention rules; use the dedicated-only tenant. On serving, build, retry and shadow paths, assert the forbidden worker's recorder shows zero requests.
3. **SC2-T3.** Drain the dedicated worker with the shared worker ready. Verify the configured refusal or authorized fallback, zero dispatch to the shared pool, and recovery when the dedicated worker returns.

**Observations.** Per-worker request counts by tenant (from Server's audit, not labels on Lens metrics), cache entries, budget reservations, refusal reasons.

**Pass/fail.** Zero content reaches a forbidden worker on every path; each tenant's budget and cache isolation holds; the unaffected tenant continues.

**Teardown.** Remove tenants and policies; GPU pool scaled to 0 at window end.

**Local rehearsal.** All of SC2's decision logic can run on L1/L2 against two instances of the Lens protocol stub labelled shared and dedicated (to be written). The cloud receipt then confirms it on the qualified image.

| Resource | Quantity | Hours | Unit price | Subtotal |
|---|---|---|---|---|
| GPU node `<gpu-vm-size>` | 2 | ___ | ___ | ___ |
| Server node | 2 | ___ | ___ | ___ |

<a id="plan-compat-t2"></a>

## COMPAT-T2 · Chart upgrade and rollback

**Purpose.** Upgrade a v1.4 chart with its original values, then exercise explicit replica-count migration and rollback, and show existing CLI, SDK and Matrix configuration keeps its effect. [COMPAT-T2](README.md#compat-t2) · [M1-T2](repository-handoffs.md#m1-t2)

**Prerequisites.** Ledger rows SCALE-1 (code ready), COMPAT-T2, M1 step 3 (pending: switched Server consumer) for the Matrix-enabled case. A pinned real Matrix peer image. Released 1.4 client packages and `mmctl`.

**Topology.** C1 (or L2, see decisions), 2 replicas, Matrix peer installed as its own release in the same cluster with `matrix.baseUrl` and `matrix.adminUrl` set for the second pass.

**Procedure.**

1. Install the v1.4.0 chart and image with original values, standalone. Seed data with the v1.4 clients (shared with COMPAT-T3 seeding).
2. Run the released Rust, Python, .NET and Java 1.4 conformance suites and `mmctl` command set against it. Record outputs and exit codes as the baseline.
3. Upgrade to the candidate chart and image with original values (`--reuse-values`). Repeat step 2 and compare.
4. Run candidate clients' 1.4 operations against a separate v1.4 release (both directions).
5. Perform the replica-count migration and rollback steps (shared with SCALE-1-T1 steps 6 and 12; record here as COMPAT-T2 evidence).
6. Repeat 1–5 with Matrix configured, including `mmctl matrix` verbs, `/admin/matrix` and `munarium_matrix_provider_requests_total` outcomes.

**Observations.** Per client and command: output, exit code, stable fields; rendered values diff; Helm history.

**Pass/fail.** Every v1.4 script, client and value retains its effect; any changed exit code, field or configuration meaning fails unless it is a documented, reviewed COMPAT exception (for example the U4 console revoke change, recorded under COMPAT-T5).

**Teardown.** Uninstall both releases and Matrix.

**Local rehearsal.** Fully on L2; client suites already run locally per the [clients README](../../../clients/README.md). Cloud adds the `az` source store and workload identity path only.

| Resource | Quantity | Hours | Unit price | Subtotal |
|---|---|---|---|---|
| Server node | 2 | ___ | ___ | ___ |
| Load generator node (clients) | 1 | ___ | ___ | ___ |

<a id="plan-compat-t3"></a>

## COMPAT-T3 · Mixed replicas and binary rollback

**Purpose.** Seed a disposable PostgreSQL cell and object store with v1.4, upgrade in place, read and write with both replica versions, then roll back the binary and re-read. [COMPAT-T3](README.md#compat-t3)

**Open decision (blocks scheduling).** Migration 0043 (R4 publication fencing) is on the branch. A v1.4.0 binary refuses to start against a database that has it (`MigrateError::VersionMissing`), so the in-place binary rollback COMPAT-T3 describes cannot pass against v1.4.0 as written. [Ledger: migrations and binary rollback](implementation-ledger.md#next-implementation-steps) · [Datastore drain and rollback](../guides/datastore.md#mixed-version-builders-drain-before-switching). The procedure branches on the decision:

| Route | What "binary rollback" means in the run | Extra prerequisites | Effect on the receipt |
|---|---|---|---|
| A · qualified v1.4.x with `ignore_missing` | Replace v1.5 pods with v1.4.x pods against the upgraded database | A separately qualified v1.4.x release ([maintenance releases](README.md#release-evidence)) | Baseline for rollback is v1.4.x, not v1.4.0; record both. v1.4.0 mixed replicas are impossible after 0043 applies. |
| B · restore a pre-upgrade backup | Stop v1.5, restore the backup taken before upgrade, start v1.4.0 | CNPG WAL archive and backup to object storage (to be written), the [backup/restore runbook](../ops/backup-restore.md) | Writes made under v1.5 are lost by design; the "re-read" step re-reads v1.4-written data only. COMPAT-T3's wording needs a recorded amendment. |
| C · no new migrations in v1.5 | In-place binary swap | Rework of R4 storage, already merged on the branch | Contradicts the current branch; listed for completeness. |

**Mixed replicas.** Even under route A, a v1.4.0 replica cannot start once 0043 is applied. The mixed-replica part of COMPAT-T3 is therefore: v1.4.x (route A) with v1.5, or v1.4.0 with v1.5 only *before* the first v1.5 pod applies migrations, which is not possible because migrations run at startup. Record which is exercised. The decision record must state this.

**Prerequisites.** The rollback decision recorded. Ledger rows R4 (implemented), COMPAT-T3, the Phase 1 [fixtures](../../tests/fixtures/clearview-phase1/README.md). Sealed evidence and a pinned session in the seed. For route B, a configured and drilled backup.

**Topology.** C1 or L2, 2 replicas, `az` (C1) or S3/MinIO (L2) source and artifact store, CNPG with backup target for route B.

**Procedure (route-independent part).**

1. Install v1.4.0, seed with v1.4 clients: ledger writes, sealed evidence, a pinned session, an index build with Datastore artifacts, runbook runs. Snapshot stable reads as the baseline.
2. For route B, take and verify the backup now.
3. Drain v1.4 builders per the [datastore procedure](../guides/datastore.md#mixed-version-builders-drain-before-switching).
4. Roll one replica to the candidate (held mixed replicas need a second Deployment: to be written). Read and write through both versions where the route allows; compare with baseline.
5. Complete the upgrade; enable fenced builders; read and write.
6. Roll back per the selected route; re-read; compare old indexes, hashes, approvals, provenance, session pins and evidence IDs with the baseline.
7. Exercise the documented memory-store surface separately (local only; no cluster needed).

**Observations.** Startup logs per pod (including the expected `VersionMissing` negative control on v1.4.0 for routes A and B), `_sqlx_migrations` contents, baseline diffs, drain query results.

**Pass/fail.** All baseline reads identical after rollback under the selected route's stated scope; no data loss beyond what the decision explicitly accepts.

**Teardown.** Drop the cell and containers; delete backups taken for the run.

**Local rehearsal.** Fully on L1 (compose, with MinIO) or L2; the `VersionMissing` negative control should be rehearsed locally first because it costs nothing and confirms the decision is needed.

| Resource | Quantity | Hours | Unit price | Subtotal |
|---|---|---|---|---|
| Server node | 2 | ___ | ___ | ___ |
| CNPG volumes | 2 | ___ | ___ | ___ |
| Backup storage (route B) | — | — | ___ | ___ |

<a id="plan-login1"></a>

## LOGIN1 · Mixed-replica console login cutover

**Purpose.** Cutover is refused with a v1.4 replica or unknown peer; after an explicit switch on a capable fleet, sessions work across replicas, revoke everywhere, expire, and roll back cleanly. [LOGIN1](airk-analysis.md#login1) · [LOGIN1-T1](airk-analysis.md#login1-t1) · [LOGIN1-T2](airk-analysis.md#login1-t2) · [REL1](README.md#rel1)

**Prerequisites.** Ledger row U10 · LOGIN1 (planned) and its open item for the shared session store ([open items](README.md#open-items)). Held mixed-version replicas (second Deployment, to be written). A browser or HTTP driver that follows CSRF tokens and pins requests to a chosen pod (to be written; port-forward per pod is sufficient).

**Topology.** 2–3 serving replicas: one v1.4 (or the COMPAT-T3 route's rollback binary), the others candidate. TLS termination at the gateway for `Secure` and `__Host-` cookie checks.

**Procedure.**

1. **LOGIN1-T1.** With one v1.4 replica serving console traffic, request cutover: expect refusal, legacy login continues. Make one replica's capability unknown (network-policy block of its capability report): expect refusal. Upgrade the last replica: verify mode unchanged. Then cut over explicitly.
2. **LOGIN1-T2.** Sign in through pod A; perform a CSRF-protected action through pod B; revoke on A; verify both reject the handle. Test tenant isolation, idle expiry on each pod, a denied role, and a rolling restart with a live session.
3. **Rollback.** Switch back to legacy: new handles invalid everywhere; fresh legacy login works.
4. Repeat existing v1.4 bookmarks, view-only proxy and token automation throughout (COMPAT-T5 evidence).

**Observations.** HTTP transcripts (cookies redacted), cutover preflight responses, session store rows (counts, not handles).

**Pass/fail.** Exactly the LOGIN1-T1/T2 outcomes; any acceptance of a revoked or rolled-back handle fails.

**Teardown.** Clear the session store; namespace deletion at window end.

**Local rehearsal.** Fully on L1 (two compose instances with different images) or L2. Cloud adds the real gateway TLS path. Run in the same window and on the same candidate as ART1/ART2 for REL1.

| Resource | Quantity | Hours | Unit price | Subtotal |
|---|---|---|---|---|
| Server node | 2–3 | ___ | ___ | ___ |
| Public IP and load balancer for gateway TLS | 1 | ___ | ___ | ___ |

<a id="plan-art"></a>

## ART1 and ART2 · Fleet block and verified repair

**Purpose.** Two independent authoritative mismatches block a pin fleet-wide; cold, warm, restarted and in-flight readers honor it; a preauthorized repair clears it only after two fresh independent matches. [ART1](airk-analysis.md#art1) · [ART1-T1](airk-analysis.md#art1-t1) · [ART1-T2](airk-analysis.md#art1-t2) · [ART2](airk-analysis.md#art2) · [ART2-T1](airk-analysis.md#art2-t1) · [ART2-T2](airk-analysis.md#art2-t2) · [REL1](README.md#rel1)

**Prerequisites.** Ledger rows R10, ART1, ART2 (planned) and the ART1 hub decision on verifier identities and independence. Two verifier identities with separate credentials. A fault-injection tool that replaces an object's bytes in the authoritative store and can pause a reader between admission and release (to be written; likely a test-only build hook).

**Topology.** 3 serving replicas (cold, warm, restart roles), Datastore serving from the object store, two verifier processes on separate nodes with separate workload identities.

**Procedure.**

1. Build and promote an index; warm two replicas, leave one cold.
2. **ART1-T1.** Corrupt the local cache copy only: local isolation, clean authoritative re-fetch, no fleet block. Then each insufficient case: one verifier, the same verifier twice, two names for the same fetch, a timeout, different pins. Expect no fleet block in each.
3. **ART1-T2.** Corrupt the authoritative object. Two independent verifiers report mismatches: block commits. Verify cold, warm and restarted replicas refuse. Pause a reader after admission, commit the block, resume: refused. Reverse the order: only the authorized result delivered. Stream spanning the block. Unavailable final check. Different tenant with identical bytes. Full observation history.
4. **ART2-T1/T2.** Repair the object. Duplicate, pre-repair, wrong-pin, timeout and same-verifier observations leave the block. Two fresh independent matches clear it atomically with audit. Concurrent failure, repeated workflow delivery, restart, late report from the earlier incident. Quarantined copies reverified before admission. Fresh mismatch starts a new incident.
5. **Binary rollback with an outstanding block** under the COMPAT-T3 route.

**Observations.** Catalog block rows and generations, verifier observations, per-replica refusal responses, stream event logs, audit entries.

**Pass/fail.** Exactly the ART1/ART2 outcomes; any serving of a blocked pin, or a clearance without two fresh independent matches, fails.

**Teardown.** Delete the corrupted objects and the test tenant; namespace deletion at window end.

**Local rehearsal.** Fully on L1 with MinIO, or L2. Cloud adds the Azure Blob path with workload-identity verifiers.

| Resource | Quantity | Hours | Unit price | Subtotal |
|---|---|---|---|---|
| Server node | 3 | ___ | ___ | ___ |
| Blob storage and transactions | — | — | ___ | ___ |

<a id="plan-lens"></a>

## LENS-D1 hardware, I3 and LENS-D2 · Lens capacity and workspace

**Purpose.** Qualify the Lens image on named hardware, approve the profile per corpus family, and run build, query, citation and rollback across all 16 workspace roots. [LENS-D1-T2](repository-handoffs.md#lens-d1-t2) · [LENS-D1-T3](repository-handoffs.md#lens-d1-t3) · [I3](airk-analysis.md#i3) · [I3-T1](airk-analysis.md#i3-t1) · [I3-T2](airk-analysis.md#i3-t2) · [LENS-D2](repository-handoffs.md#lens-d2) · [Workspace inventory](repository-handoffs.md#workspace-inventory)

**Prerequisites.** Ledger rows LENS-1, LENS-2, LENS-3, IR-0 to IR-4, I1, I2, I3 (all planned), LENS-D2-T1 (frozen inventory and restricted-root authorization). Frozen I3 thresholds, judgments and query sets before the run ([frozen evaluation](../guides/frozen-evaluation.md)). The manifest generator for the freeze procedure (to be written; the procedure is specified in [workspace inventory](repository-handoffs.md#workspace-inventory)).

**Topology.**

| Stage | GPU nodes | Server | Notes |
|---|---|---|---|
| Hardware probe and pilot (LENS-D1-T2/T3, I3 pilot) | 1 × `<gpu-vm-size>` | 1 replica | Measures throughput, memory and token volume on a representative sample to forecast GPU-hours for the full run. |
| I3 evaluation | 1–2 | 2 | Fixed profile; both families; four arms (hash, embedder, reranker, combined). |
| LENS-D2 full | forecast from pilot | 2 + 1 builder | Public roots in C2; restricted root in C2R. |

**Procedure.**

1. Pilot: start the Lens image with verified assets, run the candidate Server's embedding and rerank probes, record image digest, model and tokenizer revisions, dimensions, precision, driver. Ingest a representative sample from each family and measure tokens per second, peak device memory, end-to-end build time. Exercise oversize input, failed model load and timeout.
2. Compute the forecast (`F` files, source bytes, chunks, tokens, measured throughput) as in [Qualification environments](README.md#qualification-environments). Re-approve cost before step 3.
3. I3: run the frozen query sets per family against the four arms; record recall@k, nDCG@k, p95, failures, authorization outcomes. Use `scripts/frozen_eval.py` (exists) for the record format.
4. LENS-D2-T2: build each public root's baseline and Lens-backed index side by side; verify; record per-root results.
5. LENS-D2-T3: enroll; fresh sessions; content-derived queries covering every content-bearing file; resolve citations.
6. LENS-D2-T4: switch back to prior indexes and policies; with Lens unavailable, replay baselines and resolve citations.
7. Restricted root in C2R under its owner's authorization; only an opaque receipt reference leaves that environment.

**Observations.** Per root and per file as in the [release manifest record](repository-handoffs.md#workspace-inventory); I3 family receipts; device memory and throughput.

**Pass/fail.** I3: both family receipts pass their predeclared thresholds for the same profile. LENS-D2: every stage passes for every one of the 16 roots; a missing or partial root keeps the gate open.

**Teardown.** Delete indexes, collections and corpus copies; scale GPU pool to 0; destroy C2 if no later receipt needs it. C2R teardown follows its owner's retention rules.

**Local rehearsal.** The manifest freeze, ingestion accounting, query generation, citation resolution and rollback logic can run on L1 against the hash baseline and the Lens protocol stub over a small public subset (for example two roots). Only quality, latency and capacity need the GPU.

| Resource | Quantity | Hours | Unit price | Subtotal |
|---|---|---|---|---|
| GPU node (pilot) | 1 | ___ | ___ | ___ |
| GPU node (I3 and LENS-D2) | forecast | ___ | ___ | ___ |
| Server and builder nodes | 3 | ___ | ___ | ___ |
| Corpus, index and artifact storage | ___ GB | ___ | ___ | ___ |
| Restricted environment (C2R), separately approved | ___ | ___ | ___ | ___ |

<a id="plan-mcp"></a>

## Multi-replica MCP cases

[A5-T3](mcp-analysis.md#a5-t3), [A5-T5](mcp-analysis.md#a5-t5), [A6-T2](mcp-analysis.md#a6-t1) and the two-replica races in [A8-T1](mcp-analysis.md#a8-t1) and [A9-T2](mcp-analysis.md#a9-t2) need two replicas and a durable store, not cloud hardware. Run them on L1 or L2 with a recording provider. Real-client transcripts ([A3](mcp-analysis.md#a3)) need the clients, not a cluster. If the release owner wants them on the same candidate deployment as the cluster receipts, add them to window W1 at the cost of its Server nodes' hours only. Prerequisites: ledger Phase 5 rows (planned).

## Minimum-spend ordering

### Principles

1. **Local workstation cluster first.** Run every Kubernetes receipt on L3, the [workstation cluster](#first-choice-a-local-workstation-cluster), on the candidate (or a pre-candidate build for rehearsal). Use L1 for compose-only rehearsals. Record wall-clock timings; they fill the "Hours" blanks.
2. **Azure covers only the residual.** A paid window includes a receipt, or a part of one, only when the [L3 table](#what-one-docker-desktop-node-can-prove) marks it Partial or Azure, or when the release owner has not accepted L3 as an environment of record for it.
3. **Group by hardware, not by phase.** Server-only residuals share one CPU window. GPU receipts share one GPU window. Never hold a GPU node while running CPU-only receipts.
4. **Order within a window from least to most destructive.** Seed once, upgrade once, run non-destructive receipts, roll back last.
5. **Reuse state.** One v1.4 seed feeds COMPAT-T2, COMPAT-T3, SC1-T1 and the LOGIN1 legacy baseline. The LENS-D2 public-root builds become the enrolled collections for SC1-T2/T3 and SC2.
6. **Tear down between windows** unless the next window starts within the hour it would take to re-provision.

### Recommended windows

| Window | Profile | Receipts, in run order | Smallest topology | Spend |
|---|---|---|---|---|
| **W0 · workstation cluster** | L3 (and L1) | 1. Install v1.4 and seed. 2. COMPAT-T2 client baseline. 3. Upgrade: COMPAT-T2 compare, SCALE-1-T1 steps 2–10. 4. SC1-T1 (pod-level). 5. SCALE-2-T1. 6. SCALE-3-T1. 7. LOGIN1-T1/T2. 8. ART1/ART2. 9. COMPAT-T3 mixed and rollback (with the `VersionMissing` negative control), SCALE-1-T1 rollback, COMPAT-T2 rollback, LOGIN1 rollback, ART rollback with outstanding block. 10. `platform-v1` case (fresh namespace). 11. Multi-replica MCP cases. 12. SC1-T2/T3 and SC2 logic with Lens CPU replicas or stub. 13. LENS-D2 pipeline on a small public subset. | One workstation, sized as in [sizing](#sizing-within-128-gb) | None |
| **W1 · Azure Server-only residual** | C1 | Only what W0 could not prove: SC1-T1 node drain and node failure; `az` store with workload identity smoke for SCALE-1-T1, COMPAT-T2, COMPAT-T3 and ART verifiers; gateway TLS for LOGIN1; SCALE-2-T1 staging on managed disks; SCALE-3-T1 and SC1-T1 capacity numbers only if a capacity claim is to be published. Same destructive-last order as W0. | 1 system, 2–3 Server, 1 load generator node; CNPG 2 instances | Capped |
| **W2 · GPU pilot** | C2 (1 GPU node) | LENS-D1-T2, LENS-D1-T3, I3 pilot sizing | 1 GPU, 1 Server node, system node | Capped |
| **W3 · Combined fleet and workspace** | C2 | 1. I3 evaluation. 2. LENS-D2-T2 public roots. 3. LENS-D2-T3. 4. SC1-T2. 5. SC1-T3. 6. SC2-T1/T2/T3 with dedicated nodes. 7. LENS-D2-T4 rollback (last, it removes the enrolled state the others use). | GPU pool from W2 forecast (minimum 2, peak 3 for SC1-T2); 2 Server + builder; 1 load generator | Capped |
| **W3R · restricted** | C2R | LENS-D2 for the restricted root only | Owner-approved | Separately approved |

W1 can be dropped entirely if the release owner accepts L3 receipts for the Server-only IDs and records the Azure identity and managed-storage paths as covered elsewhere (for example by the example module's first end-to-end apply) or as a stated coverage gap. That is the largest single saving available. W1 and W3 can also share one provisioning if they are scheduled back to back: run the W1 residual on the C2 Server nodes before scaling up the GPU pool.

W2 is deliberately small and separate: its forecast sets the W3 budget. If the pilot can run on hardware the Lens maintainers already have access to without cost, W2 has no cloud spend.

### Rehearsal before W0

Where the code for a receipt is not yet complete, the parts that exist can still be exercised on L1 or L2 (kind or k3d) in CI-like disposable runs. These are rehearsals, not receipts, and they need no workstation time.

### Local rehearsal tooling

| Need | Exists | To be written |
|---|---|---|
| Two Server replicas on one PostgreSQL | `docker compose --profile cluster up` ([compose file](../../docker-compose.yml)); `./test.ps1 -Cluster` runs `mmp-conformance --cluster` coordination checks | Variant with two different images (v1.4 and candidate) for mixed-replica rehearsals |
| Local S3 object store | `docker compose --profile s3 up minio` | — |
| Single-host stack with persistent volumes | `server/localdeploy.ps1` | — |
| Chart render checks | `scripts/test_helm_chart.py` (8 cases, byte comparison with v1.4.0) | — |
| Workstation Docker Desktop Kubernetes (L3) | Docker Desktop's built-in cluster; the chart, CNPG and MinIO install manually as in [steps](#steps) | A bootstrap script for L3 (CNPG at a pinned version, MinIO and Azurite, local values template, post-render overlay, teardown) and a receipt template with the `local-cluster` fields |
| kind or k3d cluster with CNPG, local image load and MinIO | The chart was installed on kind once (chart README), manually | A bootstrap script (create cluster, install CNPG operator, load images, install MinIO, install chart) and matching teardown |
| Chart features for qualification | — | `nodeSelector`/affinity/tolerations, termination grace and `preStop`, `extraEnv`, builder group (SCALE-2), second Deployment for held mixed versions; until then a post-render overlay |
| v1.4 client and CLI conformance | Four SDK conformance suites and `mmp-conformance` ([scenarios](../../conformance/SCENARIOS.md)); Phase 1 job and console fixtures | A driver that runs the *released* 1.4 packages against an arbitrary endpoint and diffs stable fields against a saved baseline |
| Load generator | — (the [performance guide](../guides/measuring-performance.md#increase-load-without-hiding-overload) recommends an arrival-rate tool such as k6) | Workload scripts for the foreground mix and builds, with scheduled versus started counts |
| Provider arrival counting | Ollama compose in `server/deploy/ollama` for a real local provider | A counting OpenAI-compatible stub that reports arrivals per minute window |
| Lens stand-in | Lens CPU scaffold (typed 503 only) | A Lens protocol stub returning deterministic embeddings and permutations, with per-instance identity and request counters, for routing, placement and outage rehearsals |
| Artifact fault injection | — | Object corruption helper and a test-only pause hook between admission and release |
| Console session driver | — | HTTP driver that follows CSRF tokens and pins requests to a chosen replica |
| Evidence collection | `tools/check_validation_receipt.py` for local runner receipts; `scripts/frozen_eval.py` for I3 records | A collector that snapshots `kubectl`, `/metrics`, `/v1/reports/scale` and SQL ground truth into the receipt format above |
| Cloud environment | [example AKS module](../../deploy/terraform/example-aks/README.md) (never applied end to end) | Qualification overlay: extra pools, artifact container, CNPG backup target, monitoring, budget alert |
| Workspace manifest | Freeze procedure specified in [workspace inventory](repository-handoffs.md#workspace-inventory) | Manifest generator and reconciler |

## Tooling inventory

Existing repository tooling named in the procedures: `scripts/test_helm_chart.py`, `server/test.ps1` (`-Cluster`, `-Postgres`, `-Platform`), `server/gates.ps1`, `server/localdeploy.ps1`, `server/docker-compose.yml` profiles `cluster`, `gateway` and `s3`, `mmp-conformance`, the four client conformance suites, `server/tools/check_validation_receipt.py`, `scripts/frozen_eval.py`, the PostgreSQL-versus-Datastore benchmark, the [Phase 1 fixtures](../../tests/fixtures/clearview-phase1/README.md), the [example AKS module](../../deploy/terraform/example-aks/README.md) and the [deployment runbook](../ops/deployment-runbook.md). Everything marked "to be written" above is new work; none of it exists yet, and none of it is authorized for cloud use by this page.

## Decisions that block scheduling

| Decision | Blocks | Owner |
|---|---|---|
| Binary rollback route after migration 0043 (A, B or C above) | COMPAT-T3, ART rollback step, LOGIN1 rollback baseline, W1 step 9 | Server release engineering ([ledger](implementation-ledger.md#next-implementation-steps)) |
| Whether L3 (workstation Docker Desktop Kubernetes) receipts on the exact candidate are accepted environments of record for the IDs marked Full in the [L3 table](#what-one-docker-desktop-node-can-prove) | Whether W1 is needed and its size | Release owner |
| U10 shared session store | LOGIN1 | Server console and deployment maintainers ([open items](README.md#open-items)) |
| ART1 verifier identity and independence (hub record) | ART1/ART2 | Hub contract owners |
| SC2 placement contract (hub record) | SC2 | Hub contract owners |
| Lens runtime, model pair, precision and hardware; I3 thresholds | W2, W3 | Lens maintainers ([open items](README.md#open-items)) |
| Restricted-root authorization | W3R | Corpus owner |
| Qualification assignments, access and spending authority | Every window | Release engineering ([open items](README.md#open-items)) |
