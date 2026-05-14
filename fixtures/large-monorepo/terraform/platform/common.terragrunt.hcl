locals {
  domain = "platform"
  domain_tags = {
    Org    = "northwind"
    Domain = "platform"
  }
}

inputs = {
  domain      = local.domain
  domain_tags = local.domain_tags
}
