locals {
  domain = "security"
  domain_tags = {
    Org    = "northwind"
    Domain = "security"
  }
}

inputs = {
  domain      = local.domain
  domain_tags = local.domain_tags
}
