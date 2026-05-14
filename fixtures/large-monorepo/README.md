# large-monorepo — reference-scale Terragrunt fixture

A synthetic Terraform + Terragrunt monorepo representative of the patterns documented in
[../../docs/research/terraform-repo-shapes.md](../../docs/research/terraform-repo-shapes.md).
Used as the **anchor fixture** for tfparser milestone exit criteria — every milestone's
exit criterion includes "parses `large-monorepo` correctly."

## What it exercises

- **Terragrunt cascade**: env-level + domain-level + domain-env-level locals merged through
  `read_terragrunt_config` + `merge` in `terraform/root.hcl`.
- **Multiple AWS accounts** referenced via provider aliases (`aws.main`, `aws.data`,
  `aws.security`) — every component opts into the providers it needs.
- **Module reuse**: 6 local modules under `terraform/modules/`, each consumed by 1–3
  components. One module (`rds`) is consumed by two components against *different*
  provider aliases — exercises the provider-rewrite path in
  [../../specs/15-resource-graph.md § 3.2](../../specs/15-resource-graph.md).
- **Per-environment overrides**: `terraform/environments/*.terragrunt.hcl` (account/region
  per env) plus per-component `environments/*.tfvars` (component-specific tuning).
- **Domain split**: `platform/`, `services/`, `security/` each with their own
  `common.terragrunt.hcl` injecting domain tags / defaults.
- **`generate "backend"`**: components do not declare `backend "s3" {}` in `.tf`; the
  block is generated from `root.hcl` at apply time, and tfparser captures it via the
  Terragrunt resolver.

## Layout

```
large-monorepo/
└── terraform/
    ├── root.hcl                          # Terragrunt root: cascade + generated backend
    ├── environments/
    │   ├── staging.terragrunt.hcl        # account_id, region for staging
    │   ├── production.terragrunt.hcl     # account_id, region for production
    │   └── default.tfvars                # shared defaults
    ├── modules/                          # reusable local modules
    │   ├── vpc/                          # 4 files: main, variables, outputs, versions
    │   ├── rds/
    │   ├── s3-bucket/
    │   ├── ecr-repo/
    │   ├── iam-role/
    │   └── lambda/
    ├── platform/                         # platform-team components
    │   ├── common.terragrunt.hcl
    │   ├── main-network/                 (terragrunt-wrapped)
    │   ├── shared-buckets/
    │   └── ecr-shared/
    ├── services/                         # app-team components
    │   ├── common.terragrunt.hcl
    │   ├── api-gateway/
    │   ├── order-service/                (uses rds + s3-bucket + iam-role)
    │   └── analytics-worker/             (uses lambda + iam-role; cross-account)
    └── security/
        ├── common.terragrunt.hcl
        ├── iam-baseline/
        └── audit-bucket/                 (state in security account)
```

## Account fixtures (fictional)

| Profile                       | Account ID     | Region    | Purpose                |
| ----------------------------- | -------------- | --------- | ---------------------- |
| `northwind-main-developer`    | `100000000001` | us-west-2 | Primary workload account |
| `northwind-data-developer`    | `100000000002` | us-east-1 | Analytics / data plane |
| `northwind-security-developer`| `100000000003` | us-west-2 | Audit / log archive    |
| `org-management-orgadmin`     | `100000000099` | us-west-2 | State backend host     |

## Expected parse output (oracle)

- ~14 components, 6 modules
- ~60 resources / data sources post-module-expansion
- 2 environments (`staging`, `production`)
- 3 distinct `account_id`s populated when `--profile-map` is supplied
- 1 state account (`100000000099`) on every component
