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

### P3 — implementation hygiene (deferred)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| F-001 | P3 | `crates/core/examples/spike_eval_context.rs:78-83` | Add `name.contains('\0')` rejection to the find_in_parent_folders sandbox. Phase 6 production resolver will need it; bake the pattern into the spike now. |
| F-002 | P3 | `crates/core/examples/spike_eval_context.rs:105-109` | Loop-termination uses `cur.pop()` short-circuit + `starts_with(&repo_root)`; on a `repo_root == "/"` system this depends on `pop` returning false. Production resolver: track the bound directly with `cur == repo_root` check. |
| F-003 | P3 | `crates/core/src/diagnostic.rs:23-35` | Severity doc cites `50-cli.md § 4.3`; that section reference is unverified — drop the section number or change to "see 50-cli.md `--fail-on-diagnostics`". |
| F-004 | P3 | `crates/core/src/ir/mod.rs:1-7` | Module-level doc does not list which I-IR-* invariants are pinned in Phase 1 vs deferred to loader/exporter. Add an "Invariants pinned in Phase 1" list. |
| F-005 | P3 | `crates/core/src/ir/expression.rs::Conditional` | Missing `#[builder(field_defaults(setter(into)))]` for ergonomic `cond(expr)` instead of `cond(Box::new(expr))`. (Add when Conditional is constructed from outside the loader; cosmetic until then.) |

### Spike cleanup

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| F-006 | P3 | `crates/core/examples/spike_*.rs` | The impl plan's Phase 0 § exit gate says "spikes are deleted; the learnings live in the spec text." We kept them as runnable canaries / `cargo run --example` smoke tests. Decide whether to delete them at the start of Phase 2, or formally re-classify them as "Phase 0 canaries" and update `91-impl-plan.md`. |

### Test coverage (low-risk gaps)

| ID | Severity | Where | Fix shape |
| -- | -------- | ----- | --------- |
| T-001 | P3 | `crates/core/tests/workspace_round_trip.rs` | Round-trip is structural-only; does not assert I-IR-1 (every span's path is under `workspace.root`). Loader (Phase 2) will validate properly; once it does, extend the test. |
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
