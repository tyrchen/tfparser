# 61 — Workspace Layout, Crates & Features

Status: draft v1 · Owner: tfparser-core · Depends on: [00-prd.md § Naming conventions](./00-prd.md)

## 1. Purpose

Pin the Cargo workspace, crate names, public dependency graph, and Cargo feature surface. Once published, the crate split is harder to change than the data model — get it right.

## 2. Workspace

```
tfparser/
├── Cargo.toml                  (workspace root)
├── crates/
│   ├── core/                   tfparser-core (library)
│   └── cli/                    tfparser (binary)
└── apps/
    └── server/                 (out of scope for M0 — empty skeleton allowed)
```

The current repo has `crates/core` and `apps/server`. We add `crates/cli`; the `apps/server` skeleton remains and gets fleshed out post-M5 (or removed if not pursued).

Workspace `Cargo.toml`:

```toml
[workspace]
members = ["crates/*", "apps/*"]
resolver = "3"

[workspace.package]
version      = "0.1.0"
edition      = "2024"
license      = "MIT"
rust-version = "1.83"           # required for edition 2024 stable + native async fn in traits

[workspace.dependencies]
# … (see § 4 below)
```

`rust-toolchain.toml`:

```toml
[toolchain]
channel    = "stable"
components = ["rustfmt", "clippy", "miri"]
```

(Miri only for opt-in CI jobs; we never `unsafe` so `miri test` should pass for free.)

## 3. Crates

### 3.1 `tfparser-core` (library)

Public API surface, **the** crate downstream Rust callers depend on. `#![forbid(unsafe_code)]`. Tested heavily.

Public modules:
- `tfparser_core::ir` — Workspace, Component, Resource, Address, Span, Value, Expression. See [10-data-model.md](./10-data-model.md).
- `tfparser_core::discovery` — `Discoverer` trait, `FsDiscoverer`. See [11-discovery.md](./11-discovery.md).
- `tfparser_core::loader` — `Loader` trait, `HclEditLoader`. See [12-hcl-loader.md](./12-hcl-loader.md).
- `tfparser_core::eval` — `Evaluator` trait, `HclEvaluator`. See [13-evaluator.md](./13-evaluator.md).
- `tfparser_core::terragrunt` — `TerragruntResolver` trait, `FsTerragruntResolver`. See [14-terragrunt.md](./14-terragrunt.md).
- `tfparser_core::graph` — `GraphBuilder` trait, `DefaultGraphBuilder`. See [15-resource-graph.md](./15-resource-graph.md).
- `tfparser_core::provider` — `ProviderResolver` trait, `DefaultProviderResolver`, `ProfileMap`. See [16-provider-resolver.md](./16-provider-resolver.md).
- `tfparser_core::exporter` — `Exporter` trait, `ParquetExporter`. See [20-parquet-exporter.md](./20-parquet-exporter.md).
- `tfparser_core::pipeline` — `Pipeline::run(opts)` convenience wrapper that wires the default impls together.
- `tfparser_core::Error`, `tfparser_core::Result` — top-level error and result types.

### 3.2 `tfparser` (binary)

The CLI in [50-cli.md](./50-cli.md). Single binary crate, depends on `tfparser-core` + `clap` + `tracing-subscriber` + `indicatif` + `anyhow`. `#![forbid(unsafe_code)]`.

### 3.3 `apps/server` (deferred)

Empty/skeleton until at least M5. Eventually: an `axum` server hosting parsed workspaces with /resources, /graph endpoints. Not part of the M0–M5 scope, but the workspace already includes it so we don't restructure later.

## 4. Workspace dependencies (pinned, latest stable as of 2026-05)

```toml
[workspace.dependencies]
# Parsing
hcl-edit          = "0.9"
hcl-rs            = { package = "hcl-rs", version = "0.19" }      # eval module
regex             = "1.11"
winnow            = "1.0"
ignore            = "0.4"
globset           = "0.4"
rust-ini          = "0.21"

# Data model
arrow             = { version = "58", default-features = false, features = ["prettyprint"] }
parquet           = { version = "58", default-features = false, features = ["arrow", "zstd", "snap"] }
serde             = { version = "1", features = ["derive", "rc"] }
serde_json        = "1"
serde_yaml        = "0.9"     # YAML profile map
toml              = "0.8"     # tfparser.toml
validator         = { version = "0.20", features = ["derive"] }

# Concurrency / utility
rayon             = "1.10"
dashmap           = "6"       # 6.x stable; avoid 7-rc
crossbeam-channel = "0.5"
arc-swap          = "1.7"
once_cell         = "1.20"
smallvec          = { version = "1.13", features = ["serde"] }
bytes             = "1.7"

# Numerics / hashing
sha2              = "0.10"
md-5              = "0.10"
base64            = "0.22"
ryu               = "1"

# Errors / logging
thiserror         = "2"
anyhow            = "1"
tracing           = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }

# CLI
clap              = { version = "4", features = ["derive", "env"] }
indicatif         = "0.17"

# Time
jiff              = "0.2"     # CLAUDE.md neutral on time crate; jiff is the modern pick

# Dev / test
rstest            = "0.23"
proptest          = "1.5"
insta             = "1.40"    # snapshot testing
tempfile          = "3"
duct              = "0.13"    # integration-test process spawning
arbitrary         = { version = "1", features = ["derive"] }
libfuzzer-sys     = "0.4"
```

### 4.1 Dependency policy

Per CLAUDE.md § Dependencies:
- Pinned with `^` (default) for minor updates. Patch updates auto.
- Every dep audited via `cargo audit` on CI. `cargo-deny` for licenses.
- No FFI bindings; pure-Rust crates everywhere. (`parquet` itself is pure Rust.)
- No transitive `openssl-sys`. TLS for any future networked piece uses `rustls` + `aws-lc-rs` per CLAUDE.md.
- `default-features = false` where it saves compile time / surface area (`arrow`, `parquet`).

### 4.2 Lints applied at the workspace root

`Cargo.toml`:

```toml
[workspace.lints.rust]
unsafe_code               = "forbid"
rust_2024_compatibility   = "warn"
missing_docs              = "warn"
missing_debug_implementations = "warn"

[workspace.lints.clippy]
all       = { level = "warn", priority = -1 }
pedantic  = { level = "warn", priority = -1 }
unwrap_used        = "deny"
expect_used        = "deny"
indexing_slicing   = "deny"
panic              = "deny"
todo               = "deny"
unimplemented      = "deny"
dbg_macro          = "deny"
print_stdout       = "deny"
print_stderr       = "deny"      # CLI uses tracing; tests can override
# Allow with justification:
module_name_repetitions = "allow"
missing_errors_doc      = "allow"   # we have a Result alias; documenting per fn is noisy
```

Test code overrides locally via `#[allow(clippy::unwrap_used)]`.

## 5. Feature flags (intentionally minimal)

`tfparser-core`:

| Feature              | Default | Purpose                                                                                                                                            |
| -------------------- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `parquet`            | ✓       | Compile the Parquet exporter and `arrow`/`parquet` deps. Without it, the IR is library-only — useful for embedders who want their own emit format. |
| `aws-config`         | ✓       | Include the `~/.aws/config` profile-map loader. Off → only YAML loader available.                                                                  |
| `tracing-instrument` | ✓       | Compile-in `tracing::instrument` attributes. Off → strip for size/secrecy.                                                                         |
| `fuzz`               | off     | Wire up `arbitrary::Arbitrary` impls on IR types for fuzz harnesses.                                                                               |

Avoid feature flags for "extra parsers" or "alternate evaluators" — those go behind traits, not features. Per CLAUDE.md § Dependencies, fewer features = less compile-matrix maintenance.

## 6. Public API stability

- **`tfparser_core::ir::*`** — frozen at v0.1; additive changes only (new fields on `#[non_exhaustive]` structs).
- **Trait surfaces** (`Discoverer`, `Loader`, `Evaluator`, etc.) — frozen at v0.1; new methods require a major bump or a default impl.
- **Parquet schema** — frozen separately (see [10-data-model.md § Versioning](./10-data-model.md#versioning)). Schema major may bump independently of crate major.

## 7. CI shape

Per CLAUDE.md § Toolchain & Build:
- `cargo build --workspace --all-features`
- `cargo +nightly fmt -- --check`
- `cargo clippy --workspace --all-features --all-targets -- -D warnings`
- `cargo test --workspace --all-features` (nextest preferred)
- `cargo +nightly miri test -p tfparser-core` (we never `unsafe`, but proves it)
- `cargo audit`
- `cargo deny check`
- `cargo doc --no-deps --workspace` (warn on missing docs)
- Fuzz: `cargo +nightly fuzz run hcl_loader -- -max_total_time=600` (CI)
- Benchmark gate: `cargo bench --bench parse_large_monorepo -- --baseline=main` post-Phase 9

## 8. Cross-references

- ← Depends on: [00-prd.md](./00-prd.md)
- ↔ All component specs (every spec references the dep graph defined here)
- ↔ CLAUDE.md § Dependencies / § Toolchain & Build
