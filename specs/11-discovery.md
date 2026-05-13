# 11 — Discovery (Workspace Walker)

Status: draft v1 · Owner: tfparser-core · Depends on: [10-data-model.md](./10-data-model.md)

## 1. Purpose

Walk the workspace root, classify directories into **components** vs **modules** vs **environments** vs **noise**, and produce an ordered list of `(path, kind)` pairs that the loader will then read. Filesystem-only — no HCL parsing yet, no expression evaluation.

This is the first slice of the pipeline that crosses the trust boundary: the user supplies a path, we walk it. Everything that follows trusts the discovery output is well-formed.

## 2. Interface

```rust
// crates/core/src/discovery/mod.rs
pub trait Discoverer: Send + Sync {
    fn discover(&self, root: &Path, opts: &DiscoveryOptions) -> Result<Discovered>;
}

pub struct FsDiscoverer;                       // default impl, walks the real fs

pub struct Discovered {
    pub root:          Arc<Path>,              // canonicalized
    pub components:    Vec<DiscoveredDir>,     // kind = Component
    pub modules:       Vec<DiscoveredDir>,     // kind = Module
    pub envs_dir:      Option<Arc<Path>>,      // workspace-level environments/ if found
    pub root_hcl:      Option<Arc<Path>>,      // Terragrunt root.hcl if found
    pub diagnostics:   Vec<Diagnostic>,        // unclassified dirs, broken symlinks, …
}

pub struct DiscoveredDir {
    pub path:   Arc<Path>,                     // relative to root
    pub kind:   DirKind,
    pub files:  Vec<DiscoveredFile>,           // .tf / .tfvars / .hcl / .json
    pub reason: ClassificationReason,          // why we chose this kind (audit trail)
}

pub enum DirKind { Component, Module, Environments, Files, Other }

pub struct DiscoveredFile {
    pub path:   Arc<Path>,                     // relative to root
    pub ext:    FileExt,                       // Tf | Tfvars | Hcl | TerragruntHcl | Json
    pub size:   u64,
}

pub struct DiscoveryOptions {
    pub follow_symlinks:     bool,             // default: false
    pub max_depth:           u32,               // default: 16
    pub exclude_globs:       Vec<Glob>,         // default: [".git/**", ".terraform/**", "**/.terragrunt-cache/**"]
    pub module_glob:         Vec<Glob>,         // dirs matching are pre-classified as Module
    pub component_marker:    ComponentMarker,   // see § 3.2
    pub max_file_size_bytes: u64,               // default: 8 MiB (per file)
    pub max_total_files:     u64,               // default: 200_000 (workspace-wide cap)
}
```

## 3. Behaviour

### 3.1 Walk

Use the [`ignore`](https://crates.io/crates/ignore) crate's `WalkBuilder` (same engine as `ripgrep`): respects `.gitignore`, supports custom excludes, parallel walk, and is the de facto standard. Override exclude defaults with `--include-gitignored`.

For each directory entry:
- Reject if path doesn't canonicalize *underneath* `root` (defence in depth against TOCTOU symlink escapes).
- Skip if its file size > `max_file_size_bytes`. Emit a `Diagnostic::FileTooLarge`.
- Stop walking deeper if `max_depth` exceeded.
- If the total file count exceeds `max_total_files`, abort with an explicit error.

### 3.2 Classification

A directory becomes a **component** if any of the following triggers (in priority order):

1. Contains a `terragrunt.hcl` whose body parses (shallowly) and contains an `include` block referencing a parent file. → `ClassificationReason::TerragruntInclude`
2. Contains any `.tf` file whose body declares a `terraform { backend "..." {} }` block. (We need a *shallow* HCL probe — see § 3.3 — not a full parse.) → `BackendBlock`.
3. Contains a `.tf` file declaring at least one `resource "..." "..." {}` or `data "..." "..." {}` block, AND the dir does *not* match any `module_glob`. → `HasResources`.

A directory becomes a **module** if any:
1. Path matches `module_glob` (defaults: `modules/**`, `modules-tf12/**`, `**/modules/*`, `**/modules-tf12/*`).
2. Discovered transitively via `module "x" { source = "./..." }` from a component (handled in the *resolution* phase later — see [15-resource-graph.md](./15-resource-graph.md)). Discovery seeds with the static heuristic; the resolver patches kinds in place.
3. Contains `.tf` declaring `variable {}` blocks but **no** `resource`/`data` blocks and **no** `backend` and **no** `terragrunt.hcl`.

A directory becomes **environments** if its name is `environments` and its parent is the workspace root.

Other dirs (`files`, `data`, README-only) → `Other`. Not parsed, but tracked for round-tripping.

If a dir is ambiguous (e.g. has both a `terragrunt.hcl` and a `modules/` glob match), the component classification wins and a `Diagnostic::Ambiguous` is emitted. The CLI surfaces this in `--verbose`.

### 3.3 Shallow HCL probe

We must not do a full `hcl-edit` parse during discovery — that's the loader's job and would double parse time. Instead, a regex-grade scan over the file bytes detects the presence of:

```text
^(\s*terraform\s*\{)
^(\s*resource\s+"[^"]+"\s+"[^"]+"\s*\{)
^(\s*data\s+"[^"]+"\s+"[^"]+"\s*\{)
^(\s*variable\s+"[^"]+"\s*\{)
^(\s*include\s+"[^"]*"\s*\{)
```

This is **not** a parser; it deliberately uses anchored line-start patterns. We use the [`regex`](https://crates.io/crates/regex) crate (linear-time guarantee per CLAUDE.md § Injection Prevention). A `regex::RegexSet` matches all patterns in one pass.

False positives (a string literal containing `resource "x" "y" {`) are rare and tolerated; loader re-classifies definitively.

### 3.4 Ordering

`Discovered.components` is sorted by relative path **byte-lexicographically** for reproducibility. This makes `tfparser parse` produce identical output bytes across runs (modulo `parsed_at`), which matters for CI diffing.

### 3.5 Parallelism

Discovery is I/O-bound on cold caches. Use `ignore::WalkBuilder::threads(N)` where `N = num_cpus().min(8)`. Cap because beyond 8, fs metadata calls saturate on macOS APFS / Linux ext4.

## 4. Invariants

- **I-DISC-1**: Every `DiscoveredDir.path` is **relative** to `Discovered.root`. Absolute paths never escape `Discovered`.
- **I-DISC-2**: `Discovered.root` is canonicalized (`Path::canonicalize`). No `..`, no symlinks remaining.
- **I-DISC-3**: For every `DiscoveredFile.path`, the file's canonical path begins with `Discovered.root.as_os_str()`. Enforced — paths outside the root are dropped with a `Diagnostic::PathEscape`.
- **I-DISC-4**: `Discovered` is deterministic given identical filesystem state and `DiscoveryOptions`.
- **I-DISC-5**: Discovery is **read-only** — no writes, no `chmod`, no `chown`.
- **I-DISC-6**: Discovery respects the resource caps (`max_file_size_bytes`, `max_total_files`, `max_depth`); breaching any returns `Err(Error::Limit(…))` rather than continuing.

## 5. Error model

```rust
#[derive(thiserror::Error, Debug)]
pub enum DiscoveryError {
    #[error("workspace root not found: {0}")]
    RootMissing(Arc<Path>),

    #[error("path escape: {0} resolves outside the workspace root")]
    PathEscape(Arc<Path>),

    #[error("limit exceeded: {kind} (limit = {limit}, observed = {observed})")]
    Limit { kind: LimitKind, limit: u64, observed: u64 },

    #[error("i/o error walking {path}: {source}")]
    Io { path: Arc<Path>, #[source] source: io::Error },
}
```

`Diagnostic` (non-fatal) is separate and accumulated in `Discovered.diagnostics`. Per CLAUDE.md § Error Handling — `thiserror`, `#[source]` chains, `Result<T>` everywhere.

## 6. Security

Discovery is the *first* boundary we cross. From [70-security.md](./70-security.md):
- All user inputs (root, exclude globs) are validated at entry: root must be a directory; exclude globs are compiled once via `globset::GlobSetBuilder` (length-bounded patterns).
- Symlinks **off by default**. With `--follow-symlinks`, every resolved path is re-checked for descendant-of-root.
- NUL bytes in paths reject immediately.
- File size and total file caps prevent zip-bomb-style runaway parses.

## 7. Testing

- **Unit**: fixture trees with every classification edge case (empty dir, dir with only `README`, `terragrunt.hcl` with no `include`, dir with only `variable` blocks, …).
- **Property** (`proptest`): generate random directory shapes with known oracle classifications; assert.
- **Snapshot**: full reference-scale fixture under `crates/core/tests/fixtures/large-monorepo/` — a ~30-component synthetic monorepo representative of real-world Terragrunt shape. Snapshot the `Discovered` output.
- **Security**: symlink-to-outside-root, NUL-in-path, oversized file, recursion bomb — each test ensures we either reject or cap.

Per CLAUDE.md § Testing: `rstest` for parameterised inputs; `proptest` for invariants; `#[ignore]` on the full reference-scale integration test (run in CI only).

## 8. Cross-references

- ← Depends on: [10-data-model.md](./10-data-model.md)
- → Consumed by: [12-hcl-loader.md](./12-hcl-loader.md), [14-terragrunt.md](./14-terragrunt.md), [15-resource-graph.md](./15-resource-graph.md)
- ↔ Research: [terraform-repo-shapes.md](../docs/research/terraform-repo-shapes.md)
- ↔ Security: [70-security.md](./70-security.md)
