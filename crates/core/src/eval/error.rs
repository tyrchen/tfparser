//! Evaluator-specific errors.
//!
//! Per [13-evaluator.md § 8]: cycle detection, configurable limit breaches,
//! function-call failures, and path-escape attempts get a structured
//! variant. An *unbound* reference (e.g. `var.x` when nothing supplied it)
//! is **not** an error — the walker leaves it as
//! [`crate::ir::Expression::Unresolved`] and continues. That is the
//! best-effort contract pinned in [99-key-decisions.md] D4.
//!
//! [13-evaluator.md § 8]: ../../../specs/13-evaluator.md
//! [99-key-decisions.md]: ../../../specs/99-key-decisions.md

use std::{path::PathBuf, sync::Arc};

use thiserror::Error;

use crate::{diagnostic::LimitKind, ir::Address};

/// Recoverable evaluator failure.
///
/// `EvalError` is **not** propagated out of
/// [`crate::eval::Evaluator::evaluate`]: instead the evaluator records each
/// failure as a [`crate::Diagnostic`] on the returned
/// [`crate::eval::EvaluatedComponent`] and proceeds. The error type is
/// public so callers (and tests) can build the same diagnostic shape
/// elsewhere — see [`HclEvaluator`](crate::eval::HclEvaluator) for the
/// conversion path.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvalError {
    /// A cycle was detected in `locals`. The participants list is the cycle
    /// in order of first observation (deterministic — sorted by name) so
    /// the diagnostic is stable across runs.
    #[error("cycle in locals: {participants:?}")]
    Cycle {
        /// The locals participating in the cycle (by `Address`, e.g.
        /// `"local.a"`).
        participants: Vec<Address>,
    },

    /// A configured limit fired. `kind` identifies which one; the message
    /// embeds the observed and configured values.
    #[error("evaluator limit ({kind:?}): observed {observed} > {limit}")]
    Limit {
        /// Which limit category fired.
        kind: LimitKind,
        /// Observed value at the time of the breach.
        observed: u64,
        /// Configured limit.
        limit: u64,
    },

    /// A registered function returned an error. `name` is the function
    /// being called; `message` is the function's own diagnostic. Function
    /// failures are recoverable: the call site keeps the unresolved
    /// expression and the workspace surfaces a diagnostic.
    #[error("function `{name}` failed: {message}")]
    Func {
        /// The function name.
        name: Arc<str>,
        /// Function-supplied message.
        message: Arc<str>,
    },

    /// A sandboxed file function rejected a path because it resolved
    /// outside the workspace root.
    #[error("path escape in `{func}`: `{path}`")]
    PathEscape {
        /// Function name (`"file"`, `"templatefile"`, …).
        func: &'static str,
        /// Path supplied by the caller.
        path: PathBuf,
    },
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[test]
    fn test_should_render_cycle_with_participants() {
        let e = EvalError::Cycle {
            participants: vec![
                Address::new("local.a").expect("addr"),
                Address::new("local.b").expect("addr"),
            ],
        };
        let s = format!("{e}");
        assert!(s.contains("local.a"));
        assert!(s.contains("local.b"));
    }

    #[test]
    fn test_should_render_limit_with_kind_and_values() {
        let e = EvalError::Limit {
            kind: LimitKind::EvalIterations,
            observed: 10,
            limit: 5,
        };
        let s = format!("{e}");
        assert!(s.contains("10"));
        assert!(s.contains('5'));
    }

    #[test]
    fn test_should_render_path_escape_with_func_and_path() {
        let e = EvalError::PathEscape {
            func: "file",
            path: PathBuf::from("../../etc/passwd"),
        };
        let s = format!("{e}");
        assert!(s.contains("file"));
        assert!(s.contains("etc/passwd"));
    }
}
