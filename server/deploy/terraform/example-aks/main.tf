# SPDX-License-Identifier: Apache-2.0
#
# An illustrative AKS deployment of Munarium Server: a cluster, CloudNativePG,
# Envoy Gateway, and the chart in ../helm/munarium. Read README.md before
# applying it — in particular the paragraph about what has and has not been run.
#
# The shape follows docs/architecture.md sections 2 and 8. Everything it needs,
# it creates; nothing is assumed to exist except a subscription you can write to.

locals {
  # Storage account names allow no hyphens and cap at 24 characters.
  storage_name  = substr(replace("${var.name_prefix}store", "-", ""), 0, 24)
  use_doc_intel = var.doc_intel_endpoint != null
}

resource "azurerm_resource_group" "this" {
  name     = "${var.name_prefix}-rg"
  location = var.location
  tags     = var.tags
}

# --- The cluster -------------------------------------------------------------

resource "azurerm_kubernetes_cluster" "this" {
  name                = "${var.name_prefix}-aks"
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  dns_prefix          = var.name_prefix
  sku_tier            = "Free"
  tags                = var.tags

  default_node_pool {
    name       = "system"
    node_count = 1
    vm_size    = var.system_node_vm_size
  }

  identity {
    type = "SystemAssigned"
  }

  key_vault_secrets_provider {
    secret_rotation_enabled = true
  }

  # Workload identity: the pod's ServiceAccount federates to a managed identity,
  # so the application authenticates to Blob storage with no secret anywhere.
  # The OIDC issuer is the trust anchor for that exchange.
  oidc_issuer_enabled       = true
  workload_identity_enabled = true
}

resource "azurerm_kubernetes_cluster_node_pool" "user" {
  name                  = "user"
  kubernetes_cluster_id = azurerm_kubernetes_cluster.this.id
  vm_size               = var.user_node_vm_size
  node_count            = var.user_node_count
  tags                  = var.tags
}

# Skipped entirely when registry_id is null, which is the case for a public image.
resource "azurerm_role_assignment" "acr_pull" {
  count                = var.registry_id == null ? 0 : 1
  scope                = var.registry_id
  role_definition_name = "AcrPull"
  principal_id         = azurerm_kubernetes_cluster.this.kubelet_identity[0].object_id
}

# --- Source-document storage -------------------------------------------------
#
# Munarium can keep source bytes in PostgreSQL instead; this is the alternative
# for corpora large enough that you would rather not. Leaving
# sourceStore.account empty in the chart falls back to bytes-in-Postgres.

resource "azurerm_storage_account" "sources" {
  name                     = local.storage_name
  resource_group_name      = azurerm_resource_group.this.name
  location                 = azurerm_resource_group.this.location
  account_tier             = "Standard"
  account_replication_type = "LRS"
  account_kind             = "StorageV2"
  tags                     = var.tags

  min_tls_version                 = "TLS1_2"
  https_traffic_only_enabled      = true
  allow_nested_items_to_be_public = false

  # No storage key exists. The workload identity is the only credential, which
  # means there is no key to leak, rotate or accidentally commit.
  shared_access_key_enabled = false
}

resource "azurerm_storage_container" "sources" {
  name                  = "sources"
  storage_account_id    = azurerm_storage_account.sources.id
  container_access_type = "private"
}

# --- The identity the pods run as --------------------------------------------

resource "azurerm_user_assigned_identity" "munarium" {
  name                = "${var.name_prefix}-identity"
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  tags                = var.tags
}

resource "azurerm_role_assignment" "blob_data_contributor" {
  scope                = azurerm_storage_account.sources.id
  role_definition_name = "Storage Blob Data Contributor"
  principal_id         = azurerm_user_assigned_identity.munarium.principal_id
}

resource "azurerm_role_assignment" "doc_intel_user" {
  count                = local.use_doc_intel && var.doc_intel_id != null ? 1 : 0
  scope                = var.doc_intel_id
  role_definition_name = "Cognitive Services User"
  principal_id         = azurerm_user_assigned_identity.munarium.principal_id
}

# The subject MUST match `system:serviceaccount:<namespace>:<serviceAccountName>`
# exactly. A mismatch fails at runtime with an opaque token-exchange error rather
# than at apply time, so it is worth reading twice.
resource "azurerm_federated_identity_credential" "munarium" {
  name                = "${var.name_prefix}-sa"
  resource_group_name = azurerm_resource_group.this.name
  parent_id           = azurerm_user_assigned_identity.munarium.id
  audience            = ["api://AzureADTokenExchange"]
  issuer              = azurerm_kubernetes_cluster.this.oidc_issuer_url
  subject             = "system:serviceaccount:${var.namespace}:munarium"
}

# --- Platform and application ------------------------------------------------

provider "helm" {
  kubernetes {
    host                   = azurerm_kubernetes_cluster.this.kube_config[0].host
    client_certificate     = base64decode(azurerm_kubernetes_cluster.this.kube_config[0].client_certificate)
    client_key             = base64decode(azurerm_kubernetes_cluster.this.kube_config[0].client_key)
    cluster_ca_certificate = base64decode(azurerm_kubernetes_cluster.this.kube_config[0].cluster_ca_certificate)
  }
}

provider "kubernetes" {
  host                   = azurerm_kubernetes_cluster.this.kube_config[0].host
  client_certificate     = base64decode(azurerm_kubernetes_cluster.this.kube_config[0].client_certificate)
  client_key             = base64decode(azurerm_kubernetes_cluster.this.kube_config[0].client_key)
  cluster_ca_certificate = base64decode(azurerm_kubernetes_cluster.this.kube_config[0].cluster_ca_certificate)
}

resource "helm_release" "cnpg" {
  name             = "cnpg"
  repository       = "https://cloudnative-pg.github.io/charts"
  chart            = "cloudnative-pg"
  namespace        = "cnpg-system"
  create_namespace = true
}

resource "helm_release" "envoy_gateway" {
  name             = "eg"
  repository       = "oci://docker.io/envoyproxy"
  chart            = "gateway-helm"
  namespace        = "envoy-gateway-system"
  create_namespace = true
}

resource "helm_release" "munarium" {
  name             = "munarium"
  chart            = "${path.module}/../helm/munarium"
  namespace        = var.namespace
  create_namespace = true
  depends_on       = [helm_release.cnpg, helm_release.envoy_gateway]

  set {
    name  = "image.repository"
    value = var.image_repository
  }
  set {
    name  = "image.tag"
    value = var.image_tag
  }

  # The chart annotates its ServiceAccount with this client id and labels the
  # pod so the webhook injects a projected token.
  set {
    name  = "workloadIdentity.clientId"
    value = azurerm_user_assigned_identity.munarium.client_id
  }
  set {
    name  = "sourceStore.account"
    value = azurerm_storage_account.sources.name
  }
  set {
    name  = "sourceStore.container"
    value = azurerm_storage_container.sources.name
  }

  dynamic "set" {
    for_each = local.use_doc_intel ? { provider = "azure" } : {}
    content {
      name  = "docIntel.provider"
      value = set.value
    }
  }

  dynamic "set" {
    for_each = local.use_doc_intel ? { endpoint = var.doc_intel_endpoint } : {}
    content {
      name  = "docIntel.endpoint"
      value = set.value
    }
  }
}
