module "ci_deploy_role" {
  source = "../../modules/iam-role"

  name                = "ci-deploy-${var.environment}"
  trusted_account_ids = ["100000000099"]
  managed_policy_arns = [
    "arn:aws:iam::aws:policy/PowerUserAccess",
  ]

  tags = {
    Service = "iam-baseline"
  }
}

module "audit_reader_role" {
  source = "../../modules/iam-role"

  name                = "audit-reader-${var.environment}"
  trusted_account_ids = ["100000000003"]
  managed_policy_arns = [
    "arn:aws:iam::aws:policy/ReadOnlyAccess",
  ]

  tags = {
    Service = "iam-baseline"
  }
}
