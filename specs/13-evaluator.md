# 13 — Evaluator (Best-Effort Expression Resolution)

Status: draft v1 · Owner: tfparser-core · Depends on: [12-hcl-loader.md](./12-hcl-loader.md), [10-data-model.md](./10-data-model.md)

## 1. Purpose

Given a `RawComponent` and the workspace's `Environment` map, reduce every `Expression::Unresolved` we *can* statically resolve to a concrete `Value`, leaving the rest symbolic. This is what makes `provider "aws" { region = var.region }` produce `region = "us-east-2"` in the Parquet output.

The evaluator is **best-effort by contract**. It is *not* a Terraform re-implementation: every unresolved reference that depends on apply-time data (resource attributes, data sources, module outputs) stays symbolic. That is the correct outcome, not a failure.

## 2. Interface

```rust
// crates/core/src/eval/mod.rs
pub trait Evaluator: Send + Sync {
    fn evaluate(&self, raw: &RawComponent, ctx: &EvalContext) -> Result<EvaluatedComponent>;
}

pub struct HclEvaluator;                       // default impl, wraps hcl-rs::eval

pub struct EvalContext {
    pub workspace_root: Arc<Path>,
    pub environment:    Option<Arc<str>>,      // "staging" / "production" / …
    pub env_vars:       EnvVarMode,            // see § 5
    pub repo_vars:      Map,                   // from environments/<env>.tfvars
    pub cascade_locals: Map,                   // from Terragrunt cascade (see 14-terragrunt.md)
    pub funcs:          Arc<FuncRegistry>,     // HCL stdlib + TF + Terragrunt funcs
    pub limits:         EvalLimits,
}

pub struct EvaluatedComponent {
    pub raw:        Arc<RawComponent>,         // kept for span access; cheap (Arc)
    pub variables:  Vec<Variable>,             // resolved defaults; description; type
    pub locals:     Vec<Local>,                // each Local.value: Expression (Value if resolved, Unresolved otherwise)
    pub providers:  Vec<ProviderBlock>,        // region_expr / profile_expr now reduced where possible
    pub resources:  Vec<Resource>,             // count_expr / for_each_expr / attributes all reduced
    pub modules:    Vec<ModuleCall>,
    pub outputs:    Vec<Output>,
    pub diagnostics:Vec<Diagnostic>,
}

pub enum EnvVarMode {
    Passthrough,                               // get_env() reads process env (default off in CLI; on for `--unsafe-env`)
    Strict { allowed: BTreeSet<Arc<str>> },    // only listed names visible (default for CLI)
    Mock,                                       // get_env always returns the default arg or ""
}

pub struct EvalLimits {
    pub max_func_args:    u32,                 // default: 64
    pub max_str_size:     u32,                 // default: 1 MiB rendered string
    pub max_list_len:     u32,                 // default: 100_000
    pub max_iterations:   u32,                 // default: 1_000_000 reductions (anti-DoS for `for`)
}
```

## 3. Evaluation pipeline

For each `RawComponent`, in order:

1. **Bind variables.** For each `variable "x" {}` block:
   - If the user supplied `x` via `repo_vars` (from a `.tfvars`), bind to that value.
   - Else if a `default = ...` attribute exists, eagerly evaluate it (with current context) and bind to that.
   - Else leave unbound. References to `var.x` stay `Unresolved`.
2. **Bind locals.** Topologically sort `locals` by their inter-references. Locals can refer to `var.*`, other `local.*`, and HCL/Terraform functions. Each pass evaluates the locals whose dependencies are bound; repeat until fixed-point or all remaining locals depend on `Unresolved`.
3. **Bind environment/cascade.** Inject `cascade_locals` (from Terragrunt) and per-env values into the context as `local.*` shadows where applicable. Document the precedence — see [14-terragrunt.md § 4](./14-terragrunt.md#configuration-cascade-binding).
4. **Reduce providers.** For each `provider` block, evaluate `region`, `profile`, `assume_role.role_arn`. Reduced values land in `ProviderBlock.region_expr` etc. (still typed `Expression` for uniformity — the consumer checks `as_literal()`).
5. **Reduce resources.** For each `Resource`, evaluate `count`, `for_each`, and every attribute body recursively. References that cannot resolve (`data.x.y`, `aws_*.z.attr`, `module.m.o`) stay `Unresolved`; the *parent* expression survives — we never collapse a resolvable subtree just because one leaf is unresolved.
6. **Reduce modules and outputs.** Same.
7. **Emit `EvaluatedComponent`.**

## 4. The `hcl-rs::eval::Context` we build

```rust
fn build_hcl_context(ctx: &EvalContext) -> hcl::eval::Context<'static> {
    let mut hc = hcl::eval::Context::new();
    for (name, value) in &ctx.repo_vars   { hc.declare_var(name.as_ref(), value_to_hcl(value)); }
    for (name, value) in &ctx.cascade_locals { hc.declare_var(name.as_ref(), value_to_hcl(value)); }
    for f in ctx.funcs.iter() { hc.declare_func(f.name.as_ref(), f.def.clone()); }
    hc
}
```

We **always feed our context into the hcl-rs evaluator and read the result back into our IR.** This isolates the dependency: if `hcl-rs::eval` changes API, the blast radius is one adapter file.

## 5. Functions registered

Per [terragrunt-handling.md § Functions](../docs/research/terragrunt-handling.md) plus the HCL stdlib:

**Already in `hcl-rs::eval` stdlib** (we trust): `format`, `formatlist`, `replace`, `regex`, `regexall`, `substr`, `lower`, `upper`, `trim`, `trimspace`, `length`, `keys`, `values`, `merge`, `concat`, `compact`, `coalesce`, `coalescelist`, `tolist`, `toset`, `tomap`, `tostring`, `tonumber`, `lookup`, `try`, `can`, `flatten`, `contains`, `element`, `index`, `slice`, `zipmap`, `range`, `min`, `max`, `abs`, `ceil`, `floor`, `pow`, `signum`, `parseint`, `jsonencode`, `jsondecode`, `yamlencode`, `yamldecode`, `cidrhost`, `cidrnetmask`, `cidrsubnet`, `cidrsubnets`.

**We register ourselves**:
- `file(path)`, `fileexists(path)` — sandboxed: paths resolved against `workspace_root`; canonicalised; rejected if outside root.
- `templatefile(path, vars)` — same sandbox; returns `Unresolved` if the template references unresolvable vars.
- `fileset(path, pattern)` — sandboxed, capped at `max_list_len` entries.
- `find_in_parent_folders`, `find_in_parent_folders_from`, `read_terragrunt_config`, `path_relative_to_include`, `path_relative_from_include`, `get_terragrunt_dir`, `get_repo_root`, `get_parent_terragrunt_dir`, `get_env`, `get_terraform_commands_that_need_vars`, `strcontains` — see [14-terragrunt.md](./14-terragrunt.md).
- **Terraform funcs hcl-rs doesn't ship**: `formatdate`, `timestamp`, `timeadd`, `uuid`, `bcrypt`, `sha256`, `md5`, `sha1`, `sha512`, `filesha256`, `filemd5`, `base64encode`, `base64decode`, `base64gzip`, `urlencode`. Implemented from std + `sha2`/`md5`/`base64` crates.

Unimplemented funcs → `Unresolved(FuncCall { name, args })`, marker preserved in canonical JSON.

## 6. `Unresolved` propagation

A reduce step either:
- returns a `Value` (fully resolved), or
- returns `Expression::Unresolved(_)` (the expression survives intact for emission), or
- returns a partially-reduced `Expression` (subtree has some literals, some `Unresolved` leaves).

We **do not** synthesise a `null` to "make the row complete." A subtle case the spec is explicit on: `var.x ? "a" : "b"` where `var.x` is unbound → result is the *conditional expression*, not "false → b". A blunt `false` default has bitten engineers writing similar tools.

## 7. Invariants

- **I-EVAL-1**: Evaluation is **pure on `(raw, ctx)`** — same inputs → same `EvaluatedComponent` byte-for-byte (modulo `Arc` identity).
- **I-EVAL-2**: Variable binding is **acyclic by construction** (variables can't reference other variables in Terraform — that's a parse-time rule). Detect cycles in `local.*` and fail with `Error::Cycle` listing the participants.
- **I-EVAL-3**: `get_env(name)` returns `""` (or the supplied default) unless `name ∈ allowed` (Strict) or mode is Passthrough. No leakage of `HOME`, `AWS_SECRET_*`, etc. into the IR.
- **I-EVAL-4**: `file()` and `templatefile()` are sandboxed to `workspace_root`. Canonicalise after `Path::join`, reject on path-escape, max-file-bytes cap reused from loader.
- **I-EVAL-5**: Iterations are bounded by `max_iterations`; exceeding it returns `Error::Eval("iteration cap reached at ...")` and leaves the expression `Unresolved`.
- **I-EVAL-6**: The evaluator runs in `Send + Sync` context — no thread-local `Context`s, no `RefCell`.

## 8. Error model

```rust
#[derive(thiserror::Error, Debug)]
pub enum EvalError {
    #[error("cycle in locals: {participants:?}")]
    Cycle { participants: Vec<Address> },

    #[error("evaluator limit ({kind}): observed {observed} > {limit} at {span}")]
    Limit { kind: LimitKind, observed: u64, limit: u64, span: Span },

    #[error("function `{name}` failed: {message}")]
    Func { name: Arc<str>, message: Box<str>, span: Span },

    #[error("path escape in {func}: {path}")]
    PathEscape { func: &'static str, path: Arc<Path>, span: Span },
}
```

Note: an *unbindable* reference is **not** an error — it's expected and yields `Unresolved`.

## 9. Performance

- Evaluator overhead ≤ **2 ms** per typical component on the M-class baseline. Most time is in `hcl-rs::eval`'s expression walk; we add an outer pass for locals fixpoint that converges in 1–3 iterations for real configs.
- Locals fixpoint uses a worklist algorithm (push deps-resolved locals), not a brute fixed-point loop.
- Custom funcs avoid allocation where possible: `format`, `lower`, `upper`, `replace` take `&str` views and return `Cow<str>`.

See [71-performance-budgets.md](./71-performance-budgets.md).

## 10. Testing

- Golden tests: `crates/core/tests/eval/<case>/input.tf + ctx.json + expected.json`.
- Cycle detection: a hand-rolled `local.a = local.b; local.b = local.a` fixture asserts `Error::Cycle { ... a, b }`.
- Sandbox tests: `file("../../etc/passwd")` returns `Error::PathEscape`.
- Env-mode tests: under Strict, `get_env("AWS_SECRET_ACCESS_KEY")` returns the default, not the real env.
- Property test: every input that yields `Unresolved` must also yield a `source` string equal to the original HCL source slice.

## 11. CLAUDE.md anchoring

- **Errors**: `thiserror`, `#[source]` on wrapped IO and HCL errors, no `unwrap`/`expect`.
- **Async**: synchronous CPU-bound code; called from `rayon::par_iter`. No `tokio` dependency in `eval`.
- **Validation**: every external string (`get_env`, `file`) bounded in length and char-set.
- **Safety**: `#![forbid(unsafe_code)]`; fuzz harness against `evaluator::reduce(arbitrary_expression)`.
- **Logging**: `tracing::instrument(skip(raw, ctx))` on `evaluate`. Field redaction on `repo_vars` (may contain secrets-shaped strings).

## 12. Cross-references

- ← Depends on: [12-hcl-loader.md](./12-hcl-loader.md), [10-data-model.md](./10-data-model.md)
- → Consumed by: [14-terragrunt.md](./14-terragrunt.md), [15-resource-graph.md](./15-resource-graph.md), [16-provider-resolver.md](./16-provider-resolver.md), [20-parquet-exporter.md](./20-parquet-exporter.md)
- ↔ Research: [hcl-parsing-in-rust.md](../docs/research/hcl-parsing-in-rust.md), [terragrunt-handling.md](../docs/research/terragrunt-handling.md)
- ↔ Decisions: [99-key-decisions.md](./99-key-decisions.md) — D4 (best-effort eval), D5 (Unresolved is first-class)
