# Research — Parquet/Arrow export in Rust

Status: memo · Date: 2026-05-13 · Owner: tfparser-core

## Question

How should `tfparser` emit Parquet files for ~hundreds of thousands of resources, with a schema flexible enough to grow without breaking existing readers?

## Crates evaluated

| Crate | Version | Notes |
| ----- | ------- | ----- |
| `arrow` + `parquet` (arrow-rs) | parquet 58.3.0 | Official Apache project. Stable, broad-typed, full Parquet feature set (statistics, dictionary, snappy/zstd, predicate push-down on read). Schema is built explicitly from `arrow::datatypes::{DataType, Field, Schema}`. |
| `polars` (incl. `polars-io` parquet feature) | latest 1.x | Wraps `arrow`/`parquet` under a DataFrame API. Convenient for ad-hoc work; ties our public type surface to Polars semantics and version. |
| `parquet2` / `arrow2` | unmaintained for our use | Forks from 2022; the original maintainer has migrated effort back to arrow-rs. Skip. |

## Decision

- **Use `arrow` + `parquet` (arrow-rs) directly** in the core library. Polars is a great consumer (and we recommend it for users) but our schema is the contract; a Polars `DataFrame` is an implementation detail leaking version policy onto every caller.
- **Schema declared explicitly**, not derived from Rust structs. Parquet schemas evolve; deriving from a `#[derive(ParquetRecord)]` macro couples schema changes to type changes and is easy to do wrong (column order, nullability).
- **One file = one logical table** to start (`resources.parquet`). Future tables (`dependencies.parquet`, `components.parquet`, etc.) land as separate files in the same output directory.

## M0 schema (frozen for first major)

See [10-data-model.md § Parquet schema](../../specs/10-data-model.md) for the canonical definition. Summary:

- `workspace_root: utf8` — absolute path of the input
- `component_path: utf8` — relative path of the component dir (e.g. `live-site/ads-pacer`)
- `module_path: utf8` — empty for component-level resources, dotted for nested (e.g. `pacer_db`)
- `address: utf8` — full Terraform address (e.g. `module.pacer_db.aws_db_instance.this`)
- `resource_type: utf8` — e.g. `aws_db_instance`
- `resource_name: utf8` — local label
- `kind: utf8` — `resource` | `data` | `module` | `output` | `variable` | `local` | `provider`
- `provider_local: utf8` — provider alias used (e.g. `aws.main`); empty if default
- `provider_source: utf8` — resolved provider source addr (e.g. `hashicorp/aws`)
- `account_id: utf8` — resolved AWS account (or empty if not yet resolved)
- `region: utf8` — resolved region
- `environment: utf8` — empty if env-agnostic, else `staging`/`production`/…
- `count_expr: utf8` — verbatim `count` expression, empty if absent
- `for_each_expr: utf8` — verbatim `for_each` expression, empty if absent
- `depends_on: list<utf8>` — explicit dependency addresses, plus inferred ones
- `attributes_json: utf8` — full attribute body as canonical JSON (with `Unresolved("var.x")` rendered as the source string)
- `file: utf8` — relative path
- `line: uint32`, `column: uint32` — start position
- `parsed_at: timestamp[ms,UTC]` — when the parser produced this row

Row group target: **128k rows or 64 MB**, whichever first. Compression: **zstd level 3** (good balance, default per arrow-rs guidance).

## Writer pattern

`parquet::arrow::ArrowWriter<File>` streams `RecordBatch` instances to disk; we buffer rows into a builder (one column-wise `*Builder` per field), flush every N rows, and finalize on drop.

Builder choice per column:
- `Utf8Builder` for all strings (avoids per-row alloc for short strings via small-string optimisation in arrow's `MutableBuffer`).
- `ListBuilder<Utf8Builder>` for `depends_on`.
- `UInt32Builder` for `line` / `column`.
- `TimestampMillisecondBuilder` for `parsed_at`.

We **pre-allocate** all builders to the projected row count (passed in by the caller; default = number of `.tf` files × 10) to avoid mid-write growth.

## Why not derive macros / DataFusion?

- Derive macros (`parquet_derive`, `serde_arrow`) hide column ordering and nullability. The schema is a public contract; **the spec authoritatively defines column order**, not a struct's declaration order.
- DataFusion is overkill for write-only; we add it only if/when the server grows a SQL query surface.

## Risks retired

- **R4 — "Can we represent dynamic resource bodies?"** Yes: the `attributes_json` column is a self-describing JSON string. Polars/DuckDB can `json_extract` it. We considered Arrow `Map<utf8, utf8>` and rejected it because attribute *values* are heterogeneous (string/number/bool/list/object), so the right Arrow type is `Struct` per resource type, which we don't know statically. JSON-on-utf8 is the pragmatic choice.
- **R5 — "Does zstd compress this well?"** Synthetic: 100k resources × ~2 KB JSON ≈ 200 MB raw → ~25 MB zstd-3 (typical 7–10× ratio on repetitive HCL bodies). Tolerable.

## Open risks

- **R-OPEN-3**: For very large fleets (>5M resources), the `attributes_json` column becomes the bottleneck. Plan B is to crack out top-level attributes into typed columns per `resource_type`, but that is a v2 problem.

## References

- [arrow-rs parquet crate](https://arrow.apache.org/rust/parquet/index.html)
- [Polars Parquet reader](https://docs.pola.rs/api/rust/dev/polars_io/parquet/read/struct.ParquetReader.html) — we recommend it to users
- [Arrow columnar format spec](https://arrow.apache.org/docs/format/Columnar.html)
