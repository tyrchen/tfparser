output "db_endpoint" {
  description = "RDS endpoint for the orders service"
  value       = module.orders_db.db_endpoint
}

output "service_role_arn" {
  description = "ARN of the service execution role"
  value       = module.service_role.role_arn
}

output "archive_bucket" {
  description = "S3 bucket holding archived orders"
  value       = module.order_archive_bucket.bucket_id
}

output "events_bucket" {
  description = "S3 bucket for analytics events (data account)"
  value       = module.order_events_bucket.bucket_id
}
