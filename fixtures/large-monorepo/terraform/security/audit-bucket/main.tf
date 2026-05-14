module "audit_logs" {
  source = "../../modules/s3-bucket"

  name           = "northwind-audit-logs-${var.environment}-100000000003"
  versioning     = true
  lifecycle_days = 2555 # 7 years

  tags = {
    Service       = "audit-bucket"
    Classification = "restricted"
  }
}

resource "aws_s3_bucket_policy" "audit_logs" {
  bucket = module.audit_logs.bucket_id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid    = "AllowCrossAccountWrites"
      Effect = "Allow"
      Principal = {
        AWS = [
          "arn:aws:iam::100000000001:root",
          "arn:aws:iam::100000000002:root",
        ]
      }
      Action   = ["s3:PutObject", "s3:PutObjectAcl"]
      Resource = "${module.audit_logs.bucket_arn}/*"
      Condition = {
        StringEquals = {
          "s3:x-amz-acl" = "bucket-owner-full-control"
        }
      }
    }]
  })
}
