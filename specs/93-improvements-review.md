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

## How to use this file

When a future phase starts, scan the table above for entries whose `file:line`
falls in the phase's scope, address them, and remove the entry. If new
deferred findings arise, append them under a new "Phase N+1 review" heading.

If an entry is downgraded or invalidated, strike it through and add a one-line
note in `99-key-decisions.md` referencing the reason — do not silently delete.
