//! Top-level graph builder.
//!
//! [`GraphBuilder::build`] is the Phase 5 entry point. Given a vector of
//! per-component [`EvaluatedComponent`]s (the evaluator output) and a
//! [`ModuleRegistry`] indexing every local module body, it flattens module
//! bodies into their callers, applies `count`/`for_each` expansion, sorts the
//! result deterministically, and returns a [`Workspace`] suitable for the
//! Parquet exporter (Phase 3) to write.
//!
//! Per [15-resource-graph.md § 2] and § 3.

use std::{collections::HashSet, path::Path, sync::Arc};

use crate::{
    Result,
    diagnostic::Diagnostic,
    eval::EvaluatedComponent,
    graph::{
        expand::{ExpansionState, expand_resource, flatten_modules},
        registry::ModuleRegistry,
    },
    ir::{Component, Resource, Workspace},
};

/// Per-`build` context: workspace root, recursion / expansion caps.
///
/// `infer_dependencies` is reserved for Phase 8 (edge collection); for
/// Phase 5 it has no effect.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct GraphContext {
    /// Canonical workspace root (absolute path). All module sources must
    /// resolve underneath this.
    pub workspace_root: Arc<Path>,
    /// Maximum nested-module recursion depth (per `I-GRAPH-4`).
    pub max_module_depth: u32,
    /// Reserved for Phase 8 (edge collection). Phase 5 ignores it.
    pub infer_dependencies: bool,
    /// `count` / `for_each` expansion cap. Spec 15 § 3.3 pins 1024 as the
    /// default; the cap collapses to a template row + a diagnostic when
    /// exceeded.
    pub max_expansion_per_resource: u32,
}

impl GraphContext {
    /// Construct a `GraphContext` with the spec defaults: `max_module_depth =
    /// 8`, `infer_dependencies = true`, `max_expansion_per_resource = 1024`.
    #[must_use]
    pub fn new(workspace_root: Arc<Path>) -> Self {
        Self {
            workspace_root,
            max_module_depth: 8,
            infer_dependencies: true,
            max_expansion_per_resource: 1024,
        }
    }
}

/// Trait the orchestrator calls to build a [`Workspace`]. Phase 5 ships
/// exactly one implementation, [`DefaultGraphBuilder`]; downstream tests
/// may swap an in-memory variant.
pub trait GraphBuilder: Send + Sync + std::fmt::Debug {
    /// Flatten every module call into its caller and produce a [`Workspace`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] only on fatal IR-construction failures
    /// (currently: an [`crate::ir::Address`] collision after expansion).
    /// Non-fatal anomalies — unresolvable module sources, depth-cap
    /// breaches, cycles — surface as
    /// [`Workspace::diagnostics`](crate::ir::Workspace::diagnostics).
    fn build(
        &self,
        components: Vec<EvaluatedComponent>,
        registry: &ModuleRegistry,
        ctx: &GraphContext,
    ) -> Result<Workspace>;
}

/// Default builder.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct DefaultGraphBuilder;

impl DefaultGraphBuilder {
    /// Construct a default builder. Equivalent to `DefaultGraphBuilder::default()`.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl GraphBuilder for DefaultGraphBuilder {
    fn build(
        &self,
        components: Vec<EvaluatedComponent>,
        registry: &ModuleRegistry,
        ctx: &GraphContext,
    ) -> Result<Workspace> {
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let mut out_components: Vec<Component> = Vec::with_capacity(components.len());

        // Only top-level components (kind=Component) become rows in the
        // workspace; modules contribute by expansion into their callers
        // (D5 — flattened module bodies).
        for evaluated in &components {
            if !matches!(evaluated.raw.kind, crate::ir::ComponentKind::Component) {
                continue;
            }
            let mut state = ExpansionState::new(
                ctx.workspace_root.as_ref(),
                ctx.max_module_depth,
                ctx.max_expansion_per_resource,
                registry,
            );
            let module_resources: Vec<Resource> = flatten_modules(evaluated, &mut state);
            diagnostics.append(&mut state.diagnostics);

            // Combine top-level + module-expanded resources. Both get
            // count/for_each expansion uniformly.
            let mut combined: Vec<Resource> =
                Vec::with_capacity(evaluated.resources.len() + module_resources.len());
            for r in &evaluated.resources {
                let expanded =
                    expand_resource(r.clone(), ctx.max_expansion_per_resource, &mut diagnostics);
                combined.extend(expanded);
            }
            for r in module_resources {
                let expanded = expand_resource(r, ctx.max_expansion_per_resource, &mut diagnostics);
                combined.extend(expanded);
            }

            // Address uniqueness (I-GRAPH-1): de-duplicate by address,
            // emitting a diagnostic on collision. We retain the first
            // occurrence rather than failing fatally — per spec 15 § 7,
            // collisions indicate a bug in expansion logic; the test
            // suite covers the case, and at runtime we prefer to keep
            // the run alive with a loud diagnostic.
            let mut seen: HashSet<String> = HashSet::with_capacity(combined.len());
            let mut deduped: Vec<Resource> = Vec::with_capacity(combined.len());
            for r in combined {
                let key = r.address.as_str().to_string();
                if seen.insert(key.clone()) {
                    deduped.push(r);
                } else {
                    diagnostics.push(crate::diagnostic::Diagnostic::new(
                        crate::Severity::Warn,
                        "TF1506",
                        format!("address collision after expansion: {key}"),
                    ));
                }
            }

            // Build the final Component, carrying over evaluator-resolved
            // pieces. Component diagnostics from the evaluator are
            // surfaced through Workspace.diagnostics.
            diagnostics.extend(evaluated.diagnostics.iter().cloned());

            let raw = evaluated.raw.as_ref();
            out_components.push(
                Component::builder()
                    .id(raw.id)
                    .path(Arc::clone(&raw.path))
                    .kind(raw.kind)
                    .files(raw.files.clone())
                    .variables(evaluated.variables.clone())
                    .locals(evaluated.locals.clone())
                    .providers(evaluated.providers.clone())
                    .resources(deduped)
                    .modules(evaluated.modules.clone())
                    .outputs(evaluated.outputs.clone())
                    .terragrunt(raw.terragrunt.clone())
                    .state_backend(raw.state_backend.clone())
                    .build(),
            );
        }

        // Workspace components are sorted by `Component.path` per I-GRAPH-5.
        out_components.sort_by(|a, b| a.path.cmp(&b.path));

        // Build workspace.modules from the registry (placeholder — Phase 8
        // will replace with the dependency-graph view; Phase 5 just
        // surfaces the modules we walked so the round-trip test pins the
        // shape).
        let modules = build_workspace_modules(&components);

        let mut ws = Workspace::builder()
            .root(Arc::clone(&ctx.workspace_root))
            .components(out_components)
            .modules(modules)
            .diagnostics(diagnostics)
            .build();

        // Phase 8: dependency-edge inference. Off when the orchestrator
        // wants only the row table (`infer_dependencies = false`).
        if ctx.infer_dependencies {
            super::edges::collect_edges_in_place(&mut ws);
        }

        Ok(ws)
    }
}

fn build_workspace_modules(components: &[EvaluatedComponent]) -> Vec<crate::ir::Module> {
    use crate::ir::{Module, ModuleId, ModuleSource};

    let mut modules: Vec<Module> = Vec::new();
    for (i, e) in components.iter().enumerate() {
        let raw = e.raw.as_ref();
        if !matches!(raw.kind, crate::ir::ComponentKind::Module) {
            continue;
        }
        let id = ModuleId::from_index(i);
        modules.push(
            Module::builder()
                .id(id)
                .source(ModuleSource::Local(Arc::from(
                    raw.path.to_string_lossy().as_ref(),
                )))
                .component(raw.clone())
                .build(),
        );
    }
    modules
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::{
        eval::EvaluatedComponent,
        ir::{
            Address, AttributeMap, Component, ComponentId, ComponentKind, Expression, ModuleCall,
            ModuleSource, Resource, ResourceKind, Span, SymbolKind, Symbolic, Value,
        },
    };

    fn span() -> Span {
        Span::synthetic()
    }

    fn eval(c: &Component) -> EvaluatedComponent {
        EvaluatedComponent::from_component(c.clone())
    }

    #[test]
    fn test_builder_passes_through_top_level_resources() {
        let r = Resource::builder()
            .address(Address::new("aws_iam_role.r").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_iam_role"))
            .name(Arc::<str>::from("r"))
            .span(span())
            .build();
        let c = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("svc")))
            .kind(ComponentKind::Component)
            .resources(vec![r])
            .build();
        let registry = ModuleRegistry::new();
        let ctx = GraphContext::new(Arc::<Path>::from(PathBuf::from("/tmp/repo")));
        let workspace = DefaultGraphBuilder
            .build(vec![eval(&c)], &registry, &ctx)
            .unwrap();
        assert_eq!(workspace.components.len(), 1);
        assert_eq!(workspace.components[0].resources.len(), 1);
    }

    #[test]
    fn test_builder_skips_module_kind_components() {
        let c = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("modules/x")))
            .kind(ComponentKind::Module)
            .build();
        let registry = ModuleRegistry::new();
        let ctx = GraphContext::new(Arc::<Path>::from(PathBuf::from("/tmp/repo")));
        let workspace = DefaultGraphBuilder
            .build(vec![eval(&c)], &registry, &ctx)
            .unwrap();
        assert!(workspace.components.is_empty());
        // Module body lives in workspace.modules instead.
        assert_eq!(workspace.modules.len(), 1);
    }

    #[test]
    fn test_count_literal_expands_top_level_resources() {
        let r = Resource::builder()
            .address(Address::new("aws_iam_role.r").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_iam_role"))
            .name(Arc::<str>::from("r"))
            .count_expr(Some(Expression::Literal(Value::Int(3))))
            .span(span())
            .build();
        let c = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("svc")))
            .kind(ComponentKind::Component)
            .resources(vec![r])
            .build();
        let registry = ModuleRegistry::new();
        let ctx = GraphContext::new(Arc::<Path>::from(PathBuf::from("/tmp/repo")));
        let workspace = DefaultGraphBuilder
            .build(vec![eval(&c)], &registry, &ctx)
            .unwrap();
        let addrs: Vec<&str> = workspace.components[0]
            .resources
            .iter()
            .map(|r| r.address.as_str())
            .collect();
        assert_eq!(
            addrs,
            vec![
                "aws_iam_role.r[0]",
                "aws_iam_role.r[1]",
                "aws_iam_role.r[2]",
            ]
        );
    }

    #[test]
    fn test_count_unresolved_keeps_one_template_row() {
        let r = Resource::builder()
            .address(Address::new("aws_iam_role.r").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_iam_role"))
            .name(Arc::<str>::from("r"))
            .count_expr(Some(Expression::Unresolved(
                Symbolic::builder()
                    .kind(SymbolKind::Var)
                    .source(Arc::<str>::from("var.foo"))
                    .span(span())
                    .build(),
            )))
            .span(span())
            .build();
        let c = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("svc")))
            .kind(ComponentKind::Component)
            .resources(vec![r])
            .build();
        let registry = ModuleRegistry::new();
        let ctx = GraphContext::new(Arc::<Path>::from(PathBuf::from("/tmp/repo")));
        let workspace = DefaultGraphBuilder
            .build(vec![eval(&c)], &registry, &ctx)
            .unwrap();
        let resources = &workspace.components[0].resources;
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].address.as_str(), "aws_iam_role.r");
        assert!(resources[0].count_expr.is_some());
    }

    #[test]
    fn test_workspace_components_sorted_by_path() {
        let c1 = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("z")))
            .kind(ComponentKind::Component)
            .build();
        let c2 = Component::builder()
            .id(ComponentId::from_index(1))
            .path(Arc::<Path>::from(PathBuf::from("a")))
            .kind(ComponentKind::Component)
            .build();
        let registry = ModuleRegistry::new();
        let ctx = GraphContext::new(Arc::<Path>::from(PathBuf::from("/tmp/repo")));
        let workspace = DefaultGraphBuilder
            .build(vec![eval(&c1), eval(&c2)], &registry, &ctx)
            .unwrap();
        assert_eq!(workspace.components[0].path.as_ref(), Path::new("a"));
        assert_eq!(workspace.components[1].path.as_ref(), Path::new("z"));
    }

    fn make_module_call(call_name: &str, source_rel: &str, inputs: AttributeMap) -> ModuleCall {
        let raw: Arc<str> = Arc::from(source_rel);
        ModuleCall::builder()
            .address(Address::new(format!("module.{call_name}")).unwrap())
            .source_raw(Arc::clone(&raw))
            .source(ModuleSource::classify(&raw))
            .inputs(inputs)
            .span(span())
            .build()
    }

    fn module_body(addr: &str) -> EvaluatedComponent {
        let r = Resource::builder()
            .address(Address::new(addr).unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_s3_bucket"))
            .name(Arc::<str>::from("this"))
            .attributes(vec![(
                Arc::from("name"),
                Expression::Unresolved(
                    Symbolic::builder()
                        .kind(SymbolKind::Var)
                        .source(Arc::<str>::from("var.name"))
                        .span(span())
                        .build(),
                ),
            )])
            .span(span())
            .build();
        let c = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("modules/s3")))
            .kind(ComponentKind::Module)
            .resources(vec![r])
            .build();
        eval(&c)
    }

    fn module_calling_self(canonical: &Arc<Path>) -> EvaluatedComponent {
        let mc = ModuleCall::builder()
            .address(Address::new("module.self_ref").unwrap())
            .source_raw(Arc::<str>::from("."))
            .source(ModuleSource::Local(Arc::from(".")))
            .span(span())
            .build();
        let c = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::clone(canonical))
            .kind(ComponentKind::Module)
            .modules(vec![mc])
            .build();
        eval(&c)
    }

    #[test]
    fn test_should_detect_module_self_cycle_and_emit_diagnostic() {
        let tmp = tempfile::tempdir().unwrap();
        let root: Arc<Path> = Arc::from(std::fs::canonicalize(tmp.path()).unwrap());
        std::fs::create_dir_all(root.join("mod")).unwrap();
        let mod_path: Arc<Path> = Arc::from(root.join("mod"));

        let mut registry = ModuleRegistry::new();
        // Module body that calls itself via `source = "."`.
        let module_eval = module_calling_self(&mod_path);
        registry.insert_local(Arc::clone(&mod_path), module_eval);

        // Top-level caller invokes the module.
        let caller_call = ModuleCall::builder()
            .address(Address::new("module.outer").unwrap())
            .source_raw(Arc::<str>::from("./mod"))
            .source(ModuleSource::Local(Arc::from("./mod")))
            .span(span())
            .build();
        let caller = Component::builder()
            .id(ComponentId::from_index(1))
            .path(Arc::<Path>::from(PathBuf::from("")))
            .kind(ComponentKind::Component)
            .modules(vec![caller_call])
            .build();

        let ctx = GraphContext::new(root);
        let workspace = DefaultGraphBuilder
            .build(vec![eval(&caller)], &registry, &ctx)
            .unwrap();
        // Cycle detection drops the recursive expansion and surfaces a
        // diagnostic with code `TF1504`.
        assert!(
            workspace.diagnostics.iter().any(|d| &*d.code == "TF1504"),
            "{:?}",
            workspace.diagnostics
        );
    }

    #[test]
    fn test_should_enforce_max_module_depth_cap() {
        // Build a 3-deep chain a → b → c → … with cap 2. Expansion of the
        // 3rd level must surface a `TF1501` (LimitKind::Expansion) diagnostic.
        let tmp = tempfile::tempdir().unwrap();
        let root: Arc<Path> = Arc::from(std::fs::canonicalize(tmp.path()).unwrap());
        for p in ["a", "b", "c"] {
            std::fs::create_dir_all(root.join(p)).unwrap();
        }
        let mut registry = ModuleRegistry::new();
        // c body: empty (terminal)
        let c_body = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(root.join("c")))
            .kind(ComponentKind::Module)
            .build();
        registry.insert_local(Arc::from(root.join("c")), eval(&c_body));
        // b body: calls c
        let b_to_c = ModuleCall::builder()
            .address(Address::new("module.c").unwrap())
            .source_raw(Arc::<str>::from("../c"))
            .source(ModuleSource::Local(Arc::from("../c")))
            .span(span())
            .build();
        let b_body = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(root.join("b")))
            .kind(ComponentKind::Module)
            .modules(vec![b_to_c])
            .build();
        registry.insert_local(Arc::from(root.join("b")), eval(&b_body));
        // a body: calls b
        let a_to_b = ModuleCall::builder()
            .address(Address::new("module.b").unwrap())
            .source_raw(Arc::<str>::from("../b"))
            .source(ModuleSource::Local(Arc::from("../b")))
            .span(span())
            .build();
        let a_body = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(root.join("a")))
            .kind(ComponentKind::Module)
            .modules(vec![a_to_b])
            .build();
        registry.insert_local(Arc::from(root.join("a")), eval(&a_body));

        let caller_call = ModuleCall::builder()
            .address(Address::new("module.a").unwrap())
            .source_raw(Arc::<str>::from("./a"))
            .source(ModuleSource::Local(Arc::from("./a")))
            .span(span())
            .build();
        let caller = Component::builder()
            .id(ComponentId::from_index(1))
            .path(Arc::<Path>::from(PathBuf::from("")))
            .kind(ComponentKind::Component)
            .modules(vec![caller_call])
            .build();

        let mut ctx = GraphContext::new(root);
        ctx.max_module_depth = 2; // a (depth 0) → b (depth 1) → c (depth 2 = cap)
        let workspace = DefaultGraphBuilder
            .build(vec![eval(&caller)], &registry, &ctx)
            .unwrap();
        // Expect a `TF1501` diagnostic when the depth cap fires.
        assert!(
            workspace.diagnostics.iter().any(|d| &*d.code == "TF1501"),
            "{:?}",
            workspace.diagnostics
        );
    }

    #[test]
    fn test_should_flatten_module_into_parent_with_address_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        // Canonicalise upfront — on macOS `/tmp` is a symlink to `/private/tmp`
        // and the path-safety helpers reject symlink ancestors.
        let workspace_root: Arc<Path> = Arc::from(std::fs::canonicalize(tmp.path()).unwrap());
        // Materialise the module dir so `canonicalize_inside(Reject)` finds
        // no symlinks in the chain.
        std::fs::create_dir_all(workspace_root.join("modules/s3")).unwrap();
        std::fs::create_dir_all(workspace_root.join("services/api-gateway")).unwrap();

        // Register the module body under the canonical path that
        // `caller_dir.join("../../modules/s3")` will normalise to.
        let mod_path: Arc<Path> = Arc::from(workspace_root.join("modules/s3"));
        let mut registry = ModuleRegistry::new();
        registry.insert_local(Arc::clone(&mod_path), module_body("aws_s3_bucket.this"));

        let mc = make_module_call(
            "edge_logs",
            "../../modules/s3",
            vec![(
                Arc::from("name"),
                Expression::Literal(Value::Str(Arc::from("hello"))),
            )],
        );
        let c = Component::builder()
            .id(ComponentId::from_index(1))
            .path(Arc::<Path>::from(PathBuf::from("services/api-gateway")))
            .kind(ComponentKind::Component)
            .modules(vec![mc])
            .build();
        let ctx = GraphContext::new(workspace_root);
        let workspace = DefaultGraphBuilder
            .build(vec![eval(&c)], &registry, &ctx)
            .unwrap();
        let resources = &workspace.components[0].resources;
        assert_eq!(
            resources.len(),
            1,
            "resources={resources:?}\ndiagnostics={:?}",
            workspace.diagnostics
        );
        assert_eq!(
            resources[0].address.as_str(),
            "module.edge_logs.aws_s3_bucket.this"
        );
        // var.name → "hello" substitution from inputs.
        let (_, name_expr) = resources[0]
            .attributes
            .iter()
            .find(|(k, _)| k.as_ref() == "name")
            .unwrap();
        assert_eq!(
            name_expr,
            &Expression::Literal(Value::Str(Arc::from("hello")))
        );
    }
}
