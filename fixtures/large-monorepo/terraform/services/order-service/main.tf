resource "aws_security_group" "db" {
  name        = "${local.service_name}-${var.environment}-db-sg"
  description = "Database access from the service runtime"
  vpc_id      = var.vpc_id

  ingress {
    from_port   = 5432
    to_port     = 5432
    protocol    = "tcp"
    cidr_blocks = ["10.0.0.0/8"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

module "service_role" {
  source = "../../modules/iam-role"

  name            = "${local.service_name}-${var.environment}-role"
  trusted_service = "ecs-tasks.amazonaws.com"
  managed_policy_arns = [
    "arn:aws:iam::aws:policy/AmazonECSTaskExecutionRolePolicy",
  ]

  tags = {
    Service = local.service_name
  }
}
