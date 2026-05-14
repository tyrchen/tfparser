locals {
  domain = "services"
  domain_tags = {
    Org    = "northwind"
    Domain = "services"
  }

  # Read the dependent network component's outputs at apply time.
  # tfparser captures this as a Terragrunt dependency edge.
  network_outputs = "../../platform/main-network"
}

inputs = {
  domain      = local.domain
  domain_tags = local.domain_tags
}
