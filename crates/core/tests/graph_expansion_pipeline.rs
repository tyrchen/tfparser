//! Phase 5 integration test — exercises discovery → loader → projection →
//! evaluator → graph end-to-end against the `large-monorepo` fixture and
//! pins the M2 exit criteria from `specs/91-impl-plan.md § 8`:
//!
//! - Nested-module fixture's module bodies appear as Parquet rows under `module_path`.
//! - `count = 3` literal expands; `count = var.foo` (unresolved) emits one template row.
//! - Address uniqueness invariant holds (I-GRAPH-1).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use tfparser_core::{
    DefaultGraphBuilder, EnvVarMode, EvalContext, EvalLimits, EvaluatedComponent, Evaluator,
    FuncRegistry, GraphBuilder, GraphContext, HclEvaluator, ModuleRegistry,
    discovery::{Discoverer, DiscoveryOptions, FsDiscoverer},
    ir::{Address, ComponentId, ComponentKind, Expression, Resource, ResourceKind, Span, Value},
    loader::{HclEditLoader, LoadContext, Loader, LoaderLimits, SourceMap},
    projection::project_component,
};

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .expect("workspace root")
}

fn fixture(name: &str) -> PathBuf {
    workspace_root().join("fixtures").join(name)
}

/// End-to-end build for one fixture: returns the workspace + each
/// component's raw evaluator output, suitable for assertions.
fn build_workspace(
    root: &Path,
    repo_vars: &tfparser_core::ir::Map,
) -> tfparser_core::ir::Workspace {
    let canonical_root = std::fs::canonicalize(root).expect("canonicalize root");
    let canonical_arc: Arc<Path> = Arc::from(canonical_root.clone());

    // Discovery.
    let discovered = FsDiscoverer
        .discover(&canonical_root, &DiscoveryOptions::defaults())
        .expect("discovery");

    // Loader: parse every component and module dir.
    let sources = SourceMap::new();
    let limits = LoaderLimits::default();
    let ctx = LoadContext::new(&discovered.root, &sources, &limits);

    let mut diagnostics: Vec<tfparser_core::Diagnostic> = Vec::new();
    let mut next_component_id: usize = 0;
    let mut next_index = || {
        let id = ComponentId::from_index(next_component_id);
        next_component_id += 1;
        id
    };

    // Build the modules' EvaluatedComponents first so the registry is
    // populated before we expand any caller.
    let mut module_evals: Vec<EvaluatedComponent> = Vec::new();
    let mut registry = ModuleRegistry::new();
    for dir in &discovered.modules {
        let raw = HclEditLoader.load(dir, &ctx).expect("load module");
        diagnostics.extend(raw.diagnostics.iter().cloned());
        let mut diag_buf: Vec<tfparser_core::Diagnostic> = Vec::new();
        let component = project_component(&raw, next_index(), &mut diag_buf);
        diagnostics.extend(diag_buf);

        let eval_ctx = EvalContext::new(
            Arc::clone(&canonical_arc),
            None,
            EnvVarMode::default(),
            tfparser_core::ir::Map::new(),
            tfparser_core::ir::Map::new(),
            Arc::new(FuncRegistry::default_with_stdlib()),
            EvalLimits::default(),
        );
        let evaluated = HclEvaluator::new()
            .evaluate(&component, &eval_ctx)
            .expect("evaluate module");
        let mod_canonical: Arc<Path> = Arc::from(canonical_root.join(&dir.path));
        registry.insert_local(mod_canonical, evaluated.clone());
        module_evals.push(evaluated);
    }
    // The same evaluator handle for components.
    let evaluator = HclEvaluator::new();

    // Then components.
    let mut component_evals: Vec<EvaluatedComponent> = Vec::new();
    for dir in &discovered.components {
        let raw = HclEditLoader.load(dir, &ctx).expect("load component");
        diagnostics.extend(raw.diagnostics.iter().cloned());
        let mut diag_buf: Vec<tfparser_core::Diagnostic> = Vec::new();
        let component = project_component(&raw, next_index(), &mut diag_buf);
        diagnostics.extend(diag_buf);

        let eval_ctx = EvalContext::new(
            Arc::clone(&canonical_arc),
            None,
            EnvVarMode::default(),
            repo_vars.clone(),
            tfparser_core::ir::Map::new(),
            Arc::new(FuncRegistry::default_with_stdlib()),
            EvalLimits::default(),
        );
        let evaluated = evaluator
            .evaluate(&component, &eval_ctx)
            .expect("evaluate component");
        component_evals.push(evaluated);
    }

    let ctx = GraphContext::new(canonical_arc);
    let mut combined = module_evals;
    combined.extend(component_evals);
    DefaultGraphBuilder::new()
        .build(combined, &registry, &ctx)
        .expect("graph build")
}

#[test]
fn test_module_bodies_emit_as_resources_under_module_path() {
    // The `large-monorepo` fixture has `services/api-gateway` calling
    // `module "edge_logs" { source = "../../modules/s3-bucket" }`. After
    // expansion, the module's `aws_s3_bucket.this` lands at
    // `module.edge_logs.aws_s3_bucket.this` under the api-gateway
    // component.
    let root = fixture("large-monorepo");
    let workspace = build_workspace(&root, &Vec::new());

    let api_gw = workspace
        .components
        .iter()
        .find(|c| c.path.ends_with("services/api-gateway"))
        .unwrap_or_else(|| {
            panic!(
                "api-gateway component missing; got: {:?}",
                paths(&workspace)
            )
        });

    let module_resources: Vec<&Resource> = api_gw
        .resources
        .iter()
        .filter(|r| r.address.as_str().starts_with("module.edge_logs."))
        .collect();
    assert!(
        !module_resources.is_empty(),
        "no module-expanded resources; saw addresses: {:?}",
        api_gw
            .resources
            .iter()
            .map(|r| r.address.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        module_resources
            .iter()
            .any(|r| r.address.as_str() == "module.edge_logs.aws_s3_bucket.this"),
        "expected module.edge_logs.aws_s3_bucket.this in {:?}",
        module_resources
            .iter()
            .map(|r| r.address.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_address_uniqueness_invariant_holds_across_workspace() {
    // I-GRAPH-1: every resource address in the final IR is unique within
    // the workspace.
    let root = fixture("large-monorepo");
    let workspace = build_workspace(&root, &Vec::new());
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for c in &workspace.components {
        for r in &c.resources {
            let key = r.address.as_str();
            assert!(
                seen.insert(key),
                "address collision at {key} in component {}",
                c.path.display()
            );
        }
    }
}

#[test]
fn test_count_literal_expands_to_indexed_addresses() {
    // Synthesise a component with `count = 3` and run it through the
    // graph builder. The literal must expand to three indexed rows.
    use tfparser_core::ir::Component;
    let tmp = tempfile::tempdir().unwrap();
    let canonical_root: Arc<Path> = Arc::from(std::fs::canonicalize(tmp.path()).unwrap());
    let registry = ModuleRegistry::new();
    let r = Resource::builder()
        .address(Address::new("aws_iam_role.r").unwrap())
        .kind(ResourceKind::Managed)
        .type_(Arc::<str>::from("aws_iam_role"))
        .name(Arc::<str>::from("r"))
        .count_expr(Some(Expression::Literal(Value::Int(3))))
        .span(Span::synthetic())
        .build();
    let c = Component::builder()
        .id(ComponentId::from_index(0))
        .path(Arc::<Path>::from(PathBuf::from("svc")))
        .kind(ComponentKind::Component)
        .resources(vec![r])
        .build();
    let evaluated = EvaluatedComponent::from_component(c);
    let ctx = GraphContext::new(canonical_root);
    let workspace = DefaultGraphBuilder::new()
        .build(vec![evaluated], &registry, &ctx)
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
fn test_unresolved_count_emits_single_template_row() {
    use tfparser_core::ir::{Component, SymbolKind, Symbolic};
    let tmp = tempfile::tempdir().unwrap();
    let canonical_root: Arc<Path> = Arc::from(std::fs::canonicalize(tmp.path()).unwrap());
    let registry = ModuleRegistry::new();
    let r = Resource::builder()
        .address(Address::new("aws_iam_role.r").unwrap())
        .kind(ResourceKind::Managed)
        .type_(Arc::<str>::from("aws_iam_role"))
        .name(Arc::<str>::from("r"))
        .count_expr(Some(Expression::Unresolved(
            Symbolic::builder()
                .kind(SymbolKind::Var)
                .source(Arc::<str>::from("var.foo"))
                .span(Span::synthetic())
                .build(),
        )))
        .span(Span::synthetic())
        .build();
    let c = Component::builder()
        .id(ComponentId::from_index(0))
        .path(Arc::<Path>::from(PathBuf::from("svc")))
        .kind(ComponentKind::Component)
        .resources(vec![r])
        .build();
    let evaluated = EvaluatedComponent::from_component(c);
    let ctx = GraphContext::new(canonical_root);
    let workspace = DefaultGraphBuilder::new()
        .build(vec![evaluated], &registry, &ctx)
        .unwrap();
    let resources = &workspace.components[0].resources;
    assert_eq!(resources.len(), 1, "{resources:?}");
    assert_eq!(resources[0].address.as_str(), "aws_iam_role.r");
    assert!(resources[0].count_expr.is_some());
}

fn paths(workspace: &tfparser_core::ir::Workspace) -> Vec<String> {
    workspace
        .components
        .iter()
        .map(|c| c.path.display().to_string())
        .collect()
}
