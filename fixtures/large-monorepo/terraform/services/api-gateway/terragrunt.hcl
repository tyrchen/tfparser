include "root" {
  path = find_in_parent_folders("root.hcl")
}

include "domain" {
  path           = find_in_parent_folders("common.terragrunt.hcl")
  merge_strategy = "deep_map_only"
}

dependency "network" {
  config_path = "../../platform/main-network"

  mock_outputs = {
    vpc_id             = "vpc-mock"
    private_subnet_ids = ["subnet-mock-a", "subnet-mock-b"]
    public_subnet_ids  = ["subnet-mock-c", "subnet-mock-d"]
  }
}

inputs = {
  vpc_id             = dependency.network.outputs.vpc_id
  private_subnet_ids = dependency.network.outputs.private_subnet_ids
  public_subnet_ids  = dependency.network.outputs.public_subnet_ids
}
