include "root" {
  path = find_in_parent_folders("root.hcl")
}

include "domain" {
  path           = find_in_parent_folders("common.terragrunt.hcl")
  merge_strategy = "deep_map_only"
}

inputs = {
  vpc_name = "platform-main-${include.root.inputs.environment}"
  vpc_cidr = include.root.inputs.environment == "production" ? "10.40.0.0/16" : "10.41.0.0/16"
}
