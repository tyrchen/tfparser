module "orders_db" {
  source = "../../modules/rds"

  name                   = "${local.service_name}-${var.environment}"
  instance_class         = var.db_instance_class
  storage_gb             = var.db_storage_gb
  subnet_ids             = var.private_subnet_ids
  vpc_security_group_ids = [aws_security_group.db.id]
  multi_az               = local.is_prod

  tags = {
    Service = local.service_name
  }
}

module "order_archive_bucket" {
  source = "../../modules/s3-bucket"

  name           = "northwind-order-archive-${var.environment}-${var.aws_account_id}"
  versioning     = true
  lifecycle_days = local.is_prod ? 365 : 30

  tags = {
    Service = local.service_name
  }
}

# Analytics events land in the data account.
module "order_events_bucket" {
  source = "../../modules/s3-bucket"

  name           = "northwind-order-events-${var.environment}-100000000002"
  versioning     = false
  lifecycle_days = 30

  providers = {
    aws = aws.data
  }

  tags = {
    Service = local.service_name
  }
}
