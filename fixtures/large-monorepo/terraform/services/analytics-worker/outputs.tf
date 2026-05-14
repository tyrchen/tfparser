output "function_arn" {
  description = "ARN of the analytics worker Lambda"
  value       = module.worker_lambda.function_arn
}

output "role_arn" {
  description = "ARN of the Lambda execution role"
  value       = module.lambda_role.role_arn
}
