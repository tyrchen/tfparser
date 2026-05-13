# 20 — Parquet Exporter

Status: draft v1 · Owner: tfparser-core · Depends on: [16-provider-resolver.md](./16-provider-resolver.md), [10-data-model.md § Parquet schema](./10-data-model.md#parquet-schema--resourcesparquet)

## 1. Purpose

Serialize the in-memory `Workspace` into one or more Parquet files following the schema frozen in [10-data-model.md](./10-data-model.md). Schema is **authoritative** — this component does not invent columns.

## 2. Interface

```rust
// crates/core/src/exporter/mod.rs
pub trait Exporter: Send + Sync {
    fn export(&self, ws: &Workspace, opts: &ExportOptions) -> Result<ExportReport>;
}

pub struct ParquetExporter;

pub struct ExportOptions {
    pub out_dir:        Arc<Path>,
    pub tables:         TableSelection,         // bitflags: Resources | Dependencies | Components | Modules
    pub row_group_rows: usize,                  // default: 131_072 (128 k)
    pub row_group_bytes:usize,                  // default: 64 * 1024 * 1024
    pub compression:    Compression,             // default: Zstd { level: 3 }
    pub include_diagnostics: bool,              // default: true (writes diagnostics.parquet)
    pub overwrite:      bool,                   // default: false; refuse to overwrite
}

pub struct ExportReport {
    pub files:         Vec<ExportedFile>,
    pub row_counts:    BTreeMap<TableKind, u64>,
    pub bytes_written: u64,
    pub elapsed:       Duration,
}
```

## 3. Behaviour

### 3.1 Output layout

```
<out_dir>/
├── resources.parquet
├── dependencies.parquet     # if Tables::Dependencies set
├── components.parquet       # if Tables::Components set
├── modules.parquet          # if Tables::Modules set
├── diagnostics.parquet      # if include_diagnostics
└── workspace.manifest.json  # always — schema versions, hashes, command-line, parser version
```

`workspace.manifest.json` is a tiny machine-readable manifest:

```json
{
  "tfparser_version": "0.1.0",
  "schema": {"major": 0, "minor": 1},
  "generated_at": "2026-05-13T16:00:00Z",
  "workspace_root": "/Users/x/projects/...",
  "files": [
    {"name": "resources.parquet", "rows": 38412, "bytes": 12345678, "sha256": "..."}
  ],
  "command_line": "tfparser parse ... --environment staging",
  "config_digest": "sha256-..."
}
```

The manifest is the only **non-Parquet** output; it's how callers tell "are these files from the same run."

### 3.2 Writer pattern

For each table:

1. Build an `arrow::datatypes::Schema` from the spec (a function `fn resources_schema() -> Schema` in code, declared once, cross-checked by a test against [10-data-model.md](./10-data-model.md) via a code-gen or hand-checked oracle).
2. Pre-allocate per-column `*Builder`s sized to the projected row count (`workspace.resources.len()` for the resources table; ditto for others).
3. Stream rows: for each `Resource`, append one cell per column to the matching builder. The encoder is a single match-on-column-index over a const `ColumnIndex` enum — readable and lint-able.
4. Every `row_group_rows` or `row_group_bytes` (whichever first), call `ArrowWriter::write` on the assembled `RecordBatch` and reset builders.
5. On drop / on `finish`, call `ArrowWriter::close()` to flush footer + statistics.

### 3.3 Canonical JSON for `attributes_json`

A dedicated module `tfparser_core::exporter::json` renders an `AttributeMap` to canonical JSON per [10-data-model.md § 4](./10-data-model.md#canonical-json-for-attributes_json):

- Keys sorted alphabetically at every level.
- `Expression::Unresolved` rendered as `{"__unresolved__": "<source>", "__kind__": "Var|Local|Resource|Data|Module|Path|Other"}`.
- `Expression::FuncCall` rendered as `{"__unresolved_func__": "<name>", "args": [...]}`.
- `Value::Number` rendered with the smallest exact representation (no `1.0000000001` artefacts) — use `ryu` for f64-to-string.

Implementation rule: build into a single `Vec<u8>` with `serde_json::ser::Serializer<_, CompactFormatter>`, then wrap as `Arc<str>` (UTF-8 safe — `serde_json` guarantees it). This is the **only** per-row allocation in the hot path; we can pool the `Vec<u8>` across rows in a `thread_local!`.

### 3.4 Determinism

Output is byte-deterministic **modulo** `parsed_at`. To support reproducible builds, accept `--parsed-at <RFC3339>` (or `TFPARSER_FAKE_NOW=...`) to pin the timestamp; otherwise use `SystemTime::now()`. CI uses the env var to keep diffs stable.

Row order: by (`component_path`, `module_path`, `address`) ascending.

### 3.5 Concurrency

Single writer thread (Parquet's column-oriented format does not benefit from parallel append). The transform from `&Resource → row` is what we parallelise: a `rayon::par_iter()` produces row-tuples; the writer thread consumes via a bounded `crossbeam::channel` (capacity 8) so we don't materialise all rows in RAM.

Per CLAUDE.md § Async & Concurrency, this is the classic actor shape: writer owns the file handle, producers send rows. The writer task is `spawn_blocking`-able if we ever wire it under Tokio (we don't, in M0).

## 4. Invariants

- **I-EXP-1**: Schema matches the frozen spec exactly. A code test (`cargo test --test parquet_schema`) compares the Arrow `Schema::fields()` to a `serde_json` oracle stored in `tests/golden/resources-schema.json` and fails on any drift.
- **I-EXP-2**: No row is ever written with a value that violates a column's invariant (e.g. `account_id` is either `""` or `^\d{12}$`).
- **I-EXP-3**: `attributes_json` is valid JSON and parses round-trip in DuckDB.
- **I-EXP-4**: A run on the same `Workspace` with `--parsed-at <T>` produces byte-identical Parquet files.
- **I-EXP-5**: Writes are **all-or-nothing**: write to `<file>.partial`, fsync, rename to `<file>`. A crash mid-write leaves `<file>.partial` for the user to inspect; no half-written `resources.parquet`.

## 5. Error model

```rust
#[derive(thiserror::Error, Debug)]
pub enum ExportError {
    #[error("output exists and --overwrite not set: {0}")]
    OutputExists(Arc<Path>),

    #[error("parquet writer: {source}")]
    Parquet { #[source] source: parquet::errors::ParquetError, path: Arc<Path> },

    #[error("arrow: {source}")]
    Arrow { #[source] source: arrow::error::ArrowError, path: Arc<Path> },

    #[error("i/o: {source}")]
    Io { #[source] source: io::Error, path: Arc<Path> },

    #[error("invalid value for column `{column}`: {message}")]
    InvalidCell { column: &'static str, message: Box<str> },
}
```

## 6. Performance

- Target: write 100k rows of `resources.parquet` (reference-scale shape) in ≤ **600 ms** post-Workspace assembly. Compression dominates; zstd-3 at ~150 MB/s decoded throughput is the bottleneck for that size.
- Per-row allocation: 1 `Arc<str>` for `attributes_json` (pooled), and `arrow`'s internal `MutableBuffer` growth. Builders pre-sized to `n_rows`.
- Disk I/O is buffered (`BufWriter` 256 KiB). Final `fsync` + rename.

See [71-performance-budgets.md](./71-performance-budgets.md) for the full ladder.

## 7. Testing

- **Round-trip**: write → read with `arrow::ParquetRecordBatchReaderBuilder` → assert every cell matches the source `Workspace`.
- **DuckDB cross-check**: invoke `duckdb` CLI (gated test, requires the binary on PATH) and `SELECT *` to verify external readability.
- **Schema-drift test**: golden Arrow schema JSON; CI fails if the spec column list and the code disagree.
- **Atomic write**: kill the writer mid-stream (controlled fault); assert `<out>/resources.parquet` does not exist and `<out>/resources.parquet.partial` does.
- **Determinism**: write the same `Workspace` twice with `--parsed-at` pinned; `sha256` of each output file matches.

## 8. CLAUDE.md anchoring

- **Errors**: `thiserror` enum with `#[source]` and explicit `path` field on each variant — gives the user the actionable filename.
- **Serialization**: `serde_json::ser::CompactFormatter`; no pretty-print; deterministic key ordering.
- **Logging**: `tracing::info_span!("export", out = %out_dir.display())` wraps the full export; per-table `tracing::debug_span!` for row counts.
- **Performance**: `Vec::with_capacity(n_rows)` for every column builder; pooled `Vec<u8>` for JSON; no `dbg!` / `println!`.
- **Documentation**: every public type has `///` doc; `# Errors` section on `export()`.

## 9. Cross-references

- ← Depends on: [16-provider-resolver.md](./16-provider-resolver.md), [10-data-model.md § Parquet schema](./10-data-model.md#parquet-schema--resourcesparquet)
- → Consumed by: [50-cli.md](./50-cli.md)
- ↔ Research: [parquet-arrow-in-rust.md](../docs/research/parquet-arrow-in-rust.md)
- ↔ Decisions: [99-key-decisions.md](./99-key-decisions.md) — D7 (single flat table for M0), D10 (atomic write via `.partial` + rename)
