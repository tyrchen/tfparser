# Research — Resolving AWS account / region / environment per resource

Status: memo · Date: 2026-05-13 · Owner: tfparser-core

## Question

Given a parsed component with multiple `provider "aws"` aliases and a resource that selects one via `provider = aws.<alias>`, how do we derive `(account_id, region, environment)` for that resource, source-only?

## The chain

```
resource
  └─ .provider = aws.<alias>
        ↓
provider "aws" { alias = <alias>; profile = "..."; region = "..." }
        ↓ (profile string is the join key)
External profile→account map
        ↓
account_id, account_name
```

Each link can fail; the parser **must degrade gracefully** and emit empty strings on the columns rather than refusing to write a row.

## Step 1 — provider alias resolution

For each resource:
1. If `provider = aws.<alias>`, look up `<alias>` in the component's provider table.
2. If absent, use the default `provider "aws" {}` (no alias).
3. If multiple unaliased `aws` providers exist (illegal in Terraform but we tolerate), pick the first encountered and warn.

The provider table is built during component parsing — one entry per `provider "aws"` block, keyed by alias (or empty string for default).

## Step 2 — provider attribute evaluation

The provider block's `region` and `profile` are themselves HCL expressions:

```hcl
provider "aws" {
  alias   = "main"
  region  = var.region
  profile = var.aws_main_profile
}
```

We evaluate them in the component's normal evaluation context. Typically:
- `var.region` resolves from `environments/<env>.tfvars` or Terragrunt-cascade locals.
- `var.aws_main_profile` resolves to a string like `"main-developer"`.

If evaluation fails (variable not provided), the resolved value is `Unresolved`. Account resolution then yields `""`.

## Step 3 — profile → account map (external input)

We don't try to derive account IDs from the repo (some monorepos do encode them in a `security/aws_accounts/` style component, but that's a per-org convention, not something the parser can rely on). Instead, the user provides one of:

- `--profiles ~/.aws/config` — we parse the AWS shared config and extract `[profile <name>]` blocks. Account ID is taken from `sso_account_id` if present, else from `role_arn` (`arn:aws:iam::<id>:role/...`).
- `--profile-map path/to/profiles.yaml` — explicit user-supplied map:
  ```yaml
  profiles:
    main-developer:        { account_id: "370025973162", account_name: "main" }
    softwaremansion-developer: { account_id: "XXXXXXXXXXXX", account_name: "softwaremansion" }
  ```
- Nothing — `account_id` column stays empty. The parser still emits valid Parquet.

Both inputs are loaded into an `ArcSwap<ProfileMap>` and queried lock-free during parquet emission.

## Step 4 — region inference

In priority order:
1. The provider block's explicit `region` (after evaluation).
2. The Terragrunt cascade's `aws_region` local (if reachable from `root.hcl`).
3. The component's `default.tfvars` / `<env>.tfvars` for a `region` variable.
4. Empty.

## Step 5 — environment inference

Environment is the value of `var.environment` (or, in Terragrunt land, `TF_VAR_environment` / `get_env("TF_VAR_environment")`):
1. CLI flag `--environment staging` is the strongest source. With it set, the evaluator's `Context::declare_var("environment", ...)` and `get_env` answers it.
2. Without the flag, we run the parser **once per environment discovered** in `terraform/environments/*.terragrunt.hcl` and emit rows per `(component, environment)` pair. The Parquet schema includes the `environment` column for exactly this reason.
3. If no environments dir exists, environment is `""` and resources are emitted once.

## Edge cases observed in the reference repo

1. **Provider with no profile** (default). Account inferred from `aws_account_id` local in `environments/<env>.terragrunt.hcl` via the Terragrunt cascade.
2. **Hard-coded profile in `backend "s3" {}`** (e.g. `profile = "organization-management-orgadmin"` in `aws_accounts/backend.tf`). The component-level state-backend account is independent of the data-plane provider account; we capture both:
   - `state_account_id` / `state_region` / `state_bucket` from the `backend "s3"` block (or the `generate "backend"` contents).
   - `account_id` / `region` per resource from its resolved provider.
3. **Profile passed as variable** (`profile = var.aws_main_profile`). Common — handled by Step 2.
4. **Cross-account roles** (`assume_role { role_arn = "arn:aws:iam::<id>:role/..." }` in a provider block). We extract the `account_id` from the role ARN regex — that overrides the profile-derived account.

## Resolution algorithm (final)

```text
resolve_account(resource) =
  let provider = resource.provider_ref or default
  let profile  = eval(provider.profile)              # may be Unresolved
  let region   = eval(provider.region) or cascade.aws_region or ""

  let account_id =
       extract_from_assume_role(provider)          ?:    # highest priority
       map.lookup(profile)                          ?:
       cascade.aws_account_id                       ?:
       ""

  return (account_id, region, env)
```

## Risks retired

- **R10 — "Can we resolve account per resource, not per component?"** Yes — provider alias is per-resource.
- **R11 — "Will profiles missing from the map crash the parser?"** No, they yield empty columns and a warning per missing profile (deduped, logged once).

## Open risks

- **R-OPEN-7**: Some teams use `data "aws_caller_identity" "current" {}` and reference `.account_id` later. Source-only, we cannot resolve `data.aws_caller_identity.current.account_id`. Captured as `Unresolved` in `attributes_json`; not affecting the `account_id` column.

## Configuration shape

Spec'd in detail in [16-provider-resolver.md](../../specs/16-provider-resolver.md). Summary:

```toml
# tfparser.toml (workspace-local override; CLI flag wins)
repo_root = "./terraform"
default_environment = "staging"

[profile_map]
source = "aws-config"            # | "file" | "none"
path   = "~/.aws/config"

[evaluator]
env_mode = "strict"              # | "passthrough" | "mock"
allowed_env = ["TF_VAR_environment", "AWS_REGION"]
max_include_depth = 32
```
