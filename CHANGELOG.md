# Changelog

All notable changes to this project will be documented in this file. See [conventional commits](https://www.conventionalcommits.org/) for commit guidelines.

---
## [tfparser-v0.1.0] - 2026-05-20

### Miscellaneous Chores

- init the project - ([561f7f1](https://github.com/commit/561f7f1f5a9780dc97051b1a3b95dd4f5b668b01)) - Tyr Chen
- add specs - ([91910cc](https://github.com/commit/91910cc84c3e49c9a6aff80dc47801698e2b62ac)) - Tyr Chen
- pre for publish - ([245cd39](https://github.com/commit/245cd39825771161217b176f5f253de122290664)) - Tyr Chen

### Other

- phase 0/1: risk-retirement spikes + IR foundation

Phase 0 lands the three risk-retirement spikes under `crates/core/examples/`:

- `spike_hcl_lowering` walks `hcl-edit::Body`, reconstructs `(line, col)` from
  byte spans via a sorted-prefix `LineIndex`, validates against the
  `services/order-service/main.tf` fixture under `fixtures/large-monorepo/`.
- `spike_parquet_round_trip` declares the canonical `resources.parquet` Arrow
  schema (24 columns, frozen per spec 10 § 3), writes 10 rows with zstd-3,
  reads back via `ParquetRecordBatchReaderBuilder`, and asserts cell-level
  fidelity.
- `spike_eval_context` registers a sandboxed `find_in_parent_folders`
  `FuncDef` on an `hcl::eval::Context`. Workspace state is threaded via a
  per-thread `WorkspaceCtx` because `Func` is a bare `fn` pointer with no
  closure capture — the production Terragrunt resolver (Phase 6) will use
  the same pattern.

Phase 1 lands the IR foundation under `crates/core/src/`:

- `lib.rs` — `#![forbid(unsafe_code)]`, workspace-wide lints (deny
  unwrap/expect/panic/indexing/print), Send+Sync static assertions.
- `error.rs` — `Error`, `Result`, `ValidationError` with `#[source]` chains
  per CLAUDE.md § Error Handling.
- `diagnostic.rs` — `Diagnostic` (builder), `Severity`, `LimitKind` covering
  every limit pinned in spec 70 § 3.2.
- `pipeline.rs` — `Pipeline` trait skeleton, `PipelineOptions` (builder),
  `EnvVarMode { Passthrough, Strict { allowed: BTreeSet<Arc<str>> }, Mock }`.
- `ir/` — validated newtypes (`Address`, `AccountId`, `Region`), `Span`
  (with `byte_range: Range<u32>`), `ComponentId`/`ModuleId` (NonZeroU32),
  `Value`, `Expression` (with `Symbolic`, `FuncCall`, `Conditional`, `ForExpr`),
  `Resource`, `ProviderBlock`/`ProviderRef`/`AssumeRole`, `Module`/`ModuleCall`,
  `Component`, `Environment`, `TerragruntConfig`/`IncludePath`/`GenerateBlock`/
  `DependencyBlock`/`StateBackend`, `Workspace`.

Every public struct with >5 fields is constructed via `typed-builder`; every
public type is `#[non_exhaustive]`; every type-with-secrets (`ProviderBlock`,
sensitive `Variable`/`Output`) has a hand-rolled `Debug` impl that redacts
its secret-shaped fields, asserted by tests.

Quality gates (all green):

- `cargo build --workspace --all-targets`
- `cargo test --workspace --all-targets` — 100 tests pass (89 lib + 3
  integration + 3+3+2 example smoke tests)
- `cargo +nightly fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings` (with workspace
  pedantic + deny unwrap/expect/panic/indexing/print)
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
- `cargo deny check` (advisories ok, bans ok, licenses ok, sources ok;
  RUSTSEC-2024-0436 ignored with reason — paste@1 is an unmaintained
  transitive dep via parquet@58 with no upstream replacement).

Independent code review found 3 P1, 7 P2, 8 P3 findings. P0/P1/P2
in-phase items fixed in this commit; P3 hygiene + spec-text drift deferred
to `specs/93-improvements-review.md`.

Workspace housekeeping (per the user's request to remove placeholders):

- Removed `apps/server` (Phase 5+ placeholder; the workspace member spec
  has been simplified to `crates/*`).
- Removed `crates/core/fixtures/README.md` and `examples/README.md`
  templates — those dirs now hold real spike examples + tests.
- Removed top-level `clippy.toml` whose `disallowed-types` policy forced
  `tokio::fs::*` everywhere — contradicts D14 (sync + rayon, no tokio in
  core). The workspace's per-crate `[lints]` policy in `Cargo.toml` is now
  the spec-aligned source of truth.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([9226f21](https://github.com/commit/9226f219f780c127eac811d4a5e499bb4bef46d5)) - Tyr Chen
- phase 2: discovery + HCL loader (closes most of M0)

Lands the second slice of the pipeline per specs/91-impl-plan.md § 5.

* discovery: Discoverer trait + FsDiscoverer using ignore::WalkBuilder,
  classifier with regex::RegexSet shallow probe, deterministic
  byte-lex ordering, exclude/module globs, file/total-files caps with
  diagnostics, ambiguity reporting, environments/root.hcl detection
* loader: Loader trait + HclEditLoader, hcl_edit::expr → IR lowering
  (Literal/Unresolved/Array/Object/TemplateConcat/FuncCall/UnaryOp/
  BinaryOp/Conditional/For), block-kind classification, per-file
  caps (file size, blocks, attribute depth, template parts) surfaced as
  diagnostics, SourceMap + LineIndex
* IR: Expression::Array / Expression::Object additive variants required
  by the lowering table — recorded as spec defect S-004 in
  specs/93-improvements-review.md
* util::paths: shared NUL-reject + canonicalize_inside helper, used by
  discovery + loader (spec 70 § 3.1 P1-P5)
* fuzz harness: crates/core/fuzz/fuzz_targets/hcl_loader.rs feeds
  arbitrary bytes to HclEditLoader::parse_bytes; runnable via
  `cargo +nightly fuzz run hcl_loader -- -max_total_time=600`
* fixtures: single-component, multi-provider, and exit-criteria
  integration test (crates/core/tests/discovery_loader_pipeline.rs)
* Phase 0 spike examples removed (their learnings now live in the
  production code per F-006 in specs/93)
* Makefile gains `make ci` and `make fuzz-hcl-loader`

Exit criteria — all green:
- cargo test --workspace --all-targets: 174 passed
- cargo +nightly fmt --check: clean
- cargo clippy --workspace --all-targets -D warnings: clean (workspace
  pedantic + deny on unwrap/expect/indexing/panic/print)
- RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps: clean
- cargo deny check: advisories ok, bans ok, licenses ok, sources ok
- Discovered { components: [single-component, multi-provider] } shape
  asserted by integration tests
- I-LOAD-2 (no hcl_edit types in lowered body) asserted structurally

Spec defects surfaced: S-004 (Expression::Array/Object), S-005 (LineIndex
shape), S-006 (RawBlock.body for nested labelled blocks) — see
specs/93-improvements-review.md.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([c522c9c](https://github.com/commit/c522c9c3960394334fa1c9909e493fc0ec989c74)) - Tyr Chen
- phase 2 review: structured LimitKind on Diagnostic + dead-code cleanup

Independent review (general-purpose agent) flagged 3 P1 items against
specs/11, 12, 70 and CLAUDE.md. All addressed in this commit:

* P1.1 — Diagnostic gained an optional `limit_kind: Option<LimitKind>`
  field plus a `Diagnostic::limit(kind, code, msg)` constructor; every
  loader / discovery cap-breach diagnostic now carries the structured
  kind so consumers don't have to parse the message
  (crates/core/src/diagnostic.rs, crates/core/src/loader/lowering.rs,
  crates/core/src/loader/traits.rs, crates/core/src/discovery/fs_walker.rs)
* P1.2 — Strengthened the I-LOAD-2 invariant test with a compile-time
  type assertion that `RawBlock.body == AttributeMap`; the JSON sweep
  is kept as belt-and-braces
  (crates/core/tests/discovery_loader_pipeline.rs)
* P1.3 — Removed the dead `LoaderError` enum (loader uses Diagnostic;
  the enum was exported but never constructed). Re-exports cleaned up
  in crates/core/src/loader/mod.rs

P2 / P3 findings (5 items) appended to specs/93-improvements-review.md
under the new "Phase 2 review" heading: aggregate_signals re-reads
files, RegexSet/GlobSet silent-empty-on-error, intermediate-symlink
test gap, find_root_hcl candidate widening, walk_workspace dir-count
cap, file_ext_supports_block_kind allowlist tightening.

All quality gates green (175 tests; clippy --workspace --all-targets
-D warnings; nightly fmt; doc; deny check).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([ac5f32f](https://github.com/commit/ac5f32ff0a443ddc5e156b1c92ed95c32c79c849)) - Tyr Chen
- phase 3: resource extraction + Parquet exporter + CLI (closes M0)

- Adds `crates/core/src/projection/` — `RawComponent` → typed IR
  (`Resource`, `ProviderBlock`, `ModuleCall`, `Variable`, `Local`,
  `Output`, `Component`). Pulls `count`/`for_each`/`provider`/
  `depends_on` out as structural fields; rest survives in `attributes`.
  Per spec 10 § 2.2.
- Adds `crates/core/src/exporter/`:
  - `schema.rs` — canonical 24-column Arrow schema, frozen at v0.1
    (spec 10 § 3). `schema_major=0`, `schema_minor=1` embedded in
    Parquet key-value metadata.
  - `json.rs` — canonical-JSON renderer for `attributes_json`
    (alpha-sorted keys, `__unresolved__`/`__unresolved_func__`
    sentinels, ryu for floats, NaN→null). Spec 10 § 4 + 20 § 3.3.
  - `writer.rs` — `ParquetExporter` with pre-sized arrow builders,
    `.partial → rename` atomic write, zstd-3 default, byte-deterministic
    output under `--parsed-at`. Spec 20 § 3 + 99 D10.
  - `manifest.rs` — `workspace.manifest.json` with SHA-256 of every
    output file, schema version, command line. Spec 20 § 3.1.
- Adds `crates/cli/` — `tfparser parse <root> --out <DIR>`,
  `tfparser schema`, `tfparser version`. Synchronous wiring of
  discovery → loader → projection → exporter. Clap derive; tracing
  to stderr; exit codes per spec 50 § 4.3.
- Adds `tests/parquet_schema_golden.rs` + `tests/golden/resources-schema.json`
  — schema-drift gate (spec 72 § 4).
- Adds `tests/parquet_export_pipeline.rs` — end-to-end discovery →
  exporter against the M0 fixtures, exit-criteria assertions
  (M0 columns present; `module_path` / `account_id` / `region`
  empty for every row; arrow round-trip).
- Adds `crates/cli/tests/cli_parse.rs` — `assert_cmd`-driven CLI
  integration tests covering parse / schema / version / overwrite /
  missing root / row count.

Exit criteria (spec 91 § 6):
- `tfparser parse fixtures/single-component -o /tmp/out` writes a
  Parquet file readable via `parquet::arrow::ParquetRecordBatchReader`
  (verified in `cli_parse.rs::test_should_write_resources_parquet_with_expected_row_count`).
- Schema-drift test passes (`parquet_schema_golden`).
- All M0 columns present; `account_id` / `region` / `module_path`
  empty for every row (verified in
  `parquet_export_pipeline::test_should_emit_expected_columns_and_row_kinds`).

Gates: cargo build/test/clippy/fmt/doc/deny all clean. 229 tests
(203 lib + 26 integration), 0 failures.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([46c4dc6](https://github.com/commit/46c4dc65da0f40121bba0cb3ac47466561c37932)) - Tyr Chen
- phase 3 review: path normalisation, exit code 3, tightened provider_ref allowlist

In-phase fixes from the independent review against specs 10/20/50/70/72/93:

- **P0-002** — `render_path` normalises `component_path` and `file`
  columns to relative, `/`-separated form so Windows hosts don't leak
  `\` into the Parquet artefact (spec 10 § 3 columns #2, #20). Added
  `test_render_path_normalises_separators`.
- **P0-003** — CLI routes `canonicalize` failures through
  `tfparser_core::Error::Io` so `map_exit_code` returns the documented
  exit code 3 ("Discovery error / root missing", spec 50 § 4.3). Test
  now asserts `.code(3)` instead of generic non-zero.
- **P1-004** — `ExportError::Manifest` carries the offending path,
  consistent with every other variant (spec 20 § 5).
- **P1-005** — `Manifest` / `ManifestFile` use `#[serde(deny_unknown_fields)]`
  so a tampered or future-mismatched manifest is rejected at parse
  time (CLAUDE.md § Serialization).
- **P1-006** — End-to-end test pinning the diagnostic-propagation
  path: a malformed `resource` block in a fixture surfaces TF1301 in
  `Workspace.diagnostics`. Catches refactors that drop the
  `out_diagnostics` push.
- **P2-001** — Cap pre-allocation at `MAX_PREALLOC_ROWS = 1M` to
  prevent pathological workspaces from forcing gigabytes of up-front
  allocation (CLAUDE.md § Safety & Security — bound every collection).
- **P2-002** — Byte-pinning tests on the canonical-JSON renderer
  (`test_should_render_*_keys_in_alpha_byte_order`) lock the sentinel
  ordering against future regressions. The renderer is already
  alpha-correct under ASCII byte order; the reviewer's P0-001 premise
  was incorrect (`_` (0x5F) precedes `a` (0x61), so `__unresolved_func__`
  < `args`).
- **P2-006** — `extract_provider_ref` switched to a positive allowlist:
  only `SymbolKind::Other`/`Resource` plus the regex
  `^[a-z_][a-z0-9_]*(\.[a-z_][a-z0-9_]*)?$`. Rejects `path.module`,
  `terraform.workspace`, `each.value`, `dependency.foo`, and
  multi-dot resource references.
- **P2-008** — CLI exit-code test for missing root upgraded from
  generic-failure to `.code(3)`.
- **P2-011** — `assert_cmd` / `predicates` moved to
  `[workspace.dependencies]`.

Spec defects surfaced for the user (specs/93):
- S-007: spec 10 § 4 canonical-JSON example uses insertion order; the
  rule itself says alphabetical. Update the example.
- S-008: Phase 3 emits five additional canonical-JSON sentinels
  (`__binary_op__`, `__unary_op__`, `__template_concat__`,
  `__conditional__`, `__for__`) that the spec doesn't enumerate.
  Either document the taxonomy or fold rich expressions back.
- S-009: spec 10 § 3 doesn't say which IR entity sources each row
  kind. Add a "Row population" section.

Deferred to specs/93:
- P-015 (per-row String materialisation),
- P-016 (zstd-level validation),
- P-017 (parsed_at_ms range),
- P-018..P-022 (P2 hygiene),
- P-023..P-027 (P3 cosmetic).

Gates: cargo build/test/clippy/fmt/doc/deny clean. 237 tests
(210 lib + 27 integration), 0 failures.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([7f79ee4](https://github.com/commit/7f79ee4ada019cfe9ff7f8fd46de2bd1609b02b6)) - Tyr Chen
- phase 4: evaluator + sandboxed file funcs (closes M1)

Implements the Phase 4 evaluator per specs/13-evaluator.md: an EvalContext
threading workspace_root, env-var mode, repo_vars, cascade_locals, a
FuncRegistry, and per-call limits through HclEvaluator. The walker
(`eval/reduce.rs`) reduces our IR Expression tree directly — variable +
local + terraform.workspace binding, arithmetic / template / array /
object / func / conditional / for-comprehension folding — preserving
unresolved subtrees so apply-time refs (data.*, resource attrs, module
outputs) survive intact. Locals fixpoint runs Tarjan-style cycle
detection up front and a worklist solver afterwards; cycles surface as
TF1401 Error diagnostics rather than aborts.

Function set shipped: HCL stdlib (format, lower/upper/trim/replace,
length/keys/values, merge/concat/compact/lookup/contains/flatten,
tostring/tonumber/tobool/tolist/toset, jsonencode/jsondecode,
base64encode/decode); Terraform-only (sha256/sha512, formatdate,
strcontains, get_env honouring EnvVarMode); sandboxed file funcs (file,
fileexists, templatefile, fileset) all routed through
util::paths::canonicalize_inside with SymlinkPolicy::Reject and the
per-call file-size cap. md5/sha1/bcrypt/uuid/timestamp are deliberately
not implemented (broken per CLAUDE.md § Cryptography or
non-deterministic); their calls survive as Expression::FuncCall per
spec 13 § 5 closing rule.

M1 exit criteria:
- multi-provider-shape fixture: region = var.region binds when
  ctx.repo_vars carries `region = "us-east-2"`
  (evaluator_pipeline::test_should_resolve_multi_provider_region_from_var,
  test_should_resolve_region_via_variable_default_when_no_repo_var).
- Cycle test green (test_should_emit_cycle_diagnostic_on_self_cycle /
  ..._two_node_cycle).
- file("../../etc/passwd") returns PathEscape
  (test_should_reject_path_escape_via_file_function).
- Quality gates: cargo build, cargo test (330 tests / 0 failures),
  cargo +nightly fmt --check, cargo clippy -D warnings, RUSTDOCFLAGS=-D
  warnings cargo doc, cargo deny check all clean.

Spec defects recorded in specs/93-improvements-review.md (S-010
hcl-rs::eval ships no stdlib; S-011 FuncDef takes fn-pointer not Fn so
stateful funcs cannot route through hcl-rs::eval; S-012 md5/sha1/bcrypt
deferred; S-013 Map type cross-ref). Adapter `eval::adapter::value_to_hcl`
/ `hcl_to_value` kept for the Phase 6 Terragrunt boundary.

Next: Phase 5 (module expansion, closes M2).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([29daecc](https://github.com/commit/29daeccc8114580b55e4a7c8a484241d05b33f99)) - Tyr Chen
- phase 4 review: for-binder namespace + cosmetic fixes

Independent review of commit 29daecc against specs 13/70/72/99 surfaced
one P1 (F-007) plus several P2/P3 items.

P1 — fixed in-phase:
- F-007 reduce.rs::reduce_for — for-comprehension binders are lowered as
  SymbolKind::Other (bare HCL identifiers), not SymbolKind::Var.
  Production HCL like `[for x in [1,2,3]: x * 10]` never resolved because
  the reducer's `Other` arm just cloned the expression. Added a separate
  `Scope.binders` namespace and routed the `Other` lookup through it;
  reduce_for pushes binders into `binders` instead of `vars`. Pinned by
  reduce::tests::test_for_list_comprehension_resolves (now uses the
  production `Other` shape) + test_for_map_comprehension_resolves +
  evaluator_pipeline::test_should_resolve_for_list_comprehension_from_real_hcl.

P3 — fixed in-phase:
- F-012 stdlib.rs: rename TobooLFn → ToBoolFn.
- F-013 stdlib.rs: removed dead `cx_with_limits` test helper; replaced
  manual `CallCx { .. }` literal with `CallCx::new(...)`.
- F-018 component.rs: added tracing::instrument(skip(self, component,
  ctx), fields(component_id, component_path, n_repo_vars,
  n_cascade_locals)) — counts only, never logs ctx.repo_vars by value.

Deferred to specs/93-improvements-review.md: F-008 (TemplateConcat
non-scalar collapse), F-009 (Object collapse dead-fallback footgun),
F-010 (FilesetFn walk error leaks paths), F-011 (Component clone in
EvaluatedComponent), F-014 (cycle diag span), F-015 (S-010 enumeration),
F-016 (i64-to-f64 precision at extremes), F-017 (for-key try_from
fallback).

Quality gates: cargo build, cargo test (332 / 0 failures), cargo
+nightly fmt --check, cargo clippy -D warnings, RUSTDOCFLAGS=-D warnings
cargo doc all clean.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([fed6130](https://github.com/commit/fed61301b9c9466607f7d4e5ada72334bff24bfb)) - Tyr Chen
- phase 5: module expansion + count/for_each (closes M2)

Lands the resource-graph phase per spec 15. `crates/core/src/graph/`
introduces `ModuleRegistry`, `DefaultGraphBuilder`, and the expansion
machinery that flattens module bodies into their callers, rewrites
addresses with the `module.<name>[idx]` prefix chain, substitutes
`var.*` inputs from the call site, rewrites `provider = aws.<alias>`
through the call's `providers` map (D8), enforces the recursion-depth
cap and detects cycles (I-GRAPH-4), and applies `count`/`for_each`
expansion with the spec's 1024 default cap (§ 3.3).

Exit criteria (spec 91 § 8): integration test
`graph_expansion_pipeline.rs` against the `large-monorepo` fixture
proves module bodies surface as `module.<call>.aws_*` rows, address
uniqueness (I-GRAPH-1) holds across the workspace, literal `count = N`
expands, and unresolved `count` keeps one template row. Property test
`test_rewrite_address_commutes_with_input_substitution` pins the
spec § 9 commutativity invariant.

Spec/CLAUDE.md anchors: `#![forbid(unsafe_code)]`, `Diagnostic`-first
error model with `LimitKind::Expansion`, `dashmap`/`Arc<Path>` keyed
registry, `#[non_exhaustive]` on every public type, deterministic
component sort (I-GRAPH-5).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([944baa6](https://github.com/commit/944baa6ccf04bda6c5d3cbc2719753a81f483995)) - Tyr Chen
- phase 5 review: fix F-019/F-020 + defer P-041..P-049 + S-014..S-017

Two P1 review findings fixed in-phase:
- F-019: thread `parent_provider_map` through nested module expansions so a
  grand-parent's `providers = { aws = aws.main }` continues to apply through
  an intermediary silent call. New `merge_provider_maps` helper layers
  parent under current with current taking precedence. Regression test
  `test_merge_provider_maps_layers_parent_under_current` pins.
- F-020: `prefix_address` / `with_indexed_address` now return Result and
  surface `TF1507` (drop the resource) instead of silently falling back
  to the un-prefixed address, which would have produced bogus
  TF1506 collisions. Regression test
  `test_prefix_address_overflow_emits_diagnostic_and_drops_resource` pins.

One P1 closed as invalid: the cycle-stack push-after-resolve concern is
already covered by `test_should_detect_module_self_cycle_and_emit_diagnostic`
(top-level Module-kind components are skipped by the kind=Component filter,
so the additional push is unreachable).

P-041..P-049 (P2/P3 hygiene) and S-014..S-017 (spec defects) appended to
specs/93-improvements-review.md.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([0fc3e7b](https://github.com/commit/0fc3e7be3d5c0dc6dd2fe68bd3173c0984708344)) - Tyr Chen
- phase 6: Terragrunt resolver (closes M3)

Lands the Terragrunt mimicking resolver per spec 14. The new
`crates/core/src/terragrunt/` module ships:

- `TerragruntResolver` trait + `FsTerragruntResolver` default impl
  walking the include load chain bottom-up, applying merge cascade
  (`deep_map_only` default + `deep` / `shallow` / `no_merge`), and
  emitting a `TerragruntConfig` for downstream evaluator + graph use.
- Terragrunt-specific functions: `find_in_parent_folders`,
  `find_in_parent_folders_from`, `path_relative_to_include`,
  `path_relative_from_include`, `get_terragrunt_dir`, `get_repo_root`,
  `get_parent_terragrunt_dir`, `try`. Stateful trait objects close
  over `Arc<TgState>`.
- `read_terragrunt_config(path, fallback?)` with `dashmap::DashMap`
  memo (I-TG-3) + per-thread cycle stack + single-flight via
  `inflight` set. Returns `Value::Map { locals = {...}, inputs = {...} }`.
- `generate "label" { ... }` capture with best-effort `TemplateConcat`
  rendering so heredoc `contents` with `${local.X}` interpolations
  surface even when partial. Sub-parses `generate "backend"` contents
  for state-backend extraction.
- `dependency "name" { ... }` capture with optional `mock_outputs`.
- Include cycle / depth-cap enforcement (I-TG-2): shared visited set
  across recursive `resolve_include_chain` calls so `a → b → a`
  reports cleanly without stack overflow.

Evaluator extension (load-bearing for the cascade): the reducer now
descends `.foo.bar` attribute access on `local.<map>` / `var.<map>`
bindings (`descend_attributes`). Without this, the canonical Terragrunt
cascade `local.merged_vars.aws_region` stays unresolved and `inputs`
never reduce.

Exit criteria (spec 91 § 9): integration tests
`crates/core/tests/terragrunt_cascade.rs` against the `large-monorepo`
fixture pin (a) end-to-end resolution of api-gateway's include chain
with non-empty `effective_locals` + captured `generate "backend"`,
(b) path-escape rejection in `read_terragrunt_config`, and (c) memo
single-flight of duplicate `read_terragrunt_config` call sites.
Cycle detection covered by
`terragrunt::resolver::tests::test_detects_include_cycle`. Fuzz harness
`crates/core/fuzz/fuzz_targets/terragrunt.rs` exercises arbitrary
bytes through the full resolve path inside a tempdir sandbox.

Spec/CLAUDE.md anchors: `#![forbid(unsafe_code)]`,
`Diagnostic`-first error model with `LimitKind::IncludeDepth`,
`DashMap` (not `Mutex<HashMap>`) for the memo,
`Arc<BTreeSet<Arc<str>>>` for the env-var allowlist, `#[non_exhaustive]`
on every public type, sandboxed path resolution via
`paths::canonicalize_inside`. Dependency `dashmap = 6` added under
NCSA-permissive (deny.toml allow list expanded).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([722220f](https://github.com/commit/722220fe22a5da7c992ff235926c5c9c3aab4525)) - Tyr Chen
- phase 6 review: fix F-021/F-022/F-023 + defer P-063..P-076 + S-018..S-020

Three P1 review findings fixed in-phase:

- F-021: `read_terragrunt_config` recursive reads now see the full TG
  function set via an `Arc<OnceLock<Arc<FuncRegistry>>>` populated
  immediately after registry construction. Transitive
  `find_in_parent_folders` / `get_repo_root` / `get_terragrunt_dir`
  calls inside a parent's locals dispatch correctly.
  Regression: `test_recursive_read_sees_terragrunt_functions`.
- F-022: `extract_state_backend` now reads the `__labels__` synthetic
  key inside the loader-lowered `backend "<kind>" { ... }` block
  rather than hardcoding `kind = "s3"`. Cross-refs spec defect S-006.
- F-023: `apply_cascade` now accumulates non-literal locals across
  layers via `map_to_locals_with_inherited`. A parent layer's
  `merged_vars = merge(...)` survives through subsequent child
  layers; child entries override by name; merged-literal map wins on
  conflict.
  Regression: `test_parent_layer_non_literal_locals_survive_cascade`.

P-063..P-076 (P2/P3 hygiene) and S-018..S-020 (spec defects) appended
to specs/93-improvements-review.md.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([51e564b](https://github.com/commit/51e564b1c8b9d0129fa9917591a1a1b4486b51d7)) - Tyr Chen
- phases 7+8: Provider resolver (closes M4) + dependency graph & secondary tables (closes M5)

Phase 7 lands the last fill phase before the exporter — every Resource gets
account_id / account_name / region populated by walking the provider alias →
provider block → profile / assume_role / cascade → external ProfileMap chain
(spec 16 § 4). Three loaders ship: YAML (validator-derived, `^\d{12}$`
account-id, charset-bounded names), AWS shared-config INI (rust-ini,
`source_profile` chain capped at 8 hops with both cycle and length detection),
and an empty `none` variant. ArcSwap exposes the resolver-handle for hot
re-loads. New IR fields on Resource: `account_id: Option<AccountId>`,
`account_name: Option<Arc<str>>`, `region: Option<Region>` — already wired
into the parquet writer.

Phase 8 lands the dependency graph and the three secondary parquet tables.
collect_edges_in_place walks every resource's attribute tree, depends_on
list, module-call inputs, and Terragrunt dependency blocks; emits Edges
(`ExplicitDependsOn`, `AttrRef`, `ModuleInput`, `TerragruntDependency`) de-duped
on `(from, to, kind)` and sorted deterministically. Secondary writers ship
behind ExportOptions.tables (`dependencies.parquet` / `components.parquet` /
`modules.parquet`), each with its own schema, atomic `.partial → rename`
write, and zstd-3 default. `tfparser verify` subcommand re-hashes every file
in workspace.manifest.json against the stored SHA-256 and fails on mismatch
or tamper.

Specs covered:
  16 (§ 2 interface, § 3 loaders, § 4 chain, § 5 invariants I-PROV-1..5, § 6
  diagnostics, § 9 CLAUDE.md anchoring), 15 (§ 4 edge inference, § 5
  component summary), 10 (§ 2 IR with new Resource fields, § 5.1 / § 5.2 /
  § 5.3 secondary-table schemas), 50 (verify subcommand), 70 (path safety,
  size caps, validator-derived input bound), 72 (integration test, 95%
  account-id coverage, 3-table join), 91 § 10 (Phase 7), 91 § 11 (Phase 8),
  99 (D9 profile map is external input).

Exit criteria — Phase 7 (impl-plan § 10):
  - large-monorepo + synthesised profile map → ≥95% account_id coverage
    (test_should_meet_95pct_account_id_coverage_on_synthetic_workspace).
  - 5-profile AWS-config fixture mix of sso_account_id / role_arn / chained
    source_profile loads
    (test_should_load_aws_config_with_sso_account_id /
     test_should_load_aws_config_role_arn_extracts_account_id /
     test_should_follow_source_profile_chain /
     test_should_reject_aws_config_chain_cycle).

Exit criteria — Phase 8 (impl-plan § 11):
  - Three secondary tables emit alongside resources.parquet
    (test_three_tables_emit_alongside_resources).
  - Edge counts match the hand-curated oracle on the synthetic workspace
    (test_oracle_edges_match_expected_counts).
  - DuckDB-style 3-table join verified in-memory; full DuckDB query
    coverage is deferred to Phase 9 hardening
    (test_three_table_join_simulates_duckdb).
  - tfparser verify subcommand passes on unchanged + fails on tampered
    artefacts (test_verify_subcommand_passes_on_unchanged_artifacts /
     test_verify_subcommand_fails_when_artifact_is_tampered).

Quality gates: cargo build / test (433 total: 380 lib + 9+3 cli + 41
integration), clippy -D warnings, +nightly fmt --check, doc -D warnings,
cargo deny check all clean.

Files changed:
  - New: crates/core/src/provider/{mod,error,profile_map,resolver}.rs;
    crates/core/src/ir/edge.rs; crates/core/src/graph/edges.rs;
    crates/core/src/exporter/secondary.rs;
    crates/core/tests/provider_pipeline.rs;
    crates/core/tests/dependency_graph_pipeline.rs.
  - Augmented: Resource (account_id/region/account_name fields);
    Workspace (edges field); ExportOptions (tables + SecondaryTable);
    GraphBuilder::build (collect_edges_in_place wiring); ParquetExporter
    (writes secondary tables when requested, manifest covers them);
    CLI (verify subcommand + sha2 dep); error::Error (Provider variant).
  - Deps: arc-swap, rust-ini, validator (derive), serde_yaml — all
    permissive licenses, deny.toml already covered.

Independent review (2026-05-14, two general-purpose subagents in parallel,
one per phase):
  - Phase 7: 3 P1 fixed in-phase (F-024 deterministic reverse account-name
    lookup; F-025 chain-hop cap rewritten as explicit loop with hops
    counter, off-by-one closed; F-026 strict UTF-8 rejection in
    load_aws_config, no more lossy replacement). 2 invalid findings closed
    (profile-miss does fall through to cascade; HashMap iteration in
    load_aws_config is determinism-safe). 11 P2/P3 + 6 spec defects
    (S-027..S-032) deferred to spec 93.
  - Phase 8: 1 P0 + 1 P2 fixed in-phase (F-027 removed reachable unwrap in
    component_address by returning Option; F-028 dropped dead-code shim).
    10 P2/P3 + 4 spec defects (S-033..S-036) deferred to spec 93.

Next phase unlocked: Phase 9 / M6 hardening (impl-plan § 12) — bench
harness, flamegraph, Arc<str> interner, fuzz overnight, cargo audit, docs,
crates.io dry-run.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([5a9a044](https://github.com/commit/5a9a0444c723e1609b18ecf2fafbe018458587df)) - Tyr Chen
- phase 9: DefaultPipeline + CLI parity + hardening (closes M6)

Lands the missing DefaultPipeline orchestrator wiring discovery → loader →
projection → terragrunt → evaluator → graph → provider. The CLI is now a
thin wrapper around it and emits all four Parquet tables (resources,
dependencies, components, modules) plus the manifest by default. End-to-
end smoke-tested against every ./fixtures/* project and cross-checked with
DuckDB (216 resources / 51 deps / 9 components / 12 modules on
large-monorepo, 4 distinct edge_kinds, joinable in SQL).

Closes deferred review findings: P-016 (CompressionOpt::zstd boundary
validation + ValidationError::Range), P-020 (CLI command-line redaction —
both `--flag=value` and `--flag value` forms, UTF-8-safe truncation),
P-094 (drop redundant sorted_edges, borrow ws.edges with debug_assert!),
P-095 (idiomatic counts loop), P-097 (read_capped for manifest).

Phase 9 hardening:
- criterion bench harness in crates/core/benches/pipeline.rs (5 benches,
  make bench / bench-save-baseline / bench-vs-baseline; gates 10% per
  spec 71 § 7)
- cargo doc -D warnings clean; cargo deny check ok
- cargo publish -p tfparser-core --dry-run succeeds
- README + CHANGELOG written
- lib.rs phase-status table updated

Independent review pass — 3 P1 findings (F-029 UTF-8 truncate panic, F-030
space-separated secret flag leak, F-031 GraphError exit-code collision
with spec 50 § 4.3) fixed in-phase with regression tests. Six P2/P3
hygiene items (P-104..P-109) + four spec defects (S-037..S-040) deferred
to specs/93-improvements-review.md.

Quality gates: 446 tests pass, clippy -D warnings + --benches clean,
nightly fmt clean, doc -D warnings clean, cargo deny ok.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com> - ([9cd3d2b](https://github.com/commit/9cd3d2b7fbf80f79a9751f13110747f7e4ac4b96)) - Tyr Chen
- Parser façade, apps/cli layout, adrise resolver fixes, bilingual docs - ([f0e32cb](https://github.com/commit/f0e32cbfd32e5c2ce6f3bba4bfe956545889465d)) - Tyr Chen

<!-- generated by git-cliff -->
