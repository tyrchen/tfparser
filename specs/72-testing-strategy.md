# 72 — Testing Strategy

Status: draft v1 · Owner: tfparser-core

## 1. Pyramid

| Layer | Count target | Where |
| ----- | ------------ | ----- |
| Unit tests | hundreds | inline `#[cfg(test)] mod tests` in each module |
| Property tests | ~30 | `proptest` blocks, one per IR invariant family |
| Snapshot tests | ~50 | `insta` snapshots, mostly under `crates/core/tests/snapshots/` |
| Integration tests | ~30 | `crates/core/tests/*.rs` and `crates/cli/tests/*.rs` |
| Fuzz harnesses | 3 | `crates/core/fuzz/` per [70-security.md § 6](./70-security.md#fuzzing) |
| Benchmarks | ~8 | `crates/core/benches/` per [71-performance-budgets.md](./71-performance-budgets.md) |

## 2. Naming

Per CLAUDE.md § Testing — `test_should_<behaviour>_<context>`:

```rust
#[test]
fn test_should_classify_terragrunt_dir_with_include_as_component() { … }

#[test]
fn test_should_reject_symlink_escape_from_workspace_root() { … }

#[test]
fn test_should_emit_unresolved_for_data_source_reference() { … }
```

## 3. Fixtures

```
crates/core/tests/fixtures/
├── large-monorepo/             # ~30-component synthetic reference monorepo (Terragrunt + multi-account)
│   ├── terraform/
│   │   ├── root.hcl
│   │   ├── environments/
│   │   ├── live-site/<10 components>/
│   │   ├── platform/<10 components>/
│   │   ├── networks/<5 components>/
│   │   └── modules-tf12/<5 modules>/
│   └── expected/
│       ├── workspace.snapshot.json
│       └── resources.parquet
├── single-component/           # one .tf file, no terragrunt
├── multi-provider/             # 4 aws aliases, cross-account
├── for-each-unresolved/        # for_each over Unresolved input
├── cycle/                      # local.a → local.b → local.a
└── malformed/                  # mid-block syntax errors, ensure graceful skip
```

`large-monorepo` is the **anchor fixture**: every milestone's exit criterion includes "parses `large-monorepo` correctly." It is synthetic (no real account IDs, no real secrets), structurally representative of the patterns documented in [terraform-repo-shapes.md](../docs/research/terraform-repo-shapes.md), and checked into the repo.

## 4. Snapshot policy

`insta` snapshots are committed; updates require `cargo insta review` and a reviewer line in the PR. Snapshots cover:

- The full `Workspace` JSON for `large-monorepo`.
- The schema JSON for `resources.parquet`.
- Diagnostics list for each `malformed/` fixture.
- The `--help` output for each CLI subcommand.

Diff size budget: a structural change should not touch > 200 lines of snapshot. If it does, the change is too broad; split it.

## 5. Property tests

Each invariant family from [10-data-model.md § 2.5](./10-data-model.md) gets a `proptest!` block. Key ones:

- **Address round-trip**: parse → format → parse equals input.
- **Lowering preserves spans**: every lowered `Expression` has a `Span` whose byte range slices into the original source contiguously.
- **Provider rewrite commutativity**: expand-then-substitute equals substitute-then-expand for synthesised module trees.
- **Evaluator monotonicity**: adding bindings to the context never *removes* resolved values from the output.
- **Canonical JSON determinism**: same input → byte-identical JSON.

## 6. Integration tests

`crates/core/tests/`:

- `integration_pipeline.rs` — runs the full pipeline on `large-monorepo`, asserts row counts and a handful of spot checks.
- `integration_terragrunt_cascade.rs` — verifies that with `--environment staging` the resolved `var.environment` is `"staging"` in three specific resources.
- `integration_account_resolution.rs` — supplies a synthesised profile map; asserts `account_id` is populated for every `aws.main`-bound resource.
- `integration_export_round_trip.rs` — writes Parquet, reads it back via `arrow`, compares against the source `Workspace`.

`crates/cli/tests/`:

- `cli_parse.rs` — `Command::cargo_bin("tfparser").args([…]).assert().success()`. Validates stdout summary lines and exit code 0.
- `cli_inspect.rs` — `inspect` exits 0 even with diagnostics; output contains a "diagnostics" section.
- `cli_help.rs` — `--help` snapshot, version subcommand prints the right semver.

Use `duct` for spawning, `assert_cmd` + `predicates` for stdout/stderr assertions.

## 7. Cross-platform tests

CI runs on `ubuntu-latest`, `macos-latest`, `windows-latest`. Path-handling tests double-check that `Component.path` always serialises with `/` separators (even on Windows). Symlink tests are skipped on Windows.

## 8. Coverage

Use `cargo llvm-cov` in CI. Targets:

- `tfparser-core`: ≥ 85 % line coverage, ≥ 80 % branch coverage.
- `tfparser-cli`: ≥ 70 % (CLI glue is mostly arg parsing; less to cover deeply).

Coverage is a hint, not a gate, per CLAUDE.md § Testing. Edge cases > raw %.

## 9. DuckDB cross-check

A gated test (`#[ignore]`, run in CI only) invokes the local `duckdb` CLI on the Parquet artifact:

```sql
SELECT
  COUNT(*)               AS rows,
  COUNT(DISTINCT account_id) AS accounts,
  json_extract(attributes_json, '$.tags.Team') AS team
FROM 'resources.parquet'
WHERE environment = 'staging'
GROUP BY team
HAVING COUNT(*) > 1000;
```

Run validates that DuckDB reads our file and that `json_extract` works on `attributes_json`. The test asserts the expected row count from `large-monorepo`. Failure → schema drift or canonical-JSON bug.

## 10. Cross-references

- ← Depends on: every component spec
- ↔ Perf: [71-performance-budgets.md](./71-performance-budgets.md)
- ↔ Security: [70-security.md § Fuzzing](./70-security.md)
- ↔ CLAUDE.md § Testing
