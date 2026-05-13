# 99 — Key Decisions

Status: living document · Owner: tfparser-core · Last updated: 2026-05-13

Permanent record of load-bearing design choices. Each entry is **append-only**: supersede by adding a new D-id with `Supersedes: D-N` rather than editing in place. A future reviewer asking "why this?" should land here, not in chat history.

---

## D1 — Source-only parsing as the primary strategy

- **Context**: How we extract Terraform configuration into the IR.
- **Alternatives considered**:
  - **A**: `terraform init && terraform show -json` per component — maximum fidelity, but requires AWS credentials, network access, and per-component init (slow, opaque, breaks hermeticity).
  - **B**: Read `terraform.tfstate` files — only available post-apply, lossy on conditional/dynamic blocks.
  - **C**: Source-only HCL parsing, best-effort evaluation. Hermetic, fast, partial-data-tolerant.
- **Decision**: **C**. Optional state/plan ingestion can be added later as a `--with-plan` mode (R-OPEN deferred).
- **Why**: Hermeticity is the load-bearing property. The product is used in CI, by security audits, and on machines without provider credentials. A parser that needs `terraform init` cannot run there. The cost is that some dynamic values stay symbolic — explicitly **not a failure** in our model.
- **Pinned by**: [00-prd.md § 2](./00-prd.md), [13-evaluator.md § 1](./13-evaluator.md).
- **Date**: 2026-05-13.

## D2 — `hcl-edit` for parse + `hcl-rs::eval` for evaluation

- **Context**: Which HCL crate(s) to depend on.
- **Alternatives considered**:
  - **A**: `hcl-rs` alone — has evaluator, but discards source spans.
  - **B**: `tree-sitter-hcl` — incremental & error-tolerant, but heavy and awkward to walk from Rust types.
  - **C**: `tfconfig` — shallow inspector, would force a second parse path.
  - **D**: `hcl-edit` (preserves spans + comments) for the AST, `hcl-rs::eval` for the evaluator running over our lowered IR.
- **Decision**: **D**.
- **Why**: Spans are non-negotiable for diagnostics, UI hover, and graph navigation. `hcl-edit` is the only Rust crate that preserves them on every node. Pairing with `hcl-rs::eval` via a thin adapter keeps the dependency seam small — if either changes, blast radius is one adapter file.
- **Pinned by**: [12-hcl-loader.md § 2](./12-hcl-loader.md), [13-evaluator.md § 4](./13-evaluator.md), [hcl-parsing-in-rust.md](../docs/research/hcl-parsing-in-rust.md).
- **Date**: 2026-05-13.

## D3 — arrow-rs (`arrow` + `parquet`) for Parquet I/O, not Polars

- **Context**: Which crate emits Parquet.
- **Alternatives considered**:
  - **A**: `polars` — DataFrame API, more ergonomic for ad-hoc work.
  - **B**: `arrow2` / `parquet2` — forks, unmaintained for our purposes.
  - **C**: arrow-rs (`arrow` + `parquet`) directly.
- **Decision**: **C**. Polars is *recommended to users* as a consumer, but our crate does not depend on it.
- **Why**: The Parquet schema is the public contract. Coupling our types to Polars (and its release cadence) would leak Polars' versioning concerns onto every embedder.
- **Pinned by**: [20-parquet-exporter.md § 2](./20-parquet-exporter.md), [parquet-arrow-in-rust.md](../docs/research/parquet-arrow-in-rust.md).
- **Date**: 2026-05-13.

## D4 — Best-effort evaluator: `Unresolved` is a first-class value, not an error

- **Context**: How to handle references the source-only evaluator cannot resolve (`data.x.y`, `aws_*.z.attr`, `module.m.o`, unbound `var.foo`).
- **Alternatives considered**:
  - **A**: Treat as a hard error — fail the parse.
  - **B**: Substitute a default (`null`, `""`).
  - **C**: Make `Expression::Unresolved` a first-class IR node that survives evaluation and is rendered in canonical JSON as `{"__unresolved__": ...}`.
- **Decision**: **C**.
- **Why**: An apply-time-only value is **not a parse error** — the source is correct, we just don't have apply-time information. Substituting a default silently produces wrong analytics ("oh, 80 % of buckets have policy=null"). Surfacing the unresolved reference truthfully lets downstream queries `WHERE attributes_json LIKE '%__unresolved__%'` find templates that need follow-up.
- **Pinned by**: [13-evaluator.md § 6](./13-evaluator.md), [10-data-model.md § 4](./10-data-model.md#canonical-json-for-attributes_json).
- **Supersedes**: —
- **Date**: 2026-05-13.

## D5 — Module bodies flatten into the parent component, not emitted as separate components

- **Context**: How to represent module-defined resources in the dataset.
- **Alternatives considered**:
  - **A**: Each module call gets its own row in `components.parquet`; the module's resources reference the call.
  - **B**: Resources are emitted under their **callsite component**, with the address prefixed by `module.<name>`.
- **Decision**: **B**.
- **Why**: A module's *resources* are only meaningful in the context of who calls it (the calling component picks providers, sets inputs, decides count/for_each). Emitting them under the parent matches how an operator would query "what does `live-site/foo` actually deploy?". The module catalog still ships separately as `modules.parquet` for "who uses module X" queries.
- **Pinned by**: [15-resource-graph.md § 3.2](./15-resource-graph.md).
- **Date**: 2026-05-13.

## D6 — Mimic Terragrunt, do not invoke it

- **Context**: How to handle Terragrunt repos.
- **Alternatives considered**:
  - **A**: Shell out to `terragrunt render-json` per component.
  - **B**: Re-implement only the subset of Terragrunt that affects what HCL the component sees (locals/inputs cascade, `include`, file-reading funcs).
- **Decision**: **B**. A future `--from-render-json` flag is allowed as a separate input mode; the default remains source-only.
- **Why**: Hermeticity (D1). Shelling out reintroduces network/cred/binary dependencies. The cost is that we may drift from upstream Terragrunt; we document the subset and accept it.
- **Pinned by**: [14-terragrunt.md § 1](./14-terragrunt.md), [terragrunt-handling.md](../docs/research/terragrunt-handling.md).
- **Date**: 2026-05-13.

## D7 — Single flat `resources.parquet` table for M0; star schema added at M5

- **Context**: Parquet schema shape.
- **Alternatives considered**:
  - **A**: Star schema from day one (`resources` + `modules` + `components` + `dependencies` + `accounts`).
  - **B**: Single flat table with `attributes_json` blob.
  - **C**: Single nested Arrow table with `Struct<...>` columns per resource type.
- **Decision**: **B** for M0; secondary tables added at M5 (still simple — no foreign-key dance, joins by `address` text).
- **Why**: The schema is **the** public contract. Locking a star schema before the IR shape is exercised against ≥ 1 real repo is premature. Flat table + JSON blob is the smallest contract that supports useful queries via DuckDB / Athena from day one. (C) was rejected because resource attributes are heterogeneous per type; no static struct can cover the population.
- **Pinned by**: [10-data-model.md § 3](./10-data-model.md), [20-parquet-exporter.md § 3](./20-parquet-exporter.md).
- **Date**: 2026-05-13.

## D8 — Provider alias rewriting happens at module expansion, not at provider resolution

- **Context**: How a module's `provider = aws.foo` resolves to the right account when the *calling component* passes a different alias via `providers = { aws = aws.main }`.
- **Alternatives considered**:
  - **A**: The provider resolver follows the `ModuleCall.providers` map at resolution time.
  - **B**: At expansion time (when flattening the module body into the parent), every `ProviderRef` inside the module is rewritten according to the call's `providers` map. The downstream resolver sees only the parent's alias namespace.
- **Decision**: **B**.
- **Why**: Single-source-of-truth in the IR. After expansion, every `Resource.provider_ref` refers to the **parent component's** provider blocks — no further indirection. The resolver has no special-cased "is this inside a module" path; the same code resolves leaf and expanded resources alike.
- **Pinned by**: [15-resource-graph.md § 3.2](./15-resource-graph.md), [16-provider-resolver.md § 4.3](./16-provider-resolver.md).
- **Date**: 2026-05-13.

## D9 — `ProfileMap` is an external input, not derived from the repo

- **Context**: How to map AWS profile names to account IDs.
- **Alternatives considered**:
  - **A**: Parse a `terraform/security/aws_accounts/` style component if it exists (some monorepos encode the mapping in HCL).
  - **B**: User supplies the map via `--profile-map` YAML or `--aws-config ~/.aws/config`. The parser is org-agnostic.
- **Decision**: **B**.
- **Why**: We do not want the parser's correctness to depend on a particular repo convention. The mapping is operator data (operator picks which `~/.aws/config` they have); making it an explicit input keeps the parser portable. We may add a heuristic auto-detector later as a `--scan-accounts-component` opt-in, but that's not the default.
- **Pinned by**: [16-provider-resolver.md § 3](./16-provider-resolver.md), [multi-account-resolution.md](../docs/research/multi-account-resolution.md).
- **Date**: 2026-05-13.

## D10 — Atomic Parquet writes via `.partial` + rename

- **Context**: How to avoid corrupt output on crash mid-write.
- **Alternatives considered**:
  - **A**: Write directly to the final filename; rely on file-system semantics.
  - **B**: Write to `<name>.partial`, fsync, then `rename(2)` to `<name>`.
- **Decision**: **B**.
- **Why**: A half-written `resources.parquet` is worse than no file — downstream tooling will try to read it. POSIX `rename(2)` is atomic on the same filesystem; the `.partial` file is a clear breadcrumb when the parser was killed.
- **Pinned by**: [20-parquet-exporter.md § 4](./20-parquet-exporter.md).
- **Date**: 2026-05-13.

## D11 — Profile map wrapped in `secrecy::SecretBox` — pending re-evaluation

- **Context**: Whether to wrap the parsed `ProfileMap` in a `secrecy::SecretBox<_>` to make accidental logging harder.
- **Alternatives considered**:
  - **A**: Wrap in `SecretBox` per CLAUDE.md § Secret types — buys defence in depth at the cost of slightly more verbose access patterns.
  - **B**: Treat account IDs as low-sensitivity (they are routinely shared between teams) and skip the wrapper.
- **Decision**: **A** for now; revisit at M4 if `SecretBox` proves to add friction without measurable benefit.
- **Why**: Wrapping is cheap if `expose_secret()` happens in exactly two places (resolver + diagnostic). If those grow to ten places, we drop it.
- **Pinned by**: [70-security.md § 3.3](./70-security.md).
- **Open question**: revisit after Phase 7 lands.
- **Date**: 2026-05-13.

## D12 — One workspace = one CLI invocation; no incremental / watch mode in v0.1

- **Context**: Should `tfparser` support `--watch` or per-file re-parse?
- **Alternatives considered**:
  - **A**: Full re-parse per invocation only.
  - **B**: A `--watch` mode that re-parses changed files using a Merkle hash over file contents.
  - **C**: A long-running daemon (`tfparser serve`) holding the parsed `Workspace` in memory.
- **Decision**: **A** for v0.1. **C** is on the roadmap as a post-M6 deliverable (`apps/server` skeleton already exists for it).
- **Why**: Full re-parse is fast enough (5 s on reference scale) that incremental complexity isn't worth the bug surface. A daemon is a separate product shape; defer until there's user demand.
- **Pinned by**: [00-prd.md § 4](./00-prd.md), [50-cli.md § 7](./50-cli.md).
- **Date**: 2026-05-13.

## D13 — `Arc<str>` / `Box<str>` for short strings; never `String` in public IR

- **Context**: How to keep memory footprint reasonable at reference scale.
- **Alternatives considered**:
  - **A**: `String` everywhere (idiomatic, simpler).
  - **B**: `Box<str>` for owned-once, `Arc<str>` for shared.
- **Decision**: **B**, with a small `Arc<str>` interner for resource types and attribute names.
- **Why**: At ~40k resources × dozens of attributes each, the per-string overhead matters. `Arc<str>` is 16 bytes vs `String`'s 24, plus the interner cuts redundant copies of `"aws_iam_role"` etc. by 4–8×.
- **Pinned by**: [10-data-model.md § 2.6](./10-data-model.md#allocation-discipline), [71-performance-budgets.md § 4](./71-performance-budgets.md).
- **Date**: 2026-05-13.

## D14 — Synchronous + `rayon`, no `tokio` in the core library for v0.1

- **Context**: Concurrency model.
- **Alternatives considered**:
  - **A**: Full async with `tokio`, all phases as async tasks.
  - **B**: Synchronous code, `rayon::par_iter` for component-level parallelism, single writer thread for Parquet.
- **Decision**: **B**.
- **Why**: The workload is CPU-bound (HCL parse + eval), not I/O-bound. `tokio` adds runtime overhead and complexity (Send/Sync issues with `hcl-edit`'s parse tree) for no throughput win. If a future server (`apps/server`) needs `tokio` for its HTTP layer, it imports the sync library and wraps it in `spawn_blocking`.
- **Pinned by**: [50-cli.md § 6](./50-cli.md), [61-crates-and-features.md § 3](./61-crates-and-features.md).
- **Date**: 2026-05-13.
