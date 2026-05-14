//! Errors emitted by the Terragrunt resolver.

use std::{path::PathBuf, sync::Arc};

use thiserror::Error;

use crate::ir::Span;

/// Errors the Terragrunt resolver can surface.
///
/// Per [14-terragrunt.md § 6], the variants mirror the spec text. Cycle /
/// path-escape / depth-cap are recorded as [`crate::Diagnostic`]s on the
/// returned [`crate::ir::TerragruntConfig`] in practice — only truly fatal
/// I/O errors (the parent dir disappears mid-walk) bubble up as
/// `Err(crate::Error)` via the `From` glue.
///
/// [14-terragrunt.md § 6]: ../../../specs/14-terragrunt.md
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TerragruntError {
    /// Include cycle detected. The vector captures the canonical stack at
    /// the moment the cycle was detected; the offending file is the *next*
    /// path the resolver would have entered.
    #[error("terragrunt include cycle: {0:?}")]
    Cycle(Vec<Arc<std::path::Path>>),

    /// Include stack exceeded the configured depth cap.
    #[error("terragrunt include depth limit ({limit}) exceeded")]
    DepthExceeded {
        /// Configured cap.
        limit: u32,
    },

    /// A path resolved by a Terragrunt function escapes the workspace root.
    #[error("terragrunt path escape: {path:?}")]
    PathEscape {
        /// Path that failed the descendant-of-root check.
        path: PathBuf,
    },

    /// A Terragrunt function call failed.
    #[error("terragrunt function `{func}`: {message}")]
    Func {
        /// Function name.
        func: &'static str,
        /// Message safe to log.
        message: Box<str>,
        /// Source span.
        span: Box<Span>,
    },
}
