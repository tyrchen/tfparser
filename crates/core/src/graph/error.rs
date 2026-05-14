//! Errors emitted by the graph phase.

use std::{path::PathBuf, sync::Arc};

use thiserror::Error;

use crate::ir::{Address, Span};

/// Errors the graph builder can surface.
///
/// Per [15-resource-graph.md § 7], `UnresolvableModuleSource` and
/// `DepthExceeded` are **not fatal** — the builder records them as
/// [`crate::Diagnostic`]s and continues. Only [`GraphError::AddressCollision`]
/// is fatal: it indicates a bug in the expansion logic, not user input.
///
/// [15-resource-graph.md § 7]: ../../../specs/15-resource-graph.md
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GraphError {
    /// A module call's `source` could not be resolved to a known local module.
    /// Treated as a diagnostic at the call site, never fatal.
    #[error("module source `{module_source}` referenced from {site:?} is not resolvable")]
    UnresolvableModuleSource {
        /// Verbatim source string from the call site.
        module_source: Arc<str>,
        /// Span of the call site.
        site: Box<Span>,
    },

    /// Module recursion exceeded the configured depth cap. Records the path
    /// where the cap fired.
    #[error("module recursion exceeded depth {limit} at {site:?}")]
    DepthExceeded {
        /// Configured cap.
        limit: u32,
        /// Span of the offending call site.
        site: Box<Span>,
    },

    /// Two flattened resources resolved to the same [`Address`]. Fatal: the
    /// IR cannot represent two rows with the same address.
    #[error("address collision: {0}")]
    AddressCollision(Address),

    /// Path-safety check failed when resolving a local module source.
    /// Non-fatal in spec; the builder logs a diagnostic and skips the call.
    #[error("path safety: {path:?}: {reason}")]
    PathSafety {
        /// Candidate path that failed.
        path: PathBuf,
        /// Why it failed.
        reason: Arc<str>,
    },
}
