# 00 — PRD: tfparser

Status: draft v1 · Owner: tfparser-core · Last updated: 2026-05-13

## 1. Problem

Large Terraform monorepos are opaque. A representative scale point ([terraform-repo-shapes.md](../docs/research/terraform-repo-shapes.md)) is on the order of ~4 600 HCL files, ~320 k LOC, ~250 components, ~60 modules, and ~10 AWS accounts addressed through provider aliases. Engineers regularly ask **"where does this resource live?", "which accounts is this component touching?", "what does our cross-team dependency graph look like?", "how many `aws_iam_role`s do we have, and in which environments?"** — and the answer today is `ripgrep + tribal knowledge`.

The failure mode is concrete:
- A migration plan needs an inventory of every `aws_db_instance` across environments. There is no canonical list; engineers grep, miss `for_each`-generated resources, and produce stale spreadsheets.
- A security audit asks "every IAM role per account." The answer requires walking provider aliases through Terragrunt variable cascades. Manually unrollable in days, programmatically intractable.
- Visualisation tools (e.g. `inframap`, `terraform graph`) work per-component and need state files; nothing operates on the whole repo source.

Existing tooling — `terraform graph`, `terraform-config-inspect`, `inframap`, `rover` — either (a) needs `terraform init`/`plan` per component (slow, requires creds), (b) inspects one component at a time, or (c) only reads state, not source.

## 2. Vision

A single command:

```
$ tfparser parse ./terraform \
    --environment staging \
    --profile-map ~/.aws/config \
    --out ./workspace.parquet

✓ Discovered 247 components, 58 modules
✓ Parsed 4 612 HCL files in 1.8 s (rayon, 10 threads)
✓ Resolved 91 % of variables / locals; 9 % symbolic
✓ Resolved 8 of 10 AWS accounts via ~/.aws/config
✓ Wrote workspace.parquet — 38 412 rows, 12 MB (zstd-3)

$ duckdb -c "SELECT account_id, resource_type, COUNT(*) FROM 'workspace.parquet'
            WHERE environment='staging' GROUP BY 1,2 ORDER BY 3 DESC LIMIT 10"
```

The Parquet file becomes the source dataset for dashboards, security reviews, dependency-graph visualisations, and one-off SQL queries. The CLI is the M0 surface; the library (`tfparser-core`) is reused later by a server, an LSP, or a TUI.

The product is **read-only**, **hermetic** (no network, no AWS creds, no `terraform` binary invocation), and **stable on partial data**: an unresolved expression becomes a symbolic value, never a crash.

## 3. Goals

| #  | Goal | Measure |
| -- | ---- | ------- |
| G1 | Parse a real ~5k-file TF+Terragrunt repo end-to-end in a single command. | `tfparser parse <repo>` exits 0 on the reference-scale fixture (see [72-testing-strategy.md § Fixtures](./72-testing-strategy.md)). |
| G2 | Be fast enough to run in CI per pull request. | < 5 s wall-clock on a reference-scale repo (~5 k files / ~320 k LOC) on an M-class laptop (10-core), excluding I/O for the output file. |
| G3 | Produce a Parquet file consumable by DuckDB, Polars, Athena, and ClickHouse without translation. | Round-trip: `duckdb -c "SELECT * FROM 'workspace.parquet' LIMIT 5"` works on the artifact. |
| G4 | Resolve `account_id`, `region`, `environment` per resource when source data permits. | ≥ 95 % of `aws_*` resources in the reference-scale fixture have a non-empty `account_id` once a profile map is supplied. |
| G5 | Degrade gracefully on unresolved values; never crash on malformed input from a real repo. | Fuzz harness over `corpus/**` runs 1M iterations without a panic; partial parses still emit Parquet for the resolved subset. |
| G6 | Be embeddable: every CLI capability is exposed as a library API. | `tfparser-core` is `#![forbid(unsafe_code)]`, has rustdoc on every public item, and is the only thing the CLI depends on. |

## 4. Non-goals

- **Not a Terraform replacement.** We do not run `terraform init`, `plan`, or `apply`, and we do not call providers. State files are not read in M0–M5.
- **Not a Terragrunt re-implementation.** We mimic the subset of Terragrunt functions needed for source-parse fidelity (see [14-terragrunt.md](./14-terragrunt.md)). Drift from upstream Terragrunt is acceptable and documented; users needing exact fidelity can pre-render with `terragrunt render-json` (future input mode, out of scope).
- **Not a policy / drift / compliance engine.** Other tools (OPA, Checkov, `tflint`) own that surface. We expose the dataset; they consume it.
- **No write-back, no refactor, no `tfedit`.** The parser is read-only on the user's repo.
- **Not multi-cloud-aware in M0–M5.** GCP / Azure / Datadog providers parse fine (they are just HCL), but the `account_id`/`region` resolver is AWS-shaped. Other providers' values land in `attributes_json` only.
- **No language server / IDE integration in this scope.** A future deliverable can reuse the IR; not committed here.
- **No incremental / watch mode in M0–M5.** Full re-parse only.

## 5. Users

| Persona | Job to be done | Reach for tfparser when… |
| ------- | -------------- | ------------------------ |
| **Infra engineer** | Audits / migrations / capacity reviews. | They need a structured inventory of the whole repo *now*, not after `terraform init` finishes 247 times. |
| **Security engineer** | Inventories IAM roles, S3 buckets, KMS keys per account. | A blanket "list every `aws_iam_policy` with `Action = "*"`" question lands in their queue. |
| **Platform team / data eng** | Builds dashboards over infra (cost attribution, sprawl). | They want a daily Parquet drop in S3 → Athena, no per-component state access. |
| **Onboarding engineer** | Reads the repo to learn its topology. | They want a visual map: "what does `live-site/ads-pacer` actually touch?" |
| **Anti-persona: deploy engineer** | Cares about *will this apply succeed?* | Use Terraform/Terragrunt directly — tfparser does not predict apply behaviour. |

## 6. Success metrics

Measured 90 days post-M3:
- **M-1**: ≥ 2 distinct monorepos parse end-to-end without code changes (proves generality across organisations / conventions).
- **M-2**: `workspace.parquet` is consumed by ≥ 1 internal dashboard or audit workflow.
- **M-3**: Parse time on the reference-scale fixture, measured on the CI runner type chosen for [71-performance-budgets.md](./71-performance-budgets.md), stays under the spec's P95 budget across releases (regression gate).
- **M-4**: Library crate (`tfparser-core`) is used by ≥ 1 downstream Rust binary other than `tfparser` CLI (proves embeddability).

## 7. Naming conventions (binding)

| Surface | Convention | Example |
| ------- | ---------- | ------- |
| Workspace crate | `tfparser` | (the workspace root) |
| Core library crate | `tfparser-core` | published to crates.io |
| CLI binary crate | `tfparser-cli` | `cargo install tfparser-cli` ships `tfparser` binary |
| Public modules in core | `discovery`, `loader`, `eval`, `terragrunt`, `graph`, `provider`, `exporter` | one per component spec |
| Errors | `tfparser_core::Error`, `Result<T> = std::result::Result<T, Error>` | `thiserror` enum, per [70-security.md](./70-security.md) |
| Span type | `tfparser_core::Span { file: Arc<Path>, byte_range: Range<u32>, line: u32, col: u32 }` | exported, public |
| Resource address | `tfparser_core::Address` — Terraform-style: `module.<name>.<type>.<name>[<index>]` | newtype over `Box<str>` |
| Parquet output | `<out>/resources.parquet` for M0; later siblings `<out>/dependencies.parquet`, `<out>/components.parquet`, `<out>/modules.parquet` | flat files in one directory |
| Public types use `Box<str>` / `Arc<str>` for short strings, not `String`. | | Per [CLAUDE.md § Performance](../CLAUDE.md) |
| File ordering inside a crate | `use std::…;` then `use <external>::…;` then `use crate::…;` | Per [CLAUDE.md § Code Style](../CLAUDE.md) |

## 8. Cross-references

- → Drives: [10-data-model.md](./10-data-model.md), [90-roadmap.md](./90-roadmap.md), [91-impl-plan.md](./91-impl-plan.md)
- ↔ Anchored in: [terraform-repo-shapes.md](../docs/research/terraform-repo-shapes.md), [hcl-parsing-in-rust.md](../docs/research/hcl-parsing-in-rust.md)
- ↔ Engineering norms: [CLAUDE.md](../CLAUDE.md)
