module "artifacts" {
  source = "../../modules/s3-bucket"

  name           = "northwind-artifacts-${var.environment}-${var.aws_region}"
  versioning     = true
  lifecycle_days = 90

  tags = {
    Service = "shared-artifacts"
  }
}

module "build_cache_us_east_1" {
  source = "../../modules/s3-bucket"

  name       = "northwind-build-cache-${var.environment}-us-east-1"
  versioning = false

  providers = {
    aws = aws.us_east_1
  }

  tags = {
    Service = "build-cache"
  }
}
