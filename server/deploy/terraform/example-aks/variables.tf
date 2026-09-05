# SPDX-License-Identifier: Apache-2.0
#
# Every input this module takes. None of them has a default that names a real
# resource: a default that happens to be someone's estate is how an example
# stops being an example.

variable "name_prefix" {
  type        = string
  description = <<-EOT
    Prefix for every resource this module creates: the cluster becomes
    "<prefix>-aks", the identity "<prefix>-identity", the storage account
    "<prefix>store" with hyphens removed. Keep it short — Azure storage account
    names are capped at 24 characters, lowercase and alphanumeric only.
  EOT

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{2,16}$", var.name_prefix))
    error_message = "name_prefix must be 3-17 characters, lowercase letters, digits and hyphens, starting with a letter."
  }
}

variable "location" {
  type        = string
  description = "Azure region, e.g. \"westeurope\". No default: the right region is yours, not ours."
}

variable "image_repository" {
  type        = string
  description = <<-EOT
    Fully qualified repository for the Munarium Server image, without a tag —
    for example "ghcr.io/<org>/munarium-server" or
    "<registry>.azurecr.io/munarium-server".
  EOT
}

variable "image_tag" {
  type        = string
  description = "Image tag to deploy."
  default     = "1.0.0"
}

variable "registry_id" {
  type        = string
  description = <<-EOT
    Optional. The Azure resource id of a container registry to grant the cluster
    AcrPull on. Leave null when the image is public or you manage the pull
    secret yourself — the role assignment is skipped entirely.
  EOT
  default     = null
}

variable "doc_intel_endpoint" {
  type        = string
  description = <<-EOT
    Optional. Azure AI Document Intelligence endpoint for layout-aware ingestion
    of scanned documents. Null leaves the feature off, which is the product
    default: Munarium ingests text without it.
  EOT
  default     = null
}

variable "doc_intel_id" {
  type        = string
  description = "Optional. Resource id of that account, so the workload identity can be granted Cognitive Services User on it. Required when doc_intel_endpoint is set."
  default     = null
}

variable "system_node_vm_size" {
  type        = string
  description = "VM size for the single-node system pool."
  default     = "Standard_D2as_v5"
}

variable "user_node_vm_size" {
  type        = string
  description = "VM size for the user pool that runs Munarium."
  default     = "Standard_D4as_v5"
}

variable "user_node_count" {
  type        = number
  description = "Nodes in the user pool. Two is enough to see the clustering behaviour; one is enough to see it run."
  default     = 2
}

variable "namespace" {
  type        = string
  description = "Kubernetes namespace for Munarium. It is also half of the workload-identity federation subject, so changing it changes that subject."
  default     = "munarium"
}

variable "tags" {
  type        = map(string)
  description = "Tags applied to every resource. Cost centre, owner, expiry — whatever your organisation asks for."
  default     = {}
}
