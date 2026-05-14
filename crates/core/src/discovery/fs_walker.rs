//! [`FsDiscoverer`]: walks the real filesystem with `ignore::WalkBuilder`,
//! groups files by directory, runs the shallow probe + classifier from
//! [`super::classifier`], and produces a deterministic [`Discovered`].

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use ignore::WalkBuilder;
use tracing::{debug, instrument, warn};

use super::{
    Discovered, DiscoveredDir, DiscoveredFile, Discoverer, DiscoveryOptions,
    classifier::{FileSignals, classify, probe_file},
    types::DirKind,
};
use crate::{
    Diagnostic, Error, LimitKind, Result, Severity,
    diagnostic::Diagnostic as Diag,
    ir::FileExt,
    util::paths::{self, SymlinkPolicy},
};

/// Default [`Discoverer`] implementation.
///
/// Stateless and `Send + Sync`; cheap to construct (no fields). The
/// per-walk caps and excludes are supplied via [`DiscoveryOptions`].
#[derive(Clone, Copy, Debug, Default)]
pub struct FsDiscoverer;

impl FsDiscoverer {
    /// Construct an [`FsDiscoverer`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Discoverer for FsDiscoverer {
    #[instrument(level = "debug", skip(self, opts), fields(root = %root.display()))]
    fn discover(&self, root: &Path, opts: &DiscoveryOptions) -> Result<Discovered> {
        run_discovery(root, opts)
    }
}

/// Internal entry point — separates the trait dispatch from the actual logic
/// so we can call it from tests without going through the trait.
fn run_discovery(root: &Path, opts: &DiscoveryOptions) -> Result<Discovered> {
    let canonical_root = canonicalize_root(root, opts)?;
    let WalkOutput {
        groups,
        seen_dirs,
        mut diagnostics,
    } = walk_workspace(&canonical_root, opts)?;
    let mut classified =
        classify_dirs(&canonical_root, opts, &groups, &seen_dirs, &mut diagnostics);
    classified
        .components
        .sort_by(|a, b| a.path.as_os_str().cmp(b.path.as_os_str()));
    classified
        .modules
        .sort_by(|a, b| a.path.as_os_str().cmp(b.path.as_os_str()));
    let root_hcl = find_root_hcl(&canonical_root);
    Ok(Discovered {
        root: Arc::from(canonical_root.as_path()),
        components: classified.components,
        modules: classified.modules,
        envs_dir: classified.envs_dir,
        root_hcl,
        diagnostics,
    })
}

struct WalkOutput {
    groups: BTreeMap<PathBuf, Vec<DiscoveredFile>>,
    seen_dirs: BTreeSet<PathBuf>,
    diagnostics: Vec<Diagnostic>,
}

fn walk_workspace(canonical_root: &Path, opts: &DiscoveryOptions) -> Result<WalkOutput> {
    let mut state = WalkState::new();
    let walker = build_walker(canonical_root, opts);
    for result in walker {
        match result {
            Ok(entry) => process_walk_entry(canonical_root, opts, &entry, &mut state)?,
            Err(err) => state.diagnostics.push(walk_error_to_diagnostic(&err)),
        }
    }
    Ok(WalkOutput {
        groups: state.groups,
        seen_dirs: state.seen_dirs,
        diagnostics: state.diagnostics,
    })
}

struct WalkState {
    groups: BTreeMap<PathBuf, Vec<DiscoveredFile>>,
    seen_dirs: BTreeSet<PathBuf>,
    diagnostics: Vec<Diagnostic>,
    total_files: u64,
}

impl WalkState {
    fn new() -> Self {
        let mut seen_dirs = BTreeSet::new();
        seen_dirs.insert(PathBuf::new());
        Self {
            groups: BTreeMap::new(),
            seen_dirs,
            diagnostics: Vec::new(),
            total_files: 0,
        }
    }
}

fn process_walk_entry(
    canonical_root: &Path,
    opts: &DiscoveryOptions,
    entry: &ignore::DirEntry,
    state: &mut WalkState,
) -> Result<()> {
    if entry.depth() == 0 {
        return Ok(());
    }
    let entry_path = entry.path().to_path_buf();
    let Ok(rel) = entry_path.strip_prefix(canonical_root) else {
        state.diagnostics.push(Diag::new(
            Severity::Warn,
            "TF1101",
            format!(
                "dropping entry outside workspace root: {}",
                entry_path.display()
            ),
        ));
        return Ok(());
    };
    let rel_path = rel.to_path_buf();
    if opts.exclude_globs.is_match(&rel_path) {
        return Ok(());
    }
    let Some(file_type) = entry.file_type() else {
        return Ok(());
    };
    if file_type.is_dir() {
        state.seen_dirs.insert(rel_path);
        return Ok(());
    }
    if !file_type.is_file() {
        if file_type.is_symlink() {
            state.diagnostics.push(Diag::new(
                Severity::Trace,
                "TF1110",
                format!("skipped symlink: {}", rel_path.display()),
            ));
        }
        return Ok(());
    }
    let metadata = match entry.metadata() {
        Ok(m) => m,
        Err(err) => {
            state.diagnostics.push(Diag::new(
                Severity::Warn,
                "TF1102",
                format!("metadata error for {}: {err}", rel_path.display()),
            ));
            return Ok(());
        }
    };
    let size = metadata.len();
    if size > opts.max_file_size_bytes {
        state.diagnostics.push(Diag::limit(
            LimitKind::FileSize,
            "TF1103",
            format!(
                "file exceeds size limit and was skipped: {} ({size} > {})",
                rel_path.display(),
                opts.max_file_size_bytes
            ),
        ));
        return Ok(());
    }
    let Some(ext) = FileExt::classify(&rel_path) else {
        return Ok(());
    };
    state.total_files = state.total_files.checked_add(1).ok_or(Error::Limit {
        kind: LimitKind::TotalFiles,
        observed: u64::MAX,
        limit: opts.max_total_files,
    })?;
    if state.total_files > opts.max_total_files {
        return Err(Error::Limit {
            kind: LimitKind::TotalFiles,
            observed: state.total_files,
            limit: opts.max_total_files,
        });
    }
    let parent_rel = rel_path
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf);
    state.seen_dirs.insert(parent_rel.clone());
    let file = DiscoveredFile {
        path: Arc::from(rel_path.as_path()),
        ext,
        size,
    };
    state.groups.entry(parent_rel).or_default().push(file);
    Ok(())
}

struct Classified {
    components: Vec<DiscoveredDir>,
    modules: Vec<DiscoveredDir>,
    envs_dir: Option<Arc<Path>>,
}

fn classify_dirs(
    canonical_root: &Path,
    opts: &DiscoveryOptions,
    groups: &BTreeMap<PathBuf, Vec<DiscoveredFile>>,
    seen_dirs: &BTreeSet<PathBuf>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Classified {
    let mut components: Vec<DiscoveredDir> = Vec::new();
    let mut modules: Vec<DiscoveredDir> = Vec::new();
    let mut envs_dir: Option<Arc<Path>> = None;
    for rel_dir in seen_dirs {
        let files = groups.get(rel_dir).cloned().unwrap_or_default();
        let signals = aggregate_signals(canonical_root, &files, opts.max_file_size_bytes);
        let is_workspace_envs_dir = is_workspace_environments_dir(rel_dir);
        let (kind, reason, ambiguous) =
            classify(rel_dir, signals, &files, opts, is_workspace_envs_dir);
        if ambiguous {
            diagnostics.push(Diag::new(
                Severity::Info,
                "TF1120",
                format!(
                    "directory is ambiguous (matches both component and module heuristics); \
                     component classification kept: {}",
                    rel_dir.display()
                ),
            ));
        }
        let dir_arc: Arc<Path> = Arc::from(rel_dir.as_path());
        match kind {
            DirKind::Component => {
                components.push(DiscoveredDir::new(dir_arc, kind, reason, files));
            }
            DirKind::Module => {
                modules.push(DiscoveredDir::new(dir_arc, kind, reason, files));
            }
            DirKind::Environments => {
                envs_dir = Some(dir_arc);
            }
            DirKind::Files | DirKind::Other => {
                debug!(?rel_dir, ?kind, "discovery: non-component dir");
            }
        }
    }
    Classified {
        components,
        modules,
        envs_dir,
    }
}

fn find_root_hcl(canonical_root: &Path) -> Option<Arc<Path>> {
    for candidate_rel in [Path::new("root.hcl"), Path::new("terraform/root.hcl")] {
        if canonical_root.join(candidate_rel).is_file() {
            return Some(Arc::from(candidate_rel));
        }
    }
    None
}

fn canonicalize_root(root: &Path, opts: &DiscoveryOptions) -> Result<PathBuf> {
    let policy = if opts.follow_symlinks {
        SymlinkPolicy::Follow
    } else {
        SymlinkPolicy::Reject
    };
    paths::reject_nul(root).map_err(|_| Error::Io {
        path: root.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL"),
    })?;

    if !root.exists() {
        return Err(Error::Io {
            path: root.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "workspace root not found"),
        });
    }
    let canonical = std::fs::canonicalize(root).map_err(|source| Error::Io {
        path: root.to_path_buf(),
        source,
    })?;

    // Final safety — running through the same canonicalizer the rest of the
    // pipeline uses ensures the helpers agree on what "inside" means.
    let resolved =
        paths::canonicalize_inside(&canonical, &canonical, policy).map_err(|err| match err {
            paths::PathSafetyError::Io { path, source } => Error::Io { path, source },
            other => Error::Io {
                path: canonical.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    other.to_string(),
                ),
            },
        })?;
    Ok(resolved)
}

fn build_walker(root: &Path, opts: &DiscoveryOptions) -> ignore::Walk {
    let mut builder = WalkBuilder::new(root);
    builder
        .follow_links(opts.follow_symlinks)
        .max_depth(Some(opts.max_depth as usize))
        // We do NOT call WalkBuilder::max_filesize because that drops the
        // entry silently. Discovery's contract is to *surface* over-cap
        // files as diagnostics so the operator knows what was skipped, so
        // the size check happens in the consumer loop below instead.
        .standard_filters(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .require_git(false)
        .hidden(true);

    // The `ignore` crate's WalkBuilder takes overrides (gitignore-style)
    // but not a `GlobSet` directly. The exclude_globs from
    // `DiscoveryOptions` are applied per-entry in the consumer loop.
    builder.build()
}

/// Probe every HCL-shaped file in `files`, capping per-file work at
/// `max_file_bytes` (so a 4 MiB `.tf` does not bloat discovery time).
fn aggregate_signals(root: &Path, files: &[DiscoveredFile], max_file_bytes: u64) -> FileSignals {
    let mut signals = FileSignals::default();
    for file in files {
        if !file.ext.is_hcl() {
            continue;
        }
        if file.size > max_file_bytes {
            // Already surfaced as a Diagnostic in the walk loop; skip the
            // probe to avoid re-tripping the cap.
            continue;
        }
        let abs = root.join(&*file.path);
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(err) => {
                warn!(path = %abs.display(), error = %err, "discovery: probe read failed");
                continue;
            }
        };
        signals.merge(probe_file(&bytes));
    }
    signals
}

/// `<workspace_root>/environments` (no deeper). Spec § 3.2.
fn is_workspace_environments_dir(rel: &Path) -> bool {
    let mut comps = rel.components();
    let first = comps.next();
    let second = comps.next();
    matches!(first, Some(c) if c.as_os_str() == "environments") && second.is_none()
}

fn walk_error_to_diagnostic(err: &ignore::Error) -> Diagnostic {
    Diag::new(Severity::Warn, "TF1100", format!("walk error: {err}"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use super::{super::classifier::ClassificationReason, *};
    use crate::ir::FileExt;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn test_should_discover_single_component() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "services/api/main.tf",
            "resource \"aws_iam_role\" \"r\" {\n  name = \"x\"\n}\n",
        );
        let opts = DiscoveryOptions::defaults();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        assert_eq!(d.components.len(), 1, "{d:?}");
        assert_eq!(&*d.components[0].path, Path::new("services/api"));
        assert_eq!(d.components[0].reason, ClassificationReason::HasResources);
        assert!(d.modules.is_empty());
    }

    #[test]
    fn test_should_discover_module_via_glob() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "modules/iam-role/variables.tf",
            "variable \"name\" {\n  type = string\n}\n",
        );
        let opts = DiscoveryOptions::defaults();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        assert_eq!(d.modules.len(), 1, "{d:?}");
        assert_eq!(d.modules[0].kind, DirKind::Module);
    }

    #[test]
    fn test_should_discover_terragrunt_component_with_include() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "services/api/terragrunt.hcl",
            "include \"root\" {\n  path = find_in_parent_folders()\n}\n",
        );
        let opts = DiscoveryOptions::defaults();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        assert_eq!(d.components.len(), 1);
        assert_eq!(
            d.components[0].reason,
            ClassificationReason::TerragruntInclude
        );
    }

    #[test]
    fn test_should_skip_files_larger_than_cap() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let big = "a".repeat(100);
        write(root, "svc/main.tf", &format!("{big}\n"));
        let opts: DiscoveryOptions = DiscoveryOptions::builder()
            .max_file_size_bytes(50_u64)
            .build();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        assert!(
            d.diagnostics.iter().any(|x| x.code.as_ref() == "TF1103"),
            "{d:?}"
        );
    }

    #[test]
    fn test_should_enforce_total_file_cap() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        for i in 0..5 {
            write(root, &format!("svc/f{i}.tf"), "");
        }
        let opts: DiscoveryOptions = DiscoveryOptions::builder().max_total_files(3_u64).build();
        let err = FsDiscoverer.discover(root, &opts).unwrap_err();
        assert!(matches!(
            err,
            Error::Limit {
                kind: LimitKind::TotalFiles,
                ..
            }
        ));
    }

    #[test]
    fn test_should_detect_environments_dir() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "environments/staging.tfvars", "");
        let opts = DiscoveryOptions::defaults();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        assert!(d.envs_dir.is_some(), "{d:?}");
    }

    #[test]
    fn test_should_detect_root_hcl() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "root.hcl", "remote_state {}\n");
        let opts = DiscoveryOptions::defaults();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        assert_eq!(d.root_hcl.as_deref(), Some(Path::new("root.hcl")));
    }

    #[test]
    fn test_should_reject_missing_root() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("not-here");
        let err = FsDiscoverer
            .discover(&missing, &DiscoveryOptions::defaults())
            .unwrap_err();
        assert!(matches!(err, Error::Io { .. }));
    }

    #[test]
    fn test_should_classify_environments_dir_only_at_root_level() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "platform/environments/x.tfvars", "");
        let opts = DiscoveryOptions::defaults();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        assert!(d.envs_dir.is_none(), "nested env dir should not match");
    }

    #[test]
    fn test_should_emit_ambiguity_diag_for_component_in_modules_glob() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "modules/strange/terragrunt.hcl",
            "include \"root\" {\n  path = find_in_parent_folders()\n}\n",
        );
        let opts = DiscoveryOptions::defaults();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        assert!(
            d.components
                .iter()
                .any(|c| c.path.as_ref() == Path::new("modules/strange"))
        );
        assert!(
            d.diagnostics.iter().any(|x| x.code.as_ref() == "TF1120"),
            "{d:?}"
        );
    }

    #[test]
    fn test_should_be_deterministic_in_ordering() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        for name in ["zeta", "alpha", "mu"] {
            write(
                root,
                &format!("services/{name}/main.tf"),
                "resource \"r\" \"r\" {}\n",
            );
        }
        let opts = DiscoveryOptions::defaults();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        let names: Vec<_> = d
            .components
            .iter()
            .map(|c| c.path.to_string_lossy().into_owned())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn test_should_classify_files_only_dir() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "policies/foo.json", "{}");
        let opts = DiscoveryOptions::defaults();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        // `Files` directories don't surface in components/modules; they are
        // implicitly tracked via tracing::debug. The presence/absence here is
        // structural, not asserted by Discovery's public API. We just verify
        // discovery does not error.
        assert!(d.components.is_empty());
        assert!(d.modules.is_empty());
    }

    #[test]
    fn test_should_record_files_for_component() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "svc/main.tf", "resource \"r\" \"r\" {}\n");
        write(root, "svc/outputs.tf", "output \"o\" { value = 1 }\n");
        let opts = DiscoveryOptions::defaults();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        assert_eq!(d.components.len(), 1);
        let exts: Vec<_> = d.components[0].files.iter().map(|f| f.ext).collect();
        assert!(exts.iter().all(|e| *e == FileExt::Tf));
    }

    #[test]
    fn test_canonical_root_strips_dot_dot() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "svc/main.tf", "resource \"r\" \"r\" {}\n");
        let nested = root.join("svc/..");
        let d = FsDiscoverer
            .discover(&nested, &DiscoveryOptions::defaults())
            .unwrap();
        assert!(!d.root.to_string_lossy().contains(".."));
    }

    // Symlinks are unix-only.
    #[cfg(unix)]
    #[test]
    fn test_should_skip_symlinks_by_default() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "real/main.tf", "resource \"r\" \"r\" {}\n");
        std::os::unix::fs::symlink(root.join("real/main.tf"), root.join("real/main_link.tf"))
            .unwrap();
        let opts = DiscoveryOptions::defaults();
        let d = FsDiscoverer.discover(root, &opts).unwrap();
        // Component still classified (the real file is there), and the
        // symlink path emits a `Trace` diagnostic.
        assert_eq!(d.components.len(), 1);
        let _ = PathBuf::from(root); // keep tempdir alive
    }
}
