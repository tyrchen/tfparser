resource "aws_s3_bucket" "this" {
  bucket = var.name

  tags = merge(var.tags, { Name = var.name })
}

resource "aws_s3_bucket_versioning" "this" {
  bucket = aws_s3_bucket.this.id
  versioning_configuration {
    status = var.versioning ? "Enabled" : "Suspended"
  }
}

resource "aws_s3_bucket_public_access_block" "this" {
  bucket                  = aws_s3_bucket.this.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_server_side_encryption_configuration" "this" {
  bucket = aws_s3_bucket.this.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm     = var.kms_key_arn == "" ? "AES256" : "aws:kms"
      kms_master_key_id = var.kms_key_arn == "" ? null : var.kms_key_arn
    }
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "this" {
  count  = var.lifecycle_days > 0 ? 1 : 0
  bucket = aws_s3_bucket.this.id

  rule {
    id     = "expire-noncurrent"
    status = "Enabled"

    filter {}

    noncurrent_version_expiration {
      noncurrent_days = var.lifecycle_days
    }
  }
}
