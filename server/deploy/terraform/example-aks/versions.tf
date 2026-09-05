# SPDX-License-Identifier: Apache-2.0
terraform {
  required_version = ">= 1.6"

  required_providers {
    azurerm    = { source = "hashicorp/azurerm", version = "~> 4.0" }
    helm       = { source = "hashicorp/helm", version = "~> 2.12" }
    kubernetes = { source = "hashicorp/kubernetes", version = "~> 2.30" }
  }

  # Deliberately unconfigured. Supply your own with
  # `terraform init -backend-config=...`, or delete this block to keep state
  # locally while you are evaluating.
  backend "azurerm" {}
}

provider "azurerm" {
  features {}

  # No subscription is named here. Terraform takes it from `az login` or from
  # ARM_SUBSCRIPTION_ID, so this configuration belongs to whoever applies it.
  storage_use_azuread = true
}
