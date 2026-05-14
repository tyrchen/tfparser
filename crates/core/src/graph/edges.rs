//! Dependency-edge inference.
//!
//! Phase 8 / M5 — per [15-resource-graph.md § 4]. The collector walks every
//! resource and module call in a workspace and produces an [`Edge`] for each
//! reference it can resolve:
//!
//! - **`ExplicitDependsOn`** — entries of `resource.depends_on = [...]`.
//! - **`AttrRef`** — `Expression::Unresolved` nodes whose `address_hint` resolves to a sibling
//!   `aws_*` / `data.*` / `module.*` address.
//! - **`ModuleInput`** — when the same `AttrRef` appears inside a `module "x"` call's inputs.
//! - **`TerragruntDependency`** — `dependency "y" { config_path = "..." }` per
//!   `Component.terragrunt.dependencies`. Target is a synthetic `component.<rel>` address.
//!
//! All edges are de-duplicated on `(from, to, kind)` and sorted by
//! `(from, to, kind)` for deterministic Parquet output.
//!
//! [15-resource-graph.md § 4]: ../../../specs/15-resource-graph.md

use std::{collections::BTreeSet, path::Path, sync::Arc};

use crate::ir::{
    Address, Component, Edge, EdgeKind, Expression, ModuleCall, Resource, SymbolKind, Symbolic,
    Value, Workspace,
};

/// Walk `ws` and append every inferred edge to `ws.edges`. Existing edges
/// (e.g. populated by a future phase) are preserved; duplicates are removed
/// in the final sort.
pub fn collect_edges_in_place(ws: &mut Workspace) {
    let mut seen: BTreeSet<(String, String, EdgeKind)> = BTreeSet::new();
    let mut out: Vec<Edge> = std::mem::take(&mut ws.edges);
    for e in &out {
        seen.insert((
            e.from.as_str().to_string(),
            e.to.as_str().to_string(),
            e.kind,
        ));
    }

    let workspace_root: Arc<Path> = Arc::clone(&ws.root);
    for component in &ws.components {
        for r in &component.resources {
            collect_resource_edges(r, &mut out, &mut seen);
        }
        for m in &component.modules {
            collect_module_call_edges(m, &mut out, &mut seen);
        }
        if let Some(tg) = component.terragrunt.as_ref() {
            collect_terragrunt_edges(component, tg, &workspace_root, &mut out, &mut seen);
        }
    }

    // Deterministic order: `(from, to, kind)` ascending.
    out.sort_by(|a, b| {
        (a.from.as_str(), a.to.as_str(), a.kind.as_str()).cmp(&(
            b.from.as_str(),
            b.to.as_str(),
            b.kind.as_str(),
        ))
    });
    ws.edges = out;
}

fn collect_resource_edges(
    r: &Resource,
    out: &mut Vec<Edge>,
    seen: &mut BTreeSet<(String, String, EdgeKind)>,
) {
    // Explicit depends_on
    for target in &r.depends_on {
        push_edge(
            out,
            seen,
            Edge::builder()
                .from(r.address.clone())
                .to(target.clone())
                .kind(EdgeKind::ExplicitDependsOn)
                .span(r.span.clone())
                .build(),
        );
    }

    // Implicit attribute refs
    for (key, expr) in &r.attributes {
        let mut path = Vec::new();
        path.push(Arc::clone(key));
        walk_expression(expr, &mut path, &r.address, EdgeKind::AttrRef, out, seen);
    }
}

fn collect_module_call_edges(
    m: &ModuleCall,
    out: &mut Vec<Edge>,
    seen: &mut BTreeSet<(String, String, EdgeKind)>,
) {
    for (key, expr) in &m.inputs {
        let mut path = Vec::new();
        path.push(Arc::clone(key));
        walk_expression(
            expr,
            &mut path,
            &m.address,
            EdgeKind::ModuleInput,
            out,
            seen,
        );
    }
}

fn collect_terragrunt_edges(
    component: &Component,
    tg: &crate::ir::TerragruntConfig,
    workspace_root: &Arc<Path>,
    out: &mut Vec<Edge>,
    seen: &mut BTreeSet<(String, String, EdgeKind)>,
) {
    let Some(from) = component_address(&component.path) else {
        // Component path failed the `Address` charset allowlist —
        // discovery should have rejected it upstream (I-IR-2), so this
        // arm is reachable only via hand-built tests. Skip silently
        // rather than fabricate a placeholder edge.
        return;
    };
    for dep in &tg.dependencies {
        let target_rel: std::path::PathBuf = dep
            .config_path
            .strip_prefix(workspace_root.as_ref())
            .map_or_else(|_| dep.config_path.to_path_buf(), Path::to_path_buf);
        let normalised = normalise_relative(&target_rel);
        let Ok(to) = Address::new(format!("component.{normalised}")) else {
            continue;
        };
        push_edge(
            out,
            seen,
            Edge::builder()
                .from(from.clone())
                .to(to)
                .kind(EdgeKind::TerragruntDependency)
                .span(dep.span.clone())
                .build(),
        );
    }
}

/// Build a synthetic `component.<rel>` address. Slashes are kept (the
/// `Address` charset allowlist accepts `/`).
///
/// Returns `None` when `path` contains a byte outside the `Address`
/// allowlist. Every `Component.path` reaching this code has already
/// cleared the loader / discovery path-safety helpers (I-IR-2), so the
/// `None` arm is reachable only through hand-built test inputs — callers
/// drop the edge.
fn component_address(path: &Path) -> Option<Address> {
    let rel = normalise_relative(path);
    Address::new(format!("component.{rel}")).ok()
}

fn normalise_relative(p: &Path) -> String {
    let mut out = String::with_capacity(p.as_os_str().len());
    for (idx, c) in p.components().enumerate() {
        if idx > 0 {
            out.push('/');
        }
        match c {
            std::path::Component::Normal(s) => out.push_str(&s.to_string_lossy()),
            std::path::Component::CurDir => out.push('.'),
            std::path::Component::ParentDir => out.push_str(".."),
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                // Absolute components should not survive past the
                // workspace-root strip; if one does, keep a literal form
                // and let `Address::new` reject if it contains illegal
                // characters.
                out.push_str(&c.as_os_str().to_string_lossy());
            }
        }
    }
    out
}

fn push_edge(out: &mut Vec<Edge>, seen: &mut BTreeSet<(String, String, EdgeKind)>, edge: Edge) {
    let key = (
        edge.from.as_str().to_string(),
        edge.to.as_str().to_string(),
        edge.kind,
    );
    if seen.insert(key) {
        out.push(edge);
    }
}

/// Recursively walk an [`Expression`] and emit edges for every
/// `Unresolved` node whose `address_hint` looks like a `resource` / `data`
/// / `module` reference.
///
/// `path` is the dot-joined attribute name lineage used for the edge's
/// `attr` column — `path[0]` is the top-level attribute name; nested
/// objects extend it via `key.subkey`.
fn walk_expression(
    expr: &Expression,
    path: &mut Vec<Arc<str>>,
    from: &Address,
    kind: EdgeKind,
    out: &mut Vec<Edge>,
    seen: &mut BTreeSet<(String, String, EdgeKind)>,
) {
    match expr {
        Expression::Literal(_) => {}
        Expression::Unresolved(s) => emit_symbol_edge(s, path, from, kind, out, seen),
        Expression::BinaryOp { lhs, rhs, .. } => {
            walk_expression(lhs, path, from, kind, out, seen);
            walk_expression(rhs, path, from, kind, out, seen);
        }
        Expression::UnaryOp { operand, .. } => {
            walk_expression(operand, path, from, kind, out, seen);
        }
        Expression::TemplateConcat(parts) | Expression::Array(parts) => {
            for p in parts {
                walk_expression(p, path, from, kind, out, seen);
            }
        }
        Expression::Object(entries) => {
            for (k, v) in entries {
                walk_expression(k, path, from, kind, out, seen);
                let pushed = match k {
                    Expression::Literal(Value::Str(s)) => {
                        path.push(Arc::clone(s));
                        true
                    }
                    _ => false,
                };
                walk_expression(v, path, from, kind, out, seen);
                if pushed {
                    path.pop();
                }
            }
        }
        Expression::FuncCall(call) => {
            for a in &call.args {
                walk_expression(a, path, from, kind, out, seen);
            }
        }
        Expression::Conditional(c) => {
            walk_expression(&c.cond, path, from, kind, out, seen);
            walk_expression(&c.then_branch, path, from, kind, out, seen);
            walk_expression(&c.else_branch, path, from, kind, out, seen);
        }
        Expression::For(f) => {
            walk_expression(&f.collection, path, from, kind, out, seen);
            walk_expression(&f.value, path, from, kind, out, seen);
            if let Some(k) = &f.key {
                walk_expression(k, path, from, kind, out, seen);
            }
            if let Some(c) = &f.cond {
                walk_expression(c, path, from, kind, out, seen);
            }
        }
    }
}

fn emit_symbol_edge(
    s: &Symbolic,
    path: &[Arc<str>],
    from: &Address,
    kind: EdgeKind,
    out: &mut Vec<Edge>,
    seen: &mut BTreeSet<(String, String, EdgeKind)>,
) {
    // Only edges that point at a workspace IR node (resource / data /
    // module) get inferred. var / local / iteration / path are intra-IR
    // and don't make a graph edge.
    let target = match s.kind {
        SymbolKind::Resource | SymbolKind::Data | SymbolKind::Module => {
            match head_resource_address(&s.source) {
                Some(addr) => addr,
                None => return,
            }
        }
        _ => return,
    };
    let attr = render_attr(path);
    push_edge(
        out,
        seen,
        Edge::builder()
            .from(from.clone())
            .to(target)
            .kind(kind)
            .attr(Some(attr))
            .span(s.span.clone())
            .build(),
    );
}

fn render_attr(path: &[Arc<str>]) -> Arc<str> {
    let joined = path.iter().map(AsRef::as_ref).collect::<Vec<_>>().join(".");
    Arc::from(joined.as_str())
}

/// Parse the *head* of a `Symbolic.source` (e.g. `aws_iam_role.r.arn` or
/// `data.aws_caller_identity.current.account_id`) into a Terraform address
/// (`aws_iam_role.r` / `data.aws_caller_identity.current` /
/// `module.<name>`).
fn head_resource_address(source: &str) -> Option<Address> {
    let head = match source.strip_prefix("data.") {
        Some(rest) => {
            // data.<type>.<name>.<...>
            let mut it = rest.splitn(3, '.');
            let type_ = it.next()?;
            let name = it.next()?;
            format!("data.{type_}.{name}")
        }
        None if source.starts_with("module.") => {
            // module.<name>.<output>
            let rest = source.strip_prefix("module.")?;
            let name = rest.split('.').next()?;
            format!("module.{name}")
        }
        None => {
            // <type>.<name>.<...>
            let mut it = source.splitn(3, '.');
            let type_ = it.next()?;
            let name = it.next()?;
            format!("{type_}.{name}")
        }
    };
    Address::new(head).ok()
}

/// Build a [`Span::synthetic`]-shaped span for tests.
#[cfg(test)]
#[must_use]
fn synthetic_span() -> crate::ir::Span {
    crate::ir::Span::synthetic()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ir::{
        Address, AttributeMap, Component, ComponentId, ComponentKind, DependencyBlock, Expression,
        ResourceKind, Symbolic, TerragruntConfig, Value,
    };

    fn symbolic_resource(source: &str) -> Symbolic {
        Symbolic::builder()
            .kind(SymbolKind::Resource)
            .source(Arc::<str>::from(source))
            .span(synthetic_span())
            .build()
    }

    fn symbolic_module(source: &str) -> Symbolic {
        Symbolic::builder()
            .kind(SymbolKind::Module)
            .source(Arc::<str>::from(source))
            .span(synthetic_span())
            .build()
    }

    fn make_resource(addr: &str, attrs: AttributeMap) -> Resource {
        Resource::builder()
            .address(Address::new(addr).unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from(
                addr.split('.').next().unwrap_or("aws_iam_role"),
            ))
            .name(Arc::<str>::from(addr.split('.').nth(1).unwrap_or("r")))
            .attributes(attrs)
            .span(synthetic_span())
            .build()
    }

    fn workspace_with(component: Component) -> Workspace {
        Workspace::builder()
            .root(Arc::<Path>::from(PathBuf::from("/repo")))
            .components(vec![component])
            .build()
    }

    #[test]
    fn test_head_resource_address_for_attr_ref() {
        assert_eq!(
            head_resource_address("aws_iam_role.r.arn")
                .unwrap()
                .as_str(),
            "aws_iam_role.r"
        );
        assert_eq!(
            head_resource_address("data.aws_caller_identity.current.account_id")
                .unwrap()
                .as_str(),
            "data.aws_caller_identity.current"
        );
        assert_eq!(
            head_resource_address("module.vpc.id").unwrap().as_str(),
            "module.vpc"
        );
        // bare `var.foo` → None (not a graph edge target).
        assert!(head_resource_address("foo").is_none());
    }

    #[test]
    fn test_explicit_depends_on_emits_edges() {
        let r = Resource::builder()
            .address(Address::new("aws_iam_role.r").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_iam_role"))
            .name(Arc::<str>::from("r"))
            .depends_on(vec![
                Address::new("aws_iam_policy.p").unwrap(),
                Address::new("aws_iam_policy.q").unwrap(),
            ])
            .span(synthetic_span())
            .build();
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("svc")))
            .kind(ComponentKind::Component)
            .resources(vec![r])
            .build();
        let mut ws = workspace_with(component);
        collect_edges_in_place(&mut ws);
        let explicit: Vec<&Edge> = ws
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::ExplicitDependsOn)
            .collect();
        assert_eq!(explicit.len(), 2, "{:?}", ws.edges);
        assert_eq!(explicit[0].to.as_str(), "aws_iam_policy.p");
        assert_eq!(explicit[1].to.as_str(), "aws_iam_policy.q");
    }

    #[test]
    fn test_attr_ref_edge_from_unresolved_symbolic() {
        let attrs: AttributeMap = vec![(
            Arc::from("policy"),
            Expression::Unresolved(symbolic_resource("aws_iam_policy.p.arn")),
        )];
        let r = make_resource("aws_iam_role.r", attrs);
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("svc")))
            .kind(ComponentKind::Component)
            .resources(vec![r])
            .build();
        let mut ws = workspace_with(component);
        collect_edges_in_place(&mut ws);
        let attr_refs: Vec<&Edge> = ws
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::AttrRef)
            .collect();
        assert_eq!(attr_refs.len(), 1);
        assert_eq!(attr_refs[0].from.as_str(), "aws_iam_role.r");
        assert_eq!(attr_refs[0].to.as_str(), "aws_iam_policy.p");
        assert_eq!(attr_refs[0].attr.as_deref(), Some("policy"));
    }

    #[test]
    fn test_module_input_edge_from_module_call() {
        let m = ModuleCall::builder()
            .address(Address::new("module.app").unwrap())
            .source_raw(Arc::<str>::from("./m"))
            .source(crate::ir::ModuleSource::Local(Arc::from("./m")))
            .inputs(vec![(
                Arc::from("vpc_id"),
                Expression::Unresolved(symbolic_module("module.vpc.id")),
            )])
            .span(synthetic_span())
            .build();
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("svc")))
            .kind(ComponentKind::Component)
            .modules(vec![m])
            .build();
        let mut ws = workspace_with(component);
        collect_edges_in_place(&mut ws);
        let module_inputs: Vec<&Edge> = ws
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::ModuleInput)
            .collect();
        assert_eq!(module_inputs.len(), 1);
        assert_eq!(module_inputs[0].from.as_str(), "module.app");
        assert_eq!(module_inputs[0].to.as_str(), "module.vpc");
    }

    #[test]
    fn test_terragrunt_dependency_emits_component_edge() {
        let dep = DependencyBlock::builder()
            .name(Arc::<str>::from("vpc"))
            .config_path(Arc::<Path>::from(PathBuf::from("/repo/svc/vpc")))
            .span(synthetic_span())
            .build();
        let tg = TerragruntConfig::builder()
            .component_dir(Arc::<Path>::from(PathBuf::from("/repo/svc/app")))
            .dependencies(vec![dep])
            .build();
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("svc/app")))
            .kind(ComponentKind::Component)
            .terragrunt(Some(tg))
            .build();
        let mut ws = workspace_with(component);
        collect_edges_in_place(&mut ws);
        let tg_edges: Vec<&Edge> = ws
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::TerragruntDependency)
            .collect();
        assert_eq!(tg_edges.len(), 1);
        assert_eq!(tg_edges[0].from.as_str(), "component.svc/app");
        assert_eq!(tg_edges[0].to.as_str(), "component.svc/vpc");
    }

    #[test]
    fn test_edge_collection_deduplicates_and_sorts() {
        let attrs: AttributeMap = vec![
            (
                Arc::from("a"),
                Expression::Unresolved(symbolic_resource("aws_iam_policy.p.arn")),
            ),
            // Same target — must be deduplicated against attr `a` above
            // because (from, to, AttrRef) is unique.
            (
                Arc::from("b"),
                Expression::Unresolved(symbolic_resource("aws_iam_policy.p.arn")),
            ),
            (
                Arc::from("c"),
                Expression::Unresolved(symbolic_resource("aws_iam_policy.q.arn")),
            ),
        ];
        let r = make_resource("aws_iam_role.r", attrs);
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("svc")))
            .kind(ComponentKind::Component)
            .resources(vec![r])
            .build();
        let mut ws = workspace_with(component);
        collect_edges_in_place(&mut ws);
        let attr_refs: Vec<&Edge> = ws
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::AttrRef)
            .collect();
        // Deduplicated: only two unique (from, to, AttrRef) pairs.
        assert_eq!(attr_refs.len(), 2);
        // Sorted ascending by `to`.
        assert_eq!(attr_refs[0].to.as_str(), "aws_iam_policy.p");
        assert_eq!(attr_refs[1].to.as_str(), "aws_iam_policy.q");
    }

    #[test]
    fn test_walk_expression_descends_into_array_and_object() {
        // policies = [{policy = aws_iam_policy.p.arn}, ...]
        let attrs: AttributeMap = vec![(
            Arc::from("policies"),
            Expression::Array(vec![Expression::Object(vec![(
                Expression::Literal(Value::Str(Arc::from("policy"))),
                Expression::Unresolved(symbolic_resource("aws_iam_policy.p.arn")),
            )])]),
        )];
        let r = make_resource("aws_iam_role.r", attrs);
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("svc")))
            .kind(ComponentKind::Component)
            .resources(vec![r])
            .build();
        let mut ws = workspace_with(component);
        collect_edges_in_place(&mut ws);
        let attr_refs: Vec<&Edge> = ws
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::AttrRef)
            .collect();
        assert_eq!(attr_refs.len(), 1);
        assert_eq!(attr_refs[0].to.as_str(), "aws_iam_policy.p");
    }
}
