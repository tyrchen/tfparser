# 70 — Security & Threat Model

Status: draft v1 · Owner: tfparser-core · Depends on: [11-discovery.md](./11-discovery.md), [12-hcl-loader.md](./12-hcl-loader.md), [13-evaluator.md](./13-evaluator.md), [14-terragrunt.md](./14-terragrunt.md)

## 1. Threat model

The parser ingests **untrusted user files** (HCL, tfvars, terragrunt.hcl, profile YAML, AWS config). The "user" includes everyone who has ever committed to the target repo, plus anyone who can supply a CLI flag or env var. Even though the *operator* invoking `tfparser` is trusted, the inputs are not. Treat every byte off disk as hostile.

Three concrete actors to harden against:

| Actor | Capability | What they can attempt |
| ----- | ---------- | --------------------- |
| Repo committer | Crafts `.tf`/`.hcl` files in the workspace | Path-escape via `file()`/`templatefile()`/`include` chains; bomb the parser with depth/size; smuggle env-var reads; inject log/output. |
| Profile-map author | Crafts the `--profile-map` YAML | YAML bombs, oversize strings, malformed account IDs, billion-laughs. |
| CLI invoker | Sets `--exclude`, `--allowed-env`, paths, etc. | Regex-DoS via custom globs; pass paths outside the workspace. |

Operator integrity (the actual process running the parser) is **assumed**. We are not defending against attackers with read access to the parser's address space.

## 2. Trust boundaries

```
[ User CLI flags + env ]   ── parsed via clap, validated, frozen into Arc<Config>
              │
              ▼
[ Workspace filesystem ]   ── discovered, classified, then read file-by-file with caps
              │
              ▼
[ HCL contents ]           ── parsed with hcl-edit; lowered; references stay Unresolved
              │
              ▼
[ Profile map / AWS config ] ── parsed with strict schemas, allowlists on every field
              │
              ▼
[ Workspace IR ]           ── trusted from here on; exporter consumes
              │
              ▼
[ Parquet output ]         ── written atomically (.partial → rename)
```

A value is **trusted** only after it has crossed a validation barrier (newtype with private constructor, validator-checked struct, sandboxed function). Internal code passes already-trusted values around; revalidation is a code smell.

## 3. Invariants (binding)

### 3.1 Path safety

- **P1**: Every file we read is *canonicalised* before open. Reads outside `workspace_root` (or its allowed sister roots: `~/.aws/config`, the `--profile-map` path) are refused.
- **P2**: Symlinks **off by default** (`--follow-symlinks` opts in, still subject to canonicalise-and-check).
- **P3**: NUL bytes in any path → reject immediately.
- **P4**: `..` allowed in *source* (HCL `source = "../../mod"` is a normal pattern), forbidden in the *resolved* result.
- **P5**: `file()`, `templatefile()`, `fileset()`, `find_in_parent_folders`, `read_terragrunt_config` all share **one** sandbox helper. No bypass via "I'll resolve paths myself."

### 3.2 Resource exhaustion

| Limit | Default | Where enforced |
| ----- | ------- | -------------- |
| Per-file size | 4 MiB | [12-hcl-loader.md](./12-hcl-loader.md) |
| Files per workspace | 200 000 | [11-discovery.md](./11-discovery.md) |
| Walk depth | 16 | [11-discovery.md](./11-discovery.md) |
| Blocks per file | 10 000 | [12-hcl-loader.md](./12-hcl-loader.md) |
| Attribute depth | 64 | [12-hcl-loader.md](./12-hcl-loader.md) |
| Template parts | 1024 | [12-hcl-loader.md](./12-hcl-loader.md) |
| Include depth | 32 | [14-terragrunt.md](./14-terragrunt.md) |
| Evaluator iterations | 1 000 000 | [13-evaluator.md](./13-evaluator.md) |
| Function arg count | 64 | [13-evaluator.md](./13-evaluator.md) |
| List length | 100 000 | [13-evaluator.md](./13-evaluator.md) |
| Rendered string size | 1 MiB | [13-evaluator.md](./13-evaluator.md) |
| Expansion per resource (count/for_each) | 1024 | [15-resource-graph.md](./15-resource-graph.md) |
| Profile-map file size | 256 KiB | [16-provider-resolver.md](./16-provider-resolver.md) |
| AWS config file size | 256 KiB | [16-provider-resolver.md](./16-provider-resolver.md) |

Every limit is configurable via `tfparser.toml` and CLI flags; defaults are the spec. Breaching any returns `Err(Error::Limit { kind, limit, observed })` with a clear message — never a panic.

### 3.3 Env / secret hygiene

- `get_env(name)` is **disabled by default** (mode `strict`, empty allowlist). User must opt in with `--allowed-env NAME` or set mode `passthrough`.
- `--env-mode passthrough` triggers a startup warning: "all process env vars are visible to HCL evaluator."
- `--env-mode mock` always returns the default arg or `""`.
- Profile map / AWS config: we read but **do not log** values. The map is held behind `secrecy::SecretBox<ProfileMap>` … *however*, profile→account is not really secret (account IDs are routinely shared); the secrecy wrapper buys us defence in depth at low cost. **TBD**: if `secrecy` adds friction without benefit, drop it. Decided in [99-key-decisions.md](./99-key-decisions.md) D11.
- Tracing: never log raw HCL bodies at `INFO`. `Debug` impls redact `provider_block.raw`, `repo_vars`, `attributes` past a certain depth.

### 3.4 Regex DoS

- We use the `regex` crate exclusively (linear-time guarantee). No `fancy-regex`, `pcre`, `onig`.
- User-supplied patterns (`--exclude`, `module_glob`) are compiled via `globset`, which itself uses `regex` under the hood with a hard length cap (we apply `pattern.len() <= 256` before passing to `globset`).
- Any pattern from HCL evaluator funcs (e.g. `regex()`/`regexall()`) compiles with `RegexBuilder::size_limit(1 << 20)` and `dfa_size_limit(1 << 20)`.

### 3.5 Decompression / external bytes

We do **not** decompress in M0–M5. If a future feature reads `.tar.gz` modules, it must wrap the decoder in a byte-counting reader with a hard cap (CLAUDE.md § Resource Limits & DoS).

### 3.6 Output safety

- Parquet output written via `.partial` → fsync → rename; no half-files.
- Output directory must exist (we don't recursively `mkdir` arbitrary paths — only `--out` itself).
- Refuses to overwrite without `--overwrite`.

## 4. Validation at boundaries (concrete)

| Input | Validator |
| ----- | --------- |
| `Address::new(s)` | length ≤ 1024 bytes, charset `[A-Za-z0-9_./\-\[\]"]`, non-empty, balanced brackets/quotes. |
| `AccountId::new(s)` | exactly 12 ASCII digits. |
| `Region::new(s)` | `^[a-z0-9-]{1,32}$`. |
| `ProfileMap` (YAML) | `validator::Validate` on each entry; `#[serde(deny_unknown_fields)]`. |
| `tfparser.toml` | same. |
| Glob patterns (`--exclude`, `module_glob`) | `len() ≤ 256`, compile must succeed. |
| `--allowed-env NAME` | `^[A-Z_][A-Z0-9_]{0,63}$`. |
| HCL file paths | canonicalised, descendant-of-root. |

Every newtype's constructor is **private**; only `try_from` / `new` exists, returning `Result`. Per CLAUDE.md § Input Validation — "Newtype every domain primitive."

## 5. Logging & redaction

- All public APIs that take "potentially-sensitive" inputs have manual `Debug` impls that redact (e.g. `ProfileEntry: { account_id: <redacted>, … }`).
- Tracing fields: never log full HCL bodies. Log `Span` (file:line) and counts.
- Diagnostics surface field *names*, not field *values*, unless the field is a known-safe identifier (resource type, alias name).

Unit test asserts: `format!("{:?}", profile_entry)` does not contain the account ID.

## 6. Fuzzing

`cargo-fuzz` harnesses:

1. `fuzz_hcl_loader` — `HclEditLoader::parse_bytes(data)` against random `&[u8]`. Asserts: no panic, no OOM beyond cap, runtime ≤ 2 s per input.
2. `fuzz_evaluator` — random `Expression` (via `arbitrary::Arbitrary`) evaluated against a fixed minimal context. Asserts: no panic, iteration cap respected.
3. `fuzz_terragrunt` — random terragrunt-shaped bodies. Asserts: cycle detection works for adversarial includes.

CI runs each ≥ 10 minutes per PR; nightly job runs each ≥ 6 hours.

## 7. Out of scope (explicit)

- **Cryptographic auth** — there is no auth (single-binary CLI). If the future server needs auth, that's a separate spec.
- **Code execution** — Terragrunt's `dependency.x.outputs.y` running terragrunt → terraform output → arbitrary providers is exactly the loop we're *avoiding*. The parser does not exec anything.
- **Compile-time sandbox** — we depend on Cargo deps; auditing those is `cargo audit`'s job, not ours.

## 8. Cross-references

- ← Anchored in: [11-discovery.md](./11-discovery.md), [12-hcl-loader.md](./12-hcl-loader.md), [13-evaluator.md](./13-evaluator.md), [14-terragrunt.md](./14-terragrunt.md), [15-resource-graph.md](./15-resource-graph.md), [16-provider-resolver.md](./16-provider-resolver.md), [20-parquet-exporter.md](./20-parquet-exporter.md)
- ↔ CLAUDE.md § Safety & Security — every subsection of CLAUDE.md is reflected here.
- ↔ Decisions: [99-key-decisions.md](./99-key-decisions.md) — D11 (secrecy wrapper trade-off)
