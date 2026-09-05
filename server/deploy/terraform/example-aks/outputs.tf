# SPDX-License-Identifier: Apache-2.0

output "kubeconfig_command" {
  value       = "az aks get-credentials -n ${azurerm_kubernetes_cluster.this.name} -g ${azurerm_resource_group.this.name}"
  description = "Run this to point kubectl at the cluster."
}

output "resource_group" {
  value       = azurerm_resource_group.this.name
  description = "Everything this module created lives here. Deleting it is the teardown."
}

output "sources_storage_account" {
  value       = azurerm_storage_account.sources.name
  description = "Blob account holding raw source documents, in the `sources` container."
}

output "workload_identity_client_id" {
  value       = azurerm_user_assigned_identity.munarium.client_id
  description = "The client id the chart annotates onto the ServiceAccount."
}
