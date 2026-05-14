provider "aws" {
  region  = var.aws_region
  profile = var.aws_main_profile

  default_tags {
    tags = merge(var.domain_tags, {
      Provisioner = "terraform"
      Environment = var.environment
      Component   = "ecr-shared"
      Team        = var.team
    })
  }
}
