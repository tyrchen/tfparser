locals {
  environment          = "staging"
  aws_account_id       = "100000000001"
  aws_region           = "us-west-2"
  aws_main_profile     = "northwind-main-developer"
  aws_data_profile     = "northwind-data-developer"
  aws_security_profile = "northwind-security-developer"
  terraform_role_name  = "iam-identity-role-terraform-user"
}
