//! Expression-tree reduction.
//!
//! Pure walk over our [`Expression`]: for each node, attempt to fold to a
//! [`Value`] using the bindings in the [`Scope`]. If any subtree resists
//! reduction, the **parent** keeps a partially-reduced shape (an
//! [`Expression`] whose children are themselves a mix of resolved and
//! unresolved nodes) — never an empty default. This is the load-bearing
//! invariant pinned in [99-key-decisions.md] D4.
//!
//! Numeric arithmetic, string concatenation, boolean operators, conditionals,
//! and `for` comprehensions are all reduced here. Function calls dispatch
//! through the [`FuncRegistry`](super::registry::FuncRegistry) once their
//! arguments are themselves fully reduced; an unbound function or a
//! function whose call fails keeps the call site as
//! [`Expression::FuncCall`].
//!
//! [99-key-decisions.md]: ../../../specs/99-key-decisions.md

use std::{path::Path, sync::Arc};

use crate::{
    eval::{
        context::{EnvVarMode, EvalLimits},
        registry::{CallCx, FuncRegistry},
    },
    ir::{BinaryOp, Conditional, Expression, ForExpr, FuncCall, Map, SymbolKind, UnaryOp, Value},
};

/// Per-evaluator-pass binding scope.
///
/// `Scope` is a thin "what's currently in scope" record that the reducer
/// updates as it walks. `vars` is the `var.*` namespace (sourced from
/// `EvalContext.repo_vars` and variable defaults); `locals` is the
/// `local.*` namespace, populated by the locals fixpoint solver before
/// providers / resources are reduced.
///
/// `terraform_workspace` is the value that
/// [`Expression::Unresolved`](crate::ir::Expression::Unresolved) of
/// [`SymbolKind::Terraform`] resolves to for the `terraform.workspace`
/// reference — the only `terraform.*` form we recognise statically.
#[derive(Debug)]
pub struct Scope<'a> {
    /// `var.*` namespace.
    pub vars: Map,
    /// `local.*` namespace, populated incrementally by the locals solver.
    pub locals: Map,
    /// **For-comprehension binders** (the `x` / `key, value` names in
    /// `[for x in ...]` / `{for key, value in ...}`).
    ///
    /// HCL's lowering classifies bare identifiers as
    /// [`SymbolKind::Other`](crate::ir::SymbolKind::Other) — neither
    /// `var.*` nor `local.*` — so they need a third namespace. Empty
    /// outside `reduce_for`.
    pub binders: Map,
    /// Workspace root for sandboxed file functions.
    pub workspace_root: &'a Path,
    /// Process-env policy for `get_env`.
    pub env_vars: &'a EnvVarMode,
    /// Per-call resource limits.
    pub limits: &'a EvalLimits,
    /// Function dispatch table.
    pub funcs: &'a FuncRegistry,
    /// Value bound to `terraform.workspace`. `None` keeps the reference
    /// unresolved.
    pub terraform_workspace: Option<Arc<str>>,
}

impl<'a> Scope<'a> {
    /// Construct a new scope from explicit pieces. Used by the component
    /// evaluator (`super::component`). `binders` defaults to empty —
    /// only the for-comprehension reducer populates it.
    #[must_use]
    pub fn new(
        vars: Map,
        locals: Map,
        workspace_root: &'a Path,
        env_vars: &'a EnvVarMode,
        limits: &'a EvalLimits,
        funcs: &'a FuncRegistry,
        terraform_workspace: Option<Arc<str>>,
    ) -> Self {
        Self {
            vars,
            locals,
            binders: Map::new(),
            workspace_root,
            env_vars,
            limits,
            funcs,
            terraform_workspace,
        }
    }

    fn lookup_var(&self, name: &str) -> Option<&Value> {
        self.vars
            .iter()
            .find_map(|(k, v)| if k.as_ref() == name { Some(v) } else { None })
    }

    fn lookup_local(&self, name: &str) -> Option<&Value> {
        self.locals
            .iter()
            .find_map(|(k, v)| if k.as_ref() == name { Some(v) } else { None })
    }

    fn lookup_binder(&self, name: &str) -> Option<&Value> {
        self.binders
            .iter()
            .find_map(|(k, v)| if k.as_ref() == name { Some(v) } else { None })
    }
}

/// Reduce `expr` as far as possible against `scope`, returning a new
/// expression. Idempotent (call twice → same output).
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn reduce_expression(expr: &Expression, scope: &Scope<'_>) -> Expression {
    match expr {
        Expression::Literal(_) => expr.clone(),

        Expression::Unresolved(sym) => match sym.kind {
            SymbolKind::Var => {
                // `var.X[.<rest>]` — handle plain identifier only;
                // attribute access on a struct binding stays unresolved
                // because the binding shape may itself need reduction.
                let rest = sym.source.strip_prefix("var.").unwrap_or(&sym.source);
                let (name, tail) = split_head(rest);
                if !tail.is_empty() {
                    return expr.clone();
                }
                match scope.lookup_var(name) {
                    Some(v) => Expression::Literal(v.clone()),
                    None => expr.clone(),
                }
            }
            SymbolKind::Local => {
                let rest = sym.source.strip_prefix("local.").unwrap_or(&sym.source);
                let (name, tail) = split_head(rest);
                if !tail.is_empty() {
                    return expr.clone();
                }
                match scope.lookup_local(name) {
                    Some(v) => Expression::Literal(v.clone()),
                    None => expr.clone(),
                }
            }
            SymbolKind::Terraform => {
                if sym.source.as_ref() == "terraform.workspace"
                    && let Some(name) = &scope.terraform_workspace
                {
                    return Expression::Literal(Value::Str(Arc::clone(name)));
                }
                expr.clone()
            }
            // For-comprehension binders are lowered as `SymbolKind::Other`
            // (bare HCL identifiers). Resolve them against the `binders`
            // namespace; if absent, leave unresolved. The same arm handles
            // any other bare-identifier reference outside a for-body —
            // those are syntactically malformed in Terraform but the
            // lowerer doesn't reject them, so we stay best-effort here.
            SymbolKind::Other => match scope.lookup_binder(sym.source.as_ref()) {
                Some(v) => Expression::Literal(v.clone()),
                None => expr.clone(),
            },
            // Apply-time references the evaluator can never resolve.
            SymbolKind::Resource
            | SymbolKind::Data
            | SymbolKind::Module
            | SymbolKind::Path
            | SymbolKind::Iteration
            | SymbolKind::TerragruntDependency => expr.clone(),
        },

        Expression::BinaryOp { op, lhs, rhs, span } => {
            let l = reduce_expression(lhs, scope);
            let r = reduce_expression(rhs, scope);
            if let (Expression::Literal(lv), Expression::Literal(rv)) = (&l, &r)
                && let Some(v) = eval_binary(*op, lv, rv)
            {
                return Expression::Literal(v);
            }
            Expression::BinaryOp {
                op: *op,
                lhs: Box::new(l),
                rhs: Box::new(r),
                span: span.clone(),
            }
        }

        Expression::UnaryOp { op, operand, span } => {
            let inner = reduce_expression(operand, scope);
            if let Expression::Literal(v) = &inner
                && let Some(out) = eval_unary(*op, v)
            {
                return Expression::Literal(out);
            }
            Expression::UnaryOp {
                op: *op,
                operand: Box::new(inner),
                span: span.clone(),
            }
        }

        Expression::TemplateConcat(parts) => {
            let reduced: Vec<Expression> =
                parts.iter().map(|p| reduce_expression(p, scope)).collect();
            if reduced.iter().all(|p| matches!(p, Expression::Literal(_))) {
                let mut out = String::new();
                for p in &reduced {
                    if let Expression::Literal(v) = p {
                        out.push_str(&render_value_as_str(v));
                    }
                }
                Expression::Literal(Value::Str(Arc::from(out)))
            } else {
                Expression::TemplateConcat(reduced)
            }
        }

        Expression::Array(items) => {
            let reduced: Vec<Expression> =
                items.iter().map(|i| reduce_expression(i, scope)).collect();
            let all_lit = reduced.iter().all(|i| matches!(i, Expression::Literal(_)));
            if all_lit {
                let values = reduced
                    .into_iter()
                    .map(|i| match i {
                        Expression::Literal(v) => v,
                        _ => Value::Null,
                    })
                    .collect();
                Expression::Literal(Value::List(values))
            } else {
                Expression::Array(reduced)
            }
        }

        Expression::Object(entries) => {
            let reduced: Vec<(Expression, Expression)> = entries
                .iter()
                .map(|(k, v)| (reduce_expression(k, scope), reduce_expression(v, scope)))
                .collect();
            // Keys must reduce to strings before we can collapse to
            // Value::Map.
            let all_string_keys = reduced
                .iter()
                .all(|(k, _)| matches!(k, Expression::Literal(Value::Str(_))));
            let all_lit_values = reduced
                .iter()
                .all(|(_, v)| matches!(v, Expression::Literal(_)));
            if all_string_keys && all_lit_values {
                let mut map: Map = Vec::with_capacity(reduced.len());
                for (k, v) in reduced {
                    let key = match k {
                        Expression::Literal(Value::Str(s)) => s,
                        _ => Arc::from(""),
                    };
                    let value = match v {
                        Expression::Literal(value) => value,
                        _ => Value::Null,
                    };
                    map.push((key, value));
                }
                Expression::Literal(Value::Map(map))
            } else {
                Expression::Object(reduced)
            }
        }

        Expression::FuncCall(call) => {
            let reduced_args: Vec<Expression> = call
                .args
                .iter()
                .map(|a| reduce_expression(a, scope))
                .collect();
            let all_lit = reduced_args
                .iter()
                .all(|a| matches!(a, Expression::Literal(_)));
            if all_lit && let Some(func) = scope.funcs.get(call.name.as_ref()) {
                let arg_values: Vec<Value> = reduced_args
                    .iter()
                    .map(|a| match a {
                        Expression::Literal(v) => v.clone(),
                        _ => Value::Null,
                    })
                    .collect();
                let cx = CallCx {
                    workspace_root: scope.workspace_root,
                    env_vars: scope.env_vars,
                    limits: scope.limits,
                };
                if let Ok(v) = func.call(&arg_values, &cx) {
                    return Expression::Literal(v);
                }
                // Func failure → keep unresolved with reduced args so the
                // canonical JSON renderer surfaces a useful shape.
            }
            Expression::FuncCall(Box::new(FuncCall {
                name: Arc::clone(&call.name),
                args: reduced_args,
                span: call.span.clone(),
            }))
        }

        Expression::Conditional(c) => {
            let cond = reduce_expression(&c.cond, scope);
            if let Expression::Literal(Value::Bool(b)) = &cond {
                let branch = if *b { &c.then_branch } else { &c.else_branch };
                return reduce_expression(branch, scope);
            }
            Expression::Conditional(Box::new(Conditional {
                cond: Box::new(cond),
                then_branch: Box::new(reduce_expression(&c.then_branch, scope)),
                else_branch: Box::new(reduce_expression(&c.else_branch, scope)),
                span: c.span.clone(),
            }))
        }

        Expression::For(f) => {
            let collection = reduce_expression(&f.collection, scope);
            // Phase 4 reduces for-comprehensions only when the collection
            // is fully resolved. Anything more nuanced (partial
            // iteration, dependent bindings) stays as-is — module
            // expansion in Phase 5 picks them up.
            let collection_resolved = matches!(&collection, Expression::Literal(_));
            if !collection_resolved {
                return Expression::For(Box::new(ForExpr {
                    binders: f.binders.clone(),
                    collection: Box::new(collection),
                    key: f.key.clone(),
                    value: f.value.clone(),
                    cond: f.cond.clone(),
                    object_form: f.object_form,
                    span: f.span.clone(),
                }));
            }
            reduce_for(f, &collection, scope)
        }
    }
}

fn split_head(s: &str) -> (&str, &str) {
    s.find('.')
        .map_or((s, ""), |idx| (&s[..idx], &s[idx + 1..]))
}

fn eval_binary(op: BinaryOp, lhs: &Value, rhs: &Value) -> Option<Value> {
    use BinaryOp::{Add, And, Div, Eq, Ge, Gt, Le, Lt, Mod, Mul, Ne, Or, Sub};
    match op {
        Add => add(lhs, rhs),
        Sub => arith(lhs, rhs, |a, b| a - b, i64::checked_sub),
        Mul => arith(lhs, rhs, |a, b| a * b, i64::checked_mul),
        Div => {
            if let (Value::Int(a), Value::Int(b)) = (lhs, rhs) {
                if *b == 0 {
                    return None;
                }
                if let Some(v) = a.checked_div(*b) {
                    return Some(Value::Int(v));
                }
            }
            let (a, b) = (to_f64(lhs)?, to_f64(rhs)?);
            if b == 0.0 {
                return None;
            }
            Some(Value::Number(a / b))
        }
        Mod => {
            if let (Value::Int(a), Value::Int(b)) = (lhs, rhs) {
                if *b == 0 {
                    return None;
                }
                return Some(Value::Int(a.rem_euclid(*b)));
            }
            let (a, b) = (to_f64(lhs)?, to_f64(rhs)?);
            Some(Value::Number(a.rem_euclid(b)))
        }
        Eq => Some(Value::Bool(values_equal(lhs, rhs))),
        Ne => Some(Value::Bool(!values_equal(lhs, rhs))),
        Lt => cmp(lhs, rhs).map(|o| Value::Bool(o == std::cmp::Ordering::Less)),
        Le => cmp(lhs, rhs).map(|o| Value::Bool(o != std::cmp::Ordering::Greater)),
        Gt => cmp(lhs, rhs).map(|o| Value::Bool(o == std::cmp::Ordering::Greater)),
        Ge => cmp(lhs, rhs).map(|o| Value::Bool(o != std::cmp::Ordering::Less)),
        And => match (lhs, rhs) {
            (Value::Bool(a), Value::Bool(b)) => Some(Value::Bool(*a && *b)),
            _ => None,
        },
        Or => match (lhs, rhs) {
            (Value::Bool(a), Value::Bool(b)) => Some(Value::Bool(*a || *b)),
            _ => None,
        },
    }
}

fn add(lhs: &Value, rhs: &Value) -> Option<Value> {
    if let (Value::Int(a), Value::Int(b)) = (lhs, rhs) {
        return a.checked_add(*b).map(Value::Int);
    }
    if let (Value::Str(a), Value::Str(b)) = (lhs, rhs) {
        let mut s = String::with_capacity(a.len() + b.len());
        s.push_str(a);
        s.push_str(b);
        return Some(Value::Str(Arc::from(s)));
    }
    let (a, b) = (to_f64(lhs)?, to_f64(rhs)?);
    Some(Value::Number(a + b))
}

fn arith(
    lhs: &Value,
    rhs: &Value,
    op_f: impl Fn(f64, f64) -> f64,
    op_i: impl Fn(i64, i64) -> Option<i64>,
) -> Option<Value> {
    if let (Value::Int(a), Value::Int(b)) = (lhs, rhs) {
        return op_i(*a, *b).map(Value::Int);
    }
    let (a, b) = (to_f64(lhs)?, to_f64(rhs)?);
    Some(Value::Number(op_f(a, b)))
}

#[allow(clippy::cast_precision_loss)]
fn to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Number(f) => Some(*f),
        _ => None,
    }
}

#[allow(clippy::float_cmp, clippy::cast_precision_loss)]
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        // Int/Number cross-comparison: Terraform treats `1 == 1.0` as
        // true, which means we *do* want exact float-vs-int comparison
        // here. The clippy lint is correctly suppressed for this
        // intentional semantic.
        (Value::Int(x), Value::Number(y)) | (Value::Number(y), Value::Int(x)) => *y == (*x as f64),
        _ => a == b,
    }
}

fn cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y),
        (Value::Int(_), Value::Number(_)) | (Value::Number(_), Value::Int(_)) => {
            to_f64(a)?.partial_cmp(&to_f64(b)?)
        }
        _ => None,
    }
}

fn eval_unary(op: UnaryOp, v: &Value) -> Option<Value> {
    match op {
        UnaryOp::Neg => match v {
            Value::Int(n) => n.checked_neg().map(Value::Int),
            Value::Number(f) => Some(Value::Number(-*f)),
            _ => None,
        },
        UnaryOp::Not => match v {
            Value::Bool(b) => Some(Value::Bool(!b)),
            _ => None,
        },
    }
}

fn render_value_as_str(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Number(f) => {
            let mut buf = ryu::Buffer::new();
            buf.format(*f).to_string()
        }
        Value::Null => String::new(),
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(render_value_as_str).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Map(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{k} = {}", render_value_as_str(v)))
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
    }
}

/// Reduce a [`Expression::For`] whose collection already resolved.
///
/// Phase 4 covers the cases the M1 fixtures actually need: iterating a
/// list (1 binder), a list of (k, v) pairs via two binders over a map, or
/// a map iteration. The `cond` clause filters, the `value` clause yields,
/// and an `object_form` flag selects list vs. object output.
fn reduce_for(f: &ForExpr, collection: &Expression, scope: &Scope<'_>) -> Expression {
    let Expression::Literal(value) = collection else {
        return Expression::For(Box::new(f.clone()));
    };

    // Materialise the (key, value) iteration pairs.
    let pairs: Vec<(Value, Value)> = match value {
        Value::List(items) => items
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let idx = i64::try_from(i).unwrap_or(i64::MAX);
                (Value::Int(idx), v.clone())
            })
            .collect(),
        Value::Map(entries) => entries
            .iter()
            .map(|(k, v)| (Value::Str(Arc::clone(k)), v.clone()))
            .collect(),
        _ => return Expression::For(Box::new(f.clone())),
    };

    let mut out_list: Vec<Value> = Vec::new();
    let mut out_map: Map = Vec::new();
    let mut all_resolved = true;
    let iterations = pairs.len();
    // Anti-DoS: clamp at limits.max_iterations (this is per-call, not the
    // global pass; the global cap is enforced in `super::component`).
    if u64::try_from(iterations).unwrap_or(u64::MAX) > u64::from(scope.limits.max_iterations) {
        // Treat as unresolved.
        return Expression::For(Box::new(f.clone()));
    }

    for (k, v) in pairs {
        // For-binders go into a *separate* namespace from `var.*` because
        // HCL lowers a bare `x` (inside `for x in ...`) as
        // `SymbolKind::Other`, not `SymbolKind::Var`. The reducer's
        // `SymbolKind::Other` arm consults `scope.binders`.
        let mut inner_binders = scope.binders.clone();
        match f.binders.as_slice() {
            [single] => {
                inner_binders.push((Arc::clone(single), v.clone()));
            }
            [key_name, value_name] => {
                inner_binders.push((Arc::clone(key_name), k.clone()));
                inner_binders.push((Arc::clone(value_name), v.clone()));
            }
            _ => return Expression::For(Box::new(f.clone())),
        }
        let inner_scope = Scope {
            vars: scope.vars.clone(),
            locals: scope.locals.clone(),
            binders: inner_binders,
            workspace_root: scope.workspace_root,
            env_vars: scope.env_vars,
            limits: scope.limits,
            funcs: scope.funcs,
            terraform_workspace: scope.terraform_workspace.clone(),
        };

        if let Some(cond) = &f.cond {
            let reduced = reduce_expression(cond, &inner_scope);
            match reduced {
                Expression::Literal(Value::Bool(false)) => continue,
                Expression::Literal(Value::Bool(true)) => {}
                _ => {
                    all_resolved = false;
                    break;
                }
            }
        }

        let value_expr = reduce_expression(&f.value, &inner_scope);
        let Expression::Literal(value) = value_expr else {
            all_resolved = false;
            break;
        };

        if f.object_form {
            let Some(key_expr) = f.key.as_ref().map(|k| reduce_expression(k, &inner_scope)) else {
                all_resolved = false;
                break;
            };
            if let Expression::Literal(Value::Str(s)) = key_expr {
                out_map.push((s, value));
            } else {
                all_resolved = false;
                break;
            }
        } else {
            out_list.push(value);
        }
    }

    if !all_resolved {
        return Expression::For(Box::new(f.clone()));
    }

    if f.object_form {
        Expression::Literal(Value::Map(out_map))
    } else {
        Expression::Literal(Value::List(out_list))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::{
        eval::registry::FuncRegistry,
        ir::{Expression, Span, SymbolKind, Symbolic, Value},
    };

    fn span() -> Span {
        Span::synthetic()
    }

    fn make_scope<'a>(
        vars: Map,
        funcs: &'a FuncRegistry,
        env: &'a EnvVarMode,
        limits: &'a EvalLimits,
    ) -> Scope<'a> {
        Scope {
            vars,
            locals: Vec::new(),
            binders: Vec::new(),
            workspace_root: Path::new("/tmp/repo"),
            env_vars: env,
            limits,
            funcs,
            terraform_workspace: None,
        }
    }

    fn var_x(name: &str) -> Expression {
        Expression::Unresolved(
            Symbolic::builder()
                .kind(SymbolKind::Var)
                .source(Arc::<str>::from(name))
                .span(span())
                .build(),
        )
    }

    #[test]
    fn test_reduces_var_lookup() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(
            vec![(Arc::from("region"), Value::Str(Arc::from("us-east-2")))],
            &funcs,
            &env,
            &limits,
        );
        let out = reduce_expression(&var_x("var.region"), &scope);
        assert_eq!(out, Expression::Literal(Value::Str(Arc::from("us-east-2"))));
    }

    #[test]
    fn test_leaves_var_unresolved_when_unbound() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(Vec::new(), &funcs, &env, &limits);
        let e = var_x("var.region");
        let out = reduce_expression(&e, &scope);
        assert_eq!(out, e);
    }

    #[test]
    fn test_reduces_int_add() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(Vec::new(), &funcs, &env, &limits);
        let e = Expression::BinaryOp {
            op: BinaryOp::Add,
            lhs: Box::new(Expression::Literal(Value::Int(1))),
            rhs: Box::new(Expression::Literal(Value::Int(2))),
            span: span(),
        };
        assert_eq!(
            reduce_expression(&e, &scope),
            Expression::Literal(Value::Int(3))
        );
    }

    #[test]
    fn test_keeps_binary_when_one_side_unresolved() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(Vec::new(), &funcs, &env, &limits);
        let e = Expression::BinaryOp {
            op: BinaryOp::Add,
            lhs: Box::new(Expression::Literal(Value::Int(1))),
            rhs: Box::new(var_x("var.unknown")),
            span: span(),
        };
        let out = reduce_expression(&e, &scope);
        assert!(matches!(out, Expression::BinaryOp { .. }));
    }

    #[test]
    fn test_reduces_template_concat_to_string() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(
            vec![(Arc::from("region"), Value::Str(Arc::from("us-east-2")))],
            &funcs,
            &env,
            &limits,
        );
        let e = Expression::TemplateConcat(vec![
            Expression::Literal(Value::Str(Arc::from("prefix-"))),
            var_x("var.region"),
        ]);
        let out = reduce_expression(&e, &scope);
        assert_eq!(
            out,
            Expression::Literal(Value::Str(Arc::from("prefix-us-east-2")))
        );
    }

    #[test]
    fn test_conditional_picks_branch_eagerly() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(Vec::new(), &funcs, &env, &limits);
        let cond = Expression::Conditional(Box::new(Conditional {
            cond: Box::new(Expression::Literal(Value::Bool(true))),
            then_branch: Box::new(Expression::Literal(Value::Int(1))),
            else_branch: Box::new(var_x("var.missing")),
            span: span(),
        }));
        assert_eq!(
            reduce_expression(&cond, &scope),
            Expression::Literal(Value::Int(1))
        );
    }

    #[test]
    fn test_conditional_keeps_shape_when_cond_unresolved() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(Vec::new(), &funcs, &env, &limits);
        let cond = Expression::Conditional(Box::new(Conditional {
            cond: Box::new(var_x("var.unknown")),
            then_branch: Box::new(Expression::Literal(Value::Int(1))),
            else_branch: Box::new(Expression::Literal(Value::Int(2))),
            span: span(),
        }));
        let out = reduce_expression(&cond, &scope);
        assert!(matches!(out, Expression::Conditional(_)));
    }

    #[test]
    fn test_array_collapses_when_all_literals() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(vec![(Arc::from("y"), Value::Int(2))], &funcs, &env, &limits);
        let e = Expression::Array(vec![Expression::Literal(Value::Int(1)), var_x("var.y")]);
        assert_eq!(
            reduce_expression(&e, &scope),
            Expression::Literal(Value::List(vec![Value::Int(1), Value::Int(2)]))
        );
    }

    #[test]
    fn test_func_call_dispatches_when_all_args_literal() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default_with_stdlib();
        let scope = make_scope(Vec::new(), &funcs, &env, &limits);
        let call = Expression::FuncCall(Box::new(FuncCall {
            name: Arc::from("upper"),
            args: vec![Expression::Literal(Value::Str(Arc::from("hello")))],
            span: span(),
        }));
        assert_eq!(
            reduce_expression(&call, &scope),
            Expression::Literal(Value::Str(Arc::from("HELLO")))
        );
    }

    #[test]
    fn test_func_call_keeps_shape_when_arg_unresolved() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default_with_stdlib();
        let scope = make_scope(Vec::new(), &funcs, &env, &limits);
        let call = Expression::FuncCall(Box::new(FuncCall {
            name: Arc::from("upper"),
            args: vec![var_x("var.x")],
            span: span(),
        }));
        let out = reduce_expression(&call, &scope);
        assert!(matches!(out, Expression::FuncCall(_)));
    }

    fn bare_ident(name: &str) -> Expression {
        // The lowering classifies bare HCL identifiers (e.g. `v` inside
        // `for v in ...`) as `SymbolKind::Other`. The reducer's `Other`
        // arm resolves them against `Scope.binders`.
        Expression::Unresolved(
            Symbolic::builder()
                .kind(SymbolKind::Other)
                .source(Arc::<str>::from(name))
                .span(span())
                .build(),
        )
    }

    #[test]
    fn test_for_list_comprehension_resolves() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(Vec::new(), &funcs, &env, &limits);
        let f = ForExpr {
            binders: vec![Arc::from("v")],
            collection: Box::new(Expression::Literal(Value::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
            ]))),
            key: None,
            // Production HCL lowers the bare binder reference as
            // `SymbolKind::Other` (see F-007). This test pins the
            // production shape — passing it confirms the reducer's
            // `binders` namespace works end-to-end.
            value: Box::new(Expression::BinaryOp {
                op: BinaryOp::Mul,
                lhs: Box::new(bare_ident("v")),
                rhs: Box::new(Expression::Literal(Value::Int(10))),
                span: span(),
            }),
            cond: None,
            object_form: false,
            span: span(),
        };
        let e = Expression::For(Box::new(f));
        assert_eq!(
            reduce_expression(&e, &scope),
            Expression::Literal(Value::List(vec![
                Value::Int(10),
                Value::Int(20),
                Value::Int(30),
            ]))
        );
    }

    #[test]
    fn test_for_map_comprehension_resolves() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(Vec::new(), &funcs, &env, &limits);
        // `{for k, v in {a=1, b=2}: k => v * 10}`
        let f = ForExpr {
            binders: vec![Arc::from("k"), Arc::from("v")],
            collection: Box::new(Expression::Literal(Value::Map(vec![
                (Arc::from("a"), Value::Int(1)),
                (Arc::from("b"), Value::Int(2)),
            ]))),
            key: Some(Box::new(bare_ident("k"))),
            value: Box::new(Expression::BinaryOp {
                op: BinaryOp::Mul,
                lhs: Box::new(bare_ident("v")),
                rhs: Box::new(Expression::Literal(Value::Int(10))),
                span: span(),
            }),
            cond: None,
            object_form: true,
            span: span(),
        };
        let out = reduce_expression(&Expression::For(Box::new(f)), &scope);
        assert_eq!(
            out,
            Expression::Literal(Value::Map(vec![
                (Arc::from("a"), Value::Int(10)),
                (Arc::from("b"), Value::Int(20)),
            ]))
        );
    }

    #[test]
    fn test_reduce_is_idempotent() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(
            vec![(Arc::from("region"), Value::Str(Arc::from("us-east-2")))],
            &funcs,
            &env,
            &limits,
        );
        let e = Expression::TemplateConcat(vec![
            Expression::Literal(Value::Str(Arc::from("p-"))),
            var_x("var.region"),
        ]);
        let r1 = reduce_expression(&e, &scope);
        let r2 = reduce_expression(&r1, &scope);
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_apply_time_refs_stay_unresolved() {
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let funcs = FuncRegistry::default();
        let scope = make_scope(Vec::new(), &funcs, &env, &limits);
        for source in [
            "aws_iam_role.r.arn",
            "data.aws_caller_identity.self.id",
            "module.vpc.id",
            "path.module",
            "each.value",
        ] {
            let e = Expression::Unresolved(
                Symbolic::builder()
                    .kind(match source {
                        s if s.starts_with("data.") => SymbolKind::Data,
                        s if s.starts_with("module.") => SymbolKind::Module,
                        s if s.starts_with("path.") => SymbolKind::Path,
                        s if s.starts_with("each.") => SymbolKind::Iteration,
                        _ => SymbolKind::Resource,
                    })
                    .source(Arc::<str>::from(source))
                    .span(span())
                    .build(),
            );
            assert_eq!(reduce_expression(&e, &scope), e);
        }
    }
}
