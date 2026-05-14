# audit-bucket lives in the security account, not the workload account.
provider "aws" {
  region  = var.aws_region
  profile = var.aws_security_profile

  default_tags {
    tags = merge(var.domain_tags, {
      Provisioner = "terraform"
      Environment = var.environment
      Component   = "audit-bucket"
      Team        = "security"
    })
  }
}
