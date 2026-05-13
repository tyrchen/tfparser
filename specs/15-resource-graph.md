# 15 — Resource Graph (Modules, Expansion, Dependencies)

Status: draft v1 · Owner: tfparser-core · Depends on: [13-evaluator.md](./13-evaluator.md), [14-terragrunt.md](./14-terragrunt.md)

## 1. Purpose

Take the workspace's `Vec<EvaluatedComponent>` and produce a single `Workspace` IR with:
- Modules resolved (local sources walked and parsed) and **module bodies flattened** into the parent component as fully-qualified resources.
- Inter-resource dependencies inferred (explicit `depends_on` plus implicit refs derived from `Unresolved` nodes).
- Component-to-component edges from `dependency` blocks.
- Per-component summaries (counts, env coverage) for `components.parquet`.

This is the phase where everything composes into the dataset the exporter will write.

## 2. Interface

```rust
// crates/core/src/graph/mod.rs
pub trait GraphBuilder: Send + Sync {
    fn build(&self, components: Vec<EvaluatedComponent>, modules: ModuleRegistry, ctx: &GraphContext)
        -> Result<Workspace>;
}

pub struct DefaultGraphBuilder;

pub struct GraphContext {
    pub workspace_root:   Arc<Path>,
    pub max_module_depth: u32,                 // default: 8
    pub infer_dependencies: bool,              // default: true
}

pub struct ModuleRegistry {
    pub local_modules: HashMap<Arc<Path>, EvaluatedComponent>,   // path-keyed
    pub external_refs: Vec<ExternalModuleRef>,                   // git/registry placeholders
}
```

## 3. Module resolution & expansion

### 3.1 Resolve sources

For every `ModuleCall` discovered in any component:

1. Inspect `source_raw`.
2. Classify:
   - Starts with `./` or `../` → **local path**. Resolve against the *component's directory*, canonicalise. Reject on path-escape. Look up in `ModuleRegistry.local_modules`; if missing, signal a discovery gap (the discoverer should have found it via the seeded glob, but we re-walk on demand).
   - Looks like `<registry>/<name>/<provider>` or starts with `git::` / `https://` / `s3::` → **external**. Captured in `ExternalModuleRef`; not parsed.
   - Anything else → diagnostic.

Once a path is resolved, the `ModuleCall.resolved` becomes `Some(ModuleId)` and the called module's `EvaluatedComponent` is reachable through the registry.

### 3.2 Expand

For each call site:
1. Clone the called module's resources/data/modules/outputs.
2. **Rewrite addresses** by prefixing `module.<call_name>` (or `module.<call_name>[<index>]` if the call has `count` or `for_each`).
3. **Substitute inputs**: the module's `var.*` references resolve against `ModuleCall.inputs` (already evaluated by [13-evaluator.md](./13-evaluator.md)). What's still `Unresolved` after substitution stays `Unresolved`.
4. **Substitute providers**: if the call has `providers = { aws = aws.main }`, every `provider = aws.<alias>` inside the module is rewritten using the mapping. If the module body uses a `default` aws provider, it inherits the *call site's* default. This is **load-bearing** for [16-provider-resolver.md](./16-provider-resolver.md): the same module instantiated by two components against different aliases will produce resources tagged with different `account_id`s.
5. **Recurse** for nested module calls, bounded by `max_module_depth`.
6. **Detect cycles** (a module that calls itself directly or transitively) — emit a diagnostic, drop the recursive expansion.

The expanded resources are appended to the *parent component's* resource list. The module is **not** double-counted as its own component in `components.parquet`.

### 3.3 `count` / `for_each` expansion

We can attempt expansion only when the expression is fully resolved:

- `count = 2` → emit two resources, addresses `…[0]` and `…[1]`.
- `count = var.foo` (Unresolved) → emit **one template row** with `count_expr` set to the verbatim source. Address omits the index. Downstream queries can `WHERE count_expr != ''` to find unexpanded templates.
- `for_each = { a = 1, b = 2 }` (resolved literal map) → emit one resource per key, addresses `…["a"]` / `…["b"]`.
- `for_each = local.aws_accounts` (Unresolved) → one template row, `for_each_expr` set verbatim.

This expansion is **bounded**: if a resolved `count` exceeds `max_expansion_per_resource` (default 1024), we emit the template row + a diagnostic. Defence against `count = 1_000_000` adversarial inputs.

## 4. Dependency inference

For each resource and module call, walk the `attributes` tree and collect every `Expression::Unresolved` whose `kind` is `Resource`, `Data`, or `Module`. Convert each `Symbolic.source` (e.g. `"aws_iam_role.kiam-server.arn"`) into a target `Address` and record an edge:

```rust
pub struct Edge {
    pub from:     Address,            // the resource holding the reference
    pub to:       Address,            // the address referenced
    pub kind:     EdgeKind,           // ExplicitDependsOn | AttrRef | ModuleInput | TerragruntDependency
    pub attr:     Option<Arc<str>>,   // attribute path that introduced the edge ("policy", "subnets[0].id", …)
    pub span:     Span,
}
```

Explicit `depends_on = [aws_x.y, aws_x.z]` → `EdgeKind::ExplicitDependsOn`.

Symbolic refs found inside any attribute → `EdgeKind::AttrRef`. The reference is parsed to find the *resource* head (`aws_iam_role.kiam-server`), regardless of any attribute trail (`.arn`).

Module input references whose value is `module.x.y` → `EdgeKind::ModuleInput`.

Terragrunt `dependency.x.outputs.y` → `EdgeKind::TerragruntDependency`, target is the *component*'s address (synthesised: `component.<relative-path>`).

Edges are de-duplicated by `(from, to, kind)`. The edge list is sorted by `(from, to, kind)` for deterministic output.

## 5. Component summary

Per `Component`, produce a `ComponentSummary` for `components.parquet`:

| Field | Notes |
| ----- | ----- |
| `component_path` | relative |
| `kind` | `component` \| `module` |
| `resource_count` | post-expansion |
| `data_count` | |
| `module_call_count` | |
| `output_count` | |
| `variable_count` | |
| `local_count` | |
| `provider_count` | distinct `provider "aws"` blocks |
| `environments_seen` | list<utf8> |
| `has_terragrunt` | bool |
| `state_backend_kind` | `s3` \| `local` \| empty |
| `state_account_id` | |
| `state_region` | |
| `unresolved_count` | how many `Unresolved` made it to the final IR |
| `first_seen_at` / `last_seen_at` | parse time anchors (Useful for diff tools) |

## 6. Invariants

- **I-GRAPH-1**: After expansion, every `Resource.address` is unique within the workspace.
- **I-GRAPH-2**: Every `Edge.from` and `Edge.to` matches an address present in the workspace, OR `to` is the synthesised `component.<path>` form for component dependencies.
- **I-GRAPH-3**: Module expansion is **idempotent**: re-running the builder on the same `EvaluatedComponent` set produces byte-identical IR.
- **I-GRAPH-4**: Cycles in module calls (`module A → module B → module A`) are detected and dropped with a single diagnostic; the partial expansion is kept up to the cycle point.
- **I-GRAPH-5**: `Workspace.components` is sorted by `Component.path` for deterministic Parquet output.

## 7. Error model

```rust
#[derive(thiserror::Error, Debug)]
pub enum GraphError {
    #[error("module source `{source}` referenced from {site} is not resolvable")]
    UnresolvableModuleSource { source: Arc<str>, site: Span },

    #[error("module recursion exceeded depth {limit} at {site}")]
    DepthExceeded { limit: u32, site: Span },

    #[error("address collision: {0}")]
    AddressCollision(Address),
}
```

`UnresolvableModuleSource` is **not fatal**: the module call is captured as `external`, a diagnostic is added, and the parse continues.

`AddressCollision` is fatal at the workspace level — it indicates a bug in our expansion logic. Tests must prevent this from happening.

## 8. Performance

- Module bodies are shared via `Arc`. A module called from 30 different components is parsed **once** and `Arc::clone`'d on each expansion.
- Expansion is per-component; we `rayon::par_iter()` components and merge the results into the workspace at the end (sequential append; the merge is O(N) and tiny).
- Address rewriting uses a `SmallVec<[Arc<str>; 4]>`-style buffer to avoid allocation in the common case (max 1–2 nested modules).

Target: graph phase ≤ **300 ms** for a reference-scale repo (post-expansion ~40k resources).

## 9. Testing

- **Unit**: known fixtures for nested-module address rewriting, provider mapping inheritance, `for_each` expansion bounds.
- **Property**: address rewriting commutes with input substitution (rewriting then substituting = substituting then rewriting), for any synthesised AST.
- **Snapshot**: full `Workspace` snapshot of the `large-monorepo` fixture; reviewed manually before commit.
- **Cycle**: assert a synthesised cycle is detected and the rest of the parse completes.

## 10. CLAUDE.md anchoring

- **Errors**: `thiserror` with `#[source]`; partial failures captured as `Diagnostic`.
- **Type design**: `EdgeKind` is an enum with `#[non_exhaustive]`; new edge kinds (e.g. `LifecycleReference`) land additively.
- **Performance**: pre-allocate `Workspace.components` with the discovery count; avoid intermediate `Vec`s in the expansion loop.

## 11. Cross-references

- ← Depends on: [13-evaluator.md](./13-evaluator.md), [14-terragrunt.md](./14-terragrunt.md)
- → Consumed by: [16-provider-resolver.md](./16-provider-resolver.md), [20-parquet-exporter.md](./20-parquet-exporter.md)
- ↔ Research: [terraform-repo-shapes.md](../docs/research/terraform-repo-shapes.md)
- ↔ Decisions: [99-key-decisions.md](./99-key-decisions.md) — D8 (flatten module bodies into parent component)
