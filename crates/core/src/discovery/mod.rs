//! Workspace discovery — the first cross-trust-boundary phase.
//!
//! Given a workspace root, walk the filesystem with the [`ignore`] crate (the
//! same engine `ripgrep` uses), classify each directory as a component,
//! module, or environment, and emit an ordered, deterministic
//! [`Discovered`] structure for the loader and Terragrunt resolver to
//! consume.
//!
//! No HCL parsing happens here — only a regex-grade shallow probe of file
//! bytes (per [11-discovery.md § 3.3]) that the loader will redo definitively.
//!
//! This module is the **first slice of code** that touches user-controlled
//! filesystem state. Every byte off disk is treated as hostile per
//! [70-security.md § 1]: paths are NUL-rejected, canonicalised, and verified
//! to remain underneath the workspace root before any open.
//!
//! [11-discovery.md § 3.3]: ../../../specs/11-discovery.md
//! [70-security.md § 1]: ../../../specs/70-security.md

mod classifier;
mod fs_walker;
mod options;
mod types;

use std::path::Path;

pub use classifier::ClassificationReason;
pub use fs_walker::FsDiscoverer;
pub use options::{
    DiscoveryOptions, DiscoveryOptionsBuilder, GlobConfigError, MAX_DISCOVERY_THREADS,
    MAX_GLOB_PATTERN_BYTES, compile_glob_set,
};
pub use types::{DirKind, Discovered, DiscoveredDir, DiscoveredFile};

use crate::Result;

/// Trait every discoverer implements. The default impl is [`FsDiscoverer`];
/// downstream tests / embedders may supply an in-memory variant by
/// implementing this trait directly.
pub trait Discoverer: Send + Sync {
    /// Walk `root` according to `opts` and produce a [`Discovered`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] when the root is missing, the workspace
    /// breaches a configured cap, or the discovery walk hits a fatal I/O
    /// failure. Non-fatal anomalies (broken symlinks, oversized files,
    /// ambiguous classifications) accumulate in
    /// [`Discovered::diagnostics`].
    fn discover(&self, root: &Path, opts: &DiscoveryOptions) -> Result<Discovered>;
}
