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

variable "source_events_bucket" { type = string }

variable "memory_mb" {
  type    = number
  default = 1024
}

variable "timeout_seconds" {
  type    = number
  default = 120
}
