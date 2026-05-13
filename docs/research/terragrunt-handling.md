# Research — Terragrunt configuration handling, source-only

Status: memo · Date: 2026-05-13 · Owner: tfparser-core

## Question

Terragrunt overlays HCL on top of Terraform with its own functions (`include`, `find_in_parent_folders`, `read_terragrunt_config`, `generate`, `get_env`, `path_relative_to_include`, …) and a config-merging semantics. How much do we mimic, source-only, without invoking the `terragrunt` binary?

## What Terragrunt does at runtime

1. **Loads** the component's `terragrunt.hcl`.
2. **Resolves `include`s** by walking up the directory tree (`find_in_parent_folders`).
3. **Merges** locals/inputs/generate blocks from included files into the current scope, with documented precedence (child overrides parent).
4. **Generates** files (`generate "backend" { ... }`) into the component's `.terragrunt-cache/<hash>/` directory.
5. **Resolves `dependency.<name>.outputs`** by running `terragrunt output -json` in the dependency's directory.
6. **Calls `terraform init` / `plan` / `apply`** in the generated working directory.

Source-only, we do **(1)–(4)** by mimicking them in the evaluator. We skip **(5)** (it requires running Terragrunt) and **(6)** entirely.

## Functions we must implement as evaluator built-ins

(`fn` here means a `FuncDef` registered in the `hcl-rs::eval::Context`; signature is HCL types.)

| Function | Signature | Behaviour |
| -------- | --------- | --------- |
| `find_in_parent_folders(name?, fallback?)` | `(string?, string?) -> string` | Walk up from `.terragrunt_dir()` looking for `name` (default `terragrunt.hcl`). Return absolute path; error if not found and no fallback. |
| `find_in_parent_folders_from(start, name?)` | `(string, string?) -> string` | Same but start from given dir. |
| `read_terragrunt_config(path, default?)` | `(string, any?) -> object` | Parse the given `terragrunt.hcl` and return an object with `locals`, `inputs`, etc. Recursive — uses the same evaluator. |
| `path_relative_to_include()` | `() -> string` | Path of the current `terragrunt.hcl` relative to the included root. |
| `path_relative_from_include()` | `() -> string` | Inverse of above. |
| `get_terragrunt_dir()` | `() -> string` | Absolute dir of the current `terragrunt.hcl`. |
| `get_repo_root()` | `() -> string` | Workspace root (we supply this from CLI). |
| `get_parent_terragrunt_dir(name?)` | `(string?) -> string` | Dir of the nearest parent `terragrunt.hcl`. |
| `get_env(name, default?)` | `(string, string?) -> string` | Read environment variable. **Behavior controlled by `--env-mode`**: `passthrough` (default — use process env), `strict` (only allow names in `--allowed-env`), `mock` (return `default` for everything). |
| `get_terraform_commands_that_need_vars()` | `() -> tuple<string>` | Hardcoded list, matches upstream. |
| `strcontains(s, substr)` | `(string, string) -> bool` | Standard. |
| `lookup(map, key, default?)` | Same as HCL stdlib | Implemented. |
| `merge(...)` | Same as HCL stdlib | Implemented (already in `hcl-rs` stdlib). |
| `jsonencode`, `jsondecode`, `format`, `formatlist`, `replace`, `lower`, `upper`, `trimspace`, `length`, `keys`, `values`, `tolist`, `toset`, `tomap`, `concat`, `compact`, `coalesce`, `coalescelist`, `try`, … | Standard HCL stdlib | Already provided by `hcl-rs`, kept as-is. |

`try(...)` is **load-bearing** for Terragrunt configs (a common pattern uses it to make `domain_vars` / `domain_env_vars` lookups optional). We do not let it swallow real parser errors — `try` returns the fallback only when expression evaluation fails with a *symbolic-resolution* error, not when the file is malformed.

## `include` block semantics

```hcl
include "root" {
  path = find_in_parent_folders("root.hcl")
}
```

The semantics we mimic:

1. Resolve `path` (call the function).
2. Parse the resolved file as Terragrunt.
3. **Deep-merge** `locals`, `inputs`, `generate`, `dependency` blocks into the current scope. Child wins on conflicts (Terragrunt default).
4. If the include has `merge_strategy = "no_merge" | "shallow" | "deep" | "deep_map_only"`, honour it.
5. Recurse: included files may themselves `include`.

The evaluator maintains an **include stack** and a `[load_path]` for diagnostics. Cycles are rejected with a clear error message.

## `generate` block

```hcl
generate "backend" {
  path      = "generated_backend.tf"
  if_exists = "overwrite_terragrunt"
  contents  = <<EOF ... EOF
}
```

The parser **does not write** the file; it captures the block as a `GeneratedFile { path, contents, if_exists }` field on the component. Downstream consumers can synthesize a virtual file if needed. M4 (provider/account resolution) reads `contents` for backend `key` / `profile` to extract account context.

## Configuration cascade (binding)

For a component at `<repo>/terraform/<domain>/<component>/terragrunt.hcl`, the effective locals are computed in this order (later wins):

1. `<repo>/terraform/environments/${TF_VAR_environment}.terragrunt.hcl` → `env_vars.locals`
2. `<repo>/terraform/<domain>/common.terragrunt.hcl` → `domain_vars.locals` (optional, `try`-wrapped)
3. `<repo>/terraform/<domain>/${TF_VAR_environment}.terragrunt.hcl` → `domain_env_vars.locals` (optional)
4. Component's own `terragrunt.hcl` → `locals`

A representative `root.hcl` codifies (1)–(3) explicitly. The parser **does not** hardcode this — it reads `root.hcl` and follows the explicit `merge(...)` call there. (The cascade is real but emerges from the user's `root.hcl`, not from us.)

## Implementation notes

- The evaluator runs **per component**, with a fresh `Context` seeded from CLI flags (`environment`, `repo_root`) plus a closed-over file-system handle.
- Reading a parent `terragrunt.hcl` is **memoised** by absolute-resolved path; we never re-parse the same file twice.
- File-system access is sandboxed: paths are resolved relative to `repo_root` and **must canonicalize to a descendant of it**. Symlink escapes are rejected. See [70-security.md](../../specs/70-security.md).
- The whole evaluator is `Send + Sync` so we can parallelise components with `rayon`.

## Risks retired

- **R8 — "Will mimicking Terragrunt drift from upstream?"** Some, eventually. We accept this: our goal is best-effort source parsing for visualization, not a Terragrunt re-implementation. We document the subset; users who need exact fidelity can pre-render with `terragrunt render-json` and feed that in (future `--from-render-json` flag).
- **R9 — "Will cycles or huge `read_terragrunt_config` chains DoS us?"** Cycle detection in the evaluator + a max-include-depth (default 32) cap.

## Open risks

- **R-OPEN-6**: `dependency "x" { config_path = "..." }` blocks reference *outputs* of other components. We can compute the **dependency edges** (component-to-component) source-only, but cannot resolve the actual output *values* without running Terragrunt. We emit the edge into the dependency graph and leave the value `Unresolved("dependency.x.outputs.y")`.

## References

- [Terragrunt function reference](https://terragrunt.gruntwork.io/docs/reference/hcl/functions/)
- Issue threads about `find_in_parent_folders` semantics: [#1719](https://github.com/gruntwork-io/terragrunt/issues/1719), [#2865](https://github.com/gruntwork-io/terragrunt/issues/2865)
- A representative `root.hcl` cascade pattern is documented in the [Terragrunt RFC for `dependency`/`include`](https://terragrunt.gruntwork.io/docs/reference/hcl/blocks/#include) and the `find_in_parent_folders` reference linked above.
