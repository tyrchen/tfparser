locals {
  name_prefix = "northwind-${var.environment}"

  common_tags = {
    Provisioner = "terraform"
    Environment = var.environment
    Team        = var.team
    Service     = "starter"
    Repo        = "infra-iac"
  }

  is_prod = var.environment == "prod"

  retention_days = local.is_prod ? 30 : 7
}
