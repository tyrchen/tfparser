---
name: research
description: Vendor reference repos as git submodules under ./vendors and produce deep research memos under ./docs/research covering architecture, design, key data structures, and load-bearing algorithms. Use whenever the user asks to research, study prior art, spike an assumption, compare upstream repos, or learn from pasted GitHub URLs before design or implementation.
---

# Research

Use this Codex skill for prior-art research and assumption-retirement work.
The canonical workflow is the Claude research skill at
`.claude/skills/research/SKILL.md`; follow it, with the Codex-specific
adjustments below.

## Codex Adjustments

- Treat `AGENTS.md` and `CLAUDE.md` as binding project guidance.
- Use `rg`, `rg --files`, `sed`, `cargo doc`, `cargo expand`, and focused file
  reads instead of Claude-only `Read` / `Grep` / `Explore` tool names.
- Use Codex subagents only when the user explicitly asks for delegated or
  parallel agent work. Otherwise perform the research locally.
- Place memos under `docs/research/`.
- Update `docs/research/index.md` if present; otherwise update `docs/index.md`.

## Required Output

Produce exactly one memo for each research topic:

- `spike-<slug>.md` for a single time-boxed question with a runnable artifact.
- `study-<slug>.md` for a deep dive into vendored source.
- `survey-<slug>.md` for web/docs research where vendoring is not warranted.

Each memo must state the question, evidence, decision, risks, and cited source
locations. For vendored repositories, pin and record the exact commit SHA.

## Quality Bar

A memo is done when another engineer can answer the question without opening
the upstream repo and can verify every behavioral claim through cited file
paths, line numbers, command output, or official source links.
