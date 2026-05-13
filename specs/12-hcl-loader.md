# 12 — HCL Loader

Status: draft v1 · Owner: tfparser-core · Depends on: [11-discovery.md](./11-discovery.md), [10-data-model.md](./10-data-model.md)

## 1. Purpose

Take a `DiscoveredDir` and return a `RawComponent` — a structured IR view of every block in every `.tf` / `.tfvars` / `terragrunt.hcl` file in that directory, with **byte spans on every node**, ready for the evaluator. No expression evaluation; references stay symbolic. No module expansion; module calls captured verbatim.

This is where source positions enter the IR. Anything we lose here is gone — `hcl-rs`'s Body type doesn't preserve them, so we deliberately use `hcl-edit`.

## 2. Interface

```rust
// crates/core/src/loader/mod.rs
pub trait Loader: Send + Sync {
    fn load(&self, dir: &DiscoveredDir, ctx: &LoadContext) -> Result<RawComponent>;
}

pub struct HclEditLoader;                      // default impl, uses hcl-edit 0.9.x

pub struct LoadContext<'a> {
    pub root:          &'a Path,               // workspace root (canonical)
    pub sources:       &'a SourceMap,          // cache: file → Arc<str> contents
    pub line_indexer:  &'a LineIndexer,        // file → LineIndex
    pub limits:        &'a LoaderLimits,
}

pub struct LoaderLimits {
    pub max_file_bytes:        u32,            // default: 4 MiB
    pub max_blocks_per_file:   u32,            // default: 10_000
    pub max_attr_depth:        u32,            // default: 64
    pub max_template_parts:    u32,            // default: 1024
}

pub struct RawComponent {
    pub path:       Arc<Path>,
    pub kind:       ComponentKind,
    pub raw_blocks: Vec<RawBlock>,             // every block, every file, in file order
    pub diagnostics: Vec<Diagnostic>,
}

pub struct RawBlock {
    pub kind:    BlockKind,                    // Resource | Data | Variable | Local | Output | Provider | Terraform | Module | Locals | Include | Generate | Dependency | Inputs | Unknown
    pub labels:  Vec<Arc<str>>,                // [type, name] for resource; [name] for variable; …
    pub body:    AttributeMap,                 // top-level only; nested blocks are nested AttributeMaps via Value::Map
    pub span:    Span,
    pub source:  Arc<Path>,
}
```

`AttributeMap` here is the `Vec<(Arc<str>, Expression)>` from [10-data-model.md § Expressions](./10-data-model.md#expressions-and-values). Critically, **the body is *lowered* — we walk the `hcl-edit` AST once and produce our own tree, dropping the `hcl-edit` tree**. This is a 5–8× memory win vs keeping the edit tree.

## 3. Behaviour

### 3.1 Parse + lower

For each file:

1. Read contents with size cap: reject files > `max_file_bytes`.
2. Cache the `Arc<str>` in `SourceMap` (keyed by canonical path) so spans can be rendered later without re-reading.
3. Parse with `hcl_edit::parser::parse_body(&src)` → `hcl_edit::structure::Body`.
4. Walk the body, lowering each block:
   - `BlockKind` derived from the block identifier (`resource`, `data`, `variable`, `local`, `locals`, `output`, `provider`, `terraform`, `module`, `include`, `generate`, `dependency`, `inputs`, …).
   - `labels` extracted from the block's label list.
   - `body` lowered recursively (see § 3.2).
   - `span` constructed from the block's `Span` (byte range) plus a `LineIndex` lookup.
5. On parse error: capture a `Diagnostic::Parse { span, message }`, **skip the file**, and continue. We never abort an entire workspace because one file is malformed.

### 3.2 Expression lowering

`hcl_edit::expr::Expression` → our `Expression`:

| HCL node                                       | Lowering |
| ---------------------------------------------- | -------- |
| literal (`null`, bool, number, string)         | `Expression::Literal(Value::*)` |
| `${ ... }` template                            | `Expression::TemplateConcat(parts)` |
| reference (`var.x`, `local.y`, `data.x.y.z`, `module.m.o`, `aws_iam_role.r.arn`) | `Expression::Unresolved(Symbolic { … })` |
| binary op                                      | `Expression::BinaryOp` |
| unary op                                       | `Expression::UnaryOp` |
| conditional `a ? b : c`                        | `Expression::Conditional` |
| function call                                  | `Expression::FuncCall` |
| `for` expr                                     | `Expression::For` |
| tuple / object literals                        | recurse → `Value::List` / `Value::Map` once children resolved at evaluator phase; during loader, kept as expression nodes |

The loader **never** decides a value can resolve — that's the evaluator's job. Every reference becomes `Unresolved`; every literal becomes `Literal`. This separation makes the loader pure and trivially parallelisable.

### 3.3 Block-kind heuristics

The HCL grammar doesn't distinguish "resource" from "user-named block 'resource'"; we use the keyword position. To prevent confusion with the rare module-author writing `block "resource" {}` (which is legal HCL), we **only** recognise the canonical kinds at the file top level. Nested `dynamic {}` blocks are captured as `Unknown` with their labels.

### 3.4 Concurrency

`HclEditLoader::load` is pure on a single component. We parallelise *across* components in the orchestrator using `rayon::par_iter()`. Per CLAUDE.md § Async & Concurrency, the loader holds no shared mutable state.

### 3.5 Resource limits (binding)

- `max_file_bytes = 4 MiB` — a single 4 MB `.tf` file is unprecedented in the wild (largest observed in surveyed monorepos is ~120 KB). Cap is a DoS guard.
- `max_blocks_per_file = 10_000` — DoS guard for adversarial inputs.
- `max_attr_depth = 64` — recursion guard. We **iterate**, not recurse, but the limit also bounds the IR depth so consumers don't blow stacks.
- `max_template_parts = 1024` — caps `TemplateConcat` size.

Breaching any → `Error::LoaderLimit { kind, observed, limit }`, file skipped, diagnostic added, parse continues for the rest of the component.

## 4. Invariants

- **I-LOAD-1**: Every `RawBlock.span` resolves to a non-empty byte range inside the source file's contents.
- **I-LOAD-2**: `RawBlock.body` contains **no** `hcl_edit` types; it is fully lowered to our own `Expression` / `Value`.
- **I-LOAD-3**: A malformed file produces a `Diagnostic`, never an `Err` at the component level. A file that exceeds limits also produces a diagnostic and a skip.
- **I-LOAD-4**: `RawComponent.raw_blocks` preserves source order (file order, then block order within file). Order is part of the contract — downstream uses it for "first wins" semantics in providers.
- **I-LOAD-5**: The loader is **pure** w.r.t. external state: same inputs → identical `RawComponent` byte-for-byte (modulo `Arc<str>` identity).
- **I-LOAD-6**: No `unwrap`/`expect`/`panic!` reachable from any code path triggered by HCL input (per CLAUDE.md § Safety & Security). Fuzz harness validates.

## 5. Error model

```rust
#[derive(thiserror::Error, Debug)]
pub enum LoaderError {
    #[error("file too large: {path} ({observed} > {limit})")]
    FileTooLarge { path: Arc<Path>, observed: u64, limit: u64 },

    #[error("limit exceeded ({kind}) at {span}: observed {observed} > {limit}")]
    Limit { kind: LimitKind, observed: u64, limit: u64, span: Span },

    #[error("i/o error reading {path}: {source}")]
    Io { path: Arc<Path>, #[source] source: io::Error },
}
```

Parse errors (from `hcl-edit`) are captured as `Diagnostic`, not `Err` — the file is skipped, parse continues.

## 6. Span construction

```rust
pub struct LineIndex {
    line_starts: Vec<u32>,  // byte offset of each line start; line_starts[0] = 0
}
impl LineIndex {
    pub fn build(src: &str) -> Self;                     // O(n)
    pub fn locate(&self, byte: u32) -> (u32, u32);       // (line, col), 1-based, O(log n)
}
```

Built once per file (lazy, behind `OnceLock`); cached by file path. `(line, col)` lookups are binary search on a `Vec<u32>` — cheap.

## 7. Testing

- Round-trip a corpus of synthetic-yet-realistic `.tf` files under `crates/core/tests/fixtures/` (covering the patterns in [terraform-repo-shapes.md](../docs/research/terraform-repo-shapes.md)).
- Span correctness: for each parsed block, assert the source substring at `[span.byte_range]` starts with the block keyword.
- Limit tests: a synthetic 5 MB file is rejected; a file with 11k blocks halts after 10k with a diagnostic.
- **Fuzz** (`cargo-fuzz`): `fuzz_target!(|data: &[u8]| { let _ = HclEditLoader.parse_bytes(data); })`. CI runs ≥ 10 min per PR.
- **Property** (`proptest`): random valid HCL inputs always lower without panic.

Per CLAUDE.md § Testing: descriptive names like `test_should_lower_resource_with_dynamic_block_to_unknown_kind`.

## 8. Performance

- Target: **≤ 1 ms median** to load one typical component (~20 files, ~3000 LOC). M-class CPU, release build.
- `Arc<str>` interning on resource type names, block kinds, and provider local names — backed by a small `Mutex<FxHashMap>` (rare writes, frequent reads) or `boxcar`/`elsa` for lock-free interning.
- `SourceMap` keeps file contents alive for the full parse; capped at `4 MiB × max_total_files`, with eviction once a file is fully lowered if the consumer doesn't need spans (the exporter does — so eviction is off by default).

See [71-performance-budgets.md](./71-performance-budgets.md) for the full table.

## 9. Cross-references

- ← Depends on: [11-discovery.md](./11-discovery.md), [10-data-model.md](./10-data-model.md)
- → Consumed by: [13-evaluator.md](./13-evaluator.md), [14-terragrunt.md](./14-terragrunt.md), [15-resource-graph.md](./15-resource-graph.md)
- ↔ Research: [hcl-parsing-in-rust.md](../docs/research/hcl-parsing-in-rust.md)
- ↔ Security: [70-security.md](./70-security.md)
- ↔ Perf: [71-performance-budgets.md](./71-performance-budgets.md)
