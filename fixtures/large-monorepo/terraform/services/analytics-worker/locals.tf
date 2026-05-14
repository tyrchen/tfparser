locals {
  service_name = "analytics-worker"
  team_tags = merge(var.domain_tags, {
    Service     = local.service_name
    Environment = var.environment
    Team        = "analytics"
  })
}
