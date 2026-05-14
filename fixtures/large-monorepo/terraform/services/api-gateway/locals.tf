locals {
  service_name = "api-gateway"
  team_tags = merge(var.domain_tags, {
    Service     = local.service_name
    Environment = var.environment
    Team        = "api-platform"
  })
}
