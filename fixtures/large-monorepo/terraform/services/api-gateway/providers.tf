provider "aws" {
  region  = var.aws_region
  profile = var.aws_main_profile

  default_tags {
    tags = local.team_tags
  }
}
