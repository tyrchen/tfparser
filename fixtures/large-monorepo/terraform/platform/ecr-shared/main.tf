module "repository" {
  source = "../../modules/ecr-repo"

  for_each        = toset(var.repository_names)
  repository_name = each.key

  tags = {
    Service = "ecr-shared"
  }
}
