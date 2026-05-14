//! Output types for the discovery phase.
//!
//! Per [11-discovery.md § 2], the walker produces a [`Discovered`] containing
//! lists of [`DiscoveredDir`] (one per classified directory) and any
//! non-fatal [`crate::diagnostic::Diagnostic`]s the walk emitted.

use std::{path::Path, sync::Arc};

use super::ClassificationReason;
use crate::{Diagnostic, ir::FileExt};

/// Output of [`crate::discovery::Discoverer::discover`].
///
/// Cheap to clone — every owned path field is wrapped in `Arc`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Discovered {
    /// Canonicalised workspace root supplied by the caller.
    pub root: Arc<Path>,

    /// Directories classified as components, sorted byte-lexicographically
    /// by relative path for reproducibility (per
    /// [11-discovery.md § 3.4](../../../specs/11-discovery.md)).
    pub components: Vec<DiscoveredDir>,

    /// Directories classified as modules. Same ordering as `components`.
    pub modules: Vec<DiscoveredDir>,

    /// `<root>/environments/`, if it exists. Used by the Terragrunt
    /// resolver later to discover environment cascades.
    pub envs_dir: Option<Arc<Path>>,

    /// Workspace-level Terragrunt root (e.g. `<root>/root.hcl` or
    /// `<root>/terraform/root.hcl`), if any.
    pub root_hcl: Option<Arc<Path>>,

    /// Non-fatal anomalies surfaced by the walker (broken symlinks,
    /// oversized files, ambiguous classifications, …).
    pub diagnostics: Vec<Diagnostic>,
}

/// What kind of directory the walker thinks it is.
///
/// Matches the spec's `DirKind`. Adding variants is non-breaking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DirKind {
    /// An apply-able root (Terragrunt, backend, or resource heuristic
    /// tripped).
    Component,
    /// A reusable module body.
    Module,
    /// The workspace-level `environments/` directory.
    Environments,
    /// A bag of resource files (`files/`, `data/`, README-only, …) tracked
    /// for round-tripping but not parsed.
    Files,
    /// Anything we couldn't place — `Diagnostic::Ambiguous` may also have
    /// been emitted.
    Other,
}

/// A single discovered directory.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DiscoveredDir {
    /// Path relative to [`Discovered::root`], `/`-separated for portability.
    pub path: Arc<Path>,
    /// Classification.
    pub kind: DirKind,
    /// Why we picked this kind. Audit trail surfaced in `--verbose`.
    pub reason: ClassificationReason,
    /// Source files in this directory (no recursion — children belong to
    /// the next dir up).
    pub files: Vec<DiscoveredFile>,
}

/// A single source file discovered inside a [`DiscoveredDir`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DiscoveredFile {
    /// Path relative to [`Discovered::root`], `/`-separated.
    pub path: Arc<Path>,
    /// Classified extension (per [`FileExt::classify`]).
    pub ext: FileExt,
    /// Size in bytes from the walker's metadata call.
    pub size: u64,
}

impl DiscoveredDir {
    /// Construct a [`DiscoveredDir`]. Crate-private — discovery owns the
    /// invariants (path is relative, files is order-stable).
    pub(crate) fn new(
        path: Arc<Path>,
        kind: DirKind,
        reason: ClassificationReason,
        files: Vec<DiscoveredFile>,
    ) -> Self {
        Self {
            path,
            kind,
            reason,
            files,
        }
    }
}
