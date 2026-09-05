# SPDX-License-Identifier: Apache-2.0
#
# Copy to terraform.tfvars and edit. Every value below is invented; none of them
# names a real resource anywhere.
#
#   terraform init
#   terraform plan  -var-file=terraform.tfvars
#   terraform apply -var-file=terraform.tfvars

name_prefix = "munarium-eval"
location    = "westeurope"

# Public image, so no registry credential is needed and registry_id stays unset.
image_repository = "ghcr.io/example-org/munarium-server"
image_tag        = "1.0.0"

# Set both together to enable layout-aware ingestion of scanned documents.
# Leaving them null is the product default: Munarium ingests text without them.
# doc_intel_endpoint = "https://example-docintel.cognitiveservices.azure.com/"
# doc_intel_id       = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/example-rg/providers/Microsoft.CognitiveServices/accounts/example-docintel"

# Only needed for a private registry. Null skips the AcrPull role assignment.
# registry_id = "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/example-rg/providers/Microsoft.ContainerRegistry/registries/exampleregistry"

user_node_count = 2

tags = {
  owner       = "platform-team"
  cost-centre = "0000"
  purpose     = "munarium-evaluation"
}
