# analytics-worker runs entirely in the data account.
provider "aws" {
  region  = "us-east-1"
  profile = var.aws_data_profile

  default_tags {
    tags = local.team_tags
  }
}
