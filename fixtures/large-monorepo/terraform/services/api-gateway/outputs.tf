output "api_endpoint" {
  description = "Public API endpoint"
  value       = aws_apigatewayv2_api.this.api_endpoint
}

output "api_id" {
  description = "API Gateway ID"
  value       = aws_apigatewayv2_api.this.id
}

output "stage_arn" {
  description = "Deployment stage ARN"
  value       = aws_apigatewayv2_stage.this.arn
}
