output "ci_deploy_role_arn" {
  description = "CI deploy role ARN"
  value       = module.ci_deploy_role.role_arn
}

output "audit_reader_role_arn" {
  description = "Audit reader role ARN"
  value       = module.audit_reader_role.role_arn
}
