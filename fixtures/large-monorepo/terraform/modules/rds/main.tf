resource "random_password" "master" {
  length  = 20
  special = false
}

resource "aws_db_subnet_group" "this" {
  name       = "${var.name}-subnet-group"
  subnet_ids = var.subnet_ids

  tags = merge(var.tags, { Name = "${var.name}-subnet-group" })
}

resource "aws_db_parameter_group" "this" {
  name_prefix = var.name
  family      = "postgres16"

  parameter {
    name  = "log_statement"
    value = "ddl"
  }

  parameter {
    name  = "log_min_duration_statement"
    value = "1000"
  }

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_db_instance" "this" {
  identifier              = var.name
  engine                  = "postgres"
  engine_version          = var.engine_version
  instance_class          = var.instance_class
  allocated_storage       = var.storage_gb
  storage_encrypted       = true
  multi_az                = var.multi_az
  db_subnet_group_name    = aws_db_subnet_group.this.name
  parameter_group_name    = aws_db_parameter_group.this.name
  vpc_security_group_ids  = var.vpc_security_group_ids
  username                = "rdsadmin"
  password                = random_password.master.result
  skip_final_snapshot     = !var.multi_az
  backup_retention_period = var.multi_az ? 30 : 7
  deletion_protection     = var.multi_az

  tags = merge(var.tags, { Name = var.name })
}
