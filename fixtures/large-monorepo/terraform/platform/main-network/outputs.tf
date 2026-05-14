output "vpc_id" {
  description = "Main platform VPC ID"
  value       = module.vpc.vpc_id
}

output "private_subnet_ids" {
  description = "Private subnets across AZs"
  value       = module.vpc.private_subnet_ids
}

output "public_subnet_ids" {
  description = "Public subnets across AZs"
  value       = module.vpc.public_subnet_ids
}

output "bastion_security_group_id" {
  description = "Bastion security group"
  value       = aws_security_group.shared_bastion.id
}
