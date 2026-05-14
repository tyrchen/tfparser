resource "aws_apigatewayv2_api" "this" {
  name          = "northwind-${var.environment}-api"
  protocol_type = "HTTP"

  cors_configuration {
    allow_methods = ["GET", "POST", "OPTIONS"]
    allow_origins = ["https://*.northwind.example.com"]
    allow_headers = ["authorization", "content-type"]
    max_age       = 600
  }
}

resource "aws_apigatewayv2_stage" "this" {
  api_id      = aws_apigatewayv2_api.this.id
  name        = var.stage_name
  auto_deploy = true

  default_route_settings {
    throttling_burst_limit = var.throttle_burst_limit
    throttling_rate_limit  = var.throttle_rate_limit
  }
}

resource "aws_cloudwatch_log_group" "api_gateway" {
  name              = "/aws/apigateway/northwind-${var.environment}-api"
  retention_in_days = var.environment == "production" ? 30 : 7
}

module "edge_logs" {
  source = "../../modules/s3-bucket"

  name           = "northwind-api-edge-logs-${var.environment}-${var.aws_account_id}"
  versioning     = false
  lifecycle_days = 30

  tags = {
    Service = local.service_name
  }
}
