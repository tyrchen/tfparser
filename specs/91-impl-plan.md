# 91 — Implementation Plan (Engineer-Facing, Dependency-Ordered)

Status: draft v1 · Owner: tfparser-core · Last updated: 2026-05-13

Organised by **build dependency**, not by user-visible feature. Reading top-to-bottom is the order an engineer should write code. The 1:1 milestone mapping back to [90-roadmap.md](./90-roadmap.md) is annotated.

## 0. Readiness assessment

| Item                     | Status       | Notes                                                                                                                 |
| ------------------------ | ------------ | --------------------------------------------------------------------------------------------------------------------- |
| Research memos           | ✅ done       | [docs/research/index.md](../docs/research/index.md) — five memos retired the load-bearing technical risks.            |
| Crate-version pins       | ✅ done       | [61-crates-and-features.md § 4](./61-crates-and-features.md).                                                         |
| Reference fixture        | ⏳ to-build   | `crates/core/tests/fixtures/large-monorepo/` — synthetic 30-component Terragrunt repo. Built incrementally per phase. |
| Profile map / AWS config | ✅ documented | Shape locked in [16-provider-resolver.md](./16-provider-resolver.md).                                                 |
| Open risks (R-OPEN-1..7) | accepted     | All listed in their source memos; each is non-blocking for v0.1.                                                      |

Nothing blocks Phase 1 today.

## 1. Why dependency order ≠ feature order

Three concrete examples (the cases that make this document necessary):

1. **Parquet schema lands before module expansion.** Modules don't affect the *columns* — they only change which rows appear. Locking the schema in Phase 4 (which closes M0) means M2's expansion just adds rows; no schema migration.
2. **Provider blocks parse before account resolution.** The IR's `ProviderBlock` is in Phase 1; the resolver that fills `account_id` is Phase 8. Without the type in place, every component spec referring to "the provider list" would be wishful thinking.
3. **Terragrunt lands after the evaluator.** Terragrunt builds on the evaluator's `Context` and on the loader's HCL types. Building it before either would require parking unused interfaces, which always rot.

## 2. Estimated total effort

11.5 engineer-weeks of focused work. Parallelism can compress phases 5 and 6, and 8 and 9, by ~1 week each — see [§ 12](#12-parallelism).

## 3. Phase 0 — risk retirement (≤ 1 wk)

| #   | Deliverable                                                                                                                                             | Lands in                                               | Effort |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------ | ------ |
| 0.1 | Research memos                                                                                                                                          | [docs/research/](../docs/research/)                    | done   |
| 0.2 | Spike: `hcl-edit` span lowering — write a 200-line proof that we can walk `Body`, lower to our `Expression`, and reconstruct `(line, col)` for any node | `crates/core/spikes/hcl_lowering.rs` (delete on close) | 1 d    |
| 0.3 | Spike: `parquet` schema declaration — emit a 10-row `resources.parquet`, read back with DuckDB, assert schema match                                     | `crates/core/spikes/parquet_round_trip.rs`             | 1 d    |
| 0.4 | Spike: `hcl-rs::eval::Context` with a custom `FuncDef` for `find_in_parent_folders`                                                                     | `crates/core/spikes/eval_context.rs`                   | 0.5 d  |
| 0.5 | Bench-fixture skeleton — minimum `large-monorepo` (3 components, no Terragrunt yet)                                                                     | `crates/core/tests/fixtures/large-monorepo/`           | 0.5 d  |

**Exit gate**: every spike compiles and runs in CI; specs unchanged or updated to reflect the finding. Spikes are deleted; the learnings live in the spec text.

## 4. Phase 1 — IR foundation (closes part of M0; week 1–2)

The spine. Every later phase imports from here. No production code that wires types together yet — just the types.

| #   | Task                                                                                                                                                                                                                    | Spec                                                                | Effort |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------- | ------ |
| 1.1 | `crates/core` skeleton, `#![forbid(unsafe_code)]`, lints from [61-crates-and-features.md § 4.2](./61-crates-and-features.md), `Error`/`Result` types via `thiserror`                                                    | [10-data-model.md § 7](./10-data-model.md#claudemd-anchoring)       | 0.5 d  |
| 1.2 | `ir::*` — `Workspace`, `Component`, `Module`, `Environment`, `Resource`, `ProviderBlock`, `ProviderRef`, `ModuleCall`, `Output`, `Variable`, `Local`, `Address`, `Span`, `AccountId`, `Region` newtypes with validation | [10-data-model.md § 2](./10-data-model.md#in-memory-ir)             | 2 d    |
| 1.3 | `Value`, `Expression`, `Symbolic`, `Map`, `AttributeMap`                                                                                                                                                                | [10-data-model.md § 2.3](./10-data-model.md#expressions-and-values) | 1 d    |
| 1.4 | `Diagnostic`, `Severity`, `LimitKind` enums                                                                                                                                                                             | [70-security.md § 3.2](./70-security.md)                            | 0.5 d  |
| 1.5 | `Pipeline::run(opts)` trait skeleton (no impl)                                                                                                                                                                          | [61-crates-and-features.md § 3.1](./61-crates-and-features.md)      | 0.5 d  |
| 1.6 | Unit tests for every newtype's validation (charset, length, allowlist)                                                                                                                                                  | [70-security.md § 4](./70-security.md)                              | 1 d    |

**Exit criteria**: `cargo test -p tfparser-core` green; `cargo clippy -D warnings -W clippy::pedantic` clean; `Workspace` and `Resource` round-trip through `serde_json` (smoke test).

## 5. Phase 2 — Discovery + HCL loader (closes most of M0; week 3–4)

Two components, one phase — they're tightly coupled: discovery emits `DiscoveredDir`, loader consumes it.

| #   | Task                                                                                                     | Spec                                         | Effort |
| --- | -------------------------------------------------------------------------------------------------------- | -------------------------------------------- | ------ |
| 2.1 | `DiscoveryOptions`, `Discoverer` trait                                                                   | [11-discovery.md § 2](./11-discovery.md)     | 0.5 d  |
| 2.2 | `FsDiscoverer` using `ignore::WalkBuilder` + classification heuristics + shallow `regex::RegexSet` probe | [11-discovery.md § 3](./11-discovery.md)     | 1.5 d  |
| 2.3 | Path-safety helpers (`canonicalize_inside`, NUL-reject, symlink-gate) — shared across phases             | [70-security.md § 3.1](./70-security.md)     | 0.5 d  |
| 2.4 | `LineIndex`, `SourceMap`                                                                                 | [12-hcl-loader.md § 6](./12-hcl-loader.md)   | 0.5 d  |
| 2.5 | `Loader` trait, `HclEditLoader` skeleton wrapping `hcl_edit::parser::parse_body`                         | [12-hcl-loader.md § 2](./12-hcl-loader.md)   | 0.5 d  |
| 2.6 | Expression lowering (`hcl_edit::expr::Expression` → our `Expression`) — the core fn                      | [12-hcl-loader.md § 3.2](./12-hcl-loader.md) | 2 d    |
| 2.7 | Block-kind lowering + label extraction                                                                   | [12-hcl-loader.md § 3.3](./12-hcl-loader.md) | 0.5 d  |
| 2.8 | Loader limits (file size, blocks/file, depth, template parts) enforced and surfaced as `Diagnostic`      | [12-hcl-loader.md § 3.5](./12-hcl-loader.md) | 0.5 d  |
| 2.9 | Fuzz harness `fuzz_hcl_loader`: `cargo +nightly fuzz run hcl_loader -- -max_total_time=600` clean        | [70-security.md § 6](./70-security.md)       | 0.5 d  |

**Exit criteria**:
- `Discovered { components: [single-component, multi-file] }` returns the expected structure for the M0 fixtures.
- Lowered `RawComponent`'s `body` contains no `hcl_edit` types (invariant I-LOAD-2).
- Fuzz harness green for 10 min in CI.

## 6. Phase 3 — Resource extraction + Parquet writer (closes M0; week 5)

The first slice that produces a user-visible artifact.

| #   | Task                                                                                                                                       | Spec                                                                        | Effort |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------- | ------ |
| 3.1 | `RawComponent → Resource[]` projection: pull `resource`/`data` blocks, extract `provider`, `count`, `for_each`, `depends_on`, `attributes` | [10-data-model.md § 2.2](./10-data-model.md#resources-providers-references) | 1 d    |
| 3.2 | Canonical JSON renderer for `AttributeMap`, with `__unresolved__` sentinel                                                                 | [20-parquet-exporter.md § 3.3](./20-parquet-exporter.md)                    | 1 d    |
| 3.3 | `ParquetExporter` + `resources.parquet` writer, schema from spec, pre-sized builders                                                       | [20-parquet-exporter.md § 3](./20-parquet-exporter.md)                      | 2 d    |
| 3.4 | `workspace.manifest.json` (versions, hashes, command line)                                                                                 | [20-parquet-exporter.md § 3.1](./20-parquet-exporter.md)                    | 0.5 d  |
| 3.5 | Atomic write (`.partial → rename`) + `--overwrite` flag wiring                                                                             | [20-parquet-exporter.md § 4](./20-parquet-exporter.md)                      | 0.5 d  |
| 3.6 | Schema golden test (`tests/golden/resources-schema.json`)                                                                                  | [72-testing-strategy.md § 4](./72-testing-strategy.md)                      | 0.5 d  |
| 3.7 | `crates/cli` skeleton, `clap` derive types, `tfparser parse --out` happy path                                                              | [50-cli.md § 2.1](./50-cli.md)                                              | 1 d    |
| 3.8 | CLI integration test: parse `single-component`, DuckDB cross-check                                                                         | [72-testing-strategy.md § 9](./72-testing-strategy.md)                      | 0.5 d  |

**Exit criteria (= M0)**:
- `tfparser parse crates/core/tests/fixtures/single-component -o /tmp/out` writes a Parquet file that DuckDB can read.
- Schema-drift test passes.
- All M0 columns present; `account_id` / `region` / `module_path` empty for every row.

## 7. Phase 4 — Evaluator (closes M1; week 6–7)

The first phase that materially affects existing rows: previously-empty `provider_local` / `region` / `environment` start populating.

| #    | Task                                                                                                                                                                            | Spec                                                                     | Effort |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ | ------ |
| 4.1  | `EvalContext` struct, `FuncRegistry`, `EnvVarMode` enum                                                                                                                         | [13-evaluator.md § 2](./13-evaluator.md)                                 | 0.5 d  |
| 4.2  | `value_to_hcl` / `hcl_to_value` adapters between our IR and `hcl-rs::eval` types                                                                                                | [13-evaluator.md § 4](./13-evaluator.md#the-hcl-rseval-context-we-build) | 1 d    |
| 4.3  | HCL stdlib funcs (lift from `hcl-rs::eval`'s built-in set; verify each is reachable)                                                                                            | [13-evaluator.md § 5](./13-evaluator.md#functions-registered)            | 0.5 d  |
| 4.4  | Terraform-only funcs not in `hcl-rs`: `formatdate`, `timestamp`, `sha256`, `md5`, `sha1`, `sha512`, `base64encode`, `base64decode`, `base64gzip`, `urlencode`, `bcrypt`, `uuid` | [13-evaluator.md § 5](./13-evaluator.md#functions-registered)            | 1 d    |
| 4.5  | Sandboxed file funcs: `file`, `fileexists`, `templatefile`, `fileset`                                                                                                           | [13-evaluator.md § 5](./13-evaluator.md#functions-registered)            | 1 d    |
| 4.6  | Locals fixpoint solver (worklist) + cycle detection                                                                                                                             | [13-evaluator.md § 3](./13-evaluator.md#evaluation-pipeline)             | 1.5 d  |
| 4.7  | Variable binding from tfvars + cascade locals injection                                                                                                                         | [13-evaluator.md § 3](./13-evaluator.md#evaluation-pipeline)             | 0.5 d  |
| 4.8  | Resource attribute reduction (preserving `Unresolved` subtrees)                                                                                                                 | [13-evaluator.md § 6](./13-evaluator.md#unresolved-propagation)          | 1 d    |
| 4.9  | Evaluator unit tests + property tests (monotonicity, determinism)                                                                                                               | [72-testing-strategy.md § 5](./72-testing-strategy.md)                   | 1 d    |
| 4.10 | Fuzz harness `fuzz_evaluator`                                                                                                                                                   | [70-security.md § 6](./70-security.md)                                   | 0.5 d  |

**Exit criteria (= M1)**:
- `multi-provider` fixture: provider blocks now have non-empty `region` post-evaluation when `region = var.region` and `var.region = "us-east-2"`.
- Cycle test green.
- Sandbox test (`file("../../etc/passwd")`) returns `Error::PathEscape`.

## 8. Phase 5 — Module expansion (closes M2; week 8–9)

| #   | Task                                                                            | Spec                                                   | Effort |
| --- | ------------------------------------------------------------------------------- | ------------------------------------------------------ | ------ |
| 5.1 | `ModuleRegistry` build (re-walk on demand)                                      | [15-resource-graph.md § 2](./15-resource-graph.md)     | 0.5 d  |
| 5.2 | `ModuleSource` classification (`Local`/`Registry`/`Git`/`External`)             | [15-resource-graph.md § 3.1](./15-resource-graph.md)   | 0.5 d  |
| 5.3 | Module body expansion + address rewriting (prefix `module.<name>`)              | [15-resource-graph.md § 3.2](./15-resource-graph.md)   | 1.5 d  |
| 5.4 | Input substitution: module's `var.*` ← `ModuleCall.inputs`                      | [15-resource-graph.md § 3.2](./15-resource-graph.md)   | 1 d    |
| 5.5 | Provider substitution: `providers = { aws = aws.main }` rewriting               | [15-resource-graph.md § 3.2](./15-resource-graph.md)   | 1 d    |
| 5.6 | `count`/`for_each` expansion with cap; one-template-row fallback for unresolved | [15-resource-graph.md § 3.3](./15-resource-graph.md)   | 1 d    |
| 5.7 | Cycle detection + max-depth cap                                                 | [15-resource-graph.md § 3.2](./15-resource-graph.md)   | 0.5 d  |
| 5.8 | Property test: rewrite-then-substitute = substitute-then-rewrite                | [72-testing-strategy.md § 5](./72-testing-strategy.md) | 0.5 d  |

**Exit criteria (= M2)**:
- Nested-module fixture's module bodies appear as Parquet rows under `module_path`.
- `count = 3` literal expands; `count = var.foo` (unresolved) emits one template row.
- Address uniqueness invariant holds (asserted in integration test).

## 9. Phase 6 — Terragrunt resolver (closes M3; week 10–11)

| #    | Task                                                                                                                                                                                  | Spec                                                   | Effort |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------ | ------ |
| 6.1  | `TerragruntResolver` trait + `FsTerragruntResolver` skeleton                                                                                                                          | [14-terragrunt.md § 2](./14-terragrunt.md)             | 0.5 d  |
| 6.2  | `find_in_parent_folders`, `find_in_parent_folders_from`, `path_relative_to_include`, `path_relative_from_include`, `get_terragrunt_dir`, `get_repo_root`, `get_parent_terragrunt_dir` | [14-terragrunt.md § 3.3](./14-terragrunt.md)           | 1.5 d  |
| 6.3  | `read_terragrunt_config` with `dashmap` memo                                                                                                                                          | [14-terragrunt.md § 3.3](./14-terragrunt.md)           | 1 d    |
| 6.4  | `include` block resolution + merge strategies (`deep_map_only` default)                                                                                                               | [14-terragrunt.md § 3.2](./14-terragrunt.md)           | 1.5 d  |
| 6.5  | `generate` block capture; sub-parse `contents` for backend extraction                                                                                                                 | [14-terragrunt.md § 3.5](./14-terragrunt.md)           | 1 d    |
| 6.6  | `dependency` block capture (component→component edge later in M5)                                                                                                                     | [14-terragrunt.md § 3.6](./14-terragrunt.md)           | 0.5 d  |
| 6.7  | Include cycle / depth-cap enforcement                                                                                                                                                 | [14-terragrunt.md § 5](./14-terragrunt.md)             | 0.5 d  |
| 6.8  | Grow `large-monorepo` to ~30 components with full cascade                                                                                                                             | `crates/core/tests/fixtures/large-monorepo/`           | 1.5 d  |
| 6.9  | Cascade integration test: assert `effective_locals` for `staging` / `production`                                                                                                      | [72-testing-strategy.md § 6](./72-testing-strategy.md) | 1 d    |
| 6.10 | Fuzz harness `fuzz_terragrunt`                                                                                                                                                        | [70-security.md § 6](./70-security.md)                 | 0.5 d  |

**Exit criteria (= M3)**:
- `large-monorepo` parses end-to-end without errors.
- Memoisation count assertion passes (≤ 1 parse per distinct included path).
- Cycle test rejects with the full path stack in the error.

## 10. Phase 7 — Provider / account / region resolver (closes M4; week 12)

| #   | Task                                                                                        | Spec                                                       | Effort |
| --- | ------------------------------------------------------------------------------------------- | ---------------------------------------------------------- | ------ |
| 7.1 | `ProfileMap`, `ProfileEntry`, `ArcSwap<ProfileMap>` wiring                                  | [16-provider-resolver.md § 2](./16-provider-resolver.md)   | 0.5 d  |
| 7.2 | `aws_config` loader using `rust-ini`, `source_profile` chain                                | [16-provider-resolver.md § 3.1](./16-provider-resolver.md) | 1 d    |
| 7.3 | YAML profile-map loader with `validator`                                                    | [16-provider-resolver.md § 3.2](./16-provider-resolver.md) | 0.5 d  |
| 7.4 | `extract_account_id(role_arn)` + `DefaultProviderResolver::resolve`                         | [16-provider-resolver.md § 4](./16-provider-resolver.md)   | 1 d    |
| 7.5 | State-backend extraction (`backend "s3"` profile / role_arn / region)                       | [16-provider-resolver.md § 4](./16-provider-resolver.md)   | 0.5 d  |
| 7.6 | `MissingProfileMapping` diagnostic deduplication                                            | [16-provider-resolver.md § 6](./16-provider-resolver.md)   | 0.5 d  |
| 7.7 | Integration test: synthesised profile map + `large-monorepo` → ≥ 95 % `account_id` coverage | [72-testing-strategy.md § 6](./72-testing-strategy.md)     | 1 d    |

**Exit criteria (= M4)**:
- Goal G4 met on the `large-monorepo` fixture.
- AWS-config loader passes a 5-profile fixture (mix of `sso_account_id`, `role_arn`, chained `source_profile`).

## 11. Phase 8 — Dependency graph & secondary tables (closes M5; week 13)

| #   | Task                                                                                       | Spec                                                                | Effort |
| --- | ------------------------------------------------------------------------------------------ | ------------------------------------------------------------------- | ------ |
| 8.1 | Edge collection — walk every `Expression::Unresolved` for resource/data/module refs        | [15-resource-graph.md § 4](./15-resource-graph.md)                  | 1 d    |
| 8.2 | Explicit `depends_on` → `EdgeKind::ExplicitDependsOn`                                      | [15-resource-graph.md § 4](./15-resource-graph.md)                  | 0.5 d  |
| 8.3 | Terragrunt `dependency` blocks → `EdgeKind::TerragruntDependency` (component-to-component) | [15-resource-graph.md § 4](./15-resource-graph.md)                  | 0.5 d  |
| 8.4 | `dependencies.parquet` schema + writer                                                     | [10-data-model.md § 5.1](./10-data-model.md#51-dependenciesparquet) | 0.5 d  |
| 8.5 | `components.parquet` summary writer                                                        | [15-resource-graph.md § 5](./15-resource-graph.md)                  | 0.5 d  |
| 8.6 | `modules.parquet` writer                                                                   | [10-data-model.md § 5.3](./10-data-model.md#53-modulesparquet)      | 0.5 d  |
| 8.7 | `tfparser verify` subcommand checks manifest hashes                                        | [50-cli.md § 2](./50-cli.md)                                        | 0.5 d  |
| 8.8 | DuckDB 3-table join integration test                                                       | [72-testing-strategy.md § 9](./72-testing-strategy.md)              | 0.5 d  |

**Exit criteria (= M5)**:
- Three secondary tables emit alongside `resources.parquet`.
- Edge counts match a hand-curated oracle on `large-monorepo`.

## 12. Phase 9 — Hardening (closes M6; week 14–15)

| #    | Task                                                                                                                 | Spec                                                         | Effort |
| ---- | -------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ | ------ |
| 9.1  | Bench harness `criterion` for the four micro-benches in [71-performance-budgets.md § 7](./71-performance-budgets.md) |                                                              | 1 d    |
| 9.2  | End-to-end bench `parse_large_monorepo`; CI gate at 10 % regression                                                  |                                                              | 0.5 d  |
| 9.3  | Profile run + flamegraph; address top 3 hotspots                                                                     |                                                              | 1.5 d  |
| 9.4  | `Arc<str>` interner for resource types & attribute names                                                             | [71-performance-budgets.md § 8](./71-performance-budgets.md) | 1 d    |
| 9.5  | Pooled `Vec<u8>` for `attributes_json` rendering                                                                     | [20-parquet-exporter.md § 3.3](./20-parquet-exporter.md)     | 0.5 d  |
| 9.6  | Long-running fuzz overnight (6 h × 3 harnesses) — fix anything found                                                 |                                                              | 1 d    |
| 9.7  | `cargo audit` / `cargo deny` clean; pin or replace any flagged dep                                                   |                                                              | 0.5 d  |
| 9.8  | Documentation pass (rustdoc on every public item)                                                                    | [CLAUDE.md § Documentation](../CLAUDE.md)                    | 1.5 d  |
| 9.9  | README, CHANGELOG, "Getting started" docs                                                                            |                                                              | 1 d    |
| 9.10 | `crates.io` dry-run for `tfparser-core` + `tfparser`                                                                 |                                                              | 0.5 d  |

**Exit criteria (= M6, v0.1 release-ready)**:
- All per-phase perf targets in [71-performance-budgets.md § 3](./71-performance-budgets.md) met on the reference machine.
- Peak RSS ≤ 1.5 GiB on `large-monorepo`.
- Fuzz harness clean for 6 h overnight.
- `cargo publish --dry-run -p tfparser-core` succeeds.
- `tfparser` package dry-run is expected to succeed after `tfparser-core`
  `0.1.0` is published to crates.io; before that, Cargo cannot resolve the
  CLI's registry dependency on `tfparser-core = "0.1.0"`.

## 13. Parallelism

Two pairs of phases can run in parallel with two engineers:

- **Phase 5 (module expansion) ‖ Phase 6 (Terragrunt)** — they touch different parts of the pipeline; the merge point is the evaluator's `cascade_locals`, which Phase 4 already settled. Compresses ~1 calendar week.
- **Phase 7 (provider resolver) ‖ Phase 8 (dependency graph)** — both consume the same `Workspace` and write into separate fields/tables. Compresses ~1 calendar week.

Phases 1–4 are strictly sequential (each builds on the previous in the IR sense).

## 14. What makes this order *correct*, not just plausible

1. **The schema (Phase 3) is locked before any optional content writer.** M2/M3/M4 add rows or fill columns; they never reshape the columns. Schema migration mid-roadmap is the most expensive kind of change in a Parquet-first product.
2. **The evaluator (Phase 4) lands before module expansion (Phase 5).** Module input substitution needs evaluator-resolved values; building expansion first would mean either redoing it or carrying around a half-evaluated tree. The reverse order has been tried in prior-art projects and consistently produced a rewrite of the expander after evaluator land.
3. **Terragrunt (Phase 6) lands after the evaluator (Phase 4).** Terragrunt's `read_terragrunt_config` and `merge` are *evaluator functions*. Without the evaluator, Terragrunt would have to ship its own expression engine — duplication and drift.
4. **Provider resolution (Phase 7) is the last fill phase.** It depends on every other field being populated. Doing it earlier means re-running it after every evaluator improvement.

## 15. Hand-off after each milestone

- M0 close → tag `v0.0.1`, README updates, post in #platform.
- M3 close → tag `v0.0.4`, demo to the security team (they're the first big consumer per [00-prd.md § Users](./00-prd.md)).
- M6 close → tag `v0.1.0`, publish `tfparser-core` to crates.io, write a blog post.

## 16. Cross-references

- ↔ Roadmap: [90-roadmap.md](./90-roadmap.md)
- ↔ Decisions: [99-key-decisions.md](./99-key-decisions.md)
- ↔ Every component spec is cited per phase row.
