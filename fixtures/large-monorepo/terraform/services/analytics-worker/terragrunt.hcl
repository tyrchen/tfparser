include "root" {
  path = find_in_parent_folders("root.hcl")
}

include "domain" {
  path           = find_in_parent_folders("common.terragrunt.hcl")
  merge_strategy = "deep_map_only"
}

dependency "orders" {
  config_path = "../order-service"

  mock_outputs = {
    events_bucket = "northwind-order-events-mock"
  }
}

inputs = {
  source_events_bucket = dependency.orders.outputs.events_bucket
}
