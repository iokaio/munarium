# Deployment runbook

How to put `munarium-server` into a cluster with what ships in this
repository, and how to know it worked. It covers the two shipped shapes — the
Helm chart ([../../deploy/helm/munarium/README.md](../../deploy/helm/munarium/README.md))
on any Kubernetes with the CloudNativePG operator, and the illustrative AKS
Terraform module ([../../deploy/terraform/example-aks/README.md](../../deploy/terraform/example-aks/README.md))
that consumes the chart. `docker compose up` is the evaluation path and is
not a deployment. Status, stated plainly: the chart's first install was
validated on kind; the AKS module is authored and syntax-checked, not yet
applied end to end. Expect a shakedown pass on a first cloud install.

## 0. Preconditions

- A cluster you can `helm install` into, with the **CNPG operator** present
  (the chart declares a `postgresql.cnpg.io/v1 Cluster`; nothing installs
  the operator for you), and **Gateway API CRDs + Envoy Gateway** if you keep
  `gateway.enabled=true`.
- A registry the cluster can pull from, and the credentials to push to it.
- Docker (BuildKit), Rust stable and PowerShell 7 on the build machine — or
  a released image, pinned by digest, in which case skip §2.
- Secrets decided ahead of time: `MUNARIUM_TOKEN_SECRET` (≥ 32 random
  bytes), the `MUNARIUM_SECRET_*` provider keys you intend to use, and the
  static tokens that replace the chart's placeholders. The chart wires none
  of these; see its README for the workaround.

## 1. Gate

```powershell
cd server
.\gates.ps1
```

The identical gate list CI runs — fmt, clippy, workspace tests against a
recreated `munarium_ci` database, conformance (in-process, pg, black-box on
both planes, platform, cluster), both doc drift checks, the crate-boundary
grep, the additive-migrations grep, cargo-deny. Build images only from a
green, clean tree.

## 2. Build and push the image

```powershell
.\build.ps1 -Image -ImageTag <tag>            # docker build of the musl -> distroless image
docker tag munarium-server:<tag> <registry>/munarium-server:<tag>
docker push <registry>/munarium-server:<tag>
```

Use a tag that names exactly one commit — the short SHA of a clean tree is
the convention CI uses — so `/version` on a running pod can be matched to a
source revision. Never re-push a tag with different bytes; a tag that moves
defeats every check in §4.

Signed release images (with SBOM and provenance) are cut by Ioka outside this
repository. A build from source per the above is the reference when in doubt.

## 3. Install or upgrade

Configuration lives in Helm values (or your overlay), not in the image. A new
image may require newly declared configuration, so change values **with or
before** the image they belong to, never after.

```bash
helm install munarium deploy/helm/munarium -n munarium --create-namespace \
  --set image.repository=<registry>/munarium-server --set image.tag=<tag> \
  --set staticTokens="<rw-token>:<tenant>:rw,<ro-token>:<tenant>:ro,<mgmt-token>:<tenant>:mgmt"

helm upgrade munarium deploy/helm/munarium -n munarium --reuse-values --set image.tag=<new-tag>
kubectl -n munarium rollout status deployment/munarium-server
```

Then add what the chart does not wire — the token secret and provider keys —
as a Kubernetes Secret patched into the deployment's environment (or a
post-render patch, which survives `helm upgrade` better); the chart README
has the exact commands. Migrations run at server startup and are
additive-only, so replicas on two adjacent versions are correct together
during the roll ([clustering.md](clustering.md)).

**With the AKS module:** copy `example.tfvars` to `terraform.tfvars`, edit
it, then `terraform init`, `plan` and `apply`. An upgrade is the image tag
changed in your tfvars and another `plan` → `apply`; read the plan, and treat
any proposed destroy as a stop — infrastructure deletion belongs in an
explicit teardown, never in a deploy.

## 4. Verify the rollout, not the hostname

A stable hostname keeps serving from old healthy pods while a new pod
crash-loops, so "the site is up" proves nothing about the new image. Check
the new pods:

```bash
kubectl -n munarium get pods -l app.kubernetes.io/name=munarium \
  -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{.spec.containers[0].image}{"\t"}{.status.phase}{"\n"}{end}'

curl -fsS https://<host>/healthz
curl -fsS https://<host>/readyz                 # ok | not ready | draining
curl -fsS https://<host>/version                # the version you built
curl -fsS https://<host>/openapi.json | jq '.paths | length'
jq '.paths | length' docs/api/openapi.json      # must be the same number
```

A served path count that differs from the committed spec is a stale image
behind the hostname — the single most common "deploy succeeded, old behaviour
serves" cause. Then exercise the planes: a gated write with the rw token and
a read-back with the ro token on REST; `grpcurl … grpc.health.v1.Health/Check`
through the gateway (and on :50051 if `directGrpc.enabled`); and, through a
port-forward (never an ingress), `/metrics` on :9090 reporting
`munarium_build_info` for the new version. Runbooks are applied to the
database, not carried by the image, so re-check `GET /v1/runbooks` after any
database change.

## 5. Roll back

```bash
helm history munarium -n munarium
helm rollback munarium <known-good-revision> -n munarium
kubectl -n munarium rollout status deployment/munarium-server
```

or set `image.tag` back to the previous value and upgrade. Migrations are
additive-only (CI-greped), so a rolled-back binary runs correctly against a
newer schema; there is no down-migration and none is needed. A datastore
rollout is independent of the image: `PUT /v1/retrieval-rollout` with
`serving: postgres` (or `mmctl datastore rollout set … postgres`) is the
per-scope rollback and is never gated.

## 6. Backups

A deploy is not a backup. Before the first production upgrade, confirm that
point-in-time recovery exists — a CNPG cell as the chart creates it has a
replica but **no WAL archive** until you configure one; a managed PostgreSQL
brings its own — and that you have drilled the restore:
[backup-restore.md](backup-restore.md).
