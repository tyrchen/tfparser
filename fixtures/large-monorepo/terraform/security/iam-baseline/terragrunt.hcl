include "root" {
  path = find_in_parent_folders("root.hcl")
}

include "domain" {
  path           = find_in_parent_folders("common.terragrunt.hcl")
  merge_strategy = "deep_map_only"
}
