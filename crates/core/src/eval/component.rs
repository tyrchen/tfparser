//! Top-level evaluator.
//!
//! [`Evaluator::evaluate`] is the Phase 4 entry point: it threads an
//! [`EvalContext`] through one [`Component`] (post-projection) and returns
//! an [`EvaluatedComponent`] with every reachable expression reduced as far
//! as the evaluator can. Per [13-evaluator.md § 3] the pipeline is:
//!
//! 1. Bind variables: defaults eagerly reduced; `repo_vars` overrides.
//! 2. Solve locals with the worklist algorithm; surface cycles as a diagnostic.
//! 3. Reduce providers / resources / modules / outputs.
//!
//! Failures inside any pass become [`Diagnostic`]s attached to the returned
//! `EvaluatedComponent.diagnostics`. The evaluator never aborts the whole
//! component on a single bad expression — best-effort by contract
//! ([99-key-decisions.md] D4).
//!
//! [13-evaluator.md § 3]: ../../../specs/13-evaluator.md
//! [99-key-decisions.md]: ../../../specs/99-key-decisions.md

use std::sync::Arc;

use crate::{
    Result,
    diagnostic::{Diagnostic, Severity},
    eval::{
        context::EvalContext,
        error::EvalError,
        locals::solve_locals,
        reduce::{Scope, reduce_expression},
    },
    ir::{
        AttributeMap, Component, Expression, Local, Map, ModuleCall, Output, ProviderBlock,
        Resource, Variable,
    },
};

/// The contract every evaluator implements. Phase 4 ships exactly one
/// implementation: [`HclEvaluator`].
pub trait Evaluator: Send + Sync + std::fmt::Debug {
    /// Reduce `component` against `ctx`, returning the resolved IR plus
    /// any diagnostics surfaced during evaluation.
    ///
    /// # Errors
    ///
    /// Only fatal errors (would-leave-the-IR-malformed) bubble out as
    /// [`crate::Error`]. Recoverable failures (cycle in locals, function
    /// call errors, sandboxed file rejects) attach to the returned
    /// `EvaluatedComponent.diagnostics` instead.
    fn evaluate(&self, component: &Component, ctx: &EvalContext) -> Result<EvaluatedComponent>;
}

/// Default evaluator implementation.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct HclEvaluator;

impl HclEvaluator {
    /// Construct a new evaluator. Free-standing convenience; the default
    /// `HclEvaluator::default()` is equivalent.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Post-evaluation view of a [`Component`].
///
/// All collections are owned; the raw component is held behind an `Arc` so
/// span lookups remain cheap (per [13-evaluator.md § 2]).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct EvaluatedComponent {
    /// The original component (kept for spans).
    pub raw: Arc<Component>,
    /// Variables with `default` reduced where possible.
    pub variables: Vec<Variable>,
    /// Locals after the fixpoint pass; each [`Local::value`] is either
    /// fully resolved or a partially reduced subtree.
    pub locals: Vec<Local>,
    /// Provider blocks with `region_expr` / `profile_expr` / `assume_role`
    /// expressions reduced.
    pub providers: Vec<ProviderBlock>,
    /// Resources with `count_expr`, `for_each_expr`, and every attribute
    /// reduced.
    pub resources: Vec<Resource>,
    /// Module call sites with inputs reduced.
    pub modules: Vec<ModuleCall>,
    /// Output blocks with their value reduced.
    pub outputs: Vec<Output>,
    /// Per-evaluation diagnostics — appended to the workspace's diagnostics
    /// vector by the orchestrator (Phase 5).
    pub diagnostics: Vec<Diagnostic>,
}

impl Evaluator for HclEvaluator {
    #[tracing::instrument(
        skip(self, component, ctx),
        fields(
            component_id = ?component.id,
            component_path = %component.path.display(),
            // repo_vars / cascade_locals can carry secrets-shaped strings
            // (`TF_VAR_*` from env) — record only counts, never values.
            n_repo_vars = ctx.repo_vars.len(),
            n_cascade_locals = ctx.cascade_locals.len(),
        ),
    )]
    fn evaluate(&self, component: &Component, ctx: &EvalContext) -> Result<EvaluatedComponent> {
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Step 1 — bind variables.
        let (variables, var_bindings) = bind_variables(&component.variables, ctx);

        // Build initial vars: repo_vars (CLI / .tfvars) overrides component
        // defaults.
        let mut vars: Map = var_bindings;
        for (k, v) in &ctx.repo_vars {
            override_or_push(&mut vars, k, v);
        }

        // Cascade locals start as initial locals namespace; component
        // locals add on top.
        let mut locals_namespace: Map = ctx.cascade_locals.clone();

        // Step 2 — solve locals.
        let mut scope = Scope::new(
            vars.clone(),
            locals_namespace.clone(),
            ctx.workspace_root.as_ref(),
            &ctx.env_vars,
            &ctx.limits,
            &ctx.funcs,
            ctx.environment.clone(),
        );

        let locals = match solve_locals(&component.locals, &mut scope) {
            Ok(reduced) => reduced,
            Err(EvalError::Cycle { participants }) => {
                let participant_names: Vec<String> = participants
                    .iter()
                    .map(|p| p.as_str().to_string())
                    .collect();
                let summary = participant_names.join(", ");
                diagnostics.push(
                    Diagnostic::new(
                        Severity::Error,
                        "TF1401",
                        format!("cycle in locals: {summary}"),
                    )
                    .with_span(component_span_for_diag(component)),
                );
                // Skip resolving locals — they stay as their source
                // expressions in the returned component.
                component.locals.clone()
            }
            Err(other) => {
                diagnostics.push(Diagnostic::new(
                    Severity::Warn,
                    "TF1499",
                    format!("locals reduction failed: {other}"),
                ));
                component.locals.clone()
            }
        };

        // Inherit successful locals into the namespace.
        for l in &locals {
            if let Expression::Literal(v) = &l.value {
                override_or_push(&mut locals_namespace, &l.name, v);
            }
        }
        scope.locals = locals_namespace;

        // Step 3 — reduce providers / resources / modules / outputs.
        let providers = component
            .providers
            .iter()
            .map(|p| reduce_provider(p, &scope))
            .collect();
        let resources = component
            .resources
            .iter()
            .map(|r| reduce_resource(r, &scope))
            .collect();
        let modules = component
            .modules
            .iter()
            .map(|m| reduce_module(m, &scope))
            .collect();
        let outputs = component
            .outputs
            .iter()
            .map(|o| reduce_output(o, &scope))
            .collect();

        Ok(EvaluatedComponent {
            raw: Arc::new(component.clone()),
            variables,
            locals,
            providers,
            resources,
            modules,
            outputs,
            diagnostics,
        })
    }
}

/// Bind variables: defaults eagerly reduced against an empty scope.
/// Returns both the post-reduction variable list (for the IR) and a Map
/// suitable for seeding the reducer's `var.*` namespace.
fn bind_variables(variables: &[Variable], ctx: &EvalContext) -> (Vec<Variable>, Map) {
    let mut out: Vec<Variable> = Vec::with_capacity(variables.len());
    let mut bindings: Map = Vec::new();

    // Reduce variable defaults against a *minimal* scope (no locals yet,
    // no other variable bindings). Variables in Terraform cannot reference
    // other variables — this is a parse-time rule (I-EVAL-2).
    let empty = Map::new();
    let minimal_scope = Scope::new(
        Map::new(),
        empty,
        ctx.workspace_root.as_ref(),
        &ctx.env_vars,
        &ctx.limits,
        &ctx.funcs,
        ctx.environment.clone(),
    );

    for var in variables {
        let reduced_default = var
            .default
            .as_ref()
            .map(|d| reduce_expression(d, &minimal_scope));

        let bound: Option<&crate::ir::Value> = reduced_default.as_ref().and_then(|d| match d {
            Expression::Literal(v) => Some(v),
            _ => None,
        });
        if let Some(v) = bound {
            bindings.push((Arc::clone(&var.name), v.clone()));
        }

        out.push(
            Variable::builder()
                .name(Arc::clone(&var.name))
                .description(var.description.clone())
                .type_expr(var.type_expr.clone())
                .default(reduced_default)
                .sensitive(var.sensitive)
                .span(var.span.clone())
                .build(),
        );
    }

    (out, bindings)
}

fn override_or_push(map: &mut Map, key: &Arc<str>, value: &crate::ir::Value) {
    if let Some(slot) = map.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value.clone();
    } else {
        map.push((Arc::clone(key), value.clone()));
    }
}

fn reduce_attrs(attrs: &AttributeMap, scope: &Scope<'_>) -> AttributeMap {
    attrs
        .iter()
        .map(|(k, v)| (Arc::clone(k), reduce_expression(v, scope)))
        .collect()
}

fn reduce_provider(p: &ProviderBlock, scope: &Scope<'_>) -> ProviderBlock {
    ProviderBlock::builder()
        .local_name(Arc::clone(&p.local_name))
        .alias(p.alias.clone())
        .source_addr(p.source_addr.clone())
        .region_expr(p.region_expr.as_ref().map(|e| reduce_expression(e, scope)))
        .profile_expr(p.profile_expr.as_ref().map(|e| reduce_expression(e, scope)))
        .assume_role(p.assume_role.clone())
        .raw(reduce_attrs(&p.raw, scope))
        .span(p.span.clone())
        .build()
}

fn reduce_resource(r: &Resource, scope: &Scope<'_>) -> Resource {
    Resource::builder()
        .address(r.address.clone())
        .kind(r.kind)
        .type_(Arc::clone(&r.type_))
        .name(Arc::clone(&r.name))
        .provider_ref(r.provider_ref.clone())
        .count_expr(r.count_expr.as_ref().map(|e| reduce_expression(e, scope)))
        .for_each_expr(
            r.for_each_expr
                .as_ref()
                .map(|e| reduce_expression(e, scope)),
        )
        .depends_on(r.depends_on.clone())
        .attributes(reduce_attrs(&r.attributes, scope))
        .span(r.span.clone())
        .build()
}

fn reduce_module(m: &ModuleCall, scope: &Scope<'_>) -> ModuleCall {
    ModuleCall::builder()
        .address(m.address.clone())
        .source_raw(Arc::clone(&m.source_raw))
        .source(m.source.clone())
        .resolved(m.resolved)
        .providers(m.providers.clone())
        .inputs(reduce_attrs(&m.inputs, scope))
        .count_expr(m.count_expr.as_ref().map(|e| reduce_expression(e, scope)))
        .for_each_expr(
            m.for_each_expr
                .as_ref()
                .map(|e| reduce_expression(e, scope)),
        )
        .span(m.span.clone())
        .build()
}

fn reduce_output(o: &Output, scope: &Scope<'_>) -> Output {
    Output::builder()
        .name(Arc::clone(&o.name))
        .value(reduce_expression(&o.value, scope))
        .description(o.description.clone())
        .sensitive(o.sensitive)
        .span(o.span.clone())
        .build()
}

fn component_span_for_diag(component: &Component) -> crate::ir::Span {
    // Use the first source file's path when one is recorded; else fall
    // back to a synthetic span. The byte range / line / column are always
    // the synthetic 1:1 placeholders — cycle diagnostics are not anchored
    // to a specific reference site (the cycle has many).
    if let Some(file) = component.files.first() {
        return crate::ir::Span::new(Arc::clone(&file.path), 0..0, 1, 1)
            .unwrap_or_else(|_| crate::ir::Span::synthetic());
    }
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
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::{
        eval::{context::EvalLimits, registry::FuncRegistry},
        ir::{
            Address, BinaryOp, ComponentId, ComponentKind, Expression, Local, ProviderBlock,
            Resource, ResourceKind, Span, SymbolKind, Symbolic, Value, Variable,
        },
    };

    fn make_ctx() -> EvalContext {
        EvalContext {
            workspace_root: Arc::from(Path::new("/tmp/repo")),
            environment: None,
            env_vars: super::super::EnvVarMode::default(),
            repo_vars: vec![(Arc::from("region"), Value::Str(Arc::from("us-east-2")))],
            cascade_locals: Vec::new(),
            funcs: Arc::new(FuncRegistry::default_with_stdlib()),
            limits: EvalLimits::default(),
        }
    }

    fn var(name: &str) -> Variable {
        Variable::builder()
            .name(Arc::<str>::from(name))
            .span(Span::synthetic())
            .build()
    }

    fn var_ref(name: &str, kind: SymbolKind) -> Expression {
        Expression::Unresolved(
            Symbolic::builder()
                .kind(kind)
                .source(Arc::<str>::from(name))
                .span(Span::synthetic())
                .build(),
        )
    }

    #[test]
    fn test_should_resolve_provider_region_from_var() {
        let provider = ProviderBlock::builder()
            .local_name(Arc::<str>::from("aws"))
            .region_expr(Some(var_ref("var.region", SymbolKind::Var)))
            .span(Span::synthetic())
            .build();
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("c")))
            .kind(ComponentKind::Component)
            .variables(vec![var("region")])
            .providers(vec![provider])
            .build();
        let evald = HclEvaluator.evaluate(&component, &make_ctx()).unwrap();
        let region = evald.providers[0].region_expr.as_ref().unwrap();
        assert_eq!(
            region,
            &Expression::Literal(Value::Str(Arc::from("us-east-2")))
        );
    }

    #[test]
    fn test_should_use_default_when_no_repo_var() {
        let mut ctx = make_ctx();
        ctx.repo_vars.clear();
        let var_block = Variable::builder()
            .name(Arc::<str>::from("region"))
            .default(Some(Expression::Literal(Value::Str(Arc::from(
                "us-west-1",
            )))))
            .span(Span::synthetic())
            .build();
        let provider = ProviderBlock::builder()
            .local_name(Arc::<str>::from("aws"))
            .region_expr(Some(var_ref("var.region", SymbolKind::Var)))
            .span(Span::synthetic())
            .build();
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("c")))
            .kind(ComponentKind::Component)
            .variables(vec![var_block])
            .providers(vec![provider])
            .build();
        let evald = HclEvaluator.evaluate(&component, &ctx).unwrap();
        let region = evald.providers[0].region_expr.as_ref().unwrap();
        assert_eq!(
            region,
            &Expression::Literal(Value::Str(Arc::from("us-west-1")))
        );
    }

    #[test]
    fn test_should_resolve_local_from_var() {
        // local.zone = "zone-" + var.region
        let local = Local::builder()
            .name(Arc::<str>::from("zone"))
            .value(Expression::BinaryOp {
                op: BinaryOp::Add,
                lhs: Box::new(Expression::Literal(Value::Str(Arc::from("zone-")))),
                rhs: Box::new(var_ref("var.region", SymbolKind::Var)),
                span: Span::synthetic(),
            })
            .span(Span::synthetic())
            .build();
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("c")))
            .kind(ComponentKind::Component)
            .variables(vec![var("region")])
            .locals(vec![local])
            .build();
        let evald = HclEvaluator.evaluate(&component, &make_ctx()).unwrap();
        assert_eq!(
            evald.locals[0].value,
            Expression::Literal(Value::Str(Arc::from("zone-us-east-2")))
        );
    }

    #[test]
    fn test_should_emit_cycle_diagnostic() {
        let a = Local::builder()
            .name(Arc::<str>::from("a"))
            .value(var_ref("local.b", SymbolKind::Local))
            .span(Span::synthetic())
            .build();
        let b = Local::builder()
            .name(Arc::<str>::from("b"))
            .value(var_ref("local.a", SymbolKind::Local))
            .span(Span::synthetic())
            .build();
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("c")))
            .kind(ComponentKind::Component)
            .locals(vec![a, b])
            .build();
        let evald = HclEvaluator.evaluate(&component, &make_ctx()).unwrap();
        assert!(
            evald
                .diagnostics
                .iter()
                .any(|d| d.severity == Severity::Error && &*d.code == "TF1401"),
            "{:?}",
            evald.diagnostics
        );
        // Locals remain in their original form.
        assert!(matches!(evald.locals[0].value, Expression::Unresolved(_)));
    }

    #[test]
    fn test_should_reduce_resource_attribute_with_var() {
        let resource = Resource::builder()
            .address(Address::new("aws_iam_role.r").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_iam_role"))
            .name(Arc::<str>::from("r"))
            .attributes(vec![(
                Arc::from("name"),
                Expression::TemplateConcat(vec![
                    Expression::Literal(Value::Str(Arc::from("role-"))),
                    var_ref("var.region", SymbolKind::Var),
                ]),
            )])
            .span(Span::synthetic())
            .build();
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("c")))
            .kind(ComponentKind::Component)
            .variables(vec![var("region")])
            .resources(vec![resource])
            .build();
        let evald = HclEvaluator.evaluate(&component, &make_ctx()).unwrap();
        let attrs = &evald.resources[0].attributes;
        let (_, v) = attrs.iter().find(|(k, _)| k.as_ref() == "name").unwrap();
        assert_eq!(
            v,
            &Expression::Literal(Value::Str(Arc::from("role-us-east-2")))
        );
    }

    #[test]
    fn test_should_keep_module_outputs_unresolved() {
        // resource attribute = module.foo.id → stays as Unresolved
        let attr = var_ref("module.foo.id", SymbolKind::Module);
        let resource = Resource::builder()
            .address(Address::new("aws_iam_role.r").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_iam_role"))
            .name(Arc::<str>::from("r"))
            .attributes(vec![(Arc::from("id"), attr.clone())])
            .span(Span::synthetic())
            .build();
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("c")))
            .kind(ComponentKind::Component)
            .resources(vec![resource])
            .build();
        let evald = HclEvaluator.evaluate(&component, &make_ctx()).unwrap();
        let (_, v) = evald.resources[0]
            .attributes
            .iter()
            .find(|(k, _)| k.as_ref() == "id")
            .unwrap();
        assert_eq!(v, &attr);
    }

    #[test]
    fn test_should_resolve_terraform_workspace() {
        let mut ctx = make_ctx();
        ctx.environment = Some(Arc::from("staging"));
        let resource = Resource::builder()
            .address(Address::new("aws_iam_role.r").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_iam_role"))
            .name(Arc::<str>::from("r"))
            .attributes(vec![(
                Arc::from("env"),
                var_ref("terraform.workspace", SymbolKind::Terraform),
            )])
            .span(Span::synthetic())
            .build();
        let component = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("c")))
            .kind(ComponentKind::Component)
            .resources(vec![resource])
            .build();
        let evald = HclEvaluator.evaluate(&component, &ctx).unwrap();
        let (_, v) = evald.resources[0]
            .attributes
            .iter()
            .find(|(k, _)| k.as_ref() == "env")
            .unwrap();
        assert_eq!(v, &Expression::Literal(Value::Str(Arc::from("staging"))));
    }

    #[test]
    fn test_evaluator_is_send_sync() {
        const fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<HclEvaluator>();
        assert_send_sync::<Box<dyn Evaluator>>();
        assert_send_sync::<EvalContext>();
        assert_send_sync::<EvaluatedComponent>();
    }
}
