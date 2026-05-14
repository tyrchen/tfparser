output "artifacts_bucket" {
  description = "Primary artifacts bucket"
  value       = module.artifacts.bucket_id
}

output "build_cache_bucket" {
  description = "Build cache bucket (us-east-1)"
  value       = module.build_cache_us_east_1.bucket_id
}
