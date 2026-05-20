---
name: spec
description: Turn a feature idea or rough requirement into a complete, dependency-ordered spec set under ./specs, including PRD, design docs, roadmap, implementation plan, glossary, key decisions, and index updates. Use for non-trivial design, phasing, PRD, roadmap, or implementation-plan requests.
---

# Spec

Use this Codex skill when the user asks for design/specification work before
implementation. The canonical workflow is the Claude spec skill at
`.claude/skills/spec/SKILL.md`; follow it, with the Codex-specific adjustments
below.

## Codex Adjustments

- Read `AGENTS.md`, `CLAUDE.md`, `specs/index.md`, and relevant
  `docs/research/` memos before writing specs.
- Use local shell/file tools (`rg`, `sed`, `find`, `cargo` as needed) rather
  than Claude-only tool names.
- Keep the spec set right-sized. Do not create every canonical file unless the
  system complexity earns it.
- If the design depends on unvalidated prior art or performance assumptions,
  run the `research` skill first.

## Output Rules

- Specs live under `specs/`.
- Update `specs/index.md` for every spec addition, deletion, or rename.
- Numbered specs use build-order numbering already present in this repo.
- Roadmap (`90-roadmap.md`) is stakeholder-facing.
- Implementation plan (`91-impl-plan.md`) is engineer-facing and
  dependency-ordered.
- Key decisions belong in `99-key-decisions.md`.

## Quality Bar

Each component design must pin its purpose, public interface, invariants,
non-trivial behavior, tests or gates for those invariants, and cross-references
to dependent specs or research. Specs must not silently relax `CLAUDE.md`;
explicitly document any necessary deviation.
