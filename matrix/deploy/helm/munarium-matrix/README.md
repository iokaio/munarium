# munarium-matrix Helm chart

The structured-evidence plane as a Kubernetes release: **one Deployment per
runtime role** (`control`, `query`, `sync`, `reconcile` — or a single `all`),
a Service per role, and nothing else. Chart `0.1.0`, app `1.0.0` (the
server/client lockstep version Matrix checks at boot).

**Status: installed and probed on a real cluster, 2026-08-30** (kind, one
node, a Postgres carrying the fixture's own `matrix_owner` posture). Five pods
— control, query ×2, sync, reconcile — reached Ready, the store migrated into
schema `matrix`, and the properties worth having were checked rather than
assumed:

| Checked | Answer |
|---|---|
| `/healthz`, `/readyz`, `/version` on the control role | 200 · `{"ok":true}` · role `control`, `server_compatibility: unknown` |
| the registry on the **query** role | **404** — role isolation is structural, not configuration |
| `…/contracts/{name}/execute` on the **control** role | **404**; on the query role, a 422 naming the missing intent field |
| the console on the control role | 303 to its login |
| the sync and reconcile roles | `role loop starting` — the queues run where they should |
| the query role's gRPC plane | listening on 50151; `grpcurl list` returns `matrix.v1.MatrixQuery` |
| an unreachable server | a warn and a boot, not a crash: the lockstep verdict is `unknown` and the runtime fails closed later |

It also found what rendering could not: reflection listed the query service
and **not** `grpc.health.v1.Health`, so the `grpcurl … Health/Check` command
[the gRPC guide](../../../docs/api/grpc.md) prints answered "target server does not
expose service" against a server that was serving it. Fixed in `grpc.rs`, and
the conformance scenario that was supposed to cover it — and never called
reflection at all — now does.

Every measured number elsewhere in these docs comes from a managed-container
deployment (Azure Container Apps), not from this chart; the chart sets the same
`MUNARIUM_MATRIX_*` environment that deployment does, variable for variable.
**No production cluster runs it, and no ingress, TLS or autoscaling has been
exercised** — a one-node kind install proves the manifests and the role
split, not an operator's environment.

## What the chart does NOT do, on purpose

- **It does not create the database, the schema or the role.** Matrix runs as
  `matrix_owner`, which owns schema `matrix` and is denied `public`; the
  `registry.matrix_owner_cannot_write_public` scenario proves that posture on
  every run of the Postgres tier. Creating a role from a chart would put a superuser
  credential in `values.yaml`. Create both with
  `fixtures/t0/sql/01-roles-and-schema.sql`, then hand the chart a Secret
  holding the URL.
- **It carries no source credential as a value.** `credentials[]` maps a
  `credentialRef` name to a key of an existing Secret; the variable Matrix
  reads (`MUNARIUM_MATRIX_SECRET_<REF>`) is populated from that Secret at pod
  start and never appears in the rendered manifest.
- **It does not front the gRPC plane with TLS.** The query role's Service
  exposes `50151` as h2c inside the cluster; a Gateway or ingress terminates
  TLS the way an Azure Container Apps `http2` ingress does.

## Composition with the server chart

The two charts are installed side by side, not nested — ground rule 1 keeps
the trees apart, and a sub-chart would put Matrix's values under the server's
key. Wire them with two values:

```
# server release: where the turn path reaches Matrix, and where a browser
# reaches its console (the /admin/matrix reciprocal link).
helm upgrade --install munarium server/deploy/helm/munarium \
  --set matrix.baseUrl=http://munarium-matrix-query:8180 \
  --set matrix.adminUrl=https://matrix.example.com/admin

# matrix release: where Matrix seals evidence and reads the ledger.
helm upgrade --install munarium-matrix matrix/deploy/helm/munarium-matrix \
  --set server.url=http://munarium-server:8080 \
  --set database.secretName=munarium-matrix-db \
  --set server.tokenSecretName=munarium-matrix-server-token
```

`matrix.baseUrl` points at the **query** role: the server executes contracts
and nothing else, and a server that could reach the registry would be a server
that could rewrite its own contracts.

## Values

| Key | Default | What it does |
|---|---|---|
| `image.repository` / `image.tag` | _(required)_ / `"1.0.0"` | the image; `helm install` refuses without a repository, and the tag is the update lever |
| `roles[]` | control 1 · query 2 (grpc) · sync 1 · reconcile 1 | one Deployment + Service each; `grpc: true` exposes 50151 on that role |
| `database.secretName` / `.key` | `munarium-matrix-db` / `url` | `MUNARIUM_MATRIX_DATABASE_URL` from an existing Secret |
| `server.url` | `http://munarium-server:8080` | `MUNARIUM_MATRIX_SERVER_URL` |
| `server.lockstepVersion` | `"1.0.0"` | `MUNARIUM_MATRIX_TARGET_SERVER_VERSION`, checked at boot |
| `server.tokenSecretName` / `.tokenKey` | `munarium-matrix-server-token` / `token` | the server token Matrix seals with |
| `staticTokens` | demo literals | `MUNARIUM_MATRIX_STATIC_TOKENS`; replace them |
| `credentials[]` | `[]` | `{ref, secretName, key}` → `MUNARIUM_MATRIX_SECRET_<REF>` |
| `egressDefaultDeny` | `true` | a source reaches only its own `egress.allowHosts` |
| `workloadIdentity.clientId` | `""` | Azure workload identity for `store: az` landing sources; empty = no annotation |
| `admin` | `enabled` | `MUNARIUM_MATRIX_ADMIN`; `disabled` unmounts the console |
| `metrics.scrapeAnnotations` | `true` | Prometheus annotations for `/metrics` on 9190 |
| `resources` | 100m/128Mi – 1/512Mi | per pod |

## Render it

```
helm template mx matrix/deploy/helm/munarium-matrix | kubectl apply --dry-run=client -f -
```

## Install it on a laptop cluster

What was actually run on 2026-08-30, start to finish. It needs a cluster
(`kind create cluster`), the image built locally, and nothing else — no
registry, no server, no cloud.

```powershell
docker build -t munarium-matrix:local matrix
kind load docker-image munarium-matrix:local

kubectl create namespace mx
kubectl -n mx create configmap matrix-init `
  --from-file=01-roles-and-schema.sql=matrix/fixtures/t0/sql/01-roles-and-schema.sql
# a Postgres that runs that SQL at first boot, so `matrix_owner` owns schema
# `matrix` and is denied `public` -- the posture the chart assumes and the
# Postgres tier proves on every run
kubectl -n mx apply -f matrix/deploy/helm/munarium-matrix/example-postgres.yaml

kubectl -n mx create secret generic munarium-matrix-db `
  --from-literal=url='postgres://matrix_owner:matrix-owner-dev@postgres:5432/matrix'
kubectl -n mx create secret generic munarium-matrix-server-token --from-literal=token='dev'

helm upgrade --install mx matrix/deploy/helm/munarium-matrix -n mx `
  --set image.repository=munarium-matrix --set image.tag=local
kubectl -n mx wait --for=condition=Ready pod --all --timeout=180s
```

Then the two probes worth running, because they are the ones that fail if the
role split is wrong:

```powershell
kubectl -n mx port-forward svc/munarium-matrix-control 8180:8180   # then:
curl -s -H "Authorization: Bearer matrix-demo-mgmt" -o /dev/null -w "%{http_code}" `
  http://127.0.0.1:8180/v1/datasources            # 200 on control
kubectl -n mx port-forward svc/munarium-matrix-query 8181:8180     # then:
curl -s -H "Authorization: Bearer matrix-demo-mgmt" -o /dev/null -w "%{http_code}" `
  http://127.0.0.1:8181/v1/datasources            # 404 on query -- structural
```

`example-postgres.yaml` is a **development** Postgres: one replica, no
persistence, a literal password. It exists so this page is reproducible, and
it is not what an operator deploys.
