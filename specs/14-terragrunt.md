# 14 — Terragrunt Support

Status: draft v1 · Owner: tfparser-core · Depends on: [13-evaluator.md](./13-evaluator.md), [12-hcl-loader.md](./12-hcl-loader.md)

## 1. Purpose

Make a Terragrunt-orchestrated repo parse end-to-end source-only. Mimic the subset of Terragrunt that affects **what HCL the component effectively sees**: `include`, `find_in_parent_folders`, `read_terragrunt_config`, locals/inputs cascade, `generate`, `dependency`. We do **not** run the `terragrunt` binary; we do **not** invoke providers.

## 2. Interface

```rust
// crates/core/src/terragrunt/mod.rs
pub trait TerragruntResolver: Send + Sync {
    fn resolve(&self, dir: &DiscoveredDir, root: &Discovered, ctx: &TgContext) -> Result<TerragruntConfig>;
}

pub struct FsTerragruntResolver;

pub struct TgContext {
    pub workspace_root: Arc<Path>,
    pub environment:    Option<Arc<str>>,
    pub env_var_mode:   EnvVarMode,
    pub allowed_env:    Arc<BTreeSet<Arc<str>>>,
    pub max_include_depth: u32,                 // default 32
}

pub struct TerragruntConfig {
    pub component_dir:  Arc<Path>,
    pub effective_locals: Map,                  // post-cascade, post-merge
    pub inputs:           Map,                  // resolved `inputs { ... }`
    pub includes:         Vec<IncludePath>,     // load chain, deepest last
    pub generates:        Vec<GenerateBlock>,
    pub dependencies:     Vec<DependencyBlock>,
    pub state_backend:    Option<StateBackend>, // from `remote_state` or extracted from a `generate "backend"` block
    pub diagnostics:      Vec<Diagnostic>,
}
```

The output `effective_locals` is what the evaluator (see [13-evaluator.md](./13-evaluator.md)) plugs into `EvalContext::cascade_locals` when processing the same component's `.tf` files. **Terragrunt's job is to shape the evaluator's context**, not to evaluate Terraform.

## 3. Behaviour

### 3.1 Load chain

For component `<repo>/<dir>/terragrunt.hcl`:

1. Parse it with [the HCL loader](./12-hcl-loader.md) (Terragrunt is HCL2).
2. For each `include "name" { path = <expr>; merge_strategy? = <expr>; expose? = <expr> }`:
   - Evaluate `path` with a *minimal* context that has the Terragrunt funcs available (`find_in_parent_folders`, `get_terragrunt_dir`, `get_repo_root`).
   - Canonicalise the resolved path; reject if outside `workspace_root`.
   - Recurse: load the included Terragrunt file with depth+1.
   - Detect cycles via include-stack inspection; reject with `Error::Cycle`.
3. The resulting **load chain** is a list of parsed-Terragrunt-bodies; the *child* (component's own `terragrunt.hcl`) is last.

### 3.2 Merge strategies

Per Terragrunt docs (`include.merge_strategy`):

- `deep_map_only` (the new default in Terragrunt ≥ 0.45) — deep-merge maps, leave non-map values from the child winning.
- `deep` — deep-merge everything, including lists (concatenate).
- `shallow` — only top-level keys; child wins on any conflict.
- `no_merge` — include the parent for *reading via `read_terragrunt_config`* but do not merge it.

Our default mirrors Terragrunt's: `deep_map_only`. The merge runs deepest-first; the child sees the merged result.

### 3.3 Function set

Implementations of every Terragrunt-specific function listed in [terragrunt-handling.md](../docs/research/terragrunt-handling.md). Each function is a `FuncDef` registered on a per-resolution `hcl::eval::Context`.

`find_in_parent_folders(name = "terragrunt.hcl", fallback?)`:
- Start from `get_terragrunt_dir()`.
- Walk up the directory tree (using canonical paths).
- Return the first absolute path whose basename matches `name`.
- If hit the FS root: return `fallback` if provided; else `Error::Func` ("not found").

`find_in_parent_folders_from(start, name?)`: same, with explicit start.

`read_terragrunt_config(path, fallback?)`:
- Canonicalise `path` against `workspace_root`; reject on escape.
- Parse the target Terragrunt file with the *same evaluator+resolver*, **memoised** by canonical path.
- Memo: a `dashmap::DashMap<Arc<Path>, Arc<ResolvedTerragrunt>>` per resolution run. A second call with the same path returns the cached body. Per CLAUDE.md § Async & Concurrency, `DashMap` is correct here over `Mutex<HashMap>`.
- Detect re-entrant cycles via a per-thread include-stack `Vec<Arc<Path>>`. (We don't expose the same parse on two threads at once for the same file because the memo enforces single-flight via `dashmap::Entry::or_insert_with` style.)
- Return value: an HCL object with the parent's `locals`, `inputs`, `terraform`, `dependency` blocks accessible.

`path_relative_to_include()` / `path_relative_from_include()`: computed against the *most recently active* include's source path. The resolver maintains an include stack and passes the active include to the funcs.

`get_env(name, default?)`: behaviour governed by `TgContext::env_var_mode` (same as evaluator's; see [13-evaluator.md § 5](./13-evaluator.md#functions-registered)).

`get_repo_root()`: returns `workspace_root` (configured at parser launch). We do **not** call out to `git rev-parse --show-toplevel` — that would require `git` on the PATH and break determinism.

### 3.4 The cascade in practice

Given a typical `root.hcl` of the shape captured in [terragrunt-handling.md § 4](../docs/research/terragrunt-handling.md#configuration-cascade-binding):

```hcl
env_vars         = read_terragrunt_config("${get_repo_root()}/terraform/environments/${get_env("TF_VAR_environment")}.terragrunt.hcl")
domain_vars      = try(read_terragrunt_config(find_in_parent_folders("common.terragrunt.hcl")), { locals = {} })
domain_env_vars  = try(read_terragrunt_config(find_in_parent_folders("${get_env("TF_VAR_environment")}.terragrunt.hcl")), { locals = {} })
merged_vars      = merge(env_vars.locals, domain_vars.locals, domain_env_vars.locals)
```

Resolution at a component:
1. The component's `terragrunt.hcl` `include`s `root.hcl`.
2. We load `root.hcl`, which calls `read_terragrunt_config` and `merge` — these are our functions, evaluated against the FS.
3. `merged_vars` becomes a concrete map (assuming `TF_VAR_environment` is set or default-provided by CLI flag).
4. `inputs { ... }` references `local.merged_vars.aws_region` etc., and those evaluate to literals.
5. `TerragruntConfig.effective_locals = merged_vars`; `TerragruntConfig.inputs` = the resolved inputs.

When the evaluator later runs on the component's `.tf` files, `cascade_locals` includes `effective_locals`, and `repo_vars` includes `inputs`. Variables like `var.aws_region` reduce to `"us-east-2"` and the provider's `region` reduces accordingly.

### 3.5 `generate` blocks

```hcl
generate "backend" {
  path      = "generated_backend.tf"
  if_exists = "overwrite_terragrunt"
  contents  = <<EOF
    terraform { backend "s3" { ... } }
  EOF
}
```

We **do not** write the file. We capture `GenerateBlock { path, if_exists, contents }`. The contents string is **also parsed as HCL** (via the HCL loader, sub-loader call) and any `terraform { backend "s3" { ... } }` it contains contributes to `TerragruntConfig.state_backend`. This is how components that don't declare `backend "s3"` in their `.tf` files — relying on a root-level `generate "backend"` to inject it — still produce a `state_account_id` / `state_region` row.

### 3.6 `dependency` blocks

```hcl
dependency "vpc" { config_path = "../networks/main-k8s-network" }
```

Captured as `DependencyBlock { name: "vpc", config_path: <abs>, mock_outputs?: Map }`. We **do not** run `terragrunt output` to resolve the dependency's actual outputs. We emit a component-to-component edge into the dependency graph at the graph phase ([15-resource-graph.md](./15-resource-graph.md)). References like `dependency.vpc.outputs.subnet_ids` stay `Unresolved` in the consumer.

If `mock_outputs` is provided, **those** values flow into the evaluator context as if they were the dependency outputs (mirroring Terragrunt's own `mock_outputs` behaviour). Useful for downstream demos / CI.

## 4. Configuration cascade (binding)

The cascade is **not** hardcoded into the resolver. It emerges from whatever the user's `root.hcl` does with `read_terragrunt_config` and `merge`. A representative pattern (env-vars / domain-vars / domain-env-vars merge chain) is documented in [terragrunt-handling.md § 4](../docs/research/terragrunt-handling.md#configuration-cascade-binding). Other repos with simpler conventions (no domain split) just see fewer files in the merge chain — same machinery.

## 5. Invariants

- **I-TG-1**: All paths resolved by Terragrunt functions canonicalize underneath `workspace_root` or error out. Defence against `../../../etc/passwd` in a malicious `terragrunt.hcl`.
- **I-TG-2**: The include stack depth is bounded by `max_include_depth`. Cycles are rejected with the full stack in the error.
- **I-TG-3**: `read_terragrunt_config` is memoised by canonical path; no file is parsed twice in one resolution run.
- **I-TG-4**: `TerragruntConfig.effective_locals` is deterministic given the same FS state and `TgContext`.
- **I-TG-5**: The resolver is `Send + Sync` and runs alongside other components in `rayon::par_iter`. Per-thread mutable state (include stack) lives inside a `thread_local!` or on the stack.

## 6. Error model

```rust
#[derive(thiserror::Error, Debug)]
pub enum TerragruntError {
    #[error("include cycle: {0:?}")]
    Cycle(Vec<Arc<Path>>),

    #[error("include depth limit ({limit}) exceeded")]
    DepthExceeded { limit: u32 },

    #[error("path escape: {path}")]
    PathEscape { path: Arc<Path> },

    #[error("function `{func}`: {message}")]
    Func { func: &'static str, message: Box<str>, span: Span },

    #[error("evaluator: {0}")]
    Eval(#[from] EvalError),

    #[error("loader: {0}")]
    Load(#[from] LoaderError),
}
```

## 7. Performance

- Memoisation of `read_terragrunt_config` is load-bearing: at reference scale, ~250 components each include `root.hcl`, which `read_terragrunt_config`s `environments/$env.terragrunt.hcl`. Without memo, that's 500 redundant parses per run.
- Per-component resolver overhead target: ≤ 5 ms (one fresh `terragrunt.hcl` + one cached parent `root.hcl` + a memo'd `env.terragrunt.hcl`).

## 8. Testing

- Golden tests: `crates/core/tests/terragrunt/<case>/` with a tiny FS fixture (`root.hcl`, `common.terragrunt.hcl`, `staging.terragrunt.hcl`, component `terragrunt.hcl`) and an `expected.json` of `effective_locals`.
- Cycle: `a.hcl` includes `b.hcl` includes `a.hcl` → `Error::Cycle`.
- Path escape: `find_in_parent_folders("../../../etc/passwd")` → `Error::PathEscape`.
- Memo: parse a fixture, count `read_terragrunt_config` calls (instrument via tracing), assert ≤ 1 per distinct path.
- **Reproduce a representative cascade** in a 10-component fixture: assert `effective_locals` matches a hand-curated oracle for `staging` and `production`.

## 9. CLAUDE.md anchoring

- **Errors**: `thiserror`, `#[from]` chains from `EvalError`/`LoaderError`.
- **Security**: every path under `workspace_root`; `get_env` gated by `env_var_mode`.
- **Concurrency**: `dashmap` for the memo (per CLAUDE.md § Async & Concurrency).
- **Type design**: `Arc<Path>` shared across the memo; `Arc<BTreeSet<Arc<str>>>` for allowed env list.

## 10. Cross-references

- ← Depends on: [12-hcl-loader.md](./12-hcl-loader.md), [13-evaluator.md](./13-evaluator.md)
- → Consumed by: [13-evaluator.md](./13-evaluator.md) (cycle is acceptable: TG feeds eval-context, eval handles the `.tf` files), [15-resource-graph.md](./15-resource-graph.md), [16-provider-resolver.md](./16-provider-resolver.md)
- ↔ Research: [terragrunt-handling.md](../docs/research/terragrunt-handling.md)
- ↔ Decisions: [99-key-decisions.md](./99-key-decisions.md) — D6 (mimic Terragrunt, don't invoke)
