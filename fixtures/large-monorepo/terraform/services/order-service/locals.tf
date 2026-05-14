locals {
  service_name = "order-service"
  team_tags = merge(var.domain_tags, {
    Service     = local.service_name
    Environment = var.environment
    Team        = "orders"
  })
  is_prod = var.environment == "production"
}
