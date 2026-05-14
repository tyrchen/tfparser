locals {
  # Environment-level locals from terraform/environments/$env.terragrunt.hcl.
  # Drives account_id, region, terraform_role per environment.
  env_vars = read_terragrunt_config(
    "${get_repo_root()}/terraform/environments/${get_env("TF_VAR_environment", "staging")}.terragrunt.hcl"
  )

  # Domain-level locals (e.g. terraform/services/common.terragrunt.hcl) — optional.
  domain_vars = try(
    read_terragrunt_config(find_in_parent_folders("common.terragrunt.hcl")),
    { locals = {} }
  )

  # Domain × environment overrides — optional.
  domain_env_vars = try(
    read_terragrunt_config(find_in_parent_folders(
      "${get_env("TF_VAR_environment", "staging")}.terragrunt.hcl",
      "fallback.hcl"
    )),
    { locals = {} }
  )

  # Merge precedence (later wins): env < domain < domain-env.
  merged_vars = merge(
    local.env_vars.locals,
    local.domain_vars.locals,
    local.domain_env_vars.locals,
  )

  terraform_state_profile = "org-management-orgadmin"
  terraform_state_bucket  = "northwind-tfstate-100000000099"
  terraform_state_region  = "us-west-2"
}

# Every component receives these as `var.*` inputs.
inputs = {
  environment              = local.merged_vars.environment
  aws_region               = local.merged_vars.aws_region
  aws_account_id           = local.merged_vars.aws_account_id
  aws_main_profile         = local.merged_vars.aws_main_profile
  aws_data_profile         = lookup(local.merged_vars, "aws_data_profile", "northwind-data-developer")
  aws_security_profile     = lookup(local.merged_vars, "aws_security_profile", "northwind-security-developer")
  terraform_role_name      = local.merged_vars.terraform_role_name
  team                     = lookup(local.merged_vars, "team", "platform")
}

# Generated backend.tf — components do not declare backend themselves.
generate "backend" {
  path      = "generated_backend.tf"
  if_exists = "overwrite_terragrunt"
  contents  = <<EOF
terraform {
  backend "s3" {
    bucket       = "${local.terraform_state_bucket}"
    region       = "${local.terraform_state_region}"
    key          = "${local.merged_vars.aws_account_id}/${path_relative_to_include()}.tfstate"
    encrypt      = true
    use_lockfile = true
    profile      = "${local.terraform_state_profile}"
  }
}
EOF
}
