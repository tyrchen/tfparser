output "vpc_id" {
  description = "ID of the created VPC"
  value       = aws_vpc.this.id
}

output "vpc_cidr" {
  description = "Primary CIDR block of the VPC"
  value       = aws_vpc.this.cidr_block
}

output "private_subnet_ids" {
  description = "Private subnet IDs across the configured AZs"
  value       = aws_subnet.private[*].id
}

output "public_subnet_ids" {
  description = "Public subnet IDs across the configured AZs"
  value       = aws_subnet.public[*].id
}
