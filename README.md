# tfparser

Parse a Terraform / Terragrunt source repository into a typed in-memory IR
and emit it as **Parquet** for queries with [DuckDB], [ClickHouse], or any
Arrow-compatible engine.

Built end-to-end in Rust 2024 — `#![forbid(unsafe_code)]`, no `unwrap`/`panic`
reachable from external input, every input boundary validated.

- 📦 [`tfparser-core`](./crates/core) — the library
- 🛠️ [`tfparser-cli`](./crates/cli) — the `tfparser` binary

## Why

You have a multi-account, multi-region AWS estate described in
Terraform/Terragrunt across many components, modules, and environments. You
want to answer questions like *"which S3 buckets exist in account 1234 and
who depends on them?"* without running `terraform plan`. `tfparser` reads
your source repo, evaluates as much as it can statically (locals, inputs,
function calls, Terragrunt cascade), and dumps the result as 4 Parquet
tables you can join in DuckDB:

| Table | Rows |
| ----- | ---- |
| `resources.parquet` | Every `resource`, `data`, `provider`, `module`, `variable`, `local`, `output` row. |
| `dependencies.parquet` | Inferred + explicit dependency edges (`attr_ref`, `explicit_depends_on`, `module_input`, `terragrunt_dependency`). |
| `components.parquet` | One row per discovered component with summary counts. |
| `modules.parquet` | One row per distinct module source with `call_count`. |

Plus `workspace.manifest.json` with SHA-256 hashes for reproducibility.

## Install

```sh
cargo install --path crates/cli --locked
# or, after publish:
cargo install tfparser-cli
```

## Quickstart

```sh
# parse a workspace
tfparser parse ./fixtures/large-monorepo -o ./out

# query with DuckDB
duckdb -c "
  SELECT r.component_path, COUNT(*) AS edges
  FROM 'out/resources.parquet' r
  LEFT JOIN 'out/dependencies.parquet' d
    ON d.from_address = r.address
  WHERE r.kind = 'resource'
  GROUP BY r.component_path
  ORDER BY edges DESC;
"

# verify a previous run hasn't been tampered with
tfparser verify --dir ./out
```

### Common flags

```text
--environment <NAME>      Pin a terraform.workspace name for Terragrunt cascade
--region <REGION>         Default AWS region when neither provider nor cascade supplies one
--profile-map <PATH>      YAML profile-map (per spec 16 § 3.2)
--aws-config <PATH>       ~/.aws/config (per spec 16 § 3.1)
--var KEY=VALUE           Bind a Terraform variable (repeatable)
--allow-env NAME          Allowlist an env var visible to get_env(...) (repeatable)
--env-mode {strict,passthrough,mock}
                          Policy for get_env(...) (default: strict)
--strict-providers        Fail when any referenced AWS profile isn't in the map
--compression {zstd,snappy,uncompressed}
--zstd-level <1..=22>     Default: 3
--tables {all,none}       Whether to emit secondary tables (default: all)
--parsed-at <RFC3339>     Pin the manifest's parsed_at for reproducible builds
--overwrite               Overwrite existing files in --out
```

## Status

| Milestone | Phase | Status |
| --------- | ----- | ------ |
| M0 — schema-locked Parquet output | 1–3 | ✅ |
| M1 — evaluator (locals / vars / stdlib) | 4 | ✅ |
| M2 — module expansion (count / for_each) | 5 | ✅ |
| M3 — Terragrunt cascade | 6 | ✅ |
| M4 — provider/account/region resolver | 7 | ✅ |
| M5 — dependency graph + secondary tables | 8 | ✅ |
| M6 — hardening + benches + docs | 9 | ✅ |

See [`./specs/`](./specs/) for the full design set and
[`./specs/91-impl-plan.md`](./specs/91-impl-plan.md) for the build order.

## Performance

End-to-end parse on the `large-monorepo` fixture (~30 components, ~280
resource rows) on Apple M-series:

| Phase | Median |
| ----- | ------ |
| Discovery | ~2 ms |
| Loader | ~3 ms |
| Evaluator | ~63 µs |
| Exporter | ~30 ms |
| **End-to-end** | **~8 ms** |

Run `make bench` to repeat the numbers locally; `make bench-save-baseline`
+ `make bench-vs-baseline` gates a 10 % regression budget.

## Development

```sh
make ci          # build + test + fmt + clippy -D warnings + cargo doc + cargo deny
make bench       # criterion micro-benches
make fuzz-hcl-loader  # 10-min fuzz pass
```

## License

MIT — see [LICENSE.md](LICENSE.md). Copyright © 2025 Tyr Chen.

[DuckDB]: https://duckdb.org
[ClickHouse]: https://clickhouse.com
