resource "aws_security_group" "app" {
  name        = "${local.name_prefix}-app-sg"
  description = "Application servers"
  vpc_id      = aws_vpc.main.id

  ingress {
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["10.0.0.0/8"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = {
    Name = "${local.name_prefix}-app-sg"
  }
}

resource "aws_kms_key" "storage" {
  description             = "Encryption key for starter storage"
  deletion_window_in_days = 30
  enable_key_rotation     = true

  tags = {
    Name = "${local.name_prefix}-storage-kms"
  }
}

resource "aws_kms_alias" "storage" {
  name          = "alias/${local.name_prefix}-storage"
  target_key_id = aws_kms_key.storage.key_id
}
