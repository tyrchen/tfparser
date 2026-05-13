# 10 — Data Model

Status: draft v1 · Owner: tfparser-core · Depends on: [00-prd.md](./00-prd.md)

The IR every other component sees, and the Parquet schema the CLI emits. **Drift here cascades everywhere.** Locked at M0; future fields land as additive columns/structs only.

## 1. Purpose

Define:
- The **in-memory IR**: types that flow between `discovery → loader → eval → terragrunt → graph → provider → exporter`.
- The **wire-out shape**: the Parquet schema downstream consumers depend on.
- Where the two diverge and why.

The IR is *richer* than the Parquet schema: it preserves spans, the parse tree, and unresolved expression nodes. The Parquet schema is the *projection* we commit to as a stable artifact.

## 2. In-memory IR

### 2.1 Top-level types

```rust
// crates/core/src/ir/mod.rs
pub struct Workspace {
    pub root: Arc<Path>,                  // absolute, canonicalized
    pub components: Vec<Component>,       // ordered by discovery (deterministic)
    pub modules:    Vec<Module>,          // referenced modules only
    pub environments: Vec<Environment>,   // discovered from environments/
    pub diagnostics: Vec<Diagnostic>,
}

pub struct Component {
    pub id:           ComponentId,        // newtype over u32 (interned)
    pub path:         Arc<Path>,          // relative to workspace.root
    pub kind:         ComponentKind,      // Component | Module
    pub files:        Vec<SourceFile>,
    pub variables:    Vec<Variable>,
    pub locals:       Vec<Local>,
    pub providers:    Vec<ProviderBlock>,
    pub resources:    Vec<Resource>,      // includes `resource` and `data`
    pub modules:      Vec<ModuleCall>,    // `module "x" { source = ... }`
    pub outputs:      Vec<Output>,
    pub terragrunt:   Option<TerragruntConfig>,
    pub state_backend: Option<StateBackend>,
}

pub struct Module {
    pub id:        ModuleId,              // newtype over u32
    pub source:    ModuleSource,          // Local(path) | RegistryAddr | GitUrl | External
    pub canonical_path: Option<Arc<Path>>,// Some iff source is local & resolvable
    pub component: Component,             // a Module is structurally a Component with kind=Module
}

pub struct Environment {
    pub name:           Arc<str>,         // "staging" | "production" | ...
    pub aws_account_id: Option<Arc<str>>,
    pub aws_region:     Option<Arc<str>>,
    pub aws_profile:    Option<Arc<str>>,
    pub source_file:    Arc<Path>,
    pub locals:         Map,              // see Value
}
```

### 2.2 Resources, providers, references

```rust
pub struct Resource {
    pub address:      Address,            // full TF address with module prefix
    pub kind:         ResourceKind,       // Managed | Data
    pub type_:        Arc<str>,           // "aws_db_instance"
    pub name:         Arc<str>,           // local label
    pub provider_ref: Option<ProviderRef>,// from `provider = aws.<alias>`
    pub count_expr:   Option<Expression>,
    pub for_each_expr:Option<Expression>,
    pub depends_on:   Vec<Address>,       // explicit + inferred (graph phase)
    pub attributes:   AttributeMap,       // top-level attributes (recursive)
    pub span:         Span,
}

pub struct ProviderBlock {
    pub local_name:   Arc<str>,           // "aws"
    pub alias:        Option<Arc<str>>,   // None for default
    pub source_addr:  Option<Arc<str>>,   // "hashicorp/aws" if known
    pub region_expr:  Option<Expression>,
    pub profile_expr: Option<Expression>,
    pub assume_role:  Option<AssumeRole>, // role_arn → account_id extraction
    pub raw:          AttributeMap,
    pub span:         Span,
}

pub struct ProviderRef {                   // a resource pointing at a provider
    pub local_name: Arc<str>,              // "aws"
    pub alias:      Option<Arc<str>>,      // "main" | "us-east-2" | None
    pub span:       Span,
}

pub struct ModuleCall {
    pub address:    Address,
    pub source_raw: Arc<str>,              // verbatim "../../modules-tf12/rds"
    pub source:     ModuleSource,
    pub resolved:   Option<ModuleId>,      // None until module-resolution phase
    pub providers:  Vec<(Arc<str>, ProviderRef)>, // `providers = { aws = aws.main }`
    pub inputs:     AttributeMap,
    pub count_expr: Option<Expression>,
    pub for_each_expr: Option<Expression>,
    pub span:       Span,
}
```

### 2.3 Expressions and values

The evaluator phase converts every `hcl-edit::expr::Expression` into our own `Expression` enum that distinguishes resolved values from symbolic references:

```rust
pub enum Expression {
    Literal(Value),
    Unresolved(Symbolic),                  // var.x, local.y, data.z, module.m.o, aws_x.y.attr
    BinaryOp { op: BinOp, lhs: Box<Expression>, rhs: Box<Expression> },
    UnaryOp  { op: UnaryOp, operand: Box<Expression> },
    TemplateConcat(Vec<Expression>),       // "${a}-${b}"
    FuncCall { name: Arc<str>, args: Vec<Expression> },
    For      { /* … */ },
    Conditional { cond: Box<Expression>, then_: Box<Expression>, else_: Box<Expression> },
}

pub enum Value {
    Null,
    Bool(bool),
    Number(f64),                           // HCL number; we keep f64, mirror upstream
    Int(i64),                              // optional fast-path for integer literals
    Str(Arc<str>),
    List(Vec<Value>),
    Map(Map),
}

pub struct Symbolic {
    pub kind: SymbolKind,                  // Var | Local | Resource | Data | Module | Path | Other
    pub source: Arc<str>,                  // verbatim "var.environment"
    pub address_hint: Option<Address>,     // Some when we can parse to an Address (var.x, local.y, etc.)
    pub span: Span,
}

pub type Map = Vec<(Arc<str>, Value)>;     // insertion-ordered; not HashMap (preserves order, smaller)
pub type AttributeMap = Vec<(Arc<str>, Expression)>;
```

**Why ordered vec instead of `HashMap`**: insertion order is semantically meaningful for the canonical JSON we emit, and at the sizes seen in practice (a few dozen attrs per resource) a Vec scan is faster than HashMap probing.

### 2.4 Addresses, spans, IDs

```rust
pub struct Address(Arc<str>);              // "module.pacer_db.aws_db_instance.this"
impl Address {
    pub fn module_path(&self) -> &str;     // "pacer_db" or ""
    pub fn resource_part(&self) -> &str;   // "aws_db_instance.this"
    pub fn parts(&self) -> AddressParts<'_>;
}

#[derive(Clone)]
pub struct Span {
    pub file:       Arc<Path>,
    pub byte_range: Range<u32>,
    pub line:       u32,                   // 1-based
    pub column:     u32,                   // 1-based
}

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct ComponentId(NonZeroU32);
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct ModuleId(NonZeroU32);
```

IDs are stable within a parse run; do not persist them across runs.

### 2.5 Invariants

- **I-IR-1**: For every `Resource`, `span.file` canonicalises to a descendant of `workspace.root`. Enforced in the loader; **reject** on violation (security: prevents path-traversal exfiltration into the IR).
- **I-IR-2**: `Component.path` is **relative** to `workspace.root` and uses `/` separators on all platforms (we normalise in discovery).
- **I-IR-3**: `Resource.address.module_path()` matches the `Component` / `ModuleCall` chain it came from. After module expansion, every flattened resource carries the full chain.
- **I-IR-4**: `ProviderRef.alias` is `Some(_) ⇔ resource source has `provider = aws.<alias>`. The default (no alias) is `None`.
- **I-IR-5**: `Expression::Unresolved` is the *only* node that survives evaluation containing a symbolic ref. Anything else has been reduced to a `Value` (possibly nested).
- **I-IR-6**: Per CLAUDE.md § Type Design — `ComponentId` / `ModuleId` are `NonZeroU32`, not `u32`; zero is invalid; the IR uses these everywhere a "missing" id might otherwise be encoded as zero.
- **I-IR-7**: All public IR types are `#[non_exhaustive]` per CLAUDE.md § Type Design. Adding fields is non-breaking.
- **I-IR-8**: All public IR types implement `Debug` with sensitive-field redaction (`provider_block.raw` is *not* logged at INFO level; tracing spans skip it).

### 2.6 Allocation discipline

- All "short text" fields (names, types, paths) are `Arc<str>` (or `Box<str>` for owned-once values). **No `String` in public IR**.
- Span byte ranges are `u32`, not `usize` — 4 GB per file is more than enough and halves the span footprint.
- `Workspace` retains one `Arc<str>` interner for resource types (most repos see < 200 distinct `resource_type` strings across 10k+ resources; interning cuts memory by 4–8×).
- We **do not** retain the `hcl-edit` parse tree after lowering. The original source string for a file is retained behind an `Arc<str>` for span display.

## 3. Parquet schema — `resources.parquet`

Frozen for v0 (M0–M5). Additive columns may be appended at the end; existing columns may not be renamed/retyped/reordered.

| #  | Column              | Arrow type                | Null? | Notes |
| -- | ------------------- | ------------------------- | ----- | ----- |
| 1  | `workspace_root`    | `Utf8`                    | ✗     | absolute, canonical |
| 2  | `component_path`    | `Utf8`                    | ✗     | relative, `/`-separated |
| 3  | `module_path`       | `Utf8`                    | ✗     | `""` for top-level; `"a.b.c"` for nested |
| 4  | `address`           | `Utf8`                    | ✗     | full TF address |
| 5  | `kind`              | `Utf8`                    | ✗     | enum string: `resource` \| `data` \| `module` \| `output` \| `variable` \| `local` \| `provider` |
| 6  | `resource_type`     | `Utf8`                    | ✗     | `""` for kinds without a type (output, local) |
| 7  | `resource_name`     | `Utf8`                    | ✗     | local label |
| 8  | `provider_local`    | `Utf8`                    | ✗     | `""` if default; `"aws.main"` otherwise |
| 9  | `provider_source`   | `Utf8`                    | ✗     | resolved provider source addr; `""` if unknown |
| 10 | `account_id`        | `Utf8`                    | ✗     | `""` if unresolved |
| 11 | `account_name`      | `Utf8`                    | ✗     | from profile map; `""` otherwise |
| 12 | `region`            | `Utf8`                    | ✗     | `""` if unresolved |
| 13 | `environment`       | `Utf8`                    | ✗     | `""` if env-agnostic |
| 14 | `count_expr`        | `Utf8`                    | ✗     | verbatim source, `""` if absent |
| 15 | `for_each_expr`     | `Utf8`                    | ✗     | verbatim source, `""` if absent |
| 16 | `depends_on`        | `List<Utf8>`              | ✗     | explicit + inferred; empty list if none |
| 17 | `attributes_json`   | `Utf8`                    | ✗     | canonical JSON of the full body; `Unresolved` rendered as raw source |
| 18 | `state_account_id`  | `Utf8`                    | ✗     | from `backend "s3"` profile/role ARN |
| 19 | `state_region`      | `Utf8`                    | ✗     | from `backend "s3"` `region` |
| 20 | `file`              | `Utf8`                    | ✗     | relative, `/`-separated |
| 21 | `line`              | `UInt32`                  | ✗     | 1-based |
| 22 | `column`            | `UInt32`                  | ✗     | 1-based |
| 23 | `parser_version`    | `Utf8`                    | ✗     | semver of `tfparser-core` |
| 24 | `parsed_at`         | `Timestamp(Millisecond, UTC)` | ✗ | when the row was produced |

All columns are non-null with `""` / empty-list / zero as the "missing" sentinel. **Rationale**: null vs empty discrimination is a query nuisance for downstream tools (DuckDB SQL, Athena) and adds no information that the empty string doesn't.

Compression: `zstd` level 3. Row group target: 128k rows or 64 MB, whichever first.

## 4. Canonical JSON for `attributes_json`

Stable, deterministic, queryable from DuckDB's `json_extract`:

- Keys sorted alphabetically at every object level.
- Numbers preserved as JSON numbers; HCL `null` → JSON `null`.
- HCL strings → JSON strings.
- HCL tuples/lists → JSON arrays.
- HCL objects/maps → JSON objects.
- `Expression::Unresolved` → JSON string of the verbatim source (e.g. `"var.environment"`), wrapped: `{"__unresolved__": "var.environment", "__kind__": "Var"}`.
- `Expression::FuncCall` left in unresolved form (we don't pretend to evaluate side-effecting funcs) → `{"__unresolved_func__": "templatefile", "args": [...]}`.

The `__unresolved__` sentinel lets downstream queries distinguish "value is the literal string `var.environment`" from "this is a reference we couldn't resolve."

## 5. Secondary tables (M5)

Same output directory; same `workspace_root` join key.

### 5.1 `dependencies.parquet`

| Column         | Type     | Notes |
| -------------- | -------- | ----- |
| `from_address` | `Utf8`   | source resource/module/component |
| `to_address`   | `Utf8`   | dependency target |
| `edge_kind`    | `Utf8`   | `explicit_depends_on` \| `attr_ref` \| `module_input` \| `terragrunt_dependency` |
| `source_attr`  | `Utf8`   | which attribute introduced the edge (empty if explicit) |
| `file` / `line` / `column` | as above | location of the edge |

### 5.2 `components.parquet`

One row per `Component`, summarising the parse (counts, env coverage, has_terragrunt, state backend, …). See [15-resource-graph.md § Component summary](./15-resource-graph.md).

### 5.3 `modules.parquet`

One row per discovered module (whether or not it was reached) with `source` form, resolution status, and call count.

## 6. Versioning

- **Schema major version** = breaking changes (rename, retype, reorder). Bumps require a deprecation cycle and a feature flag on the writer.
- **Schema minor version** = additive (new column at end, new optional table). Always backwards-compatible.
- The `parser_version` column embeds the writer's semver. Readers can branch on it.

A schema-version sentinel sits in the Parquet file's key-value metadata (`tfparser.schema.major`, `tfparser.schema.minor`) so a reader can detect the version without reading rows.

## 7. CLAUDE.md anchoring

- **Errors**: per CLAUDE.md § Error Handling — `thiserror` enum `tfparser_core::Error` with `#[source]` chains; no `unwrap`/`expect`/`panic!` in any IR construction path.
- **Serde**: `#[serde(rename_all = "camelCase")]` on every public type that has it; `#[serde(deny_unknown_fields)]` on config inputs (CLI config files), not on parsed HCL (we want to preserve unknown blocks).
- **Type design**: `NonZeroU32` IDs; `#[non_exhaustive]` on public types; `Arc<str>` / `Box<str>` instead of `String`; `Range<u32>` for byte ranges.
- **Safety**: `#![forbid(unsafe_code)]` at crate root.
- **Validation at boundary**: `Address::new` returns `Result<_, AddressError>`; empty addresses are rejected, lengths capped at 1024 bytes, character allowlist `[A-Za-z0-9_\-./\[\]"]`.

## 8. Cross-references

- ← Depends on: [00-prd.md](./00-prd.md)
- → Consumed by: [11-discovery.md](./11-discovery.md), [12-hcl-loader.md](./12-hcl-loader.md), [13-evaluator.md](./13-evaluator.md), [14-terragrunt.md](./14-terragrunt.md), [15-resource-graph.md](./15-resource-graph.md), [16-provider-resolver.md](./16-provider-resolver.md), [20-parquet-exporter.md](./20-parquet-exporter.md)
- ↔ Research: [terraform-repo-shapes.md](../docs/research/terraform-repo-shapes.md), [parquet-arrow-in-rust.md](../docs/research/parquet-arrow-in-rust.md), [hcl-parsing-in-rust.md](../docs/research/hcl-parsing-in-rust.md)
- ↔ Decisions: [99-key-decisions.md](./99-key-decisions.md) — D1, D2, D3, D7
