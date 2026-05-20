# tfparser

This repository is Codex-ready. `CLAUDE.md` remains the detailed project
contract for all coding agents; Codex must read and follow it as binding
project guidance. This file exists so Codex discovers the project rules
without needing Claude-specific entry points.

IMPORTANT: Never enter plan mode automatically.

## Start Here

1. Read `CLAUDE.md` before making changes.
2. Check `git status --short` and preserve user changes.
3. Use `rg` / `rg --files` for repository search.
4. Prefer existing Makefile targets over ad hoc command sequences.
5. For new automation, add a `Makefile` target instead of a shell script.

## Codex Skills

Project-local Codex skills live under `.codex/skills/`:

- `research` — prior-art studies and spike memos under `docs/research/`.
- `spec` — feature/spec design under `specs/`.
- `impl` — implementation of dependency-ordered phases from
  `specs/91-impl-plan.md`.

When a user request matches one of those workflows, read the matching
`.codex/skills/<name>/SKILL.md` before proceeding.

## Rust Workflow

Use Rust 2024 and the pinned toolchain in `rust-toolchain.toml`. The expected
quality gates are:

```bash
cargo build --workspace --all-targets
cargo test --workspace --all-targets
cargo +nightly fmt -- --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Use stricter Clippy for boundary modules and external-input paths:

```bash
cargo clippy --workspace --all-targets -- \
  -D warnings -W clippy::pedantic \
  -W clippy::unwrap_used -W clippy::expect_used \
  -W clippy::indexing_slicing -W clippy::panic
```

Run `cargo deny check` and `cargo audit` when dependencies change. Never run
`cargo clean` without explicit user permission.

## Documentation

- Specs go under `specs/`; update `specs/index.md`.
- Docs go under `docs/`; update `docs/index.md`.
- Research goes under `docs/research/`; update `docs/research/index.md` when
  it exists, otherwise update `docs/index.md`.
- Name spec files as `{feature-name}-{type}.md` when adding non-numbered
  auxiliary specs, following `CLAUDE.md`.

## Non-Negotiables

- No `unsafe` in project code.
- No `unwrap()`, `expect()`, `todo!()`, `unimplemented!()`, or reachable
  panics from external input.
- No dead-code suppression to hide unused work; remove dead code.
- Handle errors with project error types, `thiserror` for libraries, and
  `anyhow` for applications.
- Validate hostile input at boundaries and encode invariants in types.
- Keep changes scoped to the request and existing architecture.
