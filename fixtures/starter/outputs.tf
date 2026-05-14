output "vpc_id" {
  description = "ID of the primary VPC"
  value       = aws_vpc.main.id
}

output "private_subnet_ids" {
  description = "List of private subnet IDs"
  value       = aws_subnet.private[*].id
}

output "public_subnet_ids" {
  description = "List of public subnet IDs"
  value       = aws_subnet.public[*].id
}

output "asg_name" {
  description = "Auto-scaling group name"
  value       = aws_autoscaling_group.app.name
}

output "assets_bucket" {
  description = "S3 bucket holding application assets"
  value       = aws_s3_bucket.assets.bucket
}

output "session_table" {
  description = "DynamoDB table for sessions"
  value       = aws_dynamodb_table.session.name
}

output "account_id" {
  description = "AWS account this stack deployed into"
  value       = data.aws_caller_identity.current.account_id
}
