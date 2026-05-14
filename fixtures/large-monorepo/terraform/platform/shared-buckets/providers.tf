provider "aws" {
  region  = var.aws_region
  profile = var.aws_main_profile

  default_tags {
    tags = merge(var.domain_tags, {
      Provisioner = "terraform"
      Environment = var.environment
      Component   = "shared-buckets"
      Team        = var.team
    })
  }
}

provider "aws" {
  alias   = "us_east_1"
  region  = "us-east-1"
  profile = var.aws_main_profile

  default_tags {
    tags = merge(var.domain_tags, {
      Provisioner = "terraform"
      Environment = var.environment
      Component   = "shared-buckets"
      Team        = var.team
    })
  }
}
