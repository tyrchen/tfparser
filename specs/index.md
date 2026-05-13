# tfparser — Specifications Index

Authoritative entry point. Read this first.

## What this is

`tfparser` is a Rust library + CLI that **parses a Terraform / Terragrunt source repository** (no `terraform init`, no AWS creds), walks every component and module, resolves what can be resolved statically, and **emits the result as Parquet** so the dataset can be queried with DuckDB / Polars / Athena / ClickHouse.

The spec set below is the contract between intent and code: the PRD frames *what* and *why*, the component designs cover *how*, the cross-cuts pin engineering constraints (security, perf, testing), and the roadmap + impl-plan pair organises delivery for stakeholders and engineers respectively.

## Spec catalogue

| #  | File | Type | What it pins |
| -- | ---- | ---- | ------------ |
| 00 | [00-prd.md](./00-prd.md) | PRD | Vision, users, goals (measurable), non-goals, naming conventions. |
| 10 | [10-data-model.md](./10-data-model.md) | Design | In-memory IR + frozen Parquet schema. |
| 11 | [11-discovery.md](./11-discovery.md) | Design | Filesystem walker, component vs module classification. |
| 12 | [12-hcl-loader.md](./12-hcl-loader.md) | Design | HCL parse + lowering to IR with spans. |
| 13 | [13-evaluator.md](./13-evaluator.md) | Design | Best-effort variable / locals evaluator. |
| 14 | [14-terragrunt.md](./14-terragrunt.md) | Design | Terragrunt subset: include, find_in_parent_folders, cascade, generate. |
| 15 | [15-resource-graph.md](./15-resource-graph.md) | Design | Module expansion, count/for_each, dependency-edge inference. |
| 16 | [16-provider-resolver.md](./16-provider-resolver.md) | Design | Provider alias → account_id / region resolution. |
| 20 | [20-parquet-exporter.md](./20-parquet-exporter.md) | Design | Parquet writer + atomic output. |
| 50 | [50-cli.md](./50-cli.md) | Design | `tfparser parse / inspect / schema / version` CLI. |
| 61 | [61-crates-and-features.md](./61-crates-and-features.md) | Cross-cut | Workspace layout, crate split, deps, lints, feature flags. |
| 70 | [70-security.md](./70-security.md) | Cross-cut | Threat model, validation, sandboxing, resource limits. |
| 71 | [71-performance-budgets.md](./71-performance-budgets.md) | Cross-cut | Per-phase latency budgets, memory ceiling, CI regression gates. |
| 72 | [72-testing-strategy.md](./72-testing-strategy.md) | Cross-cut | Pyramid, fixtures, snapshots, fuzz harnesses. |
| 80 | [80-glossary.md](./80-glossary.md) | Reference | Disambiguation of overloaded terms (component, module, provider, …). |
| 90 | [90-roadmap.md](./90-roadmap.md) | Roadmap | Stakeholder-facing milestones M0..M6 with exit criteria. |
| 91 | [91-impl-plan.md](./91-impl-plan.md) | Plan | Engineer-facing dependency-ordered Phase 0..9 task tables. |
| 99 | [99-key-decisions.md](./99-key-decisions.md) | Log | D1..D14 load-bearing decisions with alternatives and rationale. |

Linked support docs:

- [docs/research/index.md](../docs/research/index.md) — five risk-retirement memos that informed the spec set.
- [CLAUDE.md](../CLAUDE.md) — project engineering norms (errors, async, type design, safety/security) that every spec references.

## Reading orders

**Stakeholder / new joiner** (calendar shape and what gets shipped):

1. [00-prd.md](./00-prd.md) — what we're building and why.
2. [80-glossary.md](./80-glossary.md) — vocabulary.
3. [90-roadmap.md](./90-roadmap.md) — milestone shape.
4. [10-data-model.md](./10-data-model.md) — the dataset they'll query.

**Engineer about to write code** (dependency-ordered):

1. [00-prd.md](./00-prd.md), [80-glossary.md](./80-glossary.md) — context.
2. [91-impl-plan.md § 3 (Phase 0)](./91-impl-plan.md) — risk retirement spikes.
3. [10-data-model.md](./10-data-model.md) — IR + Parquet schema. **Frozen at start of Phase 1.**
4. [11-discovery.md](./11-discovery.md) → [12-hcl-loader.md](./12-hcl-loader.md) → [13-evaluator.md](./13-evaluator.md) → [14-terragrunt.md](./14-terragrunt.md) → [15-resource-graph.md](./15-resource-graph.md) → [16-provider-resolver.md](./16-provider-resolver.md) → [20-parquet-exporter.md](./20-parquet-exporter.md) → [50-cli.md](./50-cli.md).
5. Cross-cuts read alongside, not in sequence: [61-crates-and-features.md](./61-crates-and-features.md), [70-security.md](./70-security.md), [71-performance-budgets.md](./71-performance-budgets.md), [72-testing-strategy.md](./72-testing-strategy.md).
6. [99-key-decisions.md](./99-key-decisions.md) — referenced whenever a "why this?" question arises.

**Security or perf reviewer**:

1. [70-security.md](./70-security.md) or [71-performance-budgets.md](./71-performance-budgets.md) directly.
2. Cross-link into the component specs from the cited invariants.

## Build-order graph

```text
00-prd
  │
  ▼
10-data-model ────────────────────────────────────┐
  │                                                │
  ▼                                                │
11-discovery ──► 12-hcl-loader ─┬──► 13-evaluator  │
                                 │        │        │
                                 │        ▼        │
                                 │   14-terragrunt │
                                 │        │        │
                                 │        ▼        ▼
                                 └─► 15-resource-graph ──► 16-provider-resolver
                                                                  │
                                                                  ▼
                                                          20-parquet-exporter
                                                                  │
                                                                  ▼
                                                              50-cli  (M0 ships here)

Cross-cuts (read alongside): 61, 70, 71, 72, 80
Decisions log: 99
```

## Pairing of roadmap and impl-plan

| Roadmap milestone (user-visible) | Impl-plan phase (dependency-order) | What ships |
| -------------------------------- | ---------------------------------- | ---------- |
| — | [Phase 0](./91-impl-plan.md#3-phase-0--risk-retirement--1-wk) | Research memos + spikes |
| [M0](./90-roadmap.md#m0--list-every-resource-as-a-flat-parquet-file) | [Phase 1](./91-impl-plan.md#4-phase-1--ir-foundation-closes-part-of-m0-week-12) + [Phase 2](./91-impl-plan.md#5-phase-2--discovery--hcl-loader-closes-most-of-m0-week-34) + [Phase 3](./91-impl-plan.md#6-phase-3--resource-extraction--parquet-writer-closes-m0-week-5) | Flat `resources.parquet` from plain TF |
| [M1](./90-roadmap.md#m1--variables-and-locals-are-resolved-when-statically-possible) | [Phase 4](./91-impl-plan.md#7-phase-4--evaluator-closes-m1-week-67) | Variable / locals evaluated |
| [M2](./90-roadmap.md#m2--modules-are-expanded--referenced-module-bodies-appear-in-parquet) | [Phase 5](./91-impl-plan.md#8-phase-5--module-expansion-closes-m2-week-89) | Module expansion |
| [M3](./90-roadmap.md#m3--terragrunt-based-repos-work-end-to-end) | [Phase 6](./91-impl-plan.md#9-phase-6--terragrunt-resolver-closes-m3-week-1011) | Terragrunt cascade |
| [M4](./90-roadmap.md#m4--every-aws-resource-has-account_id--region-populated) | [Phase 7](./91-impl-plan.md#10-phase-7--provider--account--region-resolver-closes-m4-week-12) | account / region per resource |
| [M5](./90-roadmap.md#m5--dependency-graph-is-queryable-as-parquet) | [Phase 8](./91-impl-plan.md#11-phase-8--dependency-graph--secondary-tables-closes-m5-week-13) | Graph + secondary tables |
| [M6](./90-roadmap.md#m6--hardened-fast-ready-for-v01) | [Phase 9](./91-impl-plan.md#12-phase-9--hardening-closes-m6-week-1415) | Perf, fuzz, docs, v0.1 |

## Spec set version

- Schema major / minor: 0.1 (M0 freeze). See [10-data-model.md § Versioning](./10-data-model.md#6-versioning).
- Crate version: 0.1.0 target at M6. See [61-crates-and-features.md § 6](./61-crates-and-features.md).

## How to evolve these specs

- **Adding a feature**: add a new design spec (next free 10s number in the right bucket), update `index.md`, update the roadmap with a new milestone or extension to an existing one, log the decision in `99-key-decisions.md`.
- **Changing a decision**: append a new `D-N` entry that supersedes the old; do **not** edit the old in place.
- **Schema change**: bump the minor (additive) or major (breaking) in [10-data-model.md § Versioning](./10-data-model.md#6-versioning), update the Arrow schema golden test, update consumer migration notes in the CHANGELOG.

## Anchor against CLAUDE.md

Every spec references the project engineering norms in [CLAUDE.md](../CLAUDE.md): error handling (`thiserror` + `#[source]`), async/concurrency (`rayon` + `dashmap` + `ArcSwap`), type design (`#[non_exhaustive]` + `NonZeroU32` + newtypes), safety (`#![forbid(unsafe_code)]`, no `unwrap`/`expect`/`panic` on input), and testing (`rstest` / `proptest` / fuzz). Where a spec deviates, it says so explicitly.
