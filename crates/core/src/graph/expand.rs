//! Module-body expansion: rewrite addresses, substitute inputs / providers,
//! recurse, detect cycles, bound depth.
//!
//! Per [15-resource-graph.md § 3], expansion is the load-bearing step of
//! Phase 5: a module called from `services/api-gateway` contributes
//! `module.<name>.aws_s3_bucket.this` rows to the parent component, with
//! every `var.*` in the module body replaced by the call's `inputs` and
//! every `provider = aws.<alias>` rewritten through the call's `providers`
//! map.
//!
//! [15-resource-graph.md § 3]: ../../../specs/15-resource-graph.md

use std::sync::Arc;

use crate::{
    Diagnostic, LimitKind, Severity,
    diagnostic::Diagnostic as Diag,
    eval::EvaluatedComponent,
    graph::registry::ModuleRegistry,
    ir::{
        Address, AttributeMap, Conditional, Expression, ForExpr, FuncCall, ModuleCall,
        ModuleSource, ProviderRef, Resource, Span, SymbolKind, Value,
    },
    util::paths::{self, SymlinkPolicy},
};

/// Walk-time scratchpad — a single resource list, plus the per-call chain we
/// use to break cycles, plus a diagnostic sink.
pub(super) struct ExpansionState<'a> {
    pub workspace_root: &'a std::path::Path,
    pub max_module_depth: u32,
    pub max_expansion_per_resource: u32,
    pub registry: &'a ModuleRegistry,
    pub diagnostics: Vec<Diagnostic>,
    /// Stack of canonical module paths currently being expanded, used for
    /// cycle detection. Re-entry triggers `I-GRAPH-4` and the call is
    /// dropped with a single diagnostic.
    pub stack: Vec<Arc<std::path::Path>>,
}

impl<'a> ExpansionState<'a> {
    pub(super) fn new(
        workspace_root: &'a std::path::Path,
        max_module_depth: u32,
        max_expansion_per_resource: u32,
        registry: &'a ModuleRegistry,
    ) -> Self {
        Self {
            workspace_root,
            max_module_depth,
            max_expansion_per_resource,
            registry,
            diagnostics: Vec::new(),
            stack: Vec::new(),
        }
    }
}

/// Flatten one top-level [`EvaluatedComponent`]'s `modules` into a single
/// `Vec<Resource>` keyed by full TF address. Top-level resources are passed
/// through unchanged (`count`/`for_each` expansion happens at the call site
/// of `expand_resource_list`, which is the integration point in
/// `graph::builder`).
///
/// The returned list **does not** include `count`/`for_each` expansion of
/// the top-level resources — the builder applies that step uniformly after
/// merge, so callers see exactly one source of truth.
#[must_use]
pub(super) fn flatten_modules(
    parent: &EvaluatedComponent,
    state: &mut ExpansionState<'_>,
) -> Vec<Resource> {
    let mut out: Vec<Resource> = Vec::new();
    let parent_dir = parent_dir(parent);
    for call in &parent.modules {
        expand_module_call(
            call,
            &parent_dir,
            &Prefix::Root,
            &Vec::<(Arc<str>, ProviderRef)>::new(),
            state,
            0,
            &mut out,
        );
    }
    out
}

fn parent_dir(parent: &EvaluatedComponent) -> std::path::PathBuf {
    parent.raw.path.to_path_buf()
}

/// Address prefix accumulated through nested module expansions. `Root` is
/// the empty prefix (top-level resources); each `Step` adds one
/// `module.<name>[index]` segment.
#[derive(Debug, Clone)]
pub(super) enum Prefix {
    Root,
    Step {
        parent: Box<Prefix>,
        name: Arc<str>,
        /// Optional bracketed key — `[0]`, `["a"]`, or `None` for un-indexed.
        index: Option<String>,
    },
}

impl Prefix {
    fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out);
        out
    }

    fn render_into(&self, out: &mut String) {
        match self {
            Prefix::Root => {}
            Prefix::Step {
                parent,
                name,
                index,
            } => {
                parent.render_into(out);
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str("module.");
                out.push_str(name);
                if let Some(idx) = index {
                    out.push_str(idx);
                }
            }
        }
    }
}

/// Outcome of resolving a module-call's `source` to a registry entry.
enum CallResolution<'a> {
    /// Path successfully resolved and lookup hit.
    Resolved {
        canonical: Arc<std::path::Path>,
        module_eval: &'a EvaluatedComponent,
    },
    /// Skip this call (external source, depth cap, path-escape, or
    /// missing registry entry — every case has its own diagnostic).
    Skip,
}

/// Resolve a module call's source string to a registry entry, recording a
/// diagnostic on each early-exit reason.
fn resolve_module_call<'a>(
    call: &ModuleCall,
    caller_dir: &std::path::Path,
    state: &'a mut ExpansionState<'_>,
    depth: u32,
) -> CallResolution<'a> {
    if depth >= state.max_module_depth {
        state.diagnostics.push(
            Diag::limit(
                LimitKind::Expansion,
                "TF1501",
                format!(
                    "module recursion exceeded depth {} at {} (dropping further expansion)",
                    state.max_module_depth, call.address
                ),
            )
            .with_span(call.span.clone()),
        );
        return CallResolution::Skip;
    }

    let source_rel: &str = match &call.source {
        ModuleSource::Local(rel) => rel.as_ref(),
        // External / unwalked sources: nothing to expand. The registry
        // records these so the modules.parquet writer (Phase 8) can
        // still emit a row.
        ModuleSource::Registry(_) | ModuleSource::Git(_) | ModuleSource::External(_) => {
            return CallResolution::Skip;
        }
    };

    let candidate = caller_dir.join(source_rel);
    let canonical =
        match paths::canonicalize_inside(&candidate, state.workspace_root, SymlinkPolicy::Reject) {
            Ok(p) => p,
            Err(err) => {
                state.diagnostics.push(
                    Diag::new(
                        Severity::Warn,
                        "TF1502",
                        format!(
                            "could not resolve local module `{source_rel}` from `{}`: {err}",
                            caller_dir.display()
                        ),
                    )
                    .with_span(call.span.clone()),
                );
                return CallResolution::Skip;
            }
        };
    let canonical_arc: Arc<std::path::Path> = Arc::from(canonical);

    // Cycle check before the registry lookup so we surface the cycle even
    // when the module body was never inserted (defensive).
    if state
        .stack
        .iter()
        .any(|p| Arc::ptr_eq(p, &canonical_arc) || p.as_ref() == canonical_arc.as_ref())
    {
        state.diagnostics.push(
            Diag::new(
                Severity::Warn,
                "TF1504",
                format!(
                    "module cycle detected at `{}` (chain: {})",
                    canonical_arc.display(),
                    format_chain(&state.stack)
                ),
            )
            .with_span(call.span.clone()),
        );
        return CallResolution::Skip;
    }

    if let Some(module_eval) = state.registry.get_local(&canonical_arc) {
        CallResolution::Resolved {
            canonical: canonical_arc,
            module_eval,
        }
    } else {
        state.diagnostics.push(
            Diag::new(
                Severity::Warn,
                "TF1503",
                format!(
                    "local module `{source_rel}` not found in registry (canonical path: {})",
                    canonical_arc.display()
                ),
            )
            .with_span(call.span.clone()),
        );
        CallResolution::Skip
    }
}

/// Expand a single module call site, appending the flattened resources to
/// `out`. Recurses on nested module calls inside the called module.
fn expand_module_call(
    call: &ModuleCall,
    caller_dir: &std::path::Path,
    parent_prefix: &Prefix,
    _parent_provider_map: &[(Arc<str>, ProviderRef)],
    state: &mut ExpansionState<'_>,
    depth: u32,
    out: &mut Vec<Resource>,
) {
    let (canonical_arc, module_eval_clone) =
        match resolve_module_call(call, caller_dir, state, depth) {
            CallResolution::Resolved {
                canonical,
                module_eval,
            } => (canonical, module_eval.clone()),
            CallResolution::Skip => return,
        };
    let module_eval = &module_eval_clone;

    // Per-call-site index expansion: when `count` or `for_each` resolved
    // to a literal, we expand into multiple call instances each with its
    // own index segment. Otherwise we emit a single un-indexed call.
    let instances = call_instances(call, state, &call.address);

    for index in instances {
        let inner_prefix = Prefix::Step {
            parent: Box::new(parent_prefix.clone()),
            name: address_call_name(&call.address),
            index,
        };

        state.stack.push(Arc::clone(&canonical_arc));

        // Expand resources / data into the parent. Each gets:
        // 1. its address prefixed with the module call chain;
        // 2. every `var.X` substituted from the call's inputs;
        // 3. its provider_ref rewritten via the call's providers map.
        for r in &module_eval.resources {
            let expanded = rewrite_resource(r, &inner_prefix, &call.inputs, &call.providers);
            out.push(expanded);
        }

        // Recurse into nested module calls inside the called module.
        let nested_caller_dir: std::path::PathBuf = module_eval.raw.path.to_path_buf();
        for nested_call in &module_eval.modules {
            // The nested call's inputs reference the *parent* module's
            // `var.*`; substitute them via this call's inputs before
            // descending so the recursive layer sees concrete values.
            let mut nested_call_substituted = nested_call.clone();
            nested_call_substituted.inputs =
                substitute_inputs_in_attrs(&nested_call.inputs, &call.inputs);
            expand_module_call(
                &nested_call_substituted,
                &nested_caller_dir,
                &inner_prefix,
                &call.providers,
                state,
                depth + 1,
                out,
            );
        }

        state.stack.pop();
    }
}

fn format_chain(stack: &[Arc<std::path::Path>]) -> String {
    let mut parts: Vec<String> = stack.iter().map(|p| p.display().to_string()).collect();
    parts.push("…".to_string());
    parts.join(" -> ")
}

/// Extract the call-site name from a `ModuleCall.address` like
/// `module.<name>` → `<name>`. Returns the verbatim address if the prefix
/// is missing (defensive — projector pins the shape).
fn address_call_name(address: &Address) -> Arc<str> {
    let s = address.as_str();
    s.strip_prefix("module.")
        .map_or_else(|| Arc::<str>::from(s), Arc::<str>::from)
}

/// One "expansion instance" of a module call. `None` means a single
/// un-indexed call; `Some(i)` means an `[i]` / `["k"]` indexed call.
fn call_instances(
    call: &ModuleCall,
    state: &mut ExpansionState<'_>,
    site: &Address,
) -> Vec<Option<String>> {
    if let Some(c) = &call.count_expr {
        return instances_for_count(c, &call.span, state, site);
    }
    if let Some(fe) = &call.for_each_expr {
        return instances_for_for_each(fe, &call.span, state, site);
    }
    vec![None]
}

fn instances_for_count(
    expr: &Expression,
    span: &Span,
    state: &mut ExpansionState<'_>,
    site: &Address,
) -> Vec<Option<String>> {
    match expr {
        Expression::Literal(Value::Int(n)) => {
            if *n <= 0 {
                return Vec::new();
            }
            let cap = i64::from(state.max_expansion_per_resource);
            if *n > cap {
                state.diagnostics.push(
                    Diag::limit(
                        LimitKind::Expansion,
                        "TF1505",
                        format!(
                            "count ({n}) at {site} exceeds expansion cap ({cap}); emitting \
                             template row only"
                        ),
                    )
                    .with_span(span.clone()),
                );
                return vec![None];
            }
            (0..*n).map(|i| Some(format!("[{i}]"))).collect()
        }
        // Unresolved → emit a single template instance.
        _ => vec![None],
    }
}

fn instances_for_for_each(
    expr: &Expression,
    span: &Span,
    state: &mut ExpansionState<'_>,
    site: &Address,
) -> Vec<Option<String>> {
    let Expression::Literal(value) = expr else {
        return vec![None];
    };
    match value {
        Value::Map(entries) => {
            let cap = state.max_expansion_per_resource as usize;
            if entries.len() > cap {
                state.diagnostics.push(
                    Diag::limit(
                        LimitKind::Expansion,
                        "TF1505",
                        format!(
                            "for_each ({}) at {site} exceeds expansion cap ({cap}); emitting \
                             template row only",
                            entries.len()
                        ),
                    )
                    .with_span(span.clone()),
                );
                return vec![None];
            }
            entries
                .iter()
                .map(|(k, _)| Some(format!("[\"{}\"]", escape_address_key(k))))
                .collect()
        }
        Value::List(items) => {
            // `for_each = toset([...])` evaluates to a list-shaped value
            // in our IR (toset isn't yet implemented; the literal list
            // case is what users see).
            let cap = state.max_expansion_per_resource as usize;
            if items.len() > cap {
                state.diagnostics.push(
                    Diag::limit(
                        LimitKind::Expansion,
                        "TF1505",
                        format!(
                            "for_each list ({}) at {site} exceeds expansion cap ({cap}); emitting \
                             template row only",
                            items.len()
                        ),
                    )
                    .with_span(span.clone()),
                );
                return vec![None];
            }
            items
                .iter()
                .filter_map(|v| match v {
                    Value::Str(s) => Some(Some(format!("[\"{}\"]", escape_address_key(s)))),
                    _ => None,
                })
                .collect()
        }
        _ => vec![None],
    }
}

/// Escape characters that would break [`Address`]'s charset allowlist /
/// bracket-balance check. The only contentious bytes are `"` and `\`; we
/// drop them. Addresses are diagnostic strings; lossy escape is fine.
fn escape_address_key(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' | '\\' => {} // skip
            _ => out.push(ch),
        }
    }
    out
}

/// Build an expanded copy of a module-body resource: prefixed address,
/// substituted attributes, rewritten provider ref.
fn rewrite_resource(
    src: &Resource,
    prefix: &Prefix,
    inputs: &AttributeMap,
    provider_map: &[(Arc<str>, ProviderRef)],
) -> Resource {
    let new_address = prefix_address(&src.address, prefix);
    let count_expr = src
        .count_expr
        .as_ref()
        .map(|e| substitute_inputs_in_expr(e, inputs));
    let for_each_expr = src
        .for_each_expr
        .as_ref()
        .map(|e| substitute_inputs_in_expr(e, inputs));
    let attributes = substitute_inputs_in_attrs(&src.attributes, inputs);
    let provider_ref = src
        .provider_ref
        .as_ref()
        .map(|r| substitute_provider_ref(r, provider_map));

    Resource::builder()
        .address(new_address)
        .kind(src.kind)
        .type_(Arc::clone(&src.type_))
        .name(Arc::clone(&src.name))
        .provider_ref(provider_ref)
        .count_expr(count_expr)
        .for_each_expr(for_each_expr)
        .depends_on(src.depends_on.clone())
        .attributes(attributes)
        .span(src.span.clone())
        .build()
}

/// Construct a new `Address` with the prefix applied. Falls back to the
/// original address if the prefix is empty or the resulting string fails
/// validation — never panics.
pub(super) fn prefix_address(addr: &Address, prefix: &Prefix) -> Address {
    let prefix_str = prefix.render();
    if prefix_str.is_empty() {
        return addr.clone();
    }
    let combined = format!("{}.{}", prefix_str, addr.as_str());
    Address::new(&combined).unwrap_or_else(|_| addr.clone())
}

/// Apply input substitution across an [`AttributeMap`].
pub(super) fn substitute_inputs_in_attrs(
    attrs: &AttributeMap,
    inputs: &AttributeMap,
) -> AttributeMap {
    attrs
        .iter()
        .map(|(k, v)| (Arc::clone(k), substitute_inputs_in_expr(v, inputs)))
        .collect()
}

/// Walk `expr` replacing every `var.X` whose `X` has an `inputs` binding
/// with the bound expression. Unbound `var.*` references stay as-is.
#[must_use]
pub(super) fn substitute_inputs_in_expr(expr: &Expression, inputs: &AttributeMap) -> Expression {
    match expr {
        Expression::Literal(_) => expr.clone(),

        Expression::Unresolved(sym) => {
            if matches!(sym.kind, SymbolKind::Var) {
                let rest = sym
                    .source
                    .strip_prefix("var.")
                    .unwrap_or(sym.source.as_ref());
                let (head, tail) = split_head(rest);
                // Only substitute when the reference is to the variable
                // root (`var.x`) — attribute access on a struct binding
                // (`var.tags.Service`) stays Unresolved because we cannot
                // statically destructure inside this pass.
                if !tail.is_empty() {
                    return expr.clone();
                }
                if let Some((_, bound)) = inputs.iter().find(|(k, _)| k.as_ref() == head) {
                    return bound.clone();
                }
            }
            expr.clone()
        }

        Expression::BinaryOp { op, lhs, rhs, span } => Expression::BinaryOp {
            op: *op,
            lhs: Box::new(substitute_inputs_in_expr(lhs, inputs)),
            rhs: Box::new(substitute_inputs_in_expr(rhs, inputs)),
            span: span.clone(),
        },

        Expression::UnaryOp { op, operand, span } => Expression::UnaryOp {
            op: *op,
            operand: Box::new(substitute_inputs_in_expr(operand, inputs)),
            span: span.clone(),
        },

        Expression::TemplateConcat(parts) => Expression::TemplateConcat(
            parts
                .iter()
                .map(|p| substitute_inputs_in_expr(p, inputs))
                .collect(),
        ),

        Expression::Array(parts) => Expression::Array(
            parts
                .iter()
                .map(|p| substitute_inputs_in_expr(p, inputs))
                .collect(),
        ),

        Expression::Object(entries) => Expression::Object(
            entries
                .iter()
                .map(|(k, v)| {
                    (
                        substitute_inputs_in_expr(k, inputs),
                        substitute_inputs_in_expr(v, inputs),
                    )
                })
                .collect(),
        ),

        Expression::FuncCall(call) => Expression::FuncCall(Box::new(FuncCall {
            name: Arc::clone(&call.name),
            args: call
                .args
                .iter()
                .map(|a| substitute_inputs_in_expr(a, inputs))
                .collect(),
            span: call.span.clone(),
        })),

        Expression::Conditional(c) => Expression::Conditional(Box::new(Conditional {
            cond: Box::new(substitute_inputs_in_expr(&c.cond, inputs)),
            then_branch: Box::new(substitute_inputs_in_expr(&c.then_branch, inputs)),
            else_branch: Box::new(substitute_inputs_in_expr(&c.else_branch, inputs)),
            span: c.span.clone(),
        })),

        Expression::For(f) => Expression::For(Box::new(ForExpr {
            binders: f.binders.clone(),
            collection: Box::new(substitute_inputs_in_expr(&f.collection, inputs)),
            key: f
                .key
                .as_ref()
                .map(|k| Box::new(substitute_inputs_in_expr(k, inputs))),
            value: Box::new(substitute_inputs_in_expr(&f.value, inputs)),
            cond: f
                .cond
                .as_ref()
                .map(|c| Box::new(substitute_inputs_in_expr(c, inputs))),
            object_form: f.object_form,
            span: f.span.clone(),
        })),
    }
}

fn split_head(s: &str) -> (&str, &str) {
    s.find('.')
        .map_or((s, ""), |idx| (&s[..idx], &s[idx + 1..]))
}

/// Rewrite a module-body [`ProviderRef`] through the call's
/// `providers = { aws = aws.<alias> }` map. Per [99-key-decisions.md] D8 the
/// rewrite happens at expansion, not at provider resolution: every
/// flattened resource thereafter refers to the **parent** component's
/// alias namespace.
///
/// [99-key-decisions.md]: ../../../specs/99-key-decisions.md
fn substitute_provider_ref(
    inner: &ProviderRef,
    provider_map: &[(Arc<str>, ProviderRef)],
) -> ProviderRef {
    // Match by local name only — Terraform's `providers = { aws = aws.main }`
    // keys by the module's local name (`aws`). The module's alias is
    // ignored on the rewrite (the call dictates the alias).
    match provider_map
        .iter()
        .find(|(k, _)| k.as_ref() == inner.local_name.as_ref())
    {
        Some((_, parent_ref)) => ProviderRef {
            local_name: Arc::clone(&parent_ref.local_name),
            alias: parent_ref.alias.clone(),
            span: inner.span.clone(),
        },
        None => inner.clone(),
    }
}

/// Apply `count`/`for_each` expansion to a single resource, returning the
/// expanded vector. When the expression resolved to a literal:
///
/// - `count = N` → emit N resources, addresses `…[0]` / `…[1]` / …
/// - `for_each = {k = v}` → emit one per key, addresses `…["k"]`
///
/// Otherwise emit one template row whose `count_expr` / `for_each_expr` is
/// retained verbatim so downstream queries can find templates with
/// `WHERE count_expr != ''`.
///
/// Cap-breach behaviour: a literal count exceeding
/// `max_expansion_per_resource` collapses to the template row and the
/// supplied diagnostics sink records a `LimitKind::Expansion` entry.
#[must_use]
pub(super) fn expand_resource(
    resource: Resource,
    max_per_resource: u32,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Resource> {
    if let Some(count) = resource.count_expr.clone() {
        return expand_with_count(resource, &count, max_per_resource, diagnostics);
    }
    if let Some(fe) = resource.for_each_expr.clone() {
        return expand_with_for_each(resource, &fe, max_per_resource, diagnostics);
    }
    vec![resource]
}

fn expand_with_count(
    resource: Resource,
    count: &Expression,
    max_per_resource: u32,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Resource> {
    match count {
        Expression::Literal(Value::Int(n)) => {
            if *n <= 0 {
                return Vec::new();
            }
            let cap = i64::from(max_per_resource);
            if *n > cap {
                diagnostics.push(
                    Diag::limit(
                        LimitKind::Expansion,
                        "TF1505",
                        format!(
                            "count ({n}) at {} exceeds expansion cap ({cap}); emitting template \
                             row only",
                            resource.address
                        ),
                    )
                    .with_span(resource.span.clone()),
                );
                return vec![template_row(resource)];
            }
            (0..*n)
                .map(|i| with_indexed_address(&resource, &format!("[{i}]")))
                .collect()
        }
        // Unresolved → keep one template row carrying the count expression.
        _ => vec![template_row(resource)],
    }
}

fn expand_with_for_each(
    resource: Resource,
    fe: &Expression,
    max_per_resource: u32,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Resource> {
    let Expression::Literal(value) = fe else {
        return vec![template_row(resource)];
    };
    let cap = max_per_resource as usize;
    match value {
        Value::Map(entries) => {
            if entries.len() > cap {
                diagnostics.push(
                    Diag::limit(
                        LimitKind::Expansion,
                        "TF1505",
                        format!(
                            "for_each ({}) at {} exceeds expansion cap ({cap}); emitting template \
                             row only",
                            entries.len(),
                            resource.address
                        ),
                    )
                    .with_span(resource.span.clone()),
                );
                return vec![template_row(resource)];
            }
            entries
                .iter()
                .map(|(k, _)| {
                    with_indexed_address(
                        &resource,
                        &format!("[\"{}\"]", escape_address_key(k.as_ref())),
                    )
                })
                .collect()
        }
        Value::List(items) => {
            if items.len() > cap {
                diagnostics.push(
                    Diag::limit(
                        LimitKind::Expansion,
                        "TF1505",
                        format!(
                            "for_each list ({}) at {} exceeds expansion cap ({cap}); emitting \
                             template row only",
                            items.len(),
                            resource.address
                        ),
                    )
                    .with_span(resource.span.clone()),
                );
                return vec![template_row(resource)];
            }
            items
                .iter()
                .filter_map(|v| match v {
                    Value::Str(s) => Some(with_indexed_address(
                        &resource,
                        &format!("[\"{}\"]", escape_address_key(s.as_ref())),
                    )),
                    _ => None,
                })
                .collect()
        }
        _ => vec![template_row(resource)],
    }
}

fn with_indexed_address(resource: &Resource, suffix: &str) -> Resource {
    let combined = format!("{}{}", resource.address.as_str(), suffix);
    let new_addr = Address::new(&combined).unwrap_or_else(|_| resource.address.clone());
    Resource::builder()
        .address(new_addr)
        .kind(resource.kind)
        .type_(Arc::clone(&resource.type_))
        .name(Arc::clone(&resource.name))
        .provider_ref(resource.provider_ref.clone())
        .depends_on(resource.depends_on.clone())
        .attributes(resource.attributes.clone())
        .span(resource.span.clone())
        .build()
}

fn template_row(resource: Resource) -> Resource {
    // Preserve count_expr / for_each_expr verbatim so downstream queries can
    // pivot on them (spec 15 § 3.3 "Address omits the index. Downstream
    // queries can `WHERE count_expr != ''` to find unexpanded templates").
    resource
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use crate::ir::{ResourceKind, Span, Symbolic};

    fn span() -> Span {
        Span::synthetic()
    }

    fn sym(kind: SymbolKind, source: &str) -> Expression {
        Expression::Unresolved(
            Symbolic::builder()
                .kind(kind)
                .source(Arc::<str>::from(source))
                .span(span())
                .build(),
        )
    }

    #[test]
    fn test_should_substitute_var_in_simple_expression() {
        let inputs: AttributeMap = vec![(
            Arc::from("region"),
            Expression::Literal(Value::Str(Arc::from("us-east-2"))),
        )];
        let expr = sym(SymbolKind::Var, "var.region");
        let out = substitute_inputs_in_expr(&expr, &inputs);
        assert_eq!(out, Expression::Literal(Value::Str(Arc::from("us-east-2"))));
    }

    #[test]
    fn test_should_leave_attribute_access_unresolved() {
        // var.tags.Service has a `.tail` after `tags` — we don't statically
        // index into bindings; the expression stays Unresolved.
        let inputs: AttributeMap = vec![(
            Arc::from("tags"),
            Expression::Literal(Value::Map(vec![(
                Arc::from("Service"),
                Value::Str(Arc::from("x")),
            )])),
        )];
        let expr = sym(SymbolKind::Var, "var.tags.Service");
        let out = substitute_inputs_in_expr(&expr, &inputs);
        assert_eq!(out, expr);
    }

    #[test]
    fn test_should_substitute_inside_template_concat() {
        let inputs: AttributeMap = vec![(
            Arc::from("env"),
            Expression::Literal(Value::Str(Arc::from("staging"))),
        )];
        let expr = Expression::TemplateConcat(vec![
            Expression::Literal(Value::Str(Arc::from("a-"))),
            sym(SymbolKind::Var, "var.env"),
        ]);
        let out = substitute_inputs_in_expr(&expr, &inputs);
        match out {
            Expression::TemplateConcat(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(
                    parts[1],
                    Expression::Literal(Value::Str(Arc::from("staging")))
                );
            }
            other => panic!("expected TemplateConcat, got {other:?}"),
        }
    }

    #[test]
    fn test_prefix_address_adds_module_segment() {
        let prefix = Prefix::Step {
            parent: Box::new(Prefix::Root),
            name: Arc::from("edge_logs"),
            index: None,
        };
        let addr = Address::new("aws_s3_bucket.this").unwrap();
        let prefixed = prefix_address(&addr, &prefix);
        assert_eq!(prefixed.as_str(), "module.edge_logs.aws_s3_bucket.this");
    }

    #[test]
    fn test_prefix_address_with_index() {
        let prefix = Prefix::Step {
            parent: Box::new(Prefix::Root),
            name: Arc::from("bucket"),
            index: Some("[0]".to_string()),
        };
        let addr = Address::new("aws_s3_bucket.this").unwrap();
        let prefixed = prefix_address(&addr, &prefix);
        assert_eq!(prefixed.as_str(), "module.bucket[0].aws_s3_bucket.this");
    }

    #[test]
    fn test_provider_substitution_rewrites_alias() {
        let map: Vec<(Arc<str>, ProviderRef)> = vec![(
            Arc::from("aws"),
            ProviderRef {
                local_name: Arc::from("aws"),
                alias: Some(Arc::from("main")),
                span: span(),
            },
        )];
        let inner = ProviderRef {
            local_name: Arc::from("aws"),
            alias: None,
            span: span(),
        };
        let out = substitute_provider_ref(&inner, &map);
        assert_eq!(out.alias.as_deref().map(|s| s as &str), Some("main"));
    }

    #[test]
    fn test_provider_substitution_passthrough_when_no_mapping() {
        let map: Vec<(Arc<str>, ProviderRef)> = Vec::new();
        let inner = ProviderRef {
            local_name: Arc::from("aws"),
            alias: Some(Arc::from("us-east-2")),
            span: span(),
        };
        let out = substitute_provider_ref(&inner, &map);
        assert_eq!(out.alias.as_deref().map(|s| s as &str), Some("us-east-2"));
    }

    #[test]
    fn test_expand_with_literal_count_emits_indexed_addresses() {
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let r = Resource::builder()
            .address(Address::new("aws_s3_bucket.this").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_s3_bucket"))
            .name(Arc::<str>::from("this"))
            .count_expr(Some(Expression::Literal(Value::Int(3))))
            .span(span())
            .build();
        let out = expand_resource(r, 1024, &mut diagnostics);
        assert_eq!(out.len(), 3);
        let addrs: Vec<&str> = out.iter().map(|r| r.address.as_str()).collect();
        assert_eq!(
            addrs,
            vec![
                "aws_s3_bucket.this[0]",
                "aws_s3_bucket.this[1]",
                "aws_s3_bucket.this[2]",
            ]
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn test_expand_with_unresolved_count_emits_template_row() {
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let r = Resource::builder()
            .address(Address::new("aws_s3_bucket.this").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_s3_bucket"))
            .name(Arc::<str>::from("this"))
            .count_expr(Some(sym(SymbolKind::Var, "var.foo")))
            .span(span())
            .build();
        let out = expand_resource(r, 1024, &mut diagnostics);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].address.as_str(), "aws_s3_bucket.this");
        assert!(out[0].count_expr.is_some());
    }

    #[test]
    fn test_expand_with_literal_for_each_emits_key_indexed_addresses() {
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let fe = Expression::Literal(Value::Map(vec![
            (Arc::from("a"), Value::Int(1)),
            (Arc::from("b"), Value::Int(2)),
        ]));
        let r = Resource::builder()
            .address(Address::new("aws_s3_bucket.this").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_s3_bucket"))
            .name(Arc::<str>::from("this"))
            .for_each_expr(Some(fe))
            .span(span())
            .build();
        let out = expand_resource(r, 1024, &mut diagnostics);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].address.as_str(), "aws_s3_bucket.this[\"a\"]");
        assert_eq!(out[1].address.as_str(), "aws_s3_bucket.this[\"b\"]");
    }

    // Spec 15 § 9 commutativity property: rewriting addresses (the
    // module-call prefix step) and substituting `var.*` inputs commute.
    // Both operations are independent — address rewriting touches the
    // resource's `address` only, while substitution touches expression
    // nodes inside the `attributes` tree.
    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig {
            cases: 64,
            ..proptest::prelude::ProptestConfig::default()
        })]
        #[test]
        fn test_rewrite_address_commutes_with_input_substitution(
            name in "[a-z][a-z_]{0,7}",
            attr_value in "[a-z][a-z_]{0,15}",
        ) {
            let prefix = Prefix::Step {
                parent: Box::new(Prefix::Root),
                name: Arc::from(name.as_str()),
                index: None,
            };
            let inputs: AttributeMap = vec![(
                Arc::from("region"),
                Expression::Literal(Value::Str(Arc::from(attr_value.as_str()))),
            )];
            let attrs: AttributeMap = vec![(
                Arc::from("name"),
                sym(SymbolKind::Var, "var.region"),
            )];
            let r = Resource::builder()
                .address(Address::new("aws_s3_bucket.this").unwrap())
                .kind(ResourceKind::Managed)
                .type_(Arc::<str>::from("aws_s3_bucket"))
                .name(Arc::<str>::from("this"))
                .attributes(attrs.clone())
                .span(span())
                .build();
            // Path 1: rewrite-then-substitute.
            let rewritten = rewrite_resource(&r, &prefix, &Vec::new(), &Vec::new());
            let r1_attrs = substitute_inputs_in_attrs(&rewritten.attributes, &inputs);
            let r1_addr = rewritten.address.clone();
            // Path 2: substitute-then-rewrite.
            let substituted_attrs = substitute_inputs_in_attrs(&r.attributes, &inputs);
            let substituted = Resource::builder()
                .address(r.address.clone())
                .kind(r.kind)
                .type_(Arc::clone(&r.type_))
                .name(Arc::clone(&r.name))
                .attributes(substituted_attrs)
                .span(r.span.clone())
                .build();
            let r2 = rewrite_resource(&substituted, &prefix, &Vec::new(), &Vec::new());
            proptest::prop_assert_eq!(r1_addr.as_str(), r2.address.as_str());
            proptest::prop_assert_eq!(r1_attrs, r2.attributes);
        }
    }

    #[test]
    fn test_expand_count_exceeding_cap_emits_diagnostic_and_template() {
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let r = Resource::builder()
            .address(Address::new("aws_s3_bucket.this").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_s3_bucket"))
            .name(Arc::<str>::from("this"))
            .count_expr(Some(Expression::Literal(Value::Int(2048))))
            .span(span())
            .build();
        let out = expand_resource(r, 1024, &mut diagnostics);
        assert_eq!(out.len(), 1);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].limit_kind, Some(LimitKind::Expansion));
    }
}
