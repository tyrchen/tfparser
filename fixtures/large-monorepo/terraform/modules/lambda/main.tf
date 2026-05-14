resource "aws_cloudwatch_log_group" "this" {
  name              = "/aws/lambda/${var.function_name}"
  retention_in_days = 30

  tags = merge(var.tags, { Name = "/aws/lambda/${var.function_name}" })
}

resource "aws_lambda_function" "this" {
  function_name = var.function_name
  role          = var.role_arn
  handler       = var.image_uri == "" ? var.handler : null
  runtime       = var.image_uri == "" ? var.runtime : null
  memory_size   = var.memory_mb
  timeout       = var.timeout_seconds
  package_type  = var.image_uri == "" ? "Zip" : "Image"
  image_uri     = var.image_uri == "" ? null : var.image_uri

  environment {
    variables = var.environment_variables
  }

  depends_on = [aws_cloudwatch_log_group.this]

  tags = merge(var.tags, { Name = var.function_name })
}
