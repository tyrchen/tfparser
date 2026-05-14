//! Top-level pipeline trait skeleton.
//!
//! [`Pipeline::run`] is the single entry point downstream crates (the CLI,
//! the future server) call. Phase 1 only defines the trait and an options
//! struct; the default implementation lands in Phase 5 once every phase
//! component is available.
//!
//! Per [61-crates-and-features.md § 3.1].
//!
//! [61-crates-and-features.md § 3.1]: ../../specs/61-crates-and-features.md

use std::{collections::BTreeSet, path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::{Result, ir::Workspace};

/// How [`Pipeline::run`] should treat process environment variables when an
/// HCL evaluator encounters `get_env(...)`. Mirrors the
/// [`EvalContext`-side enum](../../specs/13-evaluator.md) — duplicated here
/// to keep the pipeline-facing options struct free of evaluator types
/// until Phase 4.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "mode")]
#[non_exhaustive]
pub enum EnvVarMode {
    /// Pass the full process environment to the evaluator. CLI prints a
    /// warning at startup. Reserved for `--unsafe-env`.
    Passthrough,
    /// Only names in `allowed` are visible. Default.
    ///
    /// The allowlist is a `BTreeSet` (deduping, ordered) per
    /// [13-evaluator.md § 2](../../specs/13-evaluator.md).
    Strict {
        /// Allowed env var names (e.g. `{"TF_VAR_environment", "AWS_REGION"}`).
        allowed: BTreeSet<Arc<str>>,
    },
    /// `get_env` always returns the supplied default (or empty string).
    Mock,
}

impl Default for EnvVarMode {
    fn default() -> Self {
        Self::Strict {
            allowed: BTreeSet::new(),
        }
    }
}

/// Options for [`Pipeline::run`].
///
/// Build via [`PipelineOptionsBuilder`]; defaults are conservative
/// (resource limits at spec defaults, env mode strict-with-empty-allowlist).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct PipelineOptions {
    /// Workspace root passed by the caller.
    #[serde(with = "crate::ir::path_serde::arc_path")]
    pub root: Arc<Path>,

    /// Optional environment name to pin (`var.environment`).
    #[builder(default)]
    pub environment: Option<Arc<str>>,

    /// How the evaluator treats `get_env(...)`.
    #[builder(default)]
    pub env_var_mode: EnvVarMode,

    /// Maximum walk depth (discovery). Default: 16.
    #[builder(default = 16)]
    pub max_walk_depth: u32,

    /// Maximum total files in the workspace. Default: `200_000`.
    #[builder(default = 200_000)]
    pub max_total_files: u64,

    /// Maximum size per file. Default: 4 MiB.
    #[builder(default = 4 * 1024 * 1024)]
    pub max_file_bytes: u64,

    /// Maximum Terragrunt include depth. Default: 32.
    #[builder(default = 32)]
    pub max_include_depth: u32,

    /// Whether the discoverer follows symlinks. Default: `false`.
    #[builder(default = false)]
    pub follow_symlinks: bool,
}

impl PipelineOptions {
    /// Convenience constructor pinning the workspace root with all other
    /// fields at spec defaults.
    #[must_use]
    pub fn new(root: impl Into<Arc<Path>>) -> Self {
        Self::builder().root(root.into()).build()
    }
}

/// Top-level pipeline trait.
///
/// Phase 5 lands a `DefaultPipeline` implementation wiring discovery →
/// loader → terragrunt → evaluator → graph → provider → exporter. Until
/// then, the trait is the contract downstream crates code against.
pub trait Pipeline: Send + Sync {
    /// Parse the workspace at `options.root` and return its IR.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::Error`] if the workspace cannot be parsed at all
    /// (root missing, fatal I/O error, resource limit exceeded). Non-fatal
    /// problems are reported via `Workspace::diagnostics`.
    fn run(&self, options: &PipelineOptions) -> Result<Workspace>;
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_should_build_pipeline_options_with_defaults() {
        let opts = PipelineOptions::new(Arc::<Path>::from(PathBuf::from("/tmp/repo")));
        assert_eq!(opts.max_walk_depth, 16);
        assert_eq!(opts.max_total_files, 200_000);
        assert!(matches!(opts.env_var_mode, EnvVarMode::Strict { .. }));
        assert!(!opts.follow_symlinks);
    }

    #[test]
    fn test_should_override_specific_options() {
        let opts = PipelineOptions::builder()
            .root(Arc::<Path>::from(PathBuf::from("/tmp/repo")))
            .environment(Some(Arc::<str>::from("staging")))
            .max_walk_depth(8_u32)
            .follow_symlinks(true)
            .build();
        assert_eq!(opts.max_walk_depth, 8);
        assert!(opts.follow_symlinks);
        assert_eq!(opts.environment.as_deref(), Some("staging"));
    }

    #[test]
    fn test_should_serde_round_trip_options() {
        let opts = PipelineOptions::new(Arc::<Path>::from(PathBuf::from("/tmp/repo")));
        let json = serde_json::to_string(&opts).unwrap();
        let back: PipelineOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(opts, back);
    }
}
