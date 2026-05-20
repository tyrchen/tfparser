---
name: impl
description: Implement one phase of ./specs/91-impl-plan.md end-to-end, matching the specs and project quality bars, then review the diff and fix valid findings before declaring done. Use when the user asks to build a phase, milestone, next phase, or roadmap slice.
---

# Impl

Use this Codex skill for phase-shaped implementation from
`specs/91-impl-plan.md`. The canonical workflow is the Claude impl skill at
`.claude/skills/impl/SKILL.md`; follow it, with the Codex-specific adjustments
below.

## Codex Adjustments

- Read `AGENTS.md`, `CLAUDE.md`, `specs/91-impl-plan.md`, cited specs, and
  relevant `docs/research/` memos before editing.
- Use Codex's normal task tracking and local review. Use subagents only when
  the user explicitly asks for delegated or parallel agent work.
- Preserve user changes. Check `git status --short` before edits and avoid
  broad staging commands.
- Keep implementation scoped to the requested phase. Defer out-of-phase
  findings to `specs/93-improvements-review.md`.

## Workflow

1. Bind the exact phase or milestone from `specs/91-impl-plan.md`.
2. Read every cited spec section and relevant research memo.
3. Identify exit criteria before writing code.
4. Implement tasks in dependency order.
5. Run local tests after meaningful changes and all standard gates before done.
6. Review the diff against the specs and `CLAUDE.md`; fix valid in-phase
   findings.
7. Report changed files, quality gates, residual risk, and the next unlocked
   phase.

## Required Gates

```bash
cargo build --workspace --all-targets
cargo test --workspace --all-targets
cargo +nightly fmt -- --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

For boundary modules and external-input paths, also run the stricter Clippy
command from `AGENTS.md`. Run `cargo deny check` and `cargo audit` if
dependencies change.

## Quality Bar

No TODOs, placeholders, dead-code suppressions, `unsafe`, `unwrap`, or
`expect` in production paths. Public surfaces need docs and tests. Spec
adherence is binary: if implementation and spec disagree, stop and reconcile
the mismatch in writing before continuing.
