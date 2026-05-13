# Research — HCL parsing in Rust

Status: memo · Date: 2026-05-13 · Owner: tfparser-core

## Question

Which Rust crate(s) should we use to parse Terraform `.tf`, `.tfvars`, and `terragrunt.hcl` files, and how much of HCL2 semantics do they cover?

## Crates evaluated

| Crate | Version | Strengths | Weaknesses |
| ----- | ------- | --------- | ---------- |
| `hcl-rs` ([crates.io](https://crates.io/crates/hcl-rs)) | 0.19.7 | High-level Body/Block/Expression AST, `serde` integration, an **`eval` module** with a `Context` for variables and custom functions, and template/expression evaluation. | Loses source positions/spans after parse — fine for emit, painful for diagnostics. AST is owned, allocations everywhere. |
| `hcl-edit` ([crates.io](https://crates.io/crates/hcl-edit)) | 0.9.6 | Sibling crate to `hcl-rs` (same repo, `martinohmann/hcl-rs`). Preserves whitespace, comments, **and `Span` ranges** on every node — designed like `toml_edit`. Mutable traversal via `visit_mut`. | No eval; you reach into `hcl-rs` for that. API is more verbose. |
| `hcl-primitives` | 0.1.11 | Shared identifier/number/template primitives between the two. | Internal helper, not a top-level choice. |
| `tree-sitter-hcl` | 1.1.0 | Tree-sitter grammar — incremental parse, error-tolerant, query language. | Heavy dependency; needs a tree-sitter runtime; awkward to traverse from Rust types. |
| `tfconfig` ([crates.io](https://crates.io/crates/tfconfig) — Rust port of `hashicorp/terraform-config-inspect`) | 0.2.3 | Closest to "Terraform-aware" — knows about `module`, `provider`, `resource`, `variable` blocks at a high level. | Shallow inspection only; no evaluation, no dependency edges, no Terragrunt. |
| `terraform-parser` | crates.io | Parses Terraform **plan/state** JSON, not source. | Wrong layer for source-first parsing — relevant only if we add `--with-plan` later. |

## Decision

- **Primary parser: `hcl-edit` 0.9.x.** Spans are non-negotiable: every resource the parser emits must carry `file:line:col` for diagnostics, UI hover, and graph navigation. `hcl-rs` discards positions and forces us back to the source string for any error message.
- **Evaluator: `hcl-rs::eval` 0.19.x**, fed by an AST we lower from `hcl-edit`. `hcl-rs` provides `Context::declare_var` / `declare_func` and supports the template sublanguage — enough to resolve `${var.foo}`, `local.bar`, simple `merge()`/`format()`/`jsonencode()` calls, and string-templates. Unresolved expressions stay symbolic.
- **Reject `tfconfig`.** It is a shallow inspector and would force us to re-parse for everything it does not expose (provider aliases, locals, module bodies). The minor convenience is not worth the duplicate parse path.
- **Reject `tree-sitter-hcl`.** Excellent for an editor, overkill for a batch parser. Reconsider only if we ever ship a language server.

## What `hcl-rs::eval` covers (concrete list)

From the docs and source:
- Template interpolation `${expr}` and template directives `%{ for }`, `%{ if }`.
- All HCL2 operators (`+`, `-`, `*`, `/`, `%`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `&&`, `||`, `!`, ternary `?:`, splat `*`, attribute access `.`, index `[]`).
- Built-in evaluation of literals, tuples, objects, function calls.
- A `Context` API for declaring variables (`ctx.declare_var("environment", "staging")`) and custom functions (`FuncDef`).

What it does **not** cover, and we must implement as custom funcs:
- Terraform-specific built-ins: `cidrsubnet`, `formatdate`, `templatefile`, `file`, `fileset`, …
- Terragrunt-specific built-ins: `find_in_parent_folders`, `path_relative_to_include`, `read_terragrunt_config`, `get_env`, `get_repo_root`, `get_terraform_commands_that_need_vars`, `strcontains`, `lookup`.
- Provider lookups (`data.*`), module outputs (`module.foo.bar`), resource references (`aws_instance.x.id`).

For source-only parsing we **deliberately leave unresolved references symbolic** (a sentinel `Unresolved(String)` node) rather than failing the parse — they become edges in the dependency graph, not values.

## Performance / memory notes

- `hcl-edit` parses ~10–30 MB/s on Apple M-class CPUs in benchmarks the upstream maintainer publishes. A reference-scale Terragrunt monorepo (~320 k LOC / ~4 600 files / under 50 MB of HCL) is comfortably parseable in **single-digit seconds on one core**, sub-second with `rayon` per file. No need for incremental parse.
- `hcl-edit`'s AST is heap-heavy. We **lower to our own slimmer IR** in the same pass and drop the parse tree (see [10-data-model.md](../../specs/10-data-model.md)). Keeping the `hcl-edit` tree resident for the whole workspace would blow past 1 GB.
- Span types in `hcl-edit` are `Range<usize>` byte offsets into the source. We resolve to `file:line:col` lazily via a per-file `LineIndex` (own implementation, ~30 LOC).

## Risks retired by this choice

- **R1 — "Can we parse a reference-scale Terragrunt monorepo at all?"** Yes: spot-checks of representative components (a database-backed service, an IAM/account-fixtures component, a reusable EC2-service module, the root Terragrunt config) all use vanilla HCL2 plus the documented Terragrunt funcs — no exotic extensions.
- **R2 — "Can we get spans?"** Yes via `hcl-edit::Span`.
- **R3 — "Can we evaluate `${var.environment}` style strings?"** Yes via `hcl-rs::eval::Context`; we register Terraform/Terragrunt built-ins as custom funcs.

## Open risks (deferred, not blocking)

- **R-OPEN-1**: Some real-world TF uses `templatefile(...)` to pull in `.tpl` files. Our evaluator returns `Unresolved` for these unless the template file is read and rendered. Acceptable for M0–M5; revisit in hardening.
- **R-OPEN-2**: `for_each = { ... }` with computed values cannot be expanded source-only. We emit a single "template" resource row with `count_expansion = "unresolved"` and the for-each expression captured verbatim.

## References

- [hcl-rs README](https://github.com/martinohmann/hcl-rs)
- [hcl-edit docs.rs](https://docs.rs/hcl-edit)
- [hcl-rs eval module docs](https://docs.rs/hcl-rs/latest/hcl/eval/index.html)
- [tfconfig crate](https://crates.io/crates/tfconfig) (rejected, but worth knowing as a fallback diff target)
