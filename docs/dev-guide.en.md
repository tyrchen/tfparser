# Developer Guide

> *Looking for the spec set? See [`./specs/`](../specs/). Looking for*
> *end-user docs? See [`user-guide.en.md`](./user-guide.en.md). 中文版：*
> *[dev-guide.zh.md](./dev-guide.zh.md).*

## 1. Workspace layout

```text
.
├── apps/
│   └── cli/                tfparser-cli — thin wrapper around tfparser-core
├── crates/
│   └── core/               tfparser-core — the library
│       ├── examples/       runnable end-to-end demos
│       └── benches/        criterion micro-benches
├── docs/                   guides + research memos (you are here)
├── fixtures/               synthetic TF/Terragrunt workspaces used by tests
├── specs/                  design documents (PRD, components, etc.)
└── Makefile                CI surface (build, test, lint, fuzz, bench)
```

Cargo workspace members are declared in the top-level
[`Cargo.toml`](../Cargo.toml) as `crates/*` and `apps/*`. Add a new
crate by dropping it under either prefix; nothing else needs editing.

## 2. The library facade

`tfparser-core` exposes a layered surface. Reach for the highest level
that does the job.

### 2.1 One-liner

```rust
let workspace = tfparser_core::parse("./my-tf-repo")?;
```

Equivalent to:

```rust
tfparser_core::Parser::builder()
    .workspace_root("./my-tf-repo")
    .build()?
    .parse()?;
```

### 2.2 Builder with tuned options

```rust
use std::sync::Arc;
use std::path::Path;
use tfparser_core::{Parser, EnvVarMode, ExportOptions};

let parser = Parser::builder()
    .workspace_root("./my-tf-repo")
    .environment("production")
    .default_region("us-west-2")?
    .env_var_mode(EnvVarMode::Passthrough)
    .allow_env("TF_VAR_environment")
    .var("region", "us-east-1")
    .strict_providers(true)
    .max_walk_depth(32)
    .max_file_bytes(8 * 1024 * 1024)
    .build()?;

let export = ExportOptions::builder()
    .out_dir(Arc::<Path>::from(Path::new("./out")))
    .overwrite(true)
    .build();

let (workspace, report) = parser.parse_and_export(&export)?;
```

### 2.3 Lower-level pieces

Below `Parser` the pipeline trait + stage primitives are reachable directly
when you want to override one phase or run the parts independently:

| Want | Type / fn |
| ---- | --------- |
| Replace the whole flow in tests | impl [`Pipeline`](../crates/core/src/pipeline.rs) |
| Just discovery | [`FsDiscoverer`](../crates/core/src/discovery) |
| Just the HCL loader | [`HclEditLoader`](../crates/core/src/loader) |
| Just the evaluator | [`HclEvaluator`](../crates/core/src/eval) |
| Just the Terragrunt resolver | [`FsTerragruntResolver`](../crates/core/src/terragrunt) |
| Just the provider resolver | [`DefaultProviderResolver`](../crates/core/src/provider) |
| Just the exporter | [`ParquetExporter`](../crates/core/src/exporter) |

Every trait above is `Send + Sync` and returns `Result<_, _>` with phase-
specific error types — you can swap stubs in tests without unsafe magic.

### 2.4 The prelude

```rust
use tfparser_core::prelude::*;
```

Re-exports the ~14 names a typical consumer touches: `parse`, `Parser`,
`ParserBuilder`, `Workspace`, `Component`, `Module`, `Resource`,
`Diagnostic`, `Severity`, `Result`, `Error`, `ExportOptions`,
`ExportReport`, `Exporter`, `ParquetExporter`.

## 3. Running examples

```sh
# tiny one-liner
cargo run -p tfparser-core --example parse_one_liner -- ./fixtures/single-component

# parse + export the four Parquet tables
cargo run -p tfparser-core --example parse_and_export -- ./fixtures/single-component ./out
```

## 4. Build / test / lint

Use the `Makefile` as the single source of truth — CI runs the same
targets:

```sh
make ci          # build + test + fmt-check + clippy -D warnings + cargo doc + cargo deny
make bench       # criterion micro-benches (target/criterion/)
make fuzz-hcl-loader   # 10-min fuzz pass over the loader
```

A faster inner loop:

```sh
cargo test -p tfparser-core              # core tests only
cargo test -p tfparser-cli               # CLI integration tests
cargo test -p tfparser-core --doc        # doctests
cargo clippy --workspace --all-targets -- -D warnings
cargo +nightly fmt --all
```

## 5. Repo invariants the workspace lints enforce

| Lint | Why |
| ---- | --- |
| `unsafe_code = forbid` | Soundness contract; no `unsafe` ever. |
| `unwrap_used` / `expect_used` / `panic` / `indexing_slicing` deny | No reachable panics from external input. Tests opt out per-module. |
| `print_stdout` / `print_stderr` deny | Use `tracing` everywhere except CLI / examples. |
| `missing_docs` warn | Public items must be documented. |
| `pedantic` warn | Spec preference; the few `#[allow]`s are justified in code. |

See the [project CLAUDE.md](../CLAUDE.md) for the long-form rationale.

## 6. Adding a stage

Pipelines run linearly in [`pipeline.rs`](../crates/core/src/pipeline.rs):
discovery → loader → projection → terragrunt → evaluator → graph →
provider → (exporter). Adding a new step has three touchpoints:

1. **Type** — define a `pub trait` in the stage's module with one method
   that consumes a previous-stage output and produces the next. Make it
   `Send + Sync`. Mirror an existing trait (e.g.
   [`TerragruntResolver`](../crates/core/src/terragrunt/mod.rs)) for the
   shape.
2. **Default impl** — provide a `Default<Stage>` struct so the trait isn't
   abstract.
3. **Wire** — call it inside `DefaultPipeline::run` between the right
   neighbours and propagate any new options through `PipelineOptions` /
   `ParserBuilder`.

Touch the [`tests/`](../crates/core/tests) integration tests to lock the
new fixture round-trip; if the new stage emits Parquet columns, extend
[`specs/10-data-model.md`](../specs/10-data-model.md) before changing the
schema.

## 7. Error model

Crate-wide:

```rust
type Result<T> = std::result::Result<T, tfparser_core::Error>;
```

`Error` is `#[non_exhaustive]` and wraps phase-specific errors (`Provider`,
`Export`, …) via `#[from]`. New variants are additive; never rename a
field — `Workspace::diagnostics` is the path for non-fatal information.

## 8. Performance + repro

- `make bench` runs `criterion` on the `large-monorepo` fixture; record a
  baseline with `make bench-save-baseline` and gate diffs with
  `make bench-vs-baseline` (10 % regression budget).
- For deterministic byte-equal Parquet output, pin
  `ExportOptions::parsed_at_ms` and use zstd-3 (the default).
- The `workspace.manifest.json` carries SHA-256 of every artifact; CI
  diff-detects schema or data drift via `tfparser verify`.

## 9. Adding a doc

Drop a new file under `./docs/`. Per the
[project rule](../CLAUDE.md#documentation), every new doc must be linked
from [`docs/index.md`](./index.md). The convention is:

- end-user content → `docs/<topic>.en.md` + `docs/<topic>.zh.md`
- design notes → `docs/research/<topic>.md`

If your change requires a spec update, edit under `./specs/` and update
[`specs/index.md`](../specs/index.md) instead.
