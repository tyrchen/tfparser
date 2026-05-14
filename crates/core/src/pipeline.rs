//! Top-level pipeline trait + default implementation.
//!
//! **Most consumers want [`crate::Parser`] instead** — it wraps
//! [`DefaultPipeline`] + [`crate::exporter::ParquetExporter`] behind one
//! builder. The trait here is the seam that lets tests swap a stub in.
//!
//! [`Pipeline::run`] is the single entry point downstream crates (the CLI,
//! the future server) call. Phase 1 defined the trait skeleton; Phase 9
//! lands the [`DefaultPipeline`] implementation wiring discovery → loader
//! → projection → terragrunt → evaluator → graph → provider into a single
//! [`Workspace`].
//!
//! The exporter is intentionally **not** part of the pipeline: callers may
//! consume the IR directly (the future server) or hand it to the
//! [`crate::exporter::ParquetExporter`] (today's CLI). Phase 9 keeps that
//! boundary so a downstream test can drive the same pipeline without
//! touching disk for output.
//!
//! Per [61-crates-and-features.md § 3.1] and [91-impl-plan.md § 8/§ 11].
//!
//! [61-crates-and-features.md § 3.1]: ../../specs/61-crates-and-features.md
//! [91-impl-plan.md § 8/§ 11]: ../../specs/91-impl-plan.md

use std::{collections::BTreeSet, path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::{
    Result,
    diagnostic::Diagnostic,
    discovery::{DiscoveredDir, Discoverer, DiscoveryOptions, FsDiscoverer},
    eval::{
        EnvVarMode, EvalContext, EvalLimits, EvaluatedComponent, Evaluator, FuncRegistry,
        HclEvaluator,
    },
    graph::{DefaultGraphBuilder, GraphBuilder, GraphContext, ModuleRegistry},
    ir::{ComponentId, Map, Value, Workspace},
    loader::{HclEditLoader, LoadContext, Loader, LoaderLimits, RawComponent, SourceMap},
    projection::project_component,
    provider::{DefaultProviderResolver, ProfileMap, ProviderContext, ProviderResolver},
    terragrunt::{FsTerragruntResolver, TerragruntResolver, TgContext},
};

/// Options for [`Pipeline::run`].
///
/// Build via [`PipelineOptionsBuilder`]; defaults are conservative
/// (resource limits at spec defaults, env mode strict-with-empty-allowlist).
#[derive(Clone, Debug, Serialize, Deserialize, TypedBuilder)]
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

    /// Allowlist of `TF_VAR_*` / generic env vars visible to `get_env` /
    /// Terragrunt funcs. Defaults to empty.
    #[serde(default)]
    #[builder(default)]
    pub allowed_env: BTreeSet<Arc<str>>,

    /// Repo-level `var.*` bindings (CLI `--var k=v` / `.tfvars`). Stored as
    /// a sorted vec for stable iteration; see [`crate::ir::Map`].
    #[serde(default)]
    #[builder(default)]
    pub repo_vars: Map,

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
/// Phase 9 lands [`DefaultPipeline`] wiring discovery → loader → projection
/// → terragrunt → evaluator → graph → provider. Tests may swap in stubs
/// implementing this trait directly.
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

/// Default pipeline implementation per spec 91 § 12.
///
/// Stateless aside from the `profile_map` it carries for the provider
/// resolver. Construct via [`DefaultPipeline::new`] then chain
/// [`DefaultPipeline::with_profile_map`] / [`DefaultPipeline::with_default_region`]
/// / [`DefaultPipeline::strict`] as needed.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DefaultPipeline {
    /// Provider-profile map shared with [`crate::provider::DefaultProviderResolver`].
    /// `None` means "no AWS profile mapping configured" — the resolver
    /// still runs and uses Terragrunt-cascade / `default_region` only.
    ///
    /// Held as `Arc<ProfileMap>` (not `SharedProfileMap = ArcSwap<...>`) so
    /// the pipeline is cheap-Clone. Operators who want hot-reload semantics
    /// can keep their own `ArcSwap<ProfileMap>` and rebuild the pipeline on
    /// rotation; the resolver does not own the swappable handle.
    pub profile_map: Option<Arc<ProfileMap>>,
    /// Default AWS region applied when neither provider blocks nor
    /// Terragrunt cascade supply one. Mirrors `ProviderContext::default_region`.
    pub default_region: Option<crate::ir::Region>,
    /// When `true`, the provider resolver returns `StrictUnresolved`
    /// when any referenced profile is missing from `profile_map`.
    pub strict_providers: bool,
}

impl Default for DefaultPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultPipeline {
    /// Construct a pipeline with no profile map and lenient provider mode.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            profile_map: None,
            default_region: None,
            strict_providers: false,
        }
    }

    /// Pin a profile map. Returns `self` for chaining.
    #[must_use]
    pub fn with_profile_map(mut self, map: Arc<ProfileMap>) -> Self {
        self.profile_map = Some(map);
        self
    }

    /// Pin a default region. Returns `self` for chaining.
    #[must_use]
    pub fn with_default_region(mut self, region: crate::ir::Region) -> Self {
        self.default_region = Some(region);
        self
    }

    /// Enable strict provider-mapping enforcement. Returns `self` for chaining.
    #[must_use]
    pub const fn strict(mut self) -> Self {
        self.strict_providers = true;
        self
    }
}

impl Pipeline for DefaultPipeline {
    #[tracing::instrument(
        level = "info",
        name = "pipeline.run",
        skip(self, opts),
        fields(root = %opts.root.display())
    )]
    // The pipeline is a linear seven-step flow; splitting the steps into
    // separate functions threads ~15 borrows of `canonical_root`,
    // `diagnostics`, `funcs`, and the eval/projection helpers through the
    // call graph for no readability gain. Phase 9 keeps the inline shape;
    // the seven `// ---- N)` comments anchor where future work would
    // extract subroutines if a Phase 10 reorg ever requires it.
    #[allow(clippy::too_many_lines)]
    fn run(&self, opts: &PipelineOptions) -> Result<Workspace> {
        let canonical_root: Arc<Path> = canonicalize_root(opts.root.as_ref())?;

        // ---- 1) Discovery ----------------------------------------------
        let discovery_opts = DiscoveryOptions::builder()
            .max_depth(opts.max_walk_depth)
            .max_total_files(opts.max_total_files)
            .max_file_size_bytes(opts.max_file_bytes)
            .follow_symlinks(opts.follow_symlinks)
            .build();
        let discovered = FsDiscoverer.discover(canonical_root.as_ref(), &discovery_opts)?;

        // ---- 2) Loader (modules + components) --------------------------
        let sources = SourceMap::new();
        // LoaderLimits' max_file_bytes is u32; clamp to that range so we
        // do not silently overflow on hostile fixtures.
        let loader_file_cap: u32 = u32::try_from(opts.max_file_bytes).unwrap_or(u32::MAX);
        let limits = LoaderLimits::builder()
            .max_file_bytes(loader_file_cap)
            .build();
        let load_ctx = LoadContext::new(&discovered.root, &sources, &limits);

        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        diagnostics.extend(discovered.diagnostics.iter().cloned());

        let mut next_component_id: usize = 0;
        let mut next_index = || {
            let id = ComponentId::from_index(next_component_id);
            next_component_id += 1;
            id
        };

        // Build module RawComponents first so the registry is populated
        // before any caller tries to expand them.
        let mut module_raws: Vec<(DiscoveredDir, RawComponent, ComponentId)> = Vec::new();
        for dir in &discovered.modules {
            let raw = HclEditLoader.load(dir, &load_ctx)?;
            diagnostics.extend(raw.diagnostics.iter().cloned());
            module_raws.push((dir.clone(), raw, next_index()));
        }
        let mut component_raws: Vec<(DiscoveredDir, RawComponent, ComponentId)> = Vec::new();
        for dir in &discovered.components {
            let raw = HclEditLoader.load(dir, &load_ctx)?;
            diagnostics.extend(raw.diagnostics.iter().cloned());
            component_raws.push((dir.clone(), raw, next_index()));
        }

        // ---- 3) Projection: RawComponent → Component -------------------
        let module_components: Vec<(DiscoveredDir, crate::ir::Component)> = module_raws
            .iter()
            .map(|(dir, raw, id)| {
                let mut diag = Vec::new();
                let component = project_component(raw, *id, &mut diag);
                diagnostics.extend(diag);
                (dir.clone(), component)
            })
            .collect();
        let mut component_components: Vec<(DiscoveredDir, crate::ir::Component)> = component_raws
            .iter()
            .map(|(dir, raw, id)| {
                let mut diag = Vec::new();
                let component = project_component(raw, *id, &mut diag);
                diagnostics.extend(diag);
                (dir.clone(), component)
            })
            .collect();

        // ---- 4) Terragrunt resolve per component -----------------------
        let tg_resolver = FsTerragruntResolver::new();
        let mut tg_ctx = TgContext::new(Arc::clone(&canonical_root));
        tg_ctx.environment.clone_from(&opts.environment);
        tg_ctx.env_var_mode.clone_from(&opts.env_var_mode);
        tg_ctx.allowed_env = Arc::new(opts.allowed_env.clone());
        tg_ctx.max_include_depth = opts.max_include_depth;

        for (dir, component) in &mut component_components {
            let abs_component_dir = canonical_root.join(dir.path.as_ref());
            if !is_terragrunt_component(&abs_component_dir) {
                continue;
            }
            match tg_resolver.resolve(&abs_component_dir, &tg_ctx) {
                Ok(cfg) => {
                    diagnostics.extend(cfg.diagnostics.iter().cloned());
                    component.terragrunt = Some(cfg);
                }
                Err(err) => {
                    diagnostics.push(Diagnostic::new(
                        crate::Severity::Warn,
                        "TG2001",
                        format!(
                            "terragrunt resolve failed for {}: {err}",
                            abs_component_dir.display()
                        ),
                    ));
                }
            }
        }

        // ---- 5) Evaluator (modules first, then components) -------------
        let evaluator = HclEvaluator::new();
        let funcs: Arc<FuncRegistry> = Arc::new(FuncRegistry::default_with_stdlib());
        let limits = EvalLimits::default();

        let mut registry = ModuleRegistry::new();
        let mut module_evals: Vec<EvaluatedComponent> = Vec::with_capacity(module_components.len());
        for (dir, component) in &module_components {
            let eval_ctx = EvalContext::new(
                Arc::clone(&canonical_root),
                opts.environment.clone(),
                opts.env_var_mode.clone(),
                opts.repo_vars.clone(),
                Map::new(), // modules don't get Terragrunt cascade locals
                Arc::clone(&funcs),
                limits,
            );
            let evald = evaluator.evaluate(component, &eval_ctx)?;
            diagnostics.extend(evald.diagnostics.iter().cloned());
            let canonical: Arc<Path> = Arc::from(canonical_root.join(dir.path.as_ref()));
            registry.insert_local(canonical, evald.clone());
            module_evals.push(evald);
        }

        let mut component_evals: Vec<EvaluatedComponent> =
            Vec::with_capacity(component_components.len());
        for (_, component) in &component_components {
            // Merge cascade locals from Terragrunt with caller-supplied repo_vars.
            let cascade_locals: Map = component
                .terragrunt
                .as_ref()
                .map(|c| c.effective_locals.clone())
                .unwrap_or_default();

            // Compose repo_vars: Terragrunt inputs feed `var.*` (Terragrunt
            // semantics — inputs propagate to the underlying module/component
            // as Terraform variables) layered under the caller's overrides.
            let mut repo_vars: Map = component
                .terragrunt
                .as_ref()
                .map(|c| c.inputs.clone())
                .unwrap_or_default();
            for (k, v) in &opts.repo_vars {
                override_or_push(&mut repo_vars, k.as_ref(), v.clone());
            }

            let eval_ctx = EvalContext::new(
                Arc::clone(&canonical_root),
                opts.environment.clone(),
                opts.env_var_mode.clone(),
                repo_vars,
                cascade_locals,
                Arc::clone(&funcs),
                limits,
            );
            let evald = evaluator.evaluate(component, &eval_ctx)?;
            diagnostics.extend(evald.diagnostics.iter().cloned());
            component_evals.push(evald);
        }

        // ---- 6) Graph build (module expansion + secondary tables) -----
        let graph_ctx = GraphContext::new(Arc::clone(&canonical_root));
        let mut combined: Vec<EvaluatedComponent> =
            Vec::with_capacity(module_evals.len() + component_evals.len());
        combined.extend(module_evals);
        combined.extend(component_evals);
        let mut ws = DefaultGraphBuilder::new().build(combined, &registry, &graph_ctx)?;

        // Pipe accumulated discovery + load + projection diagnostics onto
        // the workspace (the graph builder only appends its own).
        ws.diagnostics.extend(diagnostics);

        // ---- 7) Provider resolver --------------------------------------
        let profile_map = self
            .profile_map
            .clone()
            .unwrap_or_else(crate::provider::empty_profile_map);
        let mut provider_ctx = ProviderContext::new(profile_map);
        provider_ctx.default_region.clone_from(&self.default_region);
        provider_ctx.strict = self.strict_providers;
        DefaultProviderResolver::new().resolve(&mut ws, &provider_ctx)?;

        // Preserve original discovery shape (envs_dir + root_hcl) for
        // downstream consumers that need it. Phase 9 surfaces them via the
        // workspace shape; not yet wired through the IR (would require a
        // new `Workspace.discovery` field). Kept here as a `_` to acknowledge.
        let _ = (&discovered.envs_dir, &discovered.root_hcl);

        Ok(ws)
    }
}

fn canonicalize_root(root: &Path) -> Result<Arc<Path>> {
    let canonical = root.canonicalize().map_err(|source| crate::Error::Io {
        path: root.to_path_buf(),
        source,
    })?;
    Ok(Arc::from(canonical))
}

/// A component dir is "Terragrunt-shaped" iff it contains a `terragrunt.hcl`.
fn is_terragrunt_component(abs_dir: &Path) -> bool {
    abs_dir.join("terragrunt.hcl").is_file()
}

fn override_or_push(map: &mut Map, key: &str, value: Value) {
    if let Some(slot) = map.iter_mut().find(|(k, _)| k.as_ref() == key) {
        slot.1 = value;
    } else {
        map.push((Arc::from(key), value));
    }
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
        // PipelineOptions intentionally does not derive `Eq` because
        // `repo_vars` carries `Value` (which contains `f64`). Round-trip
        // through JSON and check structural invariants instead.
        let opts = PipelineOptions::builder()
            .root(Arc::<Path>::from(PathBuf::from("/tmp/repo")))
            .max_walk_depth(8_u32)
            .build();
        let json = serde_json::to_string(&opts).unwrap();
        let back: PipelineOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back.root, opts.root);
        assert_eq!(back.max_walk_depth, opts.max_walk_depth);
        assert!(back.repo_vars.is_empty());
    }

    #[test]
    fn test_default_pipeline_smoke_run_on_single_component_fixture() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest.ancestors().nth(2).unwrap();
        let fixture = workspace_root.join("fixtures").join("single-component");
        if !fixture.exists() {
            return; // skip when fixture is missing (e.g. running from a partial checkout)
        }
        let opts = PipelineOptions::new(Arc::<Path>::from(fixture));
        let ws = DefaultPipeline::new().run(&opts).unwrap();
        assert!(
            !ws.components.is_empty(),
            "expected at least one component in single-component fixture"
        );
    }

    #[test]
    fn test_pipeline_is_send_sync_object_safe() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DefaultPipeline>();
        assert_send_sync::<Box<dyn Pipeline>>();
    }
}
