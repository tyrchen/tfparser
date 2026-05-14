output "audit_bucket" {
  description = "Audit log bucket name"
  value       = module.audit_logs.bucket_id
}

output "audit_bucket_arn" {
  description = "Audit log bucket ARN"
  value       = module.audit_logs.bucket_arn
}
