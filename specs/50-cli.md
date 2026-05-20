# 50 — CLI (`tfparser`)

Status: draft v1 · Owner: tfparser · Depends on: [20-parquet-exporter.md](./20-parquet-exporter.md), [16-provider-resolver.md](./16-provider-resolver.md)

## 1. Purpose

The user-visible surface for M0. Every capability is a thin wrapper over `tfparser-core`. No business logic lives in the CLI — its sole job is argument parsing, configuration assembly, progress reporting, and exit-code mapping.

## 2. Commands

```
tfparser parse <root>          # main: parse and export
tfparser inspect <root>         # diagnostics-only: list components/modules/diagnostics, no Parquet output
tfparser schema                 # print the canonical Arrow schema as JSON
tfparser version                # build info: version, hcl-edit version, parquet version
```

### 2.1 `tfparser parse`

```
tfparser parse <root> [OPTIONS]

ARGS:
  <root>                          Workspace root directory (the tree containing your TF files)

OPTIONS:
  -o, --out <DIR>                 Output directory                                 [default: ./tfparser-out]
      --overwrite                 Overwrite existing files in --out
      --environment <ENV>         Bind var.environment (and TF_VAR_environment) globally
      --all-environments          Run once per discovered environment and write per-env subdirs
      --profile-map <PATH>        YAML file mapping AWS profile → account
      --aws-config <PATH>         Read AWS shared config for profile→account     [default: ~/.aws/config if it exists]
      --no-profile-map            Skip AWS profile resolution entirely (account_id columns empty)
      --tables <LIST>             Subset of tables: resources,dependencies,components,modules  [default: resources]
      --exclude <GLOB>...         Additional walk excludes (multi)
      --include-gitignored        Walk into .gitignored directories
      --follow-symlinks           Follow symlinks (off by default; security)
      --env-mode <MODE>           passthrough | strict | mock                      [default: strict]
      --allowed-env <NAME>...     Names visible to get_env when --env-mode strict
      --threads <N>               Parallelism for HCL parsing                       [default: detect]
      --max-file-size <BYTES>     Per-file cap                                     [default: 4M]
      --max-include-depth <N>     Terragrunt include depth                         [default: 32]
      --parsed-at <RFC3339>       Pin timestamp (for reproducible builds)
      --config <PATH>             Read defaults from a tfparser.toml file
      --json-diagnostics          Emit diagnostics as JSONL on stderr
  -v, --verbose                   -v=info, -vv=debug, -vvv=trace
      --no-color                  Disable ANSI color
  -h, --help
      --version
```

Argument parsing: `clap` 4.x with derive macros. Subcommand dispatch is a small `match`.

### 2.2 `tfparser inspect`

Same flags as `parse` minus output ones. Prints to stdout:
- Counts: components, modules, files, resources (pre-expansion).
- Per-environment summary.
- Top-10 unresolvable expression sources (e.g. `var.foo`).
- Profile-resolution coverage.

Exit code 0 even if diagnostics are present; useful for CI sanity checks where you want a structured summary without a full Parquet write.

### 2.3 `tfparser schema`

Prints the canonical Arrow schema in DuckDB-compatible JSON. Sourced from the same in-code definition the exporter uses (no duplication). Useful for downstream tools and for `git diff`-ing the schema across releases.

## 3. Configuration file

Optional `tfparser.toml` in the workspace root (or via `--config`):

```toml
[parse]
default_environment = "staging"
out                 = "./tfparser-out"
tables              = ["resources", "components"]

[parse.evaluator]
env_mode            = "strict"
allowed_env         = ["TF_VAR_environment", "AWS_REGION"]
max_include_depth   = 32

[parse.profile_map]
source = "aws-config"          # or "file" / "none"
path   = "~/.aws/config"
```

Resolution precedence (highest wins): CLI flag > `tfparser.toml` > built-in defaults. Per CLAUDE.md § Async & Concurrency, configuration is a `serde`-deserialised struct, validated via `validator`, then frozen behind an `Arc`.

`#[serde(deny_unknown_fields)]` on every config struct so typos fail fast.

## 4. Behaviour

### 4.1 Wiring

```text
load_config(args, file)
  → DiscoveryOptions, LoaderLimits, EvalContext, TgContext, ProviderContext, ExportOptions
discoverer.discover(...)
  → Discovered
loader.load(...)         (rayon over components)
  → Vec<RawComponent>
terragrunt.resolve(...)  (rayon)
  → Vec<TerragruntConfig>
evaluator.evaluate(...)  (rayon)
  → Vec<EvaluatedComponent>
graph_builder.build(...)
  → Workspace
provider.resolve(...)
  → Workspace (filled)
exporter.export(...)
  → ExportReport
print_summary(report, diagnostics)
```

Each phase is timed (`tracing` span); a one-line per-phase summary is printed at `-v` and a JSON timing report is appended to the manifest.

### 4.2 Progress

Use [`indicatif`](https://crates.io/crates/indicatif) for two progress bars: discovery (files counted) and parse (components processed). Bars off when stdout is not a TTY or `--no-color` is set.

### 4.3 Exit codes

| Code | Meaning                                                   |
| ---- | --------------------------------------------------------- |
| 0    | Success (may have non-fatal diagnostics)                  |
| 1    | Generic failure (parser bug, panic — should never happen) |
| 2    | Configuration error (bad flags / config file / inputs)    |
| 3    | Discovery error (root missing / path escape)              |
| 4    | Loader error (limit exceeded, unrecoverable)              |
| 5    | Terragrunt error (cycle / path escape)                    |
| 6    | Provider resolution error (only in `--strict-profiles`)   |
| 7    | Export error (disk full, output exists)                   |
| 8    | Diagnostics present and `--fail-on-diagnostics` set       |

This mapping is part of the user contract — captured here, mirrored in the CLI integration tests.

### 4.4 Diagnostics

Default: human-readable, ANSI-coloured, one line per diagnostic (severity, file:line, message). Pager-friendly.

With `--json-diagnostics`: JSON Lines on stderr; same shape as `Diagnostic` IR; consumable by editors / CI.

## 5. Output examples

```
$ tfparser parse ./terraform --environment staging
✓ discovery     247 components, 58 modules, 4612 files     (220 ms)
✓ load          12 487 blocks across 247 components         (940 ms)
✓ terragrunt    1 root.hcl, 9 environments cascaded         (180 ms)
✓ evaluate      91 % expressions resolved                   (1.1 s)
✓ graph         40 218 resources after expansion            (260 ms)
✓ provider      8/10 accounts mapped from ~/.aws/config     (90 ms)
✓ export        resources.parquet (12.4 MB, 40 218 rows)    (520 ms)

3 diagnostics (run with -v for details)
total: 3.4 s
```

```
$ tfparser parse ./terraform --tables resources,dependencies,components,modules
... [as above plus secondary tables]
```

## 6. CLAUDE.md anchoring

- **Errors**: each subcommand returns `anyhow::Result<()>`. The library returns `tfparser_core::Result<_>` with `thiserror`. The CLI is the *only* layer using `anyhow`.
- **Validation**: every CLI string passes a validator before being passed to the library. Glob patterns: `globset` compiled at startup, fail fast.
- **Logging**: `tracing-subscriber` initialised once; JSON formatter when `--json-diagnostics`, human-friendly otherwise. `RUST_LOG` honoured.
- **Documentation**: every CLI flag has a `///` doc that becomes `--help` text. Examples in the README and `tfparser parse --help-long`.
- **Performance**: no `tokio` in the CLI for M0. Synchronous + rayon, simpler and faster.

## 7. Out of scope for M0

- `tfparser serve` (HTTP API) — deferred to M6+ if needed.
- `tfparser watch` (incremental re-parse) — deferred.
- `tfparser diff <old>.parquet <new>.parquet` — deferred; users use DuckDB.
- Shell completions — added as a polish task in Phase 10.

## 8. Cross-references

- ← Depends on: [20-parquet-exporter.md](./20-parquet-exporter.md), [16-provider-resolver.md](./16-provider-resolver.md), [11-discovery.md](./11-discovery.md)
- ↔ Roadmap: [90-roadmap.md § M0](./90-roadmap.md), [91-impl-plan.md § Phase 4](./91-impl-plan.md)
- ↔ Security: [70-security.md § CLI surface](./70-security.md)
