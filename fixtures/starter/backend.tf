terraform {
  backend "s3" {
    bucket       = "northwind-tfstate-100000000001"
    key          = "starter/terraform.tfstate"
    region       = "us-west-2"
    encrypt      = true
    use_lockfile = true
    profile      = "northwind-main-developer"
  }
}
