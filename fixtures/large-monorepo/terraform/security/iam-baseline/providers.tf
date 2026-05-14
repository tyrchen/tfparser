provider "aws" {
  region  = var.aws_region
  profile = var.aws_main_profile

  default_tags {
    tags = merge(var.domain_tags, {
      Provisioner = "terraform"
      Environment = var.environment
      Component   = "iam-baseline"
      Team        = "security"
    })
  }

  assume_role {
    role_arn = "arn:aws:iam::${var.aws_account_id}:role/${var.terraform_role_name}"
  }
}
