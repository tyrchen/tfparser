provider "aws" {
  region  = var.aws_region
  profile = var.aws_main_profile

  default_tags {
    tags = local.team_tags
  }
}

# Cross-account provider — order-service writes analytics events into the data account.
provider "aws" {
  alias   = "data"
  region  = "us-east-1"
  profile = var.aws_data_profile

  default_tags {
    tags = local.team_tags
  }
}
