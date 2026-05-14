# starter — plain Terraform fixture

A single-component plain Terraform stack — no Terragrunt, no module nesting. Used by tfparser tests to verify the M0 path: discovery, HCL load, simple evaluator, parquet export.

## Layout

```
starter/
├── backend.tf           remote state backend
├── compute.tf           EC2 instance + ASG
├── iam.tf               IAM role + instance profile
├── locals.tf            common tags
├── main.tf              top-level data sources + module calls
├── network.tf           VPC + subnets + route tables
├── outputs.tf
├── providers.tf         single AWS provider
├── security.tf          security groups + KMS
├── storage.tf           S3 bucket + DynamoDB table
├── variables.tf
├── versions.tf
└── environments/
    ├── dev.tfvars
    └── prod.tfvars
```

## Expected parse output

- 1 component (`starter`)
- 0 modules referenced
- ~25 resources / data sources
- 2 environment variants (`dev`, `prod`)
- 1 AWS account (default provider only)
