# 93 — Deferred Improvements & Review Findings

Status: living document · Owner: tfparser-core

A single canonical home for review findings that did **not** fall in the
phase being landed. Append-only. Each entry: severity (P0/P1/P2/P3), source
(phase / review), `file:line` citation, one-line fix shape. Pick up in a
future phase.

---

## Phase 0 + Phase 1 review (2026-05-13)

### Spec defects — surfaced to the user

| ID | Severity | Where | Issue / fix |
| -- | -------- | ----- | ----------- |
| S-001 | P2 | `specs/10-data-model.md § 2.3` | `SymbolKind` shipped with three extra variants (`Iteration`, `Terraform`, `TerragruntDependency`) the spec does not list. Update spec to enumerate the full set and pin the canonical JSON discriminator strings. |
| S-002 | P2 | `specs/10-data-model.md § 2.1` | `Environment.aws_account_id` / `aws_region` ship as validated newtypes (`AccountId` / `Region`) — stronger than the spec's `Option<Arc<str>>`. Update spec to match the strict shape and cross-reference § 7's "newtype every domain primitive". |
| S-003 | P3 | `specs/10-data-model.md § 2.3` | `Expression::FuncCall { name, args }` (inline) was implemented as `FuncCall(Box<FuncCall>)` (struct, with a span). Update spec § 2.3 to show the struct form (matching how `Conditional` / `ForExpr` are documented). |

## Phase 2 review (2026-05-13)

### Spec defects — surfaced to the user

| ID | Severity | Where | Issue / fix |
| -- | -------- | ----- | ----------- |
| S-004 | P2 | `specs/10-data-model.md § 2.3` | The `Expression` enum lacks `Array(Vec<Expression>)` and `Object(Vec<(Expression, Expression)>)` variants, but the lowering table in `specs/12-hcl-loader.md § 3.2` ("tuple / object literals → … kept as expression nodes during loader") *requires* them — a fully-literal array can collapse to `Literal(List(...))`, but `["10.0.0.0/8", var.x]` cannot. Phase 2 added the variants (additive, behind `#[non_exhaustive]`); update spec § 2.3 to document them and pin their canonical JSON shape. |
| S-005 | P3 | `specs/12-hcl-loader.md § 2` | The spec shows `LoadContext` with a `line_indexer: &LineIndexer` field. The implementation uses a per-file `LineIndex` built lazily on demand from `SourceMap` (no separate `LineIndexer` type). Reconcile by either documenting the SourceMap-builds-LineIndex pattern (chosen here) or adding the `LineIndexer` type. |
| S-006 | P3 | `specs/12-hcl-loader.md § 2` | The spec's `RawBlock.body` is typed as `AttributeMap` (top-level only) with "nested blocks are nested AttributeMaps via `Value::Map`". The implementation lowers nested blocks under a synthetic key whose value is `Expression::Object(...)` (not necessarily a `Value::Map`, because nested attributes may carry unresolved expressions). Update spec to document the `Expression::Object` form and the `__labels__` synthetic key the implementation uses for labelled nested blocks. |

### P2 — implementation hygiene (deferred)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-007 | P2 | `crates/core/src/util/paths.rs:170-191` | `check_no_symlink_ancestors` covers the leaf symlink but has no test for an *intermediate* symlink in the chain. Add `test_should_reject_symlink_ancestor_with_reject_policy` (symlink dir under root, file beneath it). |
| P-008 | P2 | `crates/core/src/discovery/fs_walker.rs::aggregate_signals` | `aggregate_signals` re-`std::fs::read`s every HCL file the walker already visited; spec § 3.3 wants discovery one-pass. Cache bytes during the walk and feed the classifier directly, or move probing into `process_walk_entry`. |
| P-009 | P2 | `crates/core/src/discovery/classifier.rs::probe_set` | `RegexSet::new(...).unwrap_or_else(|_| RegexSet::empty())` silently degrades classification on a code-level regression. Add a `#[test]` asserting `probe_set().len() == 6` (or thread the `Result` to the public surface). |
| P-010 | P2 | `crates/core/src/discovery/fs_walker.rs::find_root_hcl` | Probes only `root.hcl` and `terraform/root.hcl`. Real Terragrunt repos sometimes name it `terragrunt.hcl` at the workspace root or use a different sub-path. Widen the candidate list once Phase 6 (Terragrunt resolver) lands and a real cascade is observable. |

### P3 — implementation hygiene (deferred)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-011 | P3 | `crates/core/src/loader/lowering.rs::attr_span` | Falls back to the synthetic `0..0` range when both `attr.span()` and `attr.value.span()` are `None`. Practically only reachable on hand-built ASTs (the parser always sets a span) but worth a `debug_assert!` to surface unintended use. |
| P-012 | P3 | `crates/core/src/discovery/options.rs::default_*_globset` | Same `unwrap_or_else(|_| GlobSet::empty())` pattern as P-009; add a test that asserts both default globsets are non-empty. |
| P-013 | P3 | `crates/core/src/discovery/fs_walker.rs::walk_workspace` | No per-collection cap on `BTreeMap`/`BTreeSet`; only `max_total_files` bounds the file vector. A workspace with millions of empty directories would balloon `seen_dirs`. Add a sibling `max_total_dirs` cap or surface `seen_dirs.len()` against `max_total_files`. |
| P-014 | P3 | `crates/core/src/loader/traits.rs::file_ext_supports_block_kind` | Tfvars + Json catch-all is `false`; future canonical block kinds in `.tfvars` would be silently flagged. Convert to an explicit allowlist once the Phase 4 evaluator pins which `.tfvars`-allowable kinds are real. |

### P3 — implementation hygiene (deferred)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| ~~F-001~~ | P3 | `crates/core/examples/spike_eval_context.rs:78-83` | **Moot** (spike deleted in Phase 2). NUL-rejection now lives in `crates/core/src/util/paths.rs::reject_nul`; the Phase 6 Terragrunt resolver will route through it. |
| ~~F-002~~ | P3 | `crates/core/examples/spike_eval_context.rs:105-109` | **Moot** (spike deleted in Phase 2). The Phase 6 resolver will use `paths::canonicalize_inside` + `paths::is_descendant`, which terminate on a component-wise root match rather than on `pop` short-circuit. |
| F-003 | P3 | `crates/core/src/diagnostic.rs:23-35` | Severity doc cites `50-cli.md § 4.3`; that section reference is unverified — drop the section number or change to "see 50-cli.md `--fail-on-diagnostics`". |
| F-004 | P3 | `crates/core/src/ir/mod.rs:1-7` | Module-level doc does not list which I-IR-* invariants are pinned in Phase 1 vs deferred to loader/exporter. Add an "Invariants pinned in Phase 1" list. |
| F-005 | P3 | `crates/core/src/ir/expression.rs::Conditional` | Missing `#[builder(field_defaults(setter(into)))]` for ergonomic `cond(expr)` instead of `cond(Box::new(expr))`. (Add when Conditional is constructed from outside the loader; cosmetic until then.) |

### Spike cleanup

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| ~~F-006~~ | P3 | `crates/core/examples/spike_*.rs` | **Closed in Phase 2** (spike scripts deleted; the lowering / line-index / sandboxed-file-funcs proven by the spikes are now part of the production loader and discovery code). |

### Test coverage (low-risk gaps)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| T-001 | P3 | `crates/core/tests/workspace_round_trip.rs` | Round-trip is structural-only; does not assert I-IR-1 (every span's path is under `workspace.root`). Phase 2 added `crates/core/tests/discovery_loader_pipeline.rs::test_iload2_lowered_body_contains_no_hcl_edit_types` covering the related I-LOAD-2 invariant; extend the round-trip test when Phase 5 wires the orchestrator end-to-end. |
| T-002 | P3 | `crates/core/src/ir/value.rs::Value::Number` | No test pins NaN inequality. Add `assert_ne!(Value::Number(f64::NAN), Value::Number(f64::NAN))` to make the `!Eq` rationale explicit. |

### Out-of-phase (correctly deferred to later phases)

- `secrecy::SecretBox<ProfileMap>` ([99-key-decisions.md] D11) — Phase 7.
- `Workspace.diagnostics` population at every phase — by definition each
  later phase appends.
- Discovery / Loader / Evaluator / Terragrunt traits — only `Pipeline` skeleton
  was in Phase 1 scope.
- Parquet exporter writer code — Phase 3 (Phase 0 spike already proved the
  column layout works end-to-end).

---

## Phase 3 review (2026-05-13)

### Spec defects — surfaced to the user

| ID | Severity | Where | Issue / fix |
| -- | -------- | ----- | ----------- |
| S-007 | P2 | `specs/10-data-model.md § 4` | Canonical-JSON example shows `{"__unresolved__": "var.environment", "__kind__": "Var"}` (insertion order). The frozen rule "Keys sorted alphabetically at every object level" requires `__kind__` to come first under ASCII byte order. Phase 3's renderer is alpha-correct; update the spec example to match (and pin via the `test_should_render_unresolved_keys_in_alpha_byte_order` test). |
| S-008 | P2 | `specs/10-data-model.md § 4`, `specs/20-parquet-exporter.md § 3.3` | Phase 3 emits five additional sentinels not enumerated in the spec — `__binary_op__`, `__unary_op__`, `__template_concat__`, `__conditional__`, `__for__` — for rich `Expression` nodes that are neither `Unresolved` nor `FuncCall`. Either document the full sentinel taxonomy with their inner schema or fold rich expressions back into `__unresolved__` with verbatim source. |
| S-009 | P3 | `specs/10-data-model.md § 3` | The schema documents the `kind` column enum (`resource \| data \| module \| output \| variable \| local \| provider`) but not which IR entity sources each row. Phase 3 emits one row per `Variable` / `Local` / `Output` / `ProviderBlock` (and `ModuleCall`) in addition to `Resource`. Add a "Row population" section listing the row sources and which columns are intentionally empty for each. |

### P1 — implementation hygiene (deferred)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-015 | P1 | `crates/core/src/exporter/writer.rs::EmittedRow` | Materialises ~14 owned `String`s per row, then keeps `Vec<EmittedRow>` alive for the whole component before draining. Spec 20 § 3.3 prescribes per-row `Vec<u8>` JSON pooling as the **only** per-row allocation in the hot path. Change `EmittedRow` to borrow (`Cow<'a, str>` / `&'a str`) or yield rows directly from an iterator over the IR. Defer until Phase 9 perf-budget run shows the regression. |
| P-016 | P1 | `crates/core/src/exporter/writer.rs::CompressionOpt::to_parquet` | `ZstdLevel::try_new(level).unwrap_or_default()` silently sanitises an out-of-range zstd level. CLAUDE.md § Input Validation says reject, don't sanitize. Change `CompressionOpt::Zstd` constructor (or `to_parquet`) to surface `ValidationError`. |
| P-017 | P1 | `crates/core/src/exporter/writer.rs::ExportOptions::parsed_at_ms` | Bare `Option<i64>` lets bad epochs (e.g. `i64::MIN`) reach the Arrow timestamp builder. Wrap in a `ParsedAt` newtype with a fallible constructor pinning a sane range (≥ 0 and ≤ some far-future ceiling). |

### P2 — implementation hygiene (deferred)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-018 | P2 | `crates/core/src/exporter/writer.rs::flush_batch` + `RowBuilders::batch` | Arrow's `*Builder::finish()` resets the builder and loses the capacity hint. After the first row-group flush, every subsequent batch re-grows from zero. Re-instantiate `RowBuilders` per row group sized to the remaining projected rows, or call `*_builder.reserve(remaining)` after `finish()`. |
| P-019 | P2 | `crates/core/src/exporter/writer.rs::variable_row` / `output_row` / `local_row` | Variable/output/local rows duplicate IR fields into the synthetic `attributes` AttributeMap that feeds `attributes_json`. Two sources of truth. Render `attributes_json` directly from the IR fields via a small helper to keep one canonical path. |
| P-020 | P2 | `crates/cli/src/main.rs::run_parse::command_line` | Verbatim `std::env::args().join(" ")` lands in `workspace.manifest.json` with no redaction. M0 has no secret-bearing flags but a future `--token` / `--aws-secret` would leak. Allowlist known flags and redact unknown `--*-token=` / `--*-secret=` values; cap length at 4 KiB. |
| P-021 | P2 | `crates/core/src/exporter/writer.rs` | No coverage of the "kill the writer mid-stream" failure path (spec 20 § 7). Add a controlled-fault test (e.g. inject a failing inner writer behind a tiny trait) that asserts `<out>/resources.parquet` does not exist while `<out>/resources.parquet.partial` does. |
| P-022 | P2 | `crates/core/src/exporter/writer.rs` | No test pinning the final row sort order `(component_path, module_path, address)`. The byte-determinism test implies it but does not pin it. Add `test_should_sort_rows_by_component_then_module_then_address` driving two synthetic components with deliberately out-of-order resources. |

### P3 — implementation hygiene (deferred)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-023 | P3 | `crates/core/src/exporter/writer.rs::span_relative_file` | Name promises "relative" but the body just calls `render_path`. The relativisation step (strip workspace-root prefix when an absolute path slips through) is not yet present. Rename or implement; revisit when terragrunt lands and Span.file can be absolute. |
| P-024 | P3 | root `Cargo.toml::[workspace.dependencies]::tokio` | Pinned but unused (D14: sync + rayon). Either remove or annotate "preserved for the future apps/server crate." |
| P-025 | P3 | `crates/core/src/exporter/manifest.rs::write_manifest` | Signature takes `&Path`; callers hold `Arc<Path>`. Switch to `&Arc<Path>` so the error paths can `Arc::clone` cheaply instead of re-`Arc::from`. |
| P-026 | P3 | `crates/core/src/exporter/writer.rs::EmittedRow` | 19 owned fields per row; six per-IR-kind helpers spell every field out. Derive `Default` and use struct-update syntax to halve the boilerplate (or — preferred — adopt P-015's borrowing rewrite which removes the struct entirely). |
| P-027 | P3 | All Phase 3 module-level docs | Use repo-relative `../../specs/*.md` links that break on docs.rs. Switch to absolute repo URLs (after publishing) or drop the link target. |

### Invalid finding (closed)

- **P0-001** (reviewer): "canonical JSON key order is not alpha-sorted for `FuncCall` / sentinel wrappers." Invalid — the reviewer's premise that `args` < `__unresolved_func__` is wrong: `_` (0x5F) precedes `a` (0x61) in ASCII byte order, so `__unresolved_func__` < `args` and the implementation is already alpha-correct. Byte-pinning tests added in Phase 3 (`test_should_render_*_keys_in_alpha_byte_order`) lock the order against future regressions. Spec 10 § 4's example also needs updating — see S-007.

---

## Phase 4 review (2026-05-13)

### Spec defects — surfaced to the user

| ID | Severity | Where | Issue / fix |
| -- | -------- | ----- | ----------- |
| S-010 | P1 | `specs/13-evaluator.md § 5` | The spec says "Already in `hcl-rs::eval` stdlib (we trust): format, formatlist, replace, regex, ...". `hcl-rs::eval` ships **no** built-in stdlib — every function must be `declare_func`'d into a `Context` manually. Phase 4 implements the subset that materially affects M1 (format, replace, lower, upper, trim, length, keys, values, merge, concat, lookup, contains, flatten, jsonencode/decode, base64encode/decode, tostring/tonumber/tobool/tolist/toset, sha256/sha512, formatdate, strcontains, get_env) and leaves the rest as `Expression::FuncCall` per § 5 closing rule. Update the spec to enumerate exactly which functions ship in v0.1 and to drop the "in hcl-rs::eval stdlib" wording. |
| S-011 | P1 | `specs/13-evaluator.md § 4` | The example builds an `hcl::eval::Context<'static>` and calls `hc.declare_func(name, f.def.clone())` where `f.def` is `hcl::eval::FuncDef`. `FuncDef` takes a [`fn`-pointer](https://docs.rs/hcl-rs/latest/hcl/eval/type.Func.html) (`Func = fn(FuncArgs) -> Result<Value, String>`), not `Fn`, so stateful functions (`file()`, `get_env()`, Terragrunt helpers in Phase 6) cannot capture their workspace-root / env-mode / sandbox context through it. Phase 4 walks our own IR; `value_to_hcl` / `hcl_to_value` remain for the `hcl::Value` boundary that future Terragrunt funcs will use. Update spec § 4 to describe the actual walker-on-our-IR contract and pin the adapter's role. |
| S-012 | P2 | `specs/13-evaluator.md § 5` (md5/sha1/bcrypt/uuid) | The spec lists `md5`, `sha1`, `bcrypt`, `uuid` as Terraform-only funcs to register in Phase 4. They are broken / non-deterministic / cryptographically dangerous per CLAUDE.md § Cryptography ("Never MD5/SHA-1/SHA-256/bcrypt for new code"). Phase 4 leaves them unimplemented (FuncCall stays unresolved). Pin in spec: "the parser MAY leave these as `__unresolved_func__` sentinels; resource attributes rarely call them directly". |
| S-013 | P3 | `specs/13-evaluator.md § 2` | The spec types `EvalContext.repo_vars` and `cascade_locals` as `Map`. Phase 4 uses `crate::ir::Map = Vec<(Arc<str>, Value)>` (per spec 10 § 2.3). Add a one-line cross-reference so future readers don't think the `Map` here is `HashMap`. |

### P2 — implementation hygiene (deferred)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-028 | P2 | `crates/core/src/eval/stdlib.rs::ReplaceFn` | Literal substring replace only. Terraform's `replace()` accepts `/regex/` shape in the `from` argument; Phase 4 silently ignores the regex form. Add detection-and-error or implement via `regex::RegexBuilder` with the size caps from 70-security § 3.4. |
| P-029 | P2 | `crates/core/src/eval/files.rs::render_template` | Plain-identifier interpolation only. `${trimspace(x)}`, `${var.a.b}`, `${cond ? a : b}` all surface `FuncError::Other`. Phase 9 hardening could swap in `hcl::Template::from_str(...).evaluate(...)` for richer template support. Real-world `templatefile()` calls usually pass identifier-only refs, but the gap is documented. |
| P-030 | P2 | `crates/core/src/eval/stdlib.rs::JsonencodeFn` | Object key order in the rendered JSON is *insertion order* of the source `Map`, not alphabetic. Spec 10 § 4 pins alphabetic ordering for `attributes_json`. Phase 3's renderer already alpha-sorts; the discrepancy here is benign (jsonencode results are typically wrapped in further attributes that the exporter alpha-sorts at the outer level), but a follow-up should align the two. |
| P-031 | P2 | `crates/core/src/eval/component.rs::HclEvaluator::evaluate` | Locals reduction runs until convergence in `solve_locals`, but the surrounding evaluator does not re-reduce providers/resources after locals settle. If a provider attribute references a `local.X` that depends on another `local.Y`, the order is currently correct, but a deeper chain (`local.A` → `local.B` → provider expression that references `local.A`) might hold a partial. Add a two-pass evaluation or thread the resolved locals into the scope before any provider reduction (currently done at line ~145; verify chain depth). |

### P3 — implementation hygiene (deferred)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-032 | P3 | `crates/core/src/eval/registry.rs::FuncRegistry::iter` | Public method exposes the underlying `HashMap`'s arbitrary iteration order. Add a `sorted_iter()` for diagnostic stability, or note in the rustdoc that the order is unspecified. |
| P-033 | P3 | `crates/core/src/eval/tf_funcs.rs::FormatdateFn` | The token matcher matches `MM`/`DD`/`hh`/`mm`/`ss` greedily, so a literal string like `"checksum"` (containing `ss`) would inject the seconds. Terraform's formatdate has the same edge case but uses `'literal'` quoting to escape. Phase 4 ships without quoting support — document in rustdoc and add a deferred fix to consume `'…'` escapes. |
| P-034 | P3 | `crates/core/src/eval/reduce.rs::reduce_for` | `for` comprehension reduction only fires when the *whole* collection has resolved. Partial reduction over a resolved-prefix of an unresolved tuple (rare but possible) stays unresolved. Acceptable for Phase 4; Phase 5 module expansion picks them up. |
| P-035 | P3 | `crates/core/src/eval/stdlib.rs::render_format` | `%d` accepts `Value::Number` via `float_to_i64_truncated` (clamping). Terraform's `format("%d", 1.5)` is actually an error. Phase 4's behaviour is "best-effort"; document so a future reader does not "fix" it without checking the trade-off. |
| P-036 | P3 | `crates/core/src/eval/component.rs::component_span_for_diag` | Cycle diagnostic uses the first file's path with a synthetic byte range. The cycle has many participants; consider attaching one diagnostic per participant or use the span of the first cyclic local. |
| P-037 | P3 | `crates/core/src/eval/locals.rs::tarjan_first_cycle` | Recursive Tarjan blow-up on a pathological `locals` graph (deeper than the default thread stack). Phase 4 caps `locals` at the loader's `max_blocks_per_file` indirectly, but a fixture with 10 000 deeply-chained locals could panic on stack overflow. Convert to an iterative form or pin a per-call recursion-depth cap. |
| P-038 | P3 | `crates/core/src/eval/files.rs::FilesetFn` | Sort uses `Vec::sort()` (lexicographic on `String`). Spec 10 § 4 references "ordered list" but does not pin the comparator. Verify against Terraform's actual ordering (filename byte order) and pin explicitly. |
| P-039 | P3 | `crates/core/src/eval/files.rs::FilesetFn` | Walk uses `ignore::WalkBuilder::standard_filters(false).hidden(false)` — but no symlink policy. A symlink inside `dir` would resolve via the underlying walker; consider routing through `paths::canonicalize_inside(SymlinkPolicy::Reject)` per [70-security.md P5]. |
| P-040 | P3 | `crates/core/src/eval/files.rs::TemplatefileFn` (template error) | When a template binding is missing, the call surfaces `FuncError::Other` with a free-form message. Define a dedicated `FuncError::TemplateRef` variant for downstream tooling to pivot on. |

### Test coverage (low-risk gaps)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| T-003 | P3 | `crates/core/src/eval/reduce.rs` | No `proptest` block for monotonicity / determinism. `tests/evaluator_pipeline.rs` covers the property at the *integration* layer; a true property-based test would generate random `(Expression, Scope)` pairs and assert `reduce(reduce(e, s), s) == reduce(e, s)` (idempotence) and "adding a binding never removes a resolved value". |
| T-004 | P3 | `crates/core/tests/evaluator_pipeline.rs` | No assertion on the **byte-stability** of `attributes_json` post-evaluator. The Phase 3 byte-deterministic test covers literal-only fixtures; a follow-up should re-run that test against a `var.region`-bound fixture. |
| T-005 | P3 | `crates/core/fuzz/fuzz_targets/evaluator.rs` | Harness uses `default_with_stdlib()` against the loaded `Component`. The corpus is sourced from `hcl_loader` outputs, which means many inputs hit early termination in the loader. Adding an `Arbitrary` impl for our `Expression` IR would give the harness more reach. |

### Out-of-phase (correctly deferred to later phases)

- Bench harness for `parse_large_monorepo` per [71-performance-budgets.md] — Phase 9.
- `Arc<str>` interner reuse inside the evaluator — Phase 9.
- Variable type-expression interpretation (the `type = map(string)` mini-language) — only impacts diagnostic precision, not row population. Phase 9.

---

## Phase 4 review — independent code review (2026-05-13)

### Fixed in-phase

| ID | Severity | Where | Fix |
| -- | -------- | ----- | --- |
| F-007 | P1 | `crates/core/src/eval/reduce.rs::reduce_for` | For-comprehension binders are lowered as `SymbolKind::Other`, not `SymbolKind::Var`. Added a separate `Scope.binders` namespace (kept distinct from `vars`/`locals`) and routed `SymbolKind::Other` lookup through it. Pinned by `reduce::tests::test_for_list_comprehension_resolves` (now uses the production `SymbolKind::Other` shape) + new `test_for_map_comprehension_resolves` + end-to-end `evaluator_pipeline::test_should_resolve_for_list_comprehension_from_real_hcl`. |
| F-012 | P3 | `crates/core/src/eval/stdlib.rs` | Renamed `TobooLFn` → `ToBoolFn`. |
| F-013 | P3 | `crates/core/src/eval/stdlib.rs` | Removed dead `cx_with_limits` test helper; replaced manual `CallCx { ... }` literal with `CallCx::new(...)`. |
| F-018 | P3 | `crates/core/src/eval/component.rs` | Added `#[tracing::instrument(skip(self, component, ctx), fields(component_id, component_path, n_repo_vars, n_cascade_locals))]` to `HclEvaluator::evaluate`. `repo_vars` / `cascade_locals` are not logged by value — only counts. |

### Deferred to a future phase

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| F-008 | P2 | `crates/core/src/eval/reduce.rs::render_value_as_str` | `TemplateConcat` with a List/Map literal renders `"[1, 2, 3]"` — Terraform errors on `"${var.list}"`. Constrain the collapse to scalar parts only (`Str | Bool | Int | Number | Null`); keep the `TemplateConcat` shape otherwise. |
| F-009 | P2 | `crates/core/src/eval/reduce.rs` (Object collapse) | The `Arc::from("")` / `Value::Null` fallback arms in the Object → `Value::Map` collapse are dead under the current guard but a footgun if the guard ever weakens. Replace with `unreachable!` annotated by the guard rationale, or refactor via `try_into`. |
| F-010 | P2 | `crates/core/src/eval/files.rs::FilesetFn` walk-error path | `ignore::WalkBuilder` errors surface via `FuncError::Other` with the raw OS error rendered through `Display` — may leak absolute paths of symlink targets outside the workspace root. Redact to the relative path the caller supplied. |
| F-011 | P2 | `crates/core/src/eval/component.rs::HclEvaluator::evaluate` | `Arc::new(component.clone())` clones the entire `Component` on every call. Spec 13 § 2 implied an `Arc<RawComponent>` share. Accept `Arc<Component>` (or return a shape that does not need the source). |
| F-014 | P3 | `crates/core/src/eval/component.rs::component_span_for_diag` | Diagnostics anchor at the first file's path with a synthetic byte range — overlaps P-036. Merge / dedupe. |
| F-015 | P3 | spec 13 § 5 (S-010 follow-up) | Append the explicit list of stdlib names still **not** registered by `default_with_stdlib`: `formatlist, regex, regexall, substr, coalesce, coalescelist, tomap, try, can, element, index, slice, zipmap, range, min, max, abs, ceil, floor, pow, signum, parseint, yamlencode, yamldecode, cidrhost, cidrnetmask, cidrsubnet, cidrsubnets`. Stays under the § 5 closing rule (FuncCall preserved). |
| F-016 | P3 | `crates/core/src/eval/reduce.rs::to_f64` | `to_f64(Value::Int(i64::MAX))` widens with silent precision loss (`f64` cannot represent `i64::MAX` exactly). Document the corner case or compare via `i128` intermediate. |
| F-017 | P3 | `crates/core/src/eval/reduce.rs::reduce_for` (list-iter key) | `i64::try_from(i).unwrap_or(i64::MAX)` for the for-loop key index has an unreachable fallback (`usize` can't exceed `i64::MAX` on 64-bit hosts). Replace with documented `as i64` or `expect("usize fits i64")`. |

---

## Phase 5 review — independent code review (2026-05-14)

### Fixed in-phase

| ID | Severity | Where | Fix |
| -- | -------- | ----- | --- |
| F-019 | P1 | `crates/core/src/graph/expand.rs::expand_module_call` | Provider-map cascade dropped through nested calls (`_parent_provider_map` discarded). Added `merge_provider_maps(parent, current)` helper that layers parent's mapping under current's overrides, and threaded the effective map through both `rewrite_resource` and the recursive `expand_module_call` site. Test pins the layering at `graph::expand::tests::test_merge_provider_maps_layers_parent_under_current`. |
| F-020 | P1 | `crates/core/src/graph/expand.rs::prefix_address` / `with_indexed_address` | Silent `unwrap_or_else(|_| addr.clone())` on `Address::new` failure produced bogus address collisions only surfaceable as `TF1506`. Changed both to return `Result<_, ValidationError>` and surface `TF1507` (drop the resource) instead. Test `test_prefix_address_overflow_emits_diagnostic_and_drops_resource` pins. |

### Invalid finding (closed)

- *Reviewer's P1 on cycle-stack push-after-resolve*: The existing
  `test_should_detect_module_self_cycle_and_emit_diagnostic` already
  exercises the one-level self-cycle case (caller → mod, mod has
  `module "self" { source = "." }`). When `mod` is pushed on the stack
  at the start of expanding the caller's call, the nested `self` call's
  resolver lookup sees `mod` on the stack and trips cycle detection on
  the **first** descent into self. There is no extra push-before-resolve
  fix required because top-level Module-kind components are skipped by
  the builder's kind=Component filter. Closing as invalid.

### Deferred to a future phase

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-041 | P2 | `crates/core/src/graph/registry.rs::ModuleRegistry::local_modules` | `HashMap<Arc<Path>, EvaluatedComponent>` is read-only after orchestrator build; if Phase 8 starts concurrent reads, swap for `DashMap` per CLAUDE.md § Async & Concurrency. |
| P-042 | P2 | `crates/core/src/graph/registry.rs::ExternalModuleRef` | No test exercises `record_external`. Pin the contract before Phase 8 consumes it for `modules.parquet`. |
| P-043 | P2 | `crates/core/src/graph/expand.rs::format_chain` | Always appends `"…"` even when the stack is empty. Render only the stack segments. |
| P-044 | P2 | `crates/core/src/graph/expand.rs::template_row` | No-op identity function. Inline at call sites or evolve to clear the address-index suffix when one was accidentally set. |
| P-045 | P2 | `crates/core/src/graph/expand.rs` (per-instance Resource clone) | `count = 100` clones every resource 100×. Hoist invariant substitutions (provider rewrite) out of the index loop; consider lazy attribute substitution. |
| P-046 | P3 | `crates/core/src/graph/expand.rs::instances_for_for_each` (list case) | Non-string list elements drop silently → zero instances, no diagnostic. Emit a `TF1505`-style diagnostic or keep the template row. |
| P-047 | P3 | `crates/core/src/graph/expand.rs::rewrite_resource` (depends_on) | `src.depends_on` is cloned verbatim, not prefixed with the module call chain. Phase 8 (edge inference) is the natural fix site. |
| P-048 | P3 | `crates/core/src/graph/builder.rs::build_workspace_modules` | `to_string_lossy()` silently mangles non-UTF-8 paths. Switch to a canonical-form helper or capture the original `source_raw` from a known call site. |
| P-049 | P3 | `crates/core/src/graph/builder.rs::build` | Module-kind components' diagnostics (cycles in module locals, file-sandbox rejects) are dropped — only kind=Component diagnostics are appended. Append `module_eval.diagnostics` to the workspace buffer too. |

### Spec defects — surfaced to the user

| ID | Severity | Where | Issue / fix |
| -- | -------- | ----- | ----------- |
| S-014 | P2 | `specs/15-resource-graph.md § 2` | Spec declares `GraphBuilder::build` taking `modules: ModuleRegistry` (owned) and `GraphContext` with three fields. Impl uses `&ModuleRegistry` (more efficient) and adds `max_expansion_per_resource: u32` (the cap from § 3.3). Update spec § 2 to take `&ModuleRegistry` and list `max_expansion_per_resource`. |
| S-015 | P3 | `specs/15-resource-graph.md § 3.3` | "one template row with `count_expr` set to the verbatim source" — the IR's `count_expr` is `Expression` (typed), not "verbatim source." Update spec wording. Cross-ref S-008. |
| S-016 | P3 | `specs/15-resource-graph.md § 6 I-GRAPH-3` | Idempotency invariant has no dedicated test. Commutativity proptest is adjacent but a different property. Add an idempotency test or downgrade the invariant. |
| S-017 | P2 | `specs/15-resource-graph.md § 3.2 #4` | "If the module body uses a `default` aws provider, it inherits the *call site's* default." Impl behaviour: `substitute_provider_ref` only rewrites when the call's `providers` map has a matching `local_name`; absent that, the body's `aws` default already points at the parent's provider block list, so the no-rewrite behaviour is correct. Document the implicit "default propagation = absence of rewrite" in the spec. |

---

## Phase 6 review — independent code review (2026-05-14)

### Fixed in-phase

| ID | Severity | Where | Fix |
| -- | -------- | ----- | --- |
| F-021 | P1 | `crates/core/src/terragrunt/resolver.rs::ReadTerragruntConfigFn` | Recursive `read_terragrunt_config` calls used a fresh `FuncRegistry::default_with_stdlib()` that did **not** include TG funcs, so transitive `find_in_parent_folders` / `get_repo_root` etc. inside the parent's locals stayed unresolved. Replaced with a late-bound `Arc<OnceLock<Arc<FuncRegistry>>>` that the resolver populates immediately after constructing the registry — the recursive read now sees the same TG function set. Regression: `test_recursive_read_sees_terragrunt_functions`. |
| F-022 | P1 | `crates/core/src/terragrunt/resolver.rs::backend_from_terraform_body` | Hardcoded `kind = "s3"` for every nested `backend` block because the original logic relied on a `"backend.kind"` key that the loader never produces — labels live inside the resulting `Expression::Object` under the synthetic `__labels__` key (cross-ref S-006). Rewrote to read `__labels__` and use the first label as the backend kind. |
| F-023 | P1 | `crates/core/src/terragrunt/resolver.rs::apply_cascade` | `map_to_locals` discarded the parent layer's non-literal locals every time the cascade moved on to a child layer, so a `root.hcl` declaring `merged_vars = merge(env_vars.locals, ...)` would silently vanish if any other layer followed it. Added `map_to_locals_with_inherited` that accumulates non-literal locals across layers (child overrides parent by name; merged-literal map wins on conflict). Regression: `test_parent_layer_non_literal_locals_survive_cascade`. |

### Deferred to a future phase

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-063 | P2 | `crates/core/src/terragrunt/resolver.rs::ReadTerragruntConfigFn::call` | `inflight` machinery never fires under normal flow (cycle stack catches re-entry first). Replace the inflight insert with `debug_assert!(!inflight.contains_key(...))` and remove the cleanup branches, or document why it's defence-in-depth. |
| P-064 | P2 | `crates/core/src/terragrunt/funcs.rs::TryFn` | `try(value, fallback)` is a value-level pass-through; cannot rescue an inner FuncCall that surfaced as `Unresolved`. Special-case `try` in `eval::reduce::reduce_expression`'s FuncCall arm so it inspects its first arg expression and falls through to the fallback on non-Literal. Cross-ref spec defect S-018. |
| P-065 | P2 | `crates/core/src/terragrunt/resolver.rs::parse_merge_strategy` | Silent default on unknown / non-literal `merge_strategy` values — violates CLAUDE.md "reject, don't sanitize." Emit `TG2010` diagnostic. |
| P-066 | P2 | `crates/core/src/terragrunt/funcs.rs::find_in_parents` | Uses `SymlinkPolicy::Follow`; per [70-security.md § 3.1 P2] tighten to `Reject` for `find_in_parent_folders` while keeping `Follow` only for user-supplied `read_terragrunt_config` paths. |
| P-067 | P2 | `crates/core/src/terragrunt/resolver.rs::backend_from_terraform_body` | `StateBackend::span` is always `Span::synthetic()`. Propagate the source span from the `terraform { ... }` block or the `generate "backend"` block. |
| P-068 | P2 | `crates/core/src/terragrunt/resolver.rs::evaluate_locals` | Magic-number 16-iteration cap. Lift to `EvalLimits::max_iterations` or a new `max_locals_passes`. |
| P-069 | P3 | `crates/core/src/terragrunt/resolver.rs::evaluate_inputs` | Drops non-literal-reduced inputs silently. Either keep partial shapes or emit a diagnostic per dropped input. |
| P-070 | P3 | `crates/core/src/terragrunt/resolver.rs` (module attr) | `#![allow(clippy::too_many_lines)]` is justified for `resolve()` but `resolve()` itself can split off `assemble_terragrunt_config(...)`. |
| P-071 | P3 | `crates/core/src/terragrunt/resolver.rs::build_dependencies` | `config_path` built via `component_dir.join(s)` without `canonicalize_inside`. Route through path sandbox; drop deps that escape. |
| P-072 | P3 | `crates/core/tests/terragrunt_cascade.rs::test_memo_avoids_double_parse_of_same_path` | Does not actually pin single-flight (only verifies the cached map is returned). Add a counter (custom resolver double or tracing subscriber) and assert `read_count == 1`. |
| P-073 | P3 | `crates/core/src/terragrunt/parsed.rs::project` (Inputs arm) | `BlockKind::Inputs` arm is dead (the loader classifies `inputs = { ... }` as Unknown attr). Remove or document. |
| P-074 | P3 | `crates/core/src/eval/reduce.rs::descend_attributes` | Tests cover the happy path (`local.a.b` on Map). Add explicit non-Map / List short-circuit tests. |
| P-075 | P3 | `crates/core/fuzz/fuzz_targets/terragrunt.rs` | Harness writes only one `terragrunt.hcl`; plant a `root.hcl` and `common.terragrunt.hcl` to widen reach into cycle/merge code paths. |
| P-076 | P3 | `fixtures/large-monorepo/` | Spec 91 § 9 Phase 6.8 says ~30 components; fixture has 7. Either reduce the spec target or grow the fixture. |

### Spec defects — surfaced to the user

| ID | Severity | Where | Issue / fix |
| -- | -------- | ----- | ----------- |
| S-018 | P2 | `specs/14-terragrunt.md § 3.3` | `try(value, fallback)` described at the value level. Terraform's actual `try` is an expression-level rescuer; our value-level implementation cannot recover from an inner `FuncCall` that surfaced as `Unresolved`. Document the limitation or pin a Phase-9 redesign. |
| S-019 | P2 | `specs/14-terragrunt.md § 3.5` | Spec assumes `terraform { backend "s3" {} }` lowers as a labelled-nested-block accessible by label. Actual loader lowering keeps labels inside `__labels__` (cross-ref S-006). Document explicitly. |
| S-020 | P3 | `specs/14-terragrunt.md § 5 I-TG-1` | Pin `SymlinkPolicy::Follow` (chosen here for `read_terragrunt_config`) explicitly with rationale; document that `Follow` upholds the "under workspace_root" invariant via the descendant-of-root check. |

### Invalid findings (closed)

- Reviewer's concern about `visited` set sharing across recursion levels: the `visited` HashSet at `resolver.rs:155-156` is correctly shared — passed as `&mut HashSet<PathBuf>` through every recursive call. Test `test_detects_include_cycle` exercises the case. Closed.
- Reviewer's concern about the `inflight` DashMap race: re-reading the code confirms memo-hit returns before `inflight.insert`, cycle check returns before the insert, and every fallible path after the insert removes the entry. No leak; tracked as P-063 for the broader cleanup question.
- Reviewer's concern about `RefCell` for `active_include`: `HclFunc` requires `Send + Sync`; `RefCell` is `!Sync`, so `Mutex` is the correct choice. Closing.

---

## Phase 7 review — independent code review (2026-05-14)

### Fixed in-phase

| ID | Severity | Where | Fix |
| -- | -------- | ----- | --- |
| F-024 | P1 | `crates/core/src/provider/resolver.rs::first_resolved_account` (reverse-lookup branches) | `HashMap`-iteration order produced non-deterministic `account_name` when multiple profiles shared an account-id. Added `lookup_name_by_account` that sorts by profile name and returns the lexicographically-smallest match. Restores **I-PROV-1** determinism. |
| F-025 | P1 | `crates/core/src/provider/profile_map.rs::resolve_aws_profile` (chain-hop cap) | `for _hop in 0..=8` allowed a non-cyclic 9-hop chain to silently exit without erroring. Rewrote as an explicit `loop { … }` with a `hops` counter that errors `ProviderError::ChainTooLong` the moment `hops >= AWS_CONFIG_MAX_CHAIN_HOPS` *and* the next `source_profile` pointer is present. Symmetric with the cycle-detection branch. |
| F-026 | P1 | `crates/core/src/provider/profile_map.rs::load_aws_config` | `String::from_utf8_lossy` silently swapped `U+FFFD` for invalid bytes — violates CLAUDE.md "reject, don't sanitize". Switched to `std::str::from_utf8` with an `Io { InvalidData }` error on failure. |

### Invalid finding (closed)

- Reviewer's *F2 [P1] "profile-miss doesn't fall through to cascade"*: re-reading
  `first_resolved_account` shows the profile branch *does* fall through —
  `unresolved.insert(profile)` runs and the function continues to the cascade
  block. The early return only fires on a successful lookup. Closing.
- Reviewer's *F11 [P0] "HashMap iteration order in load_aws_config"*: the
  resulting entries are keyed by profile name and the per-profile result is
  derived from the profile name itself, not its `HashMap` slot. Determinism
  holds. Closing.

### Deferred to a future phase

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-077 | P2 | `crates/core/src/provider/profile_map.rs::read_capped` | TOCTOU between `metadata().len()` and `File::open` + `read_to_end`. Stream-read with `Read::take(CAP + 1)` and recheck. |
| P-078 | P2 | `crates/core/src/provider/profile_map.rs::YamlBody` keys | Profile names (`HashMap` keys) have no length / charset cap; only the 256 KiB file cap bounds them. Add a per-key validator regex (e.g. `^[A-Za-z0-9._\-]{1,128}$`). |
| P-079 | P2 | `crates/core/src/provider/profile_map.rs::YamlEntry::role_arn` | Length-capped at 2048 but never charset-validated. Add a `^arn:aws:iam::\d{12}:role/[\w+=,.@\-/]+$` regex. |
| P-080 | P2 | `crates/core/src/provider/resolver.rs::resolve_component` (state-backend precedence) | `component.state_backend` and `terragrunt.state_backend` can both be `Some`; exporter only reads `component.state_backend`. Prefer `terragrunt.state_backend` when the former is `None`. |
| P-081 | P2 | spec 16 § 4 line 89 (`workspace.environments` lookup) | Resolver does not consult `workspace.environments` for region (or account) cascade. Either implement or update spec — see S-027 below. |
| P-082 | P2 | `crates/core/src/provider/resolver.rs::pick_provider` | Hard-coded `local_name == "aws"` preference for the default-provider lookup. Drop or comment as deliberate when multi-cloud lands. |
| P-083 | P2 | `crates/core/src/provider/resolver.rs::fill_state_backend` | Malformed `profile` / `role_arn` / `region` values are silently dropped. Surface a `Diagnostic` per malformed value. |
| P-084 | P2 | `crates/core/src/provider/resolver.rs::DefaultProviderResolver::resolve` | `RoleArnMalformed` diagnostic from spec § 6 is not emitted (no call site). Add it on `extract_account_id` miss when the source ARN was non-empty — see S-028. |
| P-085 | P3 | `crates/core/tests/provider_pipeline.rs` | The synthetic profile map has 3 entries; impl-plan § 10 says "5-profile fixture". Either add 2 entries (chained `source_profile`, mixed `sso_account_id`+`role_arn`) or relax the spec wording. |
| P-086 | P3 | `crates/core/tests/provider_pipeline.rs` (aws_config coverage) | No integration test drives the resolver via `load_aws_config`. Add one using a synthesised `~/.aws/config`-shaped file. |
| P-087 | P3 | `crates/core/tests/provider_pipeline.rs` (determinism on shared account-id) | Existing determinism test doesn't probe the reverse-lookup path (no two profiles share an account-id). Add a regression for F-024. |

### Spec defects — surfaced to the user

| ID | Severity | Where | Issue / fix |
| -- | -------- | ----- | ----------- |
| S-027 | P2 | `specs/16-provider-resolver.md § 4` line 89 | Lists `workspace.environments.find(component.env).map(|e| e.aws_region)` as a chain step but `Component` has no `env` field, and the impl never consults `workspace.environments`. Either remove the step or define how a `Component` is paired with an `Environment`. |
| S-028 | P2 | `specs/16-provider-resolver.md § 6` | `RoleArnMalformed { role_arn }` listed as a diagnostic but no code path in spec § 4 surfaces it. Implementation doesn't emit it either. Pin the call-site or strike from spec. |
| S-029 | P3 | `specs/16-provider-resolver.md § 2` | `ProviderContext.default_region: Option<Arc<str>>`. Implementation uses the stricter `Option<Region>` newtype. Update spec to match (analogous to S-024 cross-ref). |
| S-030 | P3 | `specs/16-provider-resolver.md § 4.1` | Example returns `Option<Arc<str>>` from `extract_account_id`; implementation returns `Option<AccountId>` (stronger). Update spec. |
| S-031 | P3 | `specs/16-provider-resolver.md § 6` | Diagnostic names (`MissingProfileMapping`, `ProviderAliasNotFound`) are not mapped to the stable codes the implementation emits (`TF1601`, `TF1602`). Add a code↔name table. |
| S-032 | P3 | `specs/16-provider-resolver.md § 9` | "read via `tilde` expansion + canonicalisation" — implementation accepts any `Path` verbatim and delegates expansion to the caller. Update spec to clarify caller-responsibility (CLI work; Phase 9). |

---

## Phase 8 review — independent code review (2026-05-14)

### Fixed in-phase

| ID | Severity | Where | Fix |
| -- | -------- | ----- | --- |
| F-027 | P0 | `crates/core/src/graph/edges.rs::component_address` | Triple-fallback path ended in `Address::new("component.x").unwrap()` — a reachable `unwrap` in non-test code, banned by CLAUDE.md. Rewrote `component_address` to return `Option<Address>`; `collect_terragrunt_edges` skips the component cleanly when the address rejects. |
| F-028 | P2 | `crates/core/src/exporter/secondary.rs` (dead-code shim) | Removed `_edge_kind_used` `#[allow(dead_code)]` shim and dropped the unused `EdgeKind` import — CLAUDE.md says delete dead code, not suppress it. |

### Deferred to a future phase

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-088 | P2 | `crates/core/src/exporter/secondary.rs::write_components_parquet` (`environments_seen`) | Unconditionally surfaces every `ws.environments` entry on every row. Spec 15 § 5 wants per-component env coverage. Either emit empty list until the cascade-narrowing pass lands (Phase 9) or filter by Terragrunt discovery. |
| P-089 | P2 | `crates/core/src/exporter/secondary.rs::components_schema` | Schema is missing `first_seen_at` / `last_seen_at` columns spec 15 § 5 lists. Add columns or update the spec (S-033). |
| P-090 | P2 | `crates/core/tests/dependency_graph_pipeline.rs` | No writer-level coverage for `EdgeKind::TerragruntDependency` and `EdgeKind::ModuleInput`. Unit tests in `edges.rs` cover collection only. Add oracle workspaces. |
| P-091 | P2 | `crates/core/tests/dependency_graph_pipeline.rs` (DuckDB) | Phase 8.8 says "DuckDB 3-table join integration test". Impl ships an in-Rust simulation. Defer to Phase 9 hardening with a real `duckdb` crate integration. |
| P-092 | P3 | `crates/core/src/ir/edge.rs::Edge` | Derived `PartialOrd, Ord` use enum-discriminant order; writer sorts by `kind.as_str()` (alphabetical). Today no caller uses the derived `Ord`, but the discrepancy is a footgun. Drop the derive or implement to mirror the string order. |
| P-093 | P3 | `crates/core/src/graph/edges.rs` (dedup key) | `BTreeSet<(String, String, EdgeKind)>` allocates two `String`s per edge. Swap for `(Arc<str>, Arc<str>, EdgeKind)` using `Address::as_str()` clones. |
| P-094 | P3 | `crates/core/src/exporter/secondary.rs::sorted_edges` | Re-sorts edges with the identical comparator the collector already used. Drop the second sort. |
| P-095 | P3 | `crates/core/src/exporter/secondary.rs::write_modules_parquet` counts loop | Idiomatic `*counts.entry(k).or_insert(0) += 1` instead of the current double `source_key` call + `entry` + `.get` + `.unwrap_or(0)` pattern. Cosmetic perf. |
| P-096 | P3 | `crates/cli/src/main.rs::run_verify` | Duplicates SHA-256 hex encoding from `exporter::manifest::sha256_hex_of_file`. Lift the helper to the public exporter module and reuse. |
| P-097 | P3 | `crates/cli/src/main.rs::run_verify` | Manifest read uses `std::fs::read` with no size cap. Defence-in-depth: cap at e.g. 4 MiB with `take`. |

### Spec defects — surfaced to the user

| ID | Severity | Where | Issue / fix |
| -- | -------- | ----- | ----------- |
| S-033 | P2 | `specs/15-resource-graph.md § 5` (Component summary) | Lists `first_seen_at` / `last_seen_at` columns with no source — a single parse run has no "previous" state to compare against. Either remove or define the derivation. |
| S-034 | P2 | `specs/50-cli.md § 2` | Enumerates `parse`, `inspect`, `schema`, `version` but not `verify`. Phase 8.7 added `tfparser verify`. Add a `§ 2.5` documenting `--manifest`, `--dir`, exit code, output shape. |
| S-035 | P2 | `specs/15-resource-graph.md § 4` (`TerragruntDependency` edges) | Spec mentions `dependency.x.outputs.y` as the trigger; implementation reads `Component.terragrunt.dependencies` block list, never the symbolic-ref form. Clarify which surface is canonical. |
| S-036 | P3 | `specs/10-data-model.md § 5.3` (`modules.parquet` columns) | Spec gives prose ("source form, resolution status, call count") but not column names/types. Pin the impl's columns: `module_id, source_raw, source_kind, canonical_path, call_count, resolved`. |

## Phase 9 review — hardening + DefaultPipeline (2026-05-14)

### Fixed in-phase

| ID | Severity | Where | Fix |
| -- | -------- | ----- | --- |
| P-016 | P1 | `crates/core/src/exporter/writer.rs::CompressionOpt::to_parquet` | Added `CompressionOpt::zstd(level) -> Result<Self, ValidationError>` (range-validated at the boundary). The CLI's `--zstd-level` flag now routes through it; the bare enum variant is kept for `Default`-shaped contexts and falls back to the codec default if it ever sees an out-of-range value. New `ValidationError::Range` variant covers the general numeric-range case. |
| P-020 | P2 | `crates/cli/src/main.rs::run_parse::command_line` | Implemented `redact_command_line` — flags whose name contains `token`/`secret`/`password` are rewritten to `<flag>=<redacted>` (inline form) or trigger `<redacted>` on the next argv slot (space-separated form). Total command line capped at 4 KiB with `floor_char_boundary` to avoid `String::truncate` panicking on multi-byte UTF-8. Pinned by `tests::test_should_redact_token_flag` + `test_should_redact_space_separated_secret_flag` + `test_should_truncate_at_utf8_boundary_without_panic` + `test_should_truncate_at_cap` + `test_should_detect_secret_flags` + `test_floor_char_boundary_clamps_at_string_end`. |
| F-029 | P1 | `crates/cli/src/main.rs::redact_command_line` (review-pass finding) | `out.truncate(CAP_BYTES)` panicked on multi-byte UTF-8 input (`tfparser parse "你..."` would reach via process argv). Walks back to nearest char boundary now. Reviewer-cited; **fixed in-phase**. |
| F-030 | P1 | `crates/cli/src/main.rs::redact_arg` (review-pass finding) | Space-separated form `--token sk-xxx` leaked the secret. Now `redact_arg` returns `(rendered, redact_next: bool)` and the caller replaces the next argv slot. Reviewer-cited; **fixed in-phase**. |
| F-031 | P1 | `crates/cli/src/main.rs::map_exit_code` (review-pass finding) | `GraphError` was routed to exit code `8`, which `specs/50-cli.md § 4.3` reserves for `--fail-on-diagnostics`. Routed to `4` (loader-class limit) instead. Reviewer-cited; **fixed in-phase**. |
| P-094 | P3 | `crates/core/src/exporter/secondary.rs::sorted_edges` | Retired the redundant `sorted_edges` helper; the writer borrows `ws.edges` directly with a `debug_assert!` invariant that pins the (from, to, kind) sort produced by `graph::edges::collect_edges_in_place`. |
| P-095 | P3 | `crates/core/src/exporter/secondary.rs::write_modules_parquet` counts loop | Rewrote the double `source_key` + `entry`/`get`/`unwrap_or(0)` pattern to idiomatic `let entry = counts.entry(...).or_insert(0); *entry = entry.saturating_add(1);`. |
| P-097 | P3 | `crates/cli/src/main.rs::run_verify` | Manifest read is now capped at 4 MiB via `Read::take(cap + 1)` (`read_capped` helper). Defence-in-depth — manifests are small in practice. |

### CLI parity with the library

| Concern | Resolution |
| ------- | ---------- |
| `tfparser parse` only emitted `resources.parquet` | New `DefaultPipeline` in `crates/core/src/pipeline.rs` wires every phase (discovery → loader → projection → terragrunt → evaluator → graph → provider). CLI delegates to it; default `--tables all` emits the three secondary tables alongside. |
| `pipeline.rs` doc comments claimed Phase 5 would land a `DefaultPipeline` — never delivered | Closed in this phase. `lib.rs` phase-status table updated. |
| `EvalContext` builders were spelled out in fuzz/test code via field-struct init that no longer compiles after `#[non_exhaustive]` | `crates/core/fuzz/fuzz_targets/evaluator.rs` migrated to `EvalContext::new(...)`. Other call sites already used the constructor. |

### Bench harness (task 9.1–9.2)

`crates/core/benches/pipeline.rs` ships five `criterion` benches —
`discovery_large_monorepo`, `loader_large_monorepo`,
`evaluator_large_monorepo`, `exporter_large_monorepo`,
`parse_large_monorepo`. `make bench-save-baseline` /
`make bench-vs-baseline` gate the 10 % regression budget. Initial
numbers on Apple M-series: discovery 2.1 ms / loader 3.0 ms /
evaluator 63 µs / exporter 30 ms / e2e 8 ms.

### Quality gates

- `cargo build` / `cargo test` (443 passed) / `cargo +nightly fmt --check` / `cargo clippy -D warnings` (including `--benches`) / `RUSTDOCFLAGS="-D warnings" cargo doc` / `cargo deny check`: all green.
- `cargo publish -p tfparser-core --dry-run`: ✅ succeeds.
- `cargo publish -p tfparser-cli --dry-run`: blocked by chicken/egg (requires `tfparser-core` to be on crates.io first); structurally resolved by publishing core first in a real release. The CLI's manifest now pins `tfparser-core` to a `version = "0.1.0"` so the registry resolution step has the input it needs.

### Deferred to a future phase

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| P-098 | P2 | `crates/core/src/exporter/writer.rs::EmittedRow` (revisit P-015) | The owned-`String` row materialisation still allocates ~14 strings per row. The bench shows the exporter as the e2e hotspot (~30 ms / 288 rows). Switch to a per-row `Cow<'a, str>` borrow or an iterator over the IR; rerun benches under `make bench-vs-baseline` to confirm. |
| P-099 | P2 | task 9.4 (Arc<str> interner) | Resource types and attribute names already store as `Arc<str>` per-node but are not workspace-deduped. Adding an interner threaded through the loader would reduce peak RSS on huge workspaces. Deferred — the current `large-monorepo` doesn't surface the regression on its 280 rows. |
| P-100 | P2 | task 9.5 (pooled Vec<u8> for attributes_json) | Spec 20 § 3.3 prescribes per-row JSON pooling as the only per-row allocation in the hot path. Pairs with P-098; bench the combined change. |
| P-101 | P3 | task 9.6 (6 h overnight fuzz) | Fuzz harnesses compile (`cargo +nightly fuzz` clean). The actual 6 h × 3-harness overnight pass needs to run outside this dev cycle. |
| P-102 | P2 | task 9.10 (CLI publish dry-run) | `tfparser-cli --dry-run` is blocked until `tfparser-core` is on crates.io. Document the publish order in the release runbook. |
| P-103 | P2 | `crates/core/tests/dependency_graph_pipeline.rs` (real DuckDB) | The in-Rust 3-table join simulation stays; spinning up a Rust `duckdb` crate dependency for the parquet integration test would bring a ~50 MB native dep into dev-dependencies. Defer; DuckDB compatibility is already smoke-tested manually against every fixture (see CHANGELOG). |
| P-104 | P2 | `crates/cli/src/main.rs::parse_kv` | `--var KEY=VALUE` has no length cap on key or value — violates CLAUDE.md § Input Validation ("length limits on every string"). Add an explicit byte cap (e.g. `KEY ≤ 64`, `VALUE ≤ 4 KiB`). Reviewer-cited. |
| P-105 | P2 | `crates/cli/src/main.rs::ParseArgs` | `--var` / `--allow-env` are `Vec<String>` with no element-count cap. Bound the collections (e.g. 256 entries each). Reviewer-cited. |
| P-106 | P2 | `crates/core/src/exporter/writer.rs::CompressionOpt::Zstd` variant | The bare enum variant `Zstd(level)` is publicly constructible and silently falls back to default level when out-of-range. Either make the field private (factory-only) or split `Zstd(ZstdLevelChecked)` newtype so the type system enforces the invariant. Reviewer-cited. |
| P-107 | P3 | `crates/core/src/pipeline.rs:457-466` (`test_default_pipeline_smoke_run_on_single_component_fixture`) | Silent `return` when fixture is missing masks regressions in CI where the fixture got moved/renamed. Replace with `panic!("fixture missing at …")` or rely on `CARGO_MANIFEST_DIR` + a known-stable path. Reviewer-cited. |
| P-108 | P3 | `crates/core/src/exporter/secondary.rs::write_dependencies_parquet` | Invariant on edge sort is `debug_assert!`-only. Release builds will silently emit unsorted parquet if a future graph-builder change breaks the order. Either keep a release-mode `assert!` or unit-test the post-condition. Reviewer-cited. |
| P-109 | P3 | `crates/cli/src/main.rs::run_parse` | `--allow-env` populates `EvalContext`'s `EnvVarMode::Strict.allowed` *and* the Terragrunt allowlist, but only when `--env-mode strict`. In `passthrough`/`mock` mode, the values are silently dropped for eval and kept for Terragrunt. Unify or document. Reviewer-cited. |

### Spec defects — surfaced to the user

| ID | Severity | Where | Issue / fix |
| -- | -------- | ----- | ----------- |
| S-037 | P2 | `specs/61-crates-and-features.md § 3.1` | Documents `Pipeline::run` returning a `Workspace` but does not list the `DefaultPipeline` entrypoint or its profile-map / region-default / strict knobs. Add a `§ 3.1.1` describing it. |
| S-038 | P2 | `specs/50-cli.md § 2` | The CLI now exposes `--environment`, `--region`, `--profile-map`, `--aws-config`, `--var`, `--allow-env`, `--env-mode`, `--strict-providers`, `--compression`, `--zstd-level`, `--tables`. Spec lists none of them. Expand `§ 2.1` (parse). |
| S-039 | P2 | `specs/91-impl-plan.md § 8` (Phase 5 exit) | Spec implied `DefaultPipeline` landed in Phase 5; it didn't (a oversight surfaced when wiring the CLI). Update the Phase 5 row to scope it to "graph builder only" and add a Phase 9.0 row covering DefaultPipeline. |
| S-040 | P3 | `specs/71-performance-budgets.md § 7` | Spec lists four micro-benches but does not pin a path / function name. Phase 9 ships them as `crates/core/benches/pipeline.rs`; update the spec to cite the file and the bench-id naming convention. |

## How to use this file

When a future phase starts, scan the table above for entries whose `file:line`
falls in the phase's scope, address them, and remove the entry. If new
deferred findings arise, append them under a new "Phase N+1 review" heading.

If an entry is downgraded or invalidated, strike it through and add a one-line
note in `99-key-decisions.md` referencing the reason — do not silently delete.
