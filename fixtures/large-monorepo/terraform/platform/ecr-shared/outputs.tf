output "repository_urls" {
  description = "Map of repository name to repository URL"
  value       = { for name, repo in module.repository : name => repo.repository_url }
}
