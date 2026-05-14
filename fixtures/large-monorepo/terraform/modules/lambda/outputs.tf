output "function_arn" {
  description = "ARN of the Lambda function"
  value       = aws_lambda_function.this.arn
}

output "function_name" {
  description = "Name of the Lambda function"
  value       = aws_lambda_function.this.function_name
}

output "log_group_name" {
  description = "CloudWatch log group name"
  value       = aws_cloudwatch_log_group.this.name
}
