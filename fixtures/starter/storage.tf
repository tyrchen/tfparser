resource "aws_s3_bucket" "assets" {
  bucket = "${local.name_prefix}-assets-${random_id.suffix.hex}"
}

resource "aws_s3_bucket_versioning" "assets" {
  bucket = aws_s3_bucket.assets.id
  versioning_configuration {
    status = local.is_prod ? "Enabled" : "Suspended"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "assets" {
  bucket = aws_s3_bucket.assets.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm     = "aws:kms"
      kms_master_key_id = aws_kms_key.storage.arn
    }
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "assets" {
  bucket = aws_s3_bucket.assets.id

  rule {
    id     = "expire-old"
    status = "Enabled"

    filter {}

    expiration {
      days = local.retention_days
    }
  }
}

resource "aws_dynamodb_table" "session" {
  name         = "${local.name_prefix}-session"
  hash_key     = "id"
  billing_mode = "PAY_PER_REQUEST"

  attribute {
    name = "id"
    type = "S"
  }

  point_in_time_recovery {
    enabled = local.is_prod
  }

  server_side_encryption {
    enabled     = true
    kms_key_arn = aws_kms_key.storage.arn
  }
}
