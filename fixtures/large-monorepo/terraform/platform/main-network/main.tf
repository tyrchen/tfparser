module "vpc" {
  source = "../../modules/vpc"

  name     = var.vpc_name
  cidr     = var.vpc_cidr
  az_count = var.az_count

  tags = {
    Environment = var.environment
    Service     = "main-network"
  }
}

resource "aws_security_group" "shared_bastion" {
  name        = "${var.vpc_name}-bastion-sg"
  description = "Allow SSH from the corporate CIDR"
  vpc_id      = module.vpc.vpc_id

  ingress {
    from_port   = 22
    to_port     = 22
    protocol    = "tcp"
    cidr_blocks = ["172.16.0.0/12"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}
