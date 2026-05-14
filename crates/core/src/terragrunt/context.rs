//! Per-resolution context handed to [`crate::terragrunt::TerragruntResolver::resolve`].
//!
//! `TgContext` mirrors [14-terragrunt.md § 2]'s shape and is a read-only
//! input bundle: workspace root, env-var policy, depth caps. The mutable
//! resolution state (memo, include stack) lives inside the resolver's
//! per-call scratchpad — see [`crate::terragrunt::resolver`].
//!
//! [14-terragrunt.md § 2]: ../../../specs/14-terragrunt.md

use std::{collections::BTreeSet, path::Path, sync::Arc};

use crate::eval::EnvVarMode;

/// Read-only inputs to a single [`crate::terragrunt::TerragruntResolver::resolve`] call.
///
/// `allowed_env` is wrapped in `Arc<BTreeSet<Arc<str>>>` per
/// [14-terragrunt.md § 9 CLAUDE.md anchoring] so it's cheap to share across
/// `rayon` worker threads. `max_include_depth` defaults to 32, matching the
/// spec's pin.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TgContext {
    /// Canonical absolute workspace root. Every Terragrunt-function-resolved
    /// path must remain underneath this; the resolver enforces it via the
    /// internal `canonicalize_inside` helper.
    pub workspace_root: Arc<Path>,

    /// Pinned environment name (e.g. `Some("staging")`). Surfaces as
    /// `terraform.workspace` in downstream evaluator scopes; the resolver
    /// itself does **not** consult it for `get_env`.
    pub environment: Option<Arc<str>>,

    /// How `get_env(name, default?)` reads the process environment. Default
    /// is [`EnvVarMode::Strict`] with an empty allowlist (off by default).
    pub env_var_mode: EnvVarMode,

    /// Allowlist of environment-variable names visible to `get_env`. Same
    /// shape as [`crate::eval::EvalContext`]'s field; kept separate to
    /// allow tighter Terragrunt-only policy.
    pub allowed_env: Arc<BTreeSet<Arc<str>>>,

    /// Maximum include-chain depth before [`crate::terragrunt::TerragruntError::DepthExceeded`]
    /// fires. Default: 32.
    pub max_include_depth: u32,
}

impl TgContext {
    /// Construct a `TgContext` with the spec defaults: env-var mode strict
    /// with empty allowlist, `max_include_depth = 32`.
    #[must_use]
    pub fn new(workspace_root: Arc<Path>) -> Self {
        Self {
            workspace_root,
            environment: None,
            env_var_mode: EnvVarMode::default(),
            allowed_env: Arc::new(BTreeSet::new()),
            max_include_depth: 32,
        }
    }
}
