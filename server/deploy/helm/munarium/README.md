# Munarium Helm chart

One release = one CNPG Postgres cell + the munarium-server deployment + all three
API planes (REST, gateway, direct gRPC). Chart version `0.5.0`, app version `1.4.0`.

The chart defaults to the published 1.4.0 image. For a 1.4 upgrade, follow
the [release guide](../../../docs/guides/server-1.4.md) and select the qualified image explicitly.

**Status: first install validated on kind; identity exchange and gateway
plane still unexercised.** The chart's first `helm install` ran against a
kind v1.33.4 cluster with the CNPG 1.25.1 operator and a locally built
image (`gateway.enabled=false`, `directGrpc.enabled=false` —
no Gateway API CRDs and no LB on kind): the CNPG cell came up 2/2 with
pgvector, both server replicas passed the real-Postgres readiness probe,
and the release was verified end-to-end — a gated write with the default rw
token was accepted (`install.status=verified`, seq 1), read back with the
ro token, and `/metrics` on the ops plane reported
`munarium_build_info` for the built version. Being the chart's integration test,
it surfaced two real defects, both fixed in the same change: the pinned
tensorchord cell image tag (`pg16-v0.2.1`) was REJECTED by the CNPG
admission webhook as an invalid version tag (the default is now
`ghcr.io/cloudnative-pg/postgresql:16`, whose official operand image ships
pgvector — verified 0.8.6), and `runAsNonRoot: true` failed against the
distroless image's non-numeric `nonroot` user ("cannot verify user is
non-root"; the pod securityContext now pins `runAsUser/runAsGroup: 65532`).

**Still unexercised, stated plainly:** the workload-identity token
exchange (needs a real Azure cluster; the
[example AKS module](../../terraform/example-aks/README.md) that wires it is
authored and syntax-checked, not yet applied end to end) and the gateway
plane (as templated the listener is HTTP :80; the 443/TLS posture is
provisioned out-of-band). The install / upgrade / verify / rollback
procedure is
[../../../docs/ops/deployment-runbook.md](../../../docs/ops/deployment-runbook.md);
the server is N-replica-correct
([../../../docs/ops/clustering.md](../../../docs/ops/clustering.md)), and
the kind install ran `replicas: 2` against one cell for real.

## Prerequisites

- The **CNPG operator** (the chart declares a `postgresql.cnpg.io/v1
  Cluster`; nothing installs the operator for you).
- **Gateway API CRDs + Envoy Gateway**, if `gateway.enabled` (default true).
- A cluster that can pull `iokaio/munarium` or your own Server image.
  `image.repository` is empty by default and must be supplied explicitly.

## Install / upgrade

```bash
# Run from server/. Supply private production values in your own values file.
helm install munarium deploy/helm/munarium -n munarium --create-namespace \
  --set image.repository=iokaio/munarium --set image.tag=1.4.0
helm upgrade munarium deploy/helm/munarium -n munarium --reuse-values \
  --set image.tag=1.4.0
```

For production, add the [production profile](#replica-count-and-provider-limits)
with `-f deploy/helm/munarium/values-production.yaml` on install and on every
upgrade. The bare chart keeps the v1.4 replica-count behavior.

The default `image.tag` is `"1.4.0"`, the published server version — set the
tag you mean. The current chart constructs `repository:tag` and has no separate
digest value; use a Helm post-renderer to replace the image with
`repository@sha256:...` when pinning by digest.

## Values

| Key | Default | What it does |
|---|---|---|
| `image.repository` | _(required)_ | server image; `helm install` refuses without it |
| `image.tag` | `"1.4.0"` | image tag — the update lever |
| `replicas` | `2` | server pods |
| `providerReplicaCount` | _(omitted)_ | `MUNARIUM_REPLICA_COUNT`, the provider RPM/TPM divisor that arms the cluster startup checks. Omitted: no entry is rendered, as in v1.4. `auto`: `replicas`. A positive integer: that divisor. Anything else fails the render. See [replica count](#replica-count-and-provider-limits) |
| `strategy` | `{}` | Deployment update strategy, rendered verbatim when set. Empty keeps the Kubernetes default (RollingUpdate, 25% surge) |
| `staticTokens` | `demo-rw-token:demo:rw,demo-ro-token:demo:ro` | `MUNARIUM_STATIC_TOKENS` — demo literals; replace them |
| `workloadIdentity.clientId` | `""` | Azure workload-identity UAMI client id. Set: pods get the `azure.workload.identity/use` label + a `munarium` ServiceAccount annotated for federation. Empty: no annotation, so the chart still installs on a non-Azure cluster |
| `sourceStore.account` | `""` | Azure storage account for document bytes. Set: `MUNARIUM_SOURCE_STORE=az` via workload identity. Empty: `MUNARIUM_SOURCE_STORE=pg` (bytes in Postgres), which is what keeps a plain `helm install` working with no Azure at all |
| `sourceStore.container` | `"sources"` | blob container |
| `docIntel.provider` / `docIntel.endpoint` | `""` / `""` | document-intelligence escalation; empty = OFF, the product default — it is paid and egresses ([guide](../../../docs/guides/document-intelligence.md)) |
| `matrix.baseUrl` / `matrix.adminUrl` | `""` / `""` | Munarium Matrix, installed as its own release ([chart](https://github.com/iokaio/munarium-matrix/tree/main/deploy/helm/munarium-matrix)): `MUNARIUM_MATRIX_BASE_URL` (the query role's Service — the turn path) and `MUNARIUM_MATRIX_ADMIN_URL` (where a browser reaches Matrix's console, the `/admin/matrix` link). Empty = no Matrix; a runbook with data views then fails `verifyDataViews` rather than passing vacuously |
| `cell.instances` | `2` | CNPG cluster size (1 primary + 1 replica) |
| `cell.storage` | `10Gi` | per-instance volume |
| `cell.imageName` | `ghcr.io/cloudnative-pg/postgresql:16` | CNPG operand image; the official image ships pgvector (the old tensorchord pin was webhook-rejected — see Status) |
| `gateway.enabled` | `true` | Envoy Gateway resources; the shipped listener is HTTP on port 80. Add an HTTPS listener and certificate for TLS |
| `directGrpc.enabled` / `directGrpc.port` | `true` / `50051` | plane 3: LoadBalancer straight to the gRPC port |

The database URL is not a value: it comes from the CNPG-generated app secret
(`munarium-cell-a-app`, key `uri`), wired by the deployment template.

The direct gRPC LoadBalancer also exposes a plaintext listener. For remote
clients, terminate TLS at a configured ingress/proxy and disable the direct
LoadBalancer with `directGrpc.enabled=false` if it is not needed.

## Replica count and provider limits

Each Server process divides every configured provider RPM and TPM limit by
`MUNARIUM_REPLICA_COUNT` (`max(1, ceil(limit / count))` per local one-minute
window). A count above 1 also makes startup refuse a memory ledger or memory
source store. The binary default is 1. The v1.4 chart never rendered the
variable, so two default pods each admit the full limit and the cluster checks
stay off. `providerReplicaCount` controls it
([design](../../../docs/design/scale-out.md#helm-installation-upgrade-and-rollback)):

| Setting | Rendered on every pod | Use |
|---|---|---|
| omitted (bare chart) | nothing | v1.4 behavior. A count supplied by a post-render or `kubectl set env` override keeps its effect |
| `auto` | `replicas` (at least 1) | the [production profile](values-production.yaml). The chart has one Server group, so a pod that serves and builds counts once |
| a positive integer | that integer | a fixed divisor, even when it differs from `replicas` |

**Fresh production install.** Pass `-f deploy/helm/munarium/values-production.yaml`
with your own values. Two pods each run with count 2, so a 60 RPM provider
admits 30 per pod per window. Keep the profile in the values of every later
upgrade.

**Upgrade a v1.4 release with its original values.** Omit the key. The
rendered Deployment is identical to the v1.4 chart's for the same values, so
each pod keeps its previous effective count, ordinarily 1. `--reuse-values`
carries the v1.4 chart's values, which lack both new keys; both read as
omitted.

**Migrate that release to the production profile.** First confirm shared
stores (PostgreSQL ledger and a shared source store), pool capacity for the
peak live pod count, and the surge allowance. If a count is currently
supplied by an override, remove the override and set the equivalent chart
value in the same change. Then upgrade with the profile and wait for the
rollout. Check that `munarium_replica_count` on every pod's `/metrics` reports
the expected count, and that the console Health page shows it. A storage
refusal at startup means the storage configuration must be fixed first.

**Surge.** The default RollingUpdate adds one pod above `replicas` during a
rollout, and each surge pod opens a full share. With two replicas at count 2,
three pods can admit 1.5 times the provider limit until the old pod drains.
Set `strategy.rollingUpdate.maxSurge: 0` (with `maxUnavailable: 1`) to keep
live processes at `replicas`, at the cost of serving capacity during the
rollout. A replacement pod still opens a fresh window. Static shares cannot
bound a provider ceiling across restarts; see
[automatic scaling qualification](../../../docs/design/scale-out.md#helm-installation-upgrade-and-rollback).

**platform-v1.** The platform authority profile requires one Server process
and a count of 1; startup refuses any other count, even with shared storage.
Set `replicas: 1`, `providerReplicaCount: 1` (or `auto`) and
`strategy: {type: Recreate}`, so Kubernetes stops the old pod before starting
its replacement. A divisor of 1 on a multi-pod deployment hides the extra
processes from the startup guard, so never raise `replicas` on this profile.

**Roll back.** Restore the prior values, post-render overrides and chart
revision together (`helm rollback` restores all three for a Helm-managed
release). The v1.4 chart ignores `providerReplicaCount` and renders no count,
so its pods return to count 1 and two full windows. If a divisor of 2 is
still required, carry it through a deployment override on the v1.4 chart.
Drain or limit provider work during the transition and check the effective
count on each pod before resuming traffic.

What is not yet qualified on a cluster: startup guards, effective shares and
aggregate admissions at steady state and during surge, two upgrades and a
rollback ([SCALE-1-T1](../../../docs/design/scale-out.md#scale-1-t1)). The
rendering cases are checked by `scripts/test_helm_chart.py` from the
repository root.

## What the chart does NOT wire (and the workaround)

The chart carries **no `MUNARIUM_TOKEN_SECRET`** — so capability-token issuance
and JWT auth are unavailable — and **no `MUNARIUM_SECRET_ANTHROPIC` /
`MUNARIUM_SECRET_OPENAI` / `MUNARIUM_SECRET_OPENROUTER`**, so BYOK provider calls
are too. A chart installed as-is therefore has neither until you add them.
There is also no `extraEnv` values hook yet, so the workaround is genuinely a
workaround until one lands: create the secret yourself and patch the env in —

```bash
kubectl -n munarium create secret generic munarium-secrets \
  --from-literal=MUNARIUM_TOKEN_SECRET=$(openssl rand -hex 32)
kubectl -n munarium set env deployment/munarium-server \
  --from=secret/munarium-secrets
# The Secret key is already the exact environment-variable name.
# Add provider keys using their full MUNARIUM_SECRET_* names when needed.
```

(or a kustomize/post-render patch, which survives `helm upgrade` better).

## Source-store selection

`sourceStore.account` is the only first-class switch, and it selects **az**
(workload identity against Blob) or the **pg** fallback. The `s3` / `gcs` /
`file` backends exist in the server (`munarium-store-objects`) but have no
chart values yet — set `MUNARIUM_SOURCE_STORE` and the matching `MUNARIUM_S3_*` /
`MUNARIUM_GCS_*` / `MUNARIUM_FILE_ROOT` vars via the same raw-env override as
above. Per-backend semantics and credentials:
[../../../docs/guides/source-stores.md](../../../docs/guides/source-stores.md).

## See also

- [../../terraform/example-aks/README.md](../../terraform/example-aks/README.md) — an
  illustrative AKS module that consumes this chart.
- [../../../docs/security-posture.md](../../../docs/security-posture.md) —
  why `staticTokens` demo literals must not survive contact with production.
