variable "environment" {
  description = "Deployment environment, one of dev | prod"
  type        = string
}

variable "region" {
  description = "AWS region"
  type        = string
  default     = "us-west-2"
}

variable "aws_profile" {
  description = "AWS profile to use for the default provider"
  type        = string
  default     = "northwind-main-developer"
}

variable "instance_type" {
  description = "EC2 instance type for the application server"
  type        = string
  default     = "t3.micro"
}

variable "instance_count" {
  description = "Number of EC2 instances in the ASG"
  type        = number
  default     = 1
}

variable "vpc_cidr" {
  description = "VPC primary CIDR block"
  type        = string
  default     = "10.20.0.0/16"
}

variable "az_count" {
  description = "How many availability zones to span"
  type        = number
  default     = 2
}

variable "team" {
  description = "Owning team for tagging"
  type        = string
  default     = "platform"
}
