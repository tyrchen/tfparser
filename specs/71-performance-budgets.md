# 71 — Performance Budgets

Status: draft v1 · Owner: tfparser-core

## 1. Purpose

Numbers we commit to: latency targets per phase, memory ceiling, regression gate values. Calibrated against a **reference-scale** Terragrunt monorepo — ~4 600 files, ~320 k LOC of HCL, ~250 components, ~60 modules, ~10 AWS accounts via provider aliases. The canonical reproducible benchmark fixture is `crates/core/tests/fixtures/large-monorepo/` (see [72-testing-strategy.md § Fixtures](./72-testing-strategy.md)).

Targets are wall-clock on the **reference machine**: Apple M3 Pro (12-core, 36 GB), release build, NVMe storage warm cache. CI runners report 1.5× these numbers (see [§ 5](#5-ci-gates)).

## 2. End-to-end target

| Surface | Wall-clock | Note |
| ------- | ---------- | ---- |
| `tfparser parse <ref>` (single env) | **≤ 5 s** | Includes Parquet write. Goal G2 from [00-prd.md](./00-prd.md). |
| `tfparser parse <ref> --all-environments` (3 envs) | **≤ 12 s** | Three full runs share discovery + loader caches via `Arc`. |
| `tfparser inspect <ref>` | **≤ 4 s** | Skips Parquet, so saves the export budget. |
| Peak RSS | **≤ 1.5 GiB** | Hard ceiling — fails CI if exceeded. |

The 5-second number is the load-bearing one. Slower than that and the tool stops being usable in pre-PR loops.

## 3. Per-phase ladder (reference-scale)

| # | Phase | Median | P99 | Notes |
| - | ----- | ------ | --- | ----- |
| 1 | Discovery (walk + classify) | 220 ms | 350 ms | I/O bound, 8 threads via `ignore::WalkBuilder`. |
| 2 | HCL load + lower | 1.0 s  | 1.5 s | `rayon` over components, `hcl-edit` parse + AST lowering. |
| 3 | Terragrunt resolve | 200 ms | 350 ms | Memoised across components. |
| 4 | Evaluator (vars/locals fixpoint) | 1.1 s  | 1.7 s | Locals worklist; per-component context. |
| 5 | Graph build (module expand + deps) | 300 ms | 450 ms | Bulk Arc clones, address rewrites. |
| 6 | Provider resolution | 100 ms | 150 ms | Hash lookups; per-resource. |
| 7 | Parquet export (resources only) | 600 ms | 800 ms | Zstd-3 dominates. |
| 8 | Manifest write + fsync | 50 ms  | 100 ms | Atomic rename. |
| **Total** | **≈ 3.6 s** | **≈ 5.4 s** | Allows headroom under the 5 s wall-clock target. |

## 4. Memory budget

Workspace IR for reference-scale shape (~40 k resources post-expansion):

| Section | Bytes | Note |
| ------- | ----- | ---- |
| Source strings (raw `.tf` contents) | ~60 MiB | `Arc<str>` per file; retained for span display. |
| Lowered AST (`AttributeMap` trees) | ~250 MiB | Per-resource. Dominant. |
| Spans + `LineIndex`es | ~40 MiB | `Vec<u32>` per file. |
| Workspace structure (Vecs, ids) | ~10 MiB | |
| Parquet builders (peak during write) | ~80 MiB | All columns pre-sized to row count. |
| `attributes_json` (post-render, transient) | ~120 MiB peak | Released as written. |
| **Total peak** | **≤ 600 MiB** | Comfortable under the 1.5 GiB ceiling. |

If the lowered AST blows up beyond projections, the worklist mitigation is **eviction**: after the evaluator finishes a component and the exporter takes its rows, drop the source string and the AST. M0 keeps everything resident for simplicity.

## 5. CI gates

A `bench` job runs `cargo bench --bench parse_large_monorepo -- --save-baseline=$BASELINE` and compares to the main baseline. **Fail PR** if any of:

- Median end-to-end time worsens by > 10 %.
- Peak RSS worsens by > 10 %.
- Any per-phase median worsens by > 25 % (catches regressions even when total stays within budget).

Allowed regression: any change with a `perf-allow-X-%` label on the PR description (forces reviewer awareness). The label and the value go into the changelog.

## 6. Profiling stack

- **CPU**: `cargo flamegraph -p tfparser-cli --bench parse_large_monorepo` for release-mode flame graphs.
- **Memory**: `samply` (macOS/linux) or `bytehound`. Track `Arc<str>` interner hit rate via a `tracing` counter.
- **Allocation hotness**: a debug-only feature `--features=alloc-stats` swaps the global allocator to `dhat::DhatAlloc` and dumps `dhat-heap.json` on exit.

Profile **once per phase** during initial development to set the per-phase numbers. Re-profile if a milestone exit shows regression > 5 %.

## 7. Microbenchmarks

Under `crates/core/benches/`:

- `parse_one_component` — `HclEditLoader::load` on the `ads-pacer` fixture. Target: ≤ 4 ms median.
- `eval_one_component` — `HclEvaluator::evaluate` on the same. Target: ≤ 2 ms median.
- `render_attributes_json` — 1000 typical resources. Target: ≤ 30 ms total.
- `parquet_write_100k` — synthetic 100k rows. Target: ≤ 600 ms.

`criterion` crate. Per CLAUDE.md § Performance: benchmarks land *after* the relevant phase ships; do not block M0 on them.

## 8. Optimisation rules (priority order)

1. **Don't allocate in the hot path.** `Arc<str>` for shared strings, `Box<str>` for owned-once. No `String::new()` in per-resource code.
2. **Pre-size collections.** `Vec::with_capacity(n)` everywhere `n` is knowable.
3. **Parallelise across components**, not within. Components are independent; intra-component logic is fast enough that thread overhead would dominate.
4. **Intern resource types and attribute names.** ~200 distinct values across 40k rows — interning yields 4–8× memory cut.
5. **Lazy line indexing.** A file's `LineIndex` is built only when a span needs `(line, col)` — typically on diagnostic emit, not bulk parsing.
6. **Avoid serde_json's `to_value`.** Use `Serializer` with `CompactFormatter` writing directly into a pooled `Vec<u8>`.

## 9. Anti-goals

- We **do not** target sub-1-second parses. That would require throwing away `hcl-edit` and writing our own lexer — wrong cost / benefit at this stage.
- We **do not** target stream parsing (process N rows while still reading file N+1). Workspaces are small enough; complexity isn't worth it.
- We **do not** target incremental parses (re-parse only changed files). Deferred to a later milestone.

## 10. Cross-references

- ← Anchored by: [00-prd.md § Goals](./00-prd.md), all component specs (each carries its own per-phase target).
- ↔ Roadmap: [90-roadmap.md § Calendar shape](./90-roadmap.md), [91-impl-plan.md § Phase 10](./91-impl-plan.md)
- ↔ Testing: [72-testing-strategy.md § Benchmarks](./72-testing-strategy.md)
