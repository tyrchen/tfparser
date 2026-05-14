variable "environment" { type = string }
variable "aws_region" { type = string }
variable "aws_account_id" { type = string }
variable "aws_main_profile" { type = string }
variable "aws_data_profile" { type = string }
variable "aws_security_profile" { type = string }
variable "terraform_role_name" { type = string }
variable "team" { type = string }
variable "domain" { type = string }
variable "domain_tags" { type = map(string) }

variable "vpc_id" { type = string }
variable "private_subnet_ids" { type = list(string) }

variable "db_instance_class" {
  type    = string
  default = "db.t3.micro"
}

variable "db_storage_gb" {
  type    = number
  default = 20
}
