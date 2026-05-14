output "db_instance_id" {
  description = "ID of the RDS instance"
  value       = aws_db_instance.this.id
}

output "db_endpoint" {
  description = "Connection endpoint for the RDS instance"
  value       = aws_db_instance.this.endpoint
}

output "db_port" {
  description = "Port the RDS instance listens on"
  value       = aws_db_instance.this.port
}

output "db_subnet_group" {
  description = "Name of the DB subnet group"
  value       = aws_db_subnet_group.this.name
}
