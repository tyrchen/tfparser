module "lambda_role" {
  source = "../../modules/iam-role"

  name            = "${local.service_name}-${var.environment}-role"
  trusted_service = "lambda.amazonaws.com"
  managed_policy_arns = [
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole",
  ]

  inline_policies = {
    read_events = jsonencode({
      Version = "2012-10-17"
      Statement = [{
        Effect   = "Allow"
        Action   = ["s3:GetObject", "s3:ListBucket"]
        Resource = [
          "arn:aws:s3:::${var.source_events_bucket}",
          "arn:aws:s3:::${var.source_events_bucket}/*",
        ]
      }]
    })
  }

  tags = {
    Service = local.service_name
  }
}

module "worker_lambda" {
  source = "../../modules/lambda"

  function_name   = "${local.service_name}-${var.environment}"
  handler         = "main.handler"
  runtime         = "python3.12"
  memory_mb       = var.memory_mb
  timeout_seconds = var.timeout_seconds
  role_arn        = module.lambda_role.role_arn

  environment_variables = {
    EVENTS_BUCKET = var.source_events_bucket
    ENVIRONMENT   = var.environment
  }

  tags = {
    Service = local.service_name
  }
}

resource "aws_lambda_permission" "allow_s3" {
  statement_id  = "AllowExecutionFromS3"
  action        = "lambda:InvokeFunction"
  function_name = module.worker_lambda.function_name
  principal     = "s3.amazonaws.com"
  source_arn    = "arn:aws:s3:::${var.source_events_bucket}"
}
