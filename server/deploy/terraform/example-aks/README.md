# Munarium on AKS — an illustrative module

This builds a small AKS cluster, installs CloudNativePG and Envoy Gateway, and deploys Munarium
Server from `munarium`. It exists so that "how would I run this on a
cluster?" has an answer you can read, not because it is the only shape that works.

```bash
cp example.tfvars terraform.tfvars   # then edit it
terraform init
terraform plan  -var-file=terraform.tfvars
terraform apply -var-file=terraform.tfvars
```

Roughly 15–20 minutes. The three-node arrangement in `example.tfvars` costs on the order of
$0.50/hour while it is up, before storage and egress. Teardown is `terraform destroy`, or delete the
resource group the module created — everything it makes lives there.

## What has actually been run, and what has not

**This module is authored and syntax-checked. It has not been applied end to end.** `terraform fmt`
and `terraform validate` run against it in CI on every change, so it parses, its references resolve
and its provider constraints are satisfiable. That is not the same as knowing it converges on a real
subscription, and you should expect a shakedown pass on first apply.

Two limitations are inherited from the chart and are worth knowing before you plan a production
deployment on this shape:

- **The chart does not wire `MUNARIUM_TOKEN_SECRET` or any `MUNARIUM_SECRET_*` value.** You supply
  those yourself, through the AKS Key Vault secrets provider this module enables or by any other
  route your organisation prefers.
- **There is no WAL archive.** CloudNativePG gives you an in-cluster PostgreSQL with a replica; it
  does not give you point-in-time recovery to object storage until you configure a backup target.
  Plan backups deliberately rather than assuming this covers them.

If you want a supported, certified deployment with an architecture Ioka attests against a named
support matrix, that is Munarium Enterprise. See SUPPORT.md.

## The ladder

This is the third rung, and most people should not start here.

| | What it gives you | Where |
|---|---|---|
| 1 | The whole product on a laptop, one command | `docker compose up --build` in [server/](../../..) |
| 2 | A real cluster install, no cloud account | `helm install` from `munarium` against kind or minikube |
| 3 | A cloud deployment with managed identity and blob storage | this module |

Rung 1 is the whole evaluation. Nothing here is needed to decide whether Munarium does what you
want.

## What it creates

A resource group, an AKS cluster with a system pool and a user pool, a user-assigned managed
identity federated to the `munarium` ServiceAccount, a storage account with **no access keys** (the
workload identity is the only credential), and three Helm releases.

Nothing is assumed to exist except a subscription you can write to. There are no `data` sources
reaching for resources someone else made — an example whose first instruction is "these must already
exist" is not an example.

## Variables

Every input is declared in [variables.tf](variables.tf) with a description. Three notes:

- **`location` has no default.** The right region is yours to choose.
- **`registry_id` is optional.** Leave it null for a public image and the `AcrPull` role assignment
  is skipped entirely. It is only needed when pulling from a private Azure Container Registry.
- **`doc_intel_endpoint` and `doc_intel_id` are optional and go together.** Leaving them unset is the
  product default: Munarium ingests text without Document Intelligence, which is only needed for
  layout-aware extraction from scanned documents.

## Adapting it

The parts most likely to need changing for a real deployment: node sizes and counts, the storage
replication type (`LRS` is the cheapest and the least durable), the `Free` cluster SKU, which has no
uptime SLA, and the absence of a private endpoint on the storage account. None of those is a
recommendation — they are the smallest choices that let the example be read in one sitting.
