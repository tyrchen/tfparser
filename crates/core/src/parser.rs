//! High-level [`Parser`] facade — the recommended consumption surface.
//!
//! The pipeline, evaluator, terragrunt resolver, provider resolver, graph
//! builder and exporter compose into a long argument list of typed structs.
//! For library consumers that's flexible but noisy; [`Parser`] bundles the
//! common shape behind one ergonomic builder and two execution methods.
//!
//! ```no_run
//! # fn main() -> tfparser_core::Result<()> {
//! // One-shot: parse a workspace with all defaults.
//! let workspace = tfparser_core::parse("./my-tf-repo")?;
//! println!("{} components", workspace.components.len());
//! # Ok(()) }
//! ```
//!
//! ```no_run
//! # fn main() -> tfparser_core::Result<()> {
//! use tfparser_core::{Parser, EnvVarMode};
//!
//! // Builder for full control. Every option is optional except
//! // `workspace_root`; the rest defer to spec defaults.
//! let workspace = Parser::builder()
//!     .workspace_root("./my-tf-repo")
//!     .environment("production")
//!     .default_region("us-west-2")?
//!     .env_var_mode(EnvVarMode::Passthrough)
//!     .allow_env("TF_VAR_environment")
//!     .var("region", "us-east-1")
//!     .strict_providers(true)
//!     .build()?
//!     .parse()?;
//! # Ok(()) }
//! ```
//!
//! Add `.parse_and_export(&opts)` to write the four canonical Parquet tables
//! in the same call.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    Error, Result,
    eval::EnvVarMode,
    exporter::{ExportOptions, ExportReport, Exporter, ParquetExporter},
    ir::{Map, Region, Value, Workspace},
    pipeline::{DefaultPipeline, Pipeline, PipelineOptions},
    provider::{ProfileMap, load_aws_config, load_yaml_profile_map},
};

/// High-level wrapper around [`DefaultPipeline`] + [`ParquetExporter`].
///
/// Configure via [`Parser::builder`]; run via [`Parser::parse`] (workspace
/// only) or [`Parser::parse_and_export`] (workspace plus Parquet output).
///
/// The struct is cheap to clone (every field is an `Arc` or `Copy`).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Parser {
    /// Pipeline-level configuration (workspace root, limits, env mode, vars).
    pub pipeline_options: PipelineOptions,
    /// Optional AWS profile-map driving the provider resolver.
    pub profile_map: Option<Arc<ProfileMap>>,
    /// Default region used when neither provider blocks nor Terragrunt
    /// cascade supply one.
    pub default_region: Option<Region>,
    /// Whether the provider resolver fails when a referenced profile is
    /// missing from `profile_map`.
    pub strict_providers: bool,
}

impl Parser {
    /// Start a builder. `workspace_root(...)` is the only required field.
    #[must_use]
    pub fn builder() -> ParserBuilder {
        ParserBuilder::default()
    }

    /// Run the parsing pipeline and return the in-memory [`Workspace`].
    ///
    /// # Errors
    ///
    /// Propagates any fatal error from discovery / loader / evaluator /
    /// graph / provider stages. Non-fatal issues are surfaced via
    /// `Workspace::diagnostics`.
    pub fn parse(&self) -> Result<Workspace> {
        self.build_pipeline().run(&self.pipeline_options)
    }

    /// Run the pipeline then write the four canonical Parquet tables.
    ///
    /// Equivalent to `parse()` + `ParquetExporter::new().export(&ws, opts)`,
    /// folded into a single call that hands back both the in-memory
    /// [`Workspace`] and the exporter's [`ExportReport`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Export`] when the writer fails; otherwise as
    /// [`Self::parse`].
    pub fn parse_and_export(&self, opts: &ExportOptions) -> Result<(Workspace, ExportReport)> {
        let ws = self.parse()?;
        let report = ParquetExporter::new()
            .export(&ws, opts)
            .map_err(Error::from)?;
        Ok((ws, report))
    }

    fn build_pipeline(&self) -> DefaultPipeline {
        let mut p = DefaultPipeline::new();
        if let Some(map) = &self.profile_map {
            p = p.with_profile_map(Arc::clone(map));
        }
        if let Some(region) = &self.default_region {
            p = p.with_default_region(region.clone());
        }
        if self.strict_providers {
            p = p.strict();
        }
        p
    }
}

/// Builder for [`Parser`]. Every method consumes and returns `self` so
/// configuration reads top-to-bottom.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ParserBuilder {
    workspace_root: Option<PathBuf>,
    environment: Option<Arc<str>>,
    env_var_mode: Option<EnvVarMode>,
    allowed_env: BTreeSet<Arc<str>>,
    repo_vars: Map,
    max_walk_depth: Option<u32>,
    max_total_files: Option<u64>,
    max_file_bytes: Option<u64>,
    max_include_depth: Option<u32>,
    follow_symlinks: Option<bool>,
    profile_map: Option<Arc<ProfileMap>>,
    default_region: Option<Region>,
    strict_providers: bool,
}

impl ParserBuilder {
    /// **Required.** Workspace root (the directory that contains the
    /// Terraform / Terragrunt code).
    #[must_use]
    pub fn workspace_root(mut self, root: impl AsRef<Path>) -> Self {
        self.workspace_root = Some(root.as_ref().to_path_buf());
        self
    }

    /// Pin `terraform.workspace` / Terragrunt cascade choice. Optional;
    /// when unset the resolver leaves `var.environment` unresolved.
    #[must_use]
    pub fn environment(mut self, name: impl Into<Arc<str>>) -> Self {
        self.environment = Some(name.into());
        self
    }

    /// How `get_env(...)` / Terragrunt funcs read the process env. Default:
    /// strict with an empty allowlist (no env vars are visible to user code).
    #[must_use]
    pub fn env_var_mode(mut self, mode: EnvVarMode) -> Self {
        self.env_var_mode = Some(mode);
        self
    }

    /// Allow a single env var name through `get_env(...)`. Repeatable.
    #[must_use]
    pub fn allow_env(mut self, name: impl Into<Arc<str>>) -> Self {
        self.allowed_env.insert(name.into());
        self
    }

    /// Allowlist multiple env var names in one call.
    #[must_use]
    pub fn allow_env_many<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Arc<str>>,
    {
        for n in names {
            self.allowed_env.insert(n.into());
        }
        self
    }

    /// Repo-level `var.<key> = value` binding. Repeatable. Values are
    /// stored as `Value::Str`; for richer types build the [`Map`] manually
    /// via [`Self::repo_vars`].
    #[must_use]
    pub fn var(mut self, key: impl Into<Arc<str>>, value: impl Into<Arc<str>>) -> Self {
        let key = key.into();
        let val = Value::Str(value.into());
        match self.repo_vars.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = val,
            None => self.repo_vars.push((key, val)),
        }
        self
    }

    /// Replace the repo-level variable map. See also [`Self::var`].
    #[must_use]
    pub fn repo_vars(mut self, vars: Map) -> Self {
        self.repo_vars = vars;
        self
    }

    /// Pin an explicit [`ProfileMap`] for the provider resolver.
    #[must_use]
    pub fn profile_map(mut self, map: Arc<ProfileMap>) -> Self {
        self.profile_map = Some(map);
        self
    }

    /// Load the profile map from a YAML file (spec 16 § 3.2).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] when the file is missing, malformed, or
    /// violates the validator rules (account-id pattern, region length,
    /// etc.).
    pub fn load_profile_map_yaml(mut self, path: impl AsRef<Path>) -> Result<Self> {
        let map = load_yaml_profile_map(path.as_ref()).map_err(Error::from)?;
        self.profile_map = Some(map);
        Ok(self)
    }

    /// Load the profile map from an `~/.aws/config`-shaped INI file
    /// (spec 16 § 3.1).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] when the file is missing or malformed.
    pub fn load_aws_config(mut self, path: impl AsRef<Path>) -> Result<Self> {
        let map = load_aws_config(path.as_ref()).map_err(Error::from)?;
        self.profile_map = Some(map);
        Ok(self)
    }

    /// Default AWS region applied when neither provider blocks nor the
    /// Terragrunt cascade supply one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Validation`] when `region` fails the
    /// [`Region`] validator (charset / length).
    pub fn default_region(mut self, region: impl AsRef<str>) -> Result<Self> {
        self.default_region = Some(Region::new(region.as_ref()).map_err(Error::from)?);
        Ok(self)
    }

    /// If `true`, the provider resolver returns `StrictUnresolved` when a
    /// referenced profile is missing from the profile map. Default: `false`.
    #[must_use]
    pub const fn strict_providers(mut self, strict: bool) -> Self {
        self.strict_providers = strict;
        self
    }

    /// Maximum walk depth (discovery). Default: 16.
    #[must_use]
    pub const fn max_walk_depth(mut self, depth: u32) -> Self {
        self.max_walk_depth = Some(depth);
        self
    }

    /// Maximum total files in the workspace. Default: 200 000.
    #[must_use]
    pub const fn max_total_files(mut self, n: u64) -> Self {
        self.max_total_files = Some(n);
        self
    }

    /// Maximum size per file. Default: 4 MiB.
    #[must_use]
    pub const fn max_file_bytes(mut self, n: u64) -> Self {
        self.max_file_bytes = Some(n);
        self
    }

    /// Maximum Terragrunt include depth. Default: 32.
    #[must_use]
    pub const fn max_include_depth(mut self, n: u32) -> Self {
        self.max_include_depth = Some(n);
        self
    }

    /// Whether the discoverer follows symlinks. Default: `false`.
    #[must_use]
    pub const fn follow_symlinks(mut self, follow: bool) -> Self {
        self.follow_symlinks = Some(follow);
        self
    }

    /// Finalise into a [`Parser`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Validation`] when `workspace_root` was not set.
    pub fn build(self) -> Result<Parser> {
        let Some(root) = self.workspace_root else {
            return Err(Error::Validation(crate::ValidationError::MissingField(
                "workspace_root",
            )));
        };
        let mut opts = PipelineOptions::builder()
            .root(Arc::<Path>::from(root.as_path()))
            .build();
        if let Some(env) = self.environment {
            opts.environment = Some(env);
        }
        if let Some(mode) = self.env_var_mode {
            opts.env_var_mode = mode;
        }
        if !self.allowed_env.is_empty() {
            opts.allowed_env = self.allowed_env;
        }
        if !self.repo_vars.is_empty() {
            opts.repo_vars = self.repo_vars;
        }
        if let Some(d) = self.max_walk_depth {
            opts.max_walk_depth = d;
        }
        if let Some(n) = self.max_total_files {
            opts.max_total_files = n;
        }
        if let Some(n) = self.max_file_bytes {
            opts.max_file_bytes = n;
        }
        if let Some(n) = self.max_include_depth {
            opts.max_include_depth = n;
        }
        if let Some(f) = self.follow_symlinks {
            opts.follow_symlinks = f;
        }
        Ok(Parser {
            pipeline_options: opts,
            profile_map: self.profile_map,
            default_region: self.default_region,
            strict_providers: self.strict_providers,
        })
    }
}

/// Parse a workspace with all defaults. Convenience over
/// [`Parser::builder`] when you don't need to tune anything.
///
/// ```no_run
/// # fn main() -> tfparser_core::Result<()> {
/// let workspace = tfparser_core::parse("./my-tf-repo")?;
/// # let _ = workspace;
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// Same as [`Parser::parse`].
pub fn parse(root: impl AsRef<Path>) -> Result<Workspace> {
    Parser::builder().workspace_root(root).build()?.parse()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_should_require_workspace_root() {
        let err = Parser::builder().build().unwrap_err();
        assert!(
            matches!(
                err,
                Error::Validation(crate::ValidationError::MissingField(_))
            ),
            "expected MissingField, got {err:?}"
        );
    }

    #[test]
    fn test_should_carry_builder_options_into_pipeline_options() {
        let parser = Parser::builder()
            .workspace_root("/tmp/repo")
            .environment("staging")
            .var("region", "us-east-1")
            .var("region", "us-west-2") // overrides
            .allow_env("TF_VAR_environment")
            .max_walk_depth(8_u32)
            .strict_providers(true)
            .build()
            .unwrap();
        assert_eq!(parser.pipeline_options.max_walk_depth, 8);
        assert_eq!(
            parser.pipeline_options.environment.as_deref(),
            Some("staging")
        );
        assert!(parser.strict_providers);
        let region = parser
            .pipeline_options
            .repo_vars
            .iter()
            .find(|(k, _)| k.as_ref() == "region")
            .expect("region var present");
        assert!(matches!(&region.1, Value::Str(s) if s.as_ref() == "us-west-2"));
        assert_eq!(parser.pipeline_options.allowed_env.len(), 1);
    }

    #[test]
    fn test_should_validate_default_region() {
        let err = Parser::builder()
            .workspace_root("/tmp/repo")
            .default_region("INVALID region!")
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "got {err:?}");
    }
}
