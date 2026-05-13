# 90 — Roadmap (Stakeholder-Facing)

Status: draft v1 · Owner: tfparser-core · Last updated: 2026-05-13

Organised by **user-visible feature**. Each milestone leaves the workspace green on the standard quality gates ([CLAUDE.md § Toolchain & Build](../CLAUDE.md), [61-crates-and-features.md § CI shape](./61-crates-and-features.md)) and exit criteria are observable: a command runs, a test passes, a row appears in Parquet.

For the **engineer-facing dependency order**, see [91-impl-plan.md](./91-impl-plan.md). The two pair 1:1 on milestones but the order differs because contracts land before consumers.

## 0. Principles

- **Always shippable.** Every milestone ends with `cargo test`, `cargo clippy -D warnings`, `cargo audit` green.
- **Type-safety first.** Each milestone may defer features but never relaxes guarantees: `#![forbid(unsafe_code)]`, no `unwrap`/`expect`/`panic` reachable from input, fuzz harness green.
- **Honest calibration.** Estimates are realistic; pad for review / on-call / meetings. See [§ 3](#3-calendar-shape).

## 1. Build-order graph (concise)

```text
00-prd ─┬→ 10-data-model ─┬→ 11-discovery ┬→ 12-hcl-loader ┬→ 20-parquet-exporter ┬→ M0
        │                  │              └→ 13-evaluator ──┐                     │
        │                  └→ 14-terragrunt ────────────────┤                     │
        │                                  ┌→ 15-resource-graph                   │
        │                                  └→ 16-provider-resolver                │
        │                                                                          │
        └→ 70-security / 71-performance-budgets / 72-testing-strategy (cross-cuts)
```

## 2. Milestones

### M0 — "List every resource as a flat Parquet file"

**Touches**: [00-prd.md](./00-prd.md), [10-data-model.md](./10-data-model.md), [11-discovery.md](./11-discovery.md), [12-hcl-loader.md](./12-hcl-loader.md), [20-parquet-exporter.md](./20-parquet-exporter.md), [50-cli.md](./50-cli.md), [61-crates-and-features.md](./61-crates-and-features.md), [70-security.md](./70-security.md), [72-testing-strategy.md](./72-testing-strategy.md).

**Promise**: a user can run `tfparser parse <repo> -o ./out` on a plain Terraform repo (no Terragrunt, no module expansion, no variable resolution) and get `resources.parquet` with one row per HCL `resource` / `data` block, with file/line/component/type/name populated.

**Exit criteria**:
- `cargo test -p tfparser-core` passes the single-component fixture and the multi-file plain-TF fixture.
- `tfparser parse crates/core/tests/fixtures/single-component -o /tmp/out` produces `resources.parquet` readable by `duckdb`.
- `tfparser inspect` summarises components without writing.
- `account_id`, `region`, `environment`, `module_path` columns are present but empty for every row (proves the schema is locked, even when fillers haven't arrived).

### M1 — "Variables and locals are resolved when statically possible"

**Touches**: [13-evaluator.md](./13-evaluator.md).

**Promise**: a `provider "aws" { region = var.region }` with a `region = "us-east-2"` default produces `region = "us-east-2"` in the IR (and downstream Parquet, once the resolver lands). Unresolvable refs (`data.x.y`, `aws_*.z.attr`) stay `Unresolved` and are emitted as `__unresolved__` sentinels in `attributes_json`.

**Exit criteria**:
- Locals fixpoint resolves a hand-crafted cycle of 5 chained locals.
- `${var.foo}-${var.bar}` template concatenation reduces when both vars resolve.
- `crates/core/tests/fixtures/multi-provider/` reports correct per-resource `provider_local` after evaluation.
- `cargo fuzz run fuzz_evaluator -- -max_total_time=600` clean.

### M2 — "Modules are expanded — referenced module bodies appear in Parquet"

**Touches**: [15-resource-graph.md](./15-resource-graph.md).

**Promise**: a component with `module "db" { source = "../../modules/rds" }` produces Parquet rows for every resource inside `modules/rds`, with `module_path = "db"` and addresses prefixed `module.db.aws_*`. Cross-module dependency edges captured.

**Exit criteria**:
- A two-level nested module fixture produces correctly-prefixed addresses.
- `for_each = { a = 1, b = 2 }` (literal) expands to two rows.
- `count = var.foo` (unresolved) emits one template row with `count_expr` non-empty.
- Address uniqueness invariant ([I-GRAPH-1](./15-resource-graph.md#invariants)) checked by an integration test.

### M3 — "Terragrunt-based repos work end-to-end"

**Touches**: [14-terragrunt.md](./14-terragrunt.md).

**Promise**: a repo using `terragrunt.hcl` with `include`, `find_in_parent_folders`, `read_terragrunt_config`, and a `root.hcl` cascade parses correctly. The `effective_locals` (post-cascade) flow into the evaluator and influence per-resource fields.

**Exit criteria**:
- `crates/core/tests/fixtures/large-monorepo/` (~30 components, Terragrunt cascade, multi-env) parses without errors.
- Include depth cap rejects a synthesised cycle with the full stack in the error.
- Memoisation halves the parse time vs un-memoised on the same fixture (verified by the bench).
- Per-environment runs (`--all-environments`) produce one row set per env, distinguished by the `environment` column.

### M4 — "Every AWS resource has account_id / region populated"

**Touches**: [16-provider-resolver.md](./16-provider-resolver.md).

**Promise**: with `--profile-map ~/.aws/config` (or a YAML map), every `aws_*` resource whose provider chain resolves has a non-empty `account_id` and `region`. `state_account_id` / `state_region` filled from the component's state backend.

**Exit criteria**:
- Goal G4 from [00-prd.md § Goals](./00-prd.md): ≥ 95 % `account_id` coverage on the reference-scale fixture.
- AWS-config loader handles `sso_account_id`, `role_arn` chained via `source_profile`.
- Provider-alias inheritance through modules (`providers = { aws = aws.main }`) verified by integration test.

### M5 — "Dependency graph is queryable as Parquet"

**Touches**: [15-resource-graph.md](./15-resource-graph.md), [20-parquet-exporter.md](./20-parquet-exporter.md), [10-data-model.md § Secondary tables](./10-data-model.md#5-secondary-tables-m5).

**Promise**: `dependencies.parquet`, `components.parquet`, `modules.parquet` ship alongside `resources.parquet`. A user can SQL-join across them.

**Exit criteria**:
- Explicit `depends_on` and inferred attribute references both produce edges, with `edge_kind` correct.
- Terragrunt `dependency` blocks → component-to-component edges.
- Manifest hash chains all four files; `tfparser verify` (new subcommand) confirms integrity.
- DuckDB cross-check: a 3-table join across `resources`, `dependencies`, `components` returns expected rows on the `large-monorepo` fixture.

### M6 — "Hardened, fast, ready for v0.1"

**Touches**: [70-security.md](./70-security.md), [71-performance-budgets.md](./71-performance-budgets.md), [72-testing-strategy.md](./72-testing-strategy.md).

**Promise**: parse time on `large-monorepo` ≤ 5 s; fuzz harness clean for ≥ 6 h overnight; `cargo audit` / `cargo deny` clean; doc coverage ≥ 95 %.

**Exit criteria**:
- All performance budgets in [71-performance-budgets.md § 3](./71-performance-budgets.md) met on the reference machine.
- Peak RSS ≤ 1.5 GiB on the same fixture.
- `cargo +nightly miri test` clean on `tfparser-core`.
- `crates.io` publishability dry-run passes (`cargo publish --dry-run -p tfparser-core`).
- README, CHANGELOG, and a "Getting Started" doc shipped under `docs/`.

## 3. Calendar shape

Single-developer estimates, calibrated against the research memos and the (assumed-realistic) overheads of code review, CI fixes, and on-call.

| Milestone | Engineer-weeks (single dev) | Calendar (with review) | Cumulative |
| --------- | --------------------------- | ---------------------- | ---------- |
| Phase 0 (risk retirement, [91-impl-plan.md § 3](./91-impl-plan.md)) | 0.5 | 1 wk | 1 wk |
| M0 | 2.5 | 4 wks | 5 wks |
| M1 | 1.5 | 2.5 wks | 7.5 wks |
| M2 | 1.5 | 2 wks | 9.5 wks |
| M3 | 2 | 3 wks | 12.5 wks |
| M4 | 1 | 1.5 wks | 14 wks |
| M5 | 1 | 1.5 wks | 15.5 wks |
| M6 | 1.5 | 2 wks | 17.5 wks |
| **v0.1 release** | **11.5 eng-weeks** | **~17 calendar weeks** | |

These are honest estimates; if reality says we're off by 2× after M0, the roadmap rebases — log the slip in [99-key-decisions.md](./99-key-decisions.md) with a `D-…-revised` entry rather than silently moving the dates.

## 4. Reading order (for stakeholders)

1. [00-prd.md](./00-prd.md) — what we're building and why.
2. [80-glossary.md](./80-glossary.md) — disambiguate terms.
3. [90-roadmap.md](./90-roadmap.md) (this file) — milestone shape.
4. (Optional) [10-data-model.md](./10-data-model.md) — to see the dataset shape you'll be querying.

## 5. Cross-references

- → Engineer view: [91-impl-plan.md](./91-impl-plan.md)
- ↔ All component specs feed milestones above.
- ↔ Decisions: [99-key-decisions.md](./99-key-decisions.md)
