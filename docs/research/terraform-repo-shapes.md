# Research — Real-world Terraform repository shapes

Status: memo · Date: 2026-05-13 · Owner: tfparser-core

## Question

What structural patterns must the parser handle? We use a **reference-scale Terragrunt monorepo** as the calibration point (anonymised numbers: ~4 600 `.tf|.hcl|.tfvars` files, ~320 k LOC of TF, ~250 components, ~60 modules, ~10 AWS accounts via provider aliases). The design must generalise; numbers below describe the *shape*, not any specific company.

## Observed shapes

### Top-level domains (per-team groupings)

```
terraform/
├── environments/         # global env-level vars: aws_account_id, aws_region, profile
├── live-site/            # per-team service components (largest folder; ~100 components)
├── platform/             # infra-shared components (clusters, datadog, IAM)
├── networks/             # VPCs, subnets, peering
├── security/             # IAM, identity center, organisation
├── product/              # product-specific
├── sandbox-clusters/     # ephemeral / per-user
├── tools/                # tooling components
├── modules/              # reusable TF v0.10-safe modules (legacy)
├── modules-tf12/         # reusable TF v0.12+ modules (current)
├── files/                # shared file assets (policies, scripts)
└── root.hcl              # Terragrunt root config
```

**Generalisation**: a Terraform "workspace" has:
1. Zero or more **component directories** (a leaf that is `terraform apply`-able).
2. Zero or more **module directories** (referenced only via `source = "..."`, never applied directly).
3. Zero or one **Terragrunt root** (`root.hcl` or `terragrunt.hcl` at the top).
4. Zero or more **environment-level config files** (typically `environments/*.tfvars` or `environments/*.terragrunt.hcl`).

We must distinguish (1) from (2) by inspection. Heuristics, in order:
- A dir contains `terragrunt.hcl` with an `include` of a parent `root.hcl` → component.
- A dir contains `backend "s3" {}` or `terraform { backend ... }` → component.
- A dir is reachable from any other dir's `module "x" { source = "./..." }` → module.
- A dir sits under a folder named `modules` / `modules-tf12` / `modules-*` → module.
- Otherwise: component (with a warning).

### Per-component shape

```
ads-pacer/
├── terragrunt.hcl                  # tiny: just `include "root"`
├── versions.tf                     # required_version, required_providers
├── providers.tf                    # multiple `provider "aws"` aliases
├── vars.tf                         # variable declarations
├── locals.tf                       # team_tags, derived locals
├── pacer_db.tf                     # resources + module calls
├── ecr.tf                          # more resources
├── dev.tf, ...                     # split by topic
├── outputs.tf
└── environments/
    ├── staging.tfvars
    └── production.tfvars
```

All `.tf` files in a component dir form a single HCL "body" by Terraform's own semantics — order is irrelevant. The parser concatenates them logically (preserving per-file spans).

### Multi-provider components

Real components have **multiple `provider "aws"` blocks with `alias = "..."`**, each with a different `profile` and/or `region`. A single component can deploy to **3+ AWS accounts** via aliases:

```hcl
provider "aws" { region = var.region }                              # default
provider "aws" { alias = "main"; profile = var.aws_main_profile }   # main account
provider "aws" { alias = "us-east-2"; region = "us-east-2"; profile = var.aws_main_profile }
provider "aws" { alias = "softwaremansion_us_east_2"; profile = "softwaremansion-developer" }
```

Resources opt in via `provider = aws.<alias>`:

```hcl
resource "aws_db_instance" "x" { provider = aws.main; ... }
resource "aws_iam_role" "y"   { provider = aws.softwaremansion_us_east_2; ... }
```

**Implication**: a "component owns account_id" assumption is wrong. The unit of account is the **resource**, not the component.

### Terragrunt configuration cascade

`root.hcl` reads `environments/${TF_VAR_environment}.terragrunt.hcl` for **account-level locals** (`aws_account_id`, `aws_region`, profile, role) and **generates** the backend.tf block. The merge order is:

```
domain_env_vars (highest)     # terraform/<domain>/$env.terragrunt.hcl
domain_vars                   # terraform/<domain>/common.terragrunt.hcl
env_vars       (lowest)       # terraform/environments/$env.terragrunt.hcl
```

A component's effective inputs are this merge plus its own `environments/$env.tfvars`. To resolve `var.environment` source-only, we **mimic the cascade** but never invoke Terragrunt.

### Conditional / templated resources

Common patterns we must handle gracefully (= capture, do not fail):
- `count = var.environment == "production" ? 1 : 0` — conditional resource.
- `for_each = { for n, v in local.aws_accounts : n => v if v.dns_zone != null }` — dynamic expansion.
- `provider = aws.<alias>` — late binding.
- `module "foo" { source = "../../modules-tf12/rds"; ... }` — local module references.

### Module source forms

| Form | Example | Resolvable source-only? |
| ---- | ------- | ----------------------- |
| Local path | `../../modules-tf12/rds` | Yes — just `Path::join` and walk. |
| Git registry | `git::https://...//module?ref=...` | No (would need clone). Capture as `external`. |
| Terraform registry | `terraform-aws-modules/eks/aws` | No. Capture as `external`. |
| Local with version (workspace-style) | `./modules/foo` | Yes. |

In the surveyed reference repo every observed `source` is a relative path; the design supports the other forms as `external` placeholders.

## Generalisation principles (binding)

1. **A workspace is a directory tree.** No assumption about top-level folder names. The user supplies a root path; everything is derived.
2. **A component is identified by `terragrunt.hcl` or `terraform { backend ... }`.** Both heuristics run; either suffices.
3. **A module is identified by being referenced via `source = "..."` from a component or another module, OR by sitting under a `modules*/` folder.** Modules are not parsed unless reached.
4. **Environments are first-class but optional.** A repo with no `environments/` directory still works; resources just lack environment-specific resolution.
5. **The parser is read-only.** It never writes back to the TF repo.

## Risks retired

- **R6 — "Can we model multi-account-per-component?"** Yes, with provider-alias resolution per resource.
- **R7 — "Will Terragrunt block source-only parsing?"** No — we mimic `include` / `find_in_parent_folders` / `read_terragrunt_config` / `merge` as custom evaluator funcs against the file system; we never invoke the `terragrunt` binary.

## Open risks

- **R-OPEN-4**: `generate "backend" { contents = <<EOF ... EOF }` produces a `generated_backend.tf` at apply time. Our parser does **not** materialise these files (read-only). We capture the `generate` block as metadata so the IR knows a virtual file would exist.
- **R-OPEN-5**: Repos that mix Terraform + Pulumi + CDK in one tree. Out of scope; we ignore unknown file types.

## Sample inventory

| Bucket | Count (reference repo) |
| ------ | ----------------- |
| `.tf` files | ~3800 |
| `.tfvars` files | ~600 |
| `.hcl` files (Terragrunt) | ~200 |
| `.json` (policy fragments) | ~50 |
| TF LOC total | ~322k |
| Components (live-site/platform/networks/security/…) | ~250 |
| Modules (modules + modules-tf12) | ~60 |
