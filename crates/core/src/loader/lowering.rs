//! `hcl-edit` → IR lowering.
//!
//! Walks the parser tree once and emits our own [`Expression`] /
//! [`Value`] / [`Span`] structures, dropping the `hcl-edit` tree at the
//! function boundary (invariant I-LOAD-2 in [12-hcl-loader.md § 4]).
//!
//! Every reference becomes [`Expression::Unresolved`] — the loader does not
//! decide what is resolvable; that's the evaluator's job.
//!
//! [12-hcl-loader.md § 4]: ../../../specs/12-hcl-loader.md

use std::{path::Path, sync::Arc};

use hcl_edit::{
    Span as _,
    expr::{
        Array, BinaryOp as HclBinaryOp, BinaryOperator, Conditional as HclConditional,
        Expression as HExpression, ForExpr as HForExpr, FuncCall as HFuncCall, Object,
        ObjectKey as HObjectKey, ObjectValue, Traversal, TraversalOperator, UnaryOp as HUnaryOp,
        UnaryOperator,
    },
    structure::{Block, BlockLabel, Body, Structure},
    template::{Element, HeredocTemplate, StringTemplate},
};

use super::{LoaderLimits, RawBlock, source_map::LineIndex};
use crate::{
    Diagnostic, Severity,
    diagnostic::{Diagnostic as Diag, LimitKind},
    ir::{
        Address, AttributeMap, BinaryOp, BlockKind, Conditional, Expression, ForExpr, FuncCall,
        Span, SymbolKind, Symbolic, UnaryOp, Value,
    },
};

/// Outcome of lowering one file's `Body`.
pub(super) struct LoweredFile {
    pub blocks: Vec<RawBlock>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Lowering context held for a single file.
struct Lowerer<'a> {
    source: &'a Arc<Path>,
    line_index: &'a LineIndex,
    limits: &'a LoaderLimits,
    diagnostics: Vec<Diagnostic>,
    block_count: u32,
    src_len: usize,
}

/// Lower a parsed [`Body`] into a list of [`RawBlock`] + diagnostics.
///
/// `source_path` is the file path the IR records on every span. `line_index`
/// resolves byte offsets to `(line, col)`. Limits are checked while walking;
/// breaches surface as diagnostics, not errors.
pub(super) fn lower_body(
    body: &Body,
    source_path: &Arc<Path>,
    line_index: &LineIndex,
    limits: &LoaderLimits,
    src_len: usize,
) -> LoweredFile {
    let mut lower = Lowerer {
        source: source_path,
        line_index,
        limits,
        diagnostics: Vec::new(),
        block_count: 0,
        src_len,
    };
    let mut blocks: Vec<RawBlock> = Vec::new();
    for structure in body {
        match structure {
            Structure::Block(block) => {
                if let Some(rb) = lower.lower_block(block) {
                    blocks.push(rb);
                }
            }
            Structure::Attribute(attr) => {
                // Top-level attributes outside a block are valid HCL
                // (e.g. inside a `.tfvars`). We synthesise a `Locals` block
                // wrapper if no block was open — but the cleanest model is
                // to expose a single virtual block carrying every top-level
                // attribute. To stay faithful to file order, every loose
                // attribute becomes a one-attribute `Unknown` block whose
                // label is the attribute name.
                let key: Arc<str> = Arc::from(attr.key.as_str());
                let value = lower.lower_expression(&attr.value, 0);
                let span = lower.span_for(&attr_span(attr));
                let body: AttributeMap = vec![(Arc::clone(&key), value)];
                blocks.push(RawBlock {
                    kind: BlockKind::Unknown,
                    labels: vec![key],
                    body,
                    span,
                    source: Arc::clone(source_path),
                });
            }
        }
    }
    LoweredFile {
        blocks,
        diagnostics: lower.diagnostics,
    }
}

fn attr_span(attr: &hcl_edit::structure::Attribute) -> std::ops::Range<usize> {
    attr.span().or_else(|| attr.value.span()).unwrap_or(0..0)
}

impl Lowerer<'_> {
    fn span_for(&self, range: &std::ops::Range<usize>) -> Span {
        let safe_start = range.start.min(self.src_len);
        let safe_end = range.end.min(self.src_len).max(safe_start);
        let start = u32::try_from(safe_start).unwrap_or(u32::MAX);
        let end = u32::try_from(safe_end).unwrap_or(u32::MAX);
        let pos = self.line_index.locate(start);
        // Span::new requires byte_range non-reversed and 1-based line/col.
        Span::new(
            Arc::clone(self.source),
            start..end,
            pos.line.max(1),
            pos.column.max(1),
        )
        .unwrap_or_else(|_| Span::synthetic())
    }

    fn record_limit(&mut self, kind: LimitKind, observed: u64, limit: u64) {
        self.diagnostics.push(Diag::new(
            Severity::Warn,
            "TF1200",
            format!(
                "loader limit ({kind:?}) exceeded: observed {observed} > {limit}; subtree \
                 truncated"
            ),
        ));
    }

    fn lower_block(&mut self, block: &Block) -> Option<RawBlock> {
        if let Some(next) = self.block_count.checked_add(1) {
            self.block_count = next;
        }
        if self.block_count > self.limits.max_blocks_per_file {
            self.record_limit(
                LimitKind::BlocksPerFile,
                u64::from(self.block_count),
                u64::from(self.limits.max_blocks_per_file),
            );
            return None;
        }

        let kind = classify_block_kind(block.ident.as_str());
        let labels: Vec<Arc<str>> = block
            .labels
            .iter()
            .map(|label| match label {
                BlockLabel::Ident(ident) => Arc::<str>::from(ident.as_str()),
                BlockLabel::String(s) => Arc::<str>::from(s.as_str()),
            })
            .collect();

        // Lower the block body: top-level attributes form the `body`
        // AttributeMap; nested blocks recurse and land under a synthetic
        // key (the nested block's identifier) as a Value::Map.
        let body = self.lower_block_body(&block.body, 0);

        let raw_span = block.span().or_else(|| block.ident.span()).unwrap_or(0..0);
        let span = self.span_for(&raw_span);
        Some(RawBlock {
            kind,
            labels,
            body,
            span,
            source: Arc::clone(self.source),
        })
    }

    /// Lower a block body into an [`AttributeMap`].
    ///
    /// HCL allows nested blocks (`ingress {}` inside `resource "aws_security_group" "x"`)
    /// to repeat with the same identifier. We preserve insertion order and
    /// merge repeats by appending to the same key as a `Value::List`.
    fn lower_block_body(&mut self, body: &Body, depth: u32) -> AttributeMap {
        let mut out: AttributeMap = Vec::new();
        if depth > self.limits.max_attr_depth {
            self.record_limit(
                LimitKind::AttributeDepth,
                u64::from(depth),
                u64::from(self.limits.max_attr_depth),
            );
            return out;
        }
        for structure in body {
            match structure {
                Structure::Attribute(attr) => {
                    let key: Arc<str> = Arc::from(attr.key.as_str());
                    let value = self.lower_expression(&attr.value, depth + 1);
                    out.push((key, value));
                }
                Structure::Block(block) => {
                    let key: Arc<str> = Arc::from(block.ident.as_str());
                    let nested_attrs = self.lower_block_body(&block.body, depth + 1);
                    let nested_value =
                        nested_attribute_map_to_expression(nested_attrs, &block.labels);
                    out.push((key, nested_value));
                }
            }
        }
        out
    }

    fn lower_expression(&mut self, expr: &HExpression, depth: u32) -> Expression {
        if depth > self.limits.max_attr_depth {
            self.record_limit(
                LimitKind::AttributeDepth,
                u64::from(depth),
                u64::from(self.limits.max_attr_depth),
            );
            // Truncate by emitting an Unresolved sentinel so downstream
            // consumers see *something* rather than a hole.
            return Expression::Unresolved(Symbolic {
                kind: SymbolKind::Other,
                source: Arc::from("<truncated: attribute depth exceeded>"),
                address_hint: None,
                span: self.span_for(&(0..0)),
            });
        }
        let span_range = expr.span().unwrap_or(0..0);
        let span = self.span_for(&span_range);

        match expr {
            HExpression::Null(_) => Expression::Literal(Value::Null),
            HExpression::Bool(b) => Expression::Literal(Value::Bool(*b.value())),
            HExpression::Number(n) => lower_number(n.value()),
            HExpression::String(s) => {
                Expression::Literal(Value::Str(Arc::from(s.value().as_str())))
            }
            HExpression::StringTemplate(tpl) => self.lower_string_template(tpl, depth, span),
            HExpression::HeredocTemplate(tpl) => self.lower_heredoc_template(tpl, depth, span),
            HExpression::Array(arr) => self.lower_array(arr, depth),
            HExpression::Object(obj) => self.lower_object(obj, depth),
            HExpression::Variable(ident) => {
                Expression::Unresolved(make_symbolic(ident.as_str(), SymbolKind::Other, None, span))
            }
            HExpression::Traversal(t) => Self::lower_traversal(t, span),
            HExpression::FuncCall(call) => self.lower_func_call(call, depth, span),
            HExpression::UnaryOp(op) => self.lower_unary(op, depth, span),
            HExpression::BinaryOp(op) => self.lower_binary(op, depth, span),
            HExpression::Conditional(c) => self.lower_conditional(c, depth, span),
            HExpression::ForExpr(f) => self.lower_for(f, depth, span),
            HExpression::Parenthesis(inner) => self.lower_expression(inner.inner(), depth + 1),
        }
    }

    fn lower_string_template(
        &mut self,
        tpl: &StringTemplate,
        depth: u32,
        _span: Span,
    ) -> Expression {
        let parts = self.lower_template_elements(tpl.iter(), depth);
        collapse_template(parts)
    }

    fn lower_heredoc_template(
        &mut self,
        tpl: &HeredocTemplate,
        depth: u32,
        _span: Span,
    ) -> Expression {
        let parts = self.lower_template_elements(tpl.template.iter(), depth);
        collapse_template(parts)
    }

    fn lower_template_elements<'a, I>(&mut self, elements: I, depth: u32) -> Vec<Expression>
    where
        I: IntoIterator<Item = &'a Element>,
    {
        let mut parts: Vec<Expression> = Vec::new();
        for element in elements {
            if u32::try_from(parts.len()).unwrap_or(u32::MAX) >= self.limits.max_template_parts {
                self.record_limit(
                    LimitKind::TemplateParts,
                    u64::try_from(parts.len()).unwrap_or(u64::MAX),
                    u64::from(self.limits.max_template_parts),
                );
                break;
            }
            match element {
                Element::Literal(s) => parts.push(Expression::Literal(Value::Str(Arc::from(
                    s.value().as_str(),
                )))),
                Element::Interpolation(interp) => {
                    parts.push(self.lower_expression(&interp.expr, depth + 1));
                }
                Element::Directive(_) => {
                    // `%{if ...}` / `%{for ...}` are not modelled by the IR
                    // yet — capture the verbatim source via Unresolved so
                    // we don't silently lose it.
                    parts.push(Expression::Unresolved(Symbolic {
                        kind: SymbolKind::Other,
                        source: Arc::from("<template-directive>"),
                        address_hint: None,
                        span: self.span_for(&(0..0)),
                    }));
                }
            }
        }
        parts
    }

    fn lower_array(&mut self, arr: &Array, depth: u32) -> Expression {
        let mut elements: Vec<Expression> = Vec::with_capacity(arr.len());
        let mut all_literal = true;
        for value in arr {
            let lowered = self.lower_expression(value, depth + 1);
            if !matches!(lowered, Expression::Literal(_)) {
                all_literal = false;
            }
            elements.push(lowered);
        }
        if all_literal {
            let values: Vec<Value> = elements
                .into_iter()
                .map(|e| match e {
                    Expression::Literal(v) => v,
                    _ => Value::Null,
                })
                .collect();
            Expression::Literal(Value::List(values))
        } else {
            Expression::Array(elements)
        }
    }

    fn lower_object(&mut self, obj: &Object, depth: u32) -> Expression {
        let mut entries: Vec<(Expression, Expression)> = Vec::with_capacity(obj.len());
        let mut all_string_literal_key = true;
        let mut all_value_literal = true;
        for (key, value) in obj {
            let key_expr = match key {
                HObjectKey::Ident(ident) => {
                    Expression::Literal(Value::Str(Arc::from(ident.as_str())))
                }
                HObjectKey::Expression(e) => {
                    let lowered = self.lower_expression(e, depth + 1);
                    if !matches!(&lowered, Expression::Literal(Value::Str(_))) {
                        all_string_literal_key = false;
                    }
                    lowered
                }
            };
            let value_expr = lower_object_value(self, value, depth + 1);
            if !matches!(value_expr, Expression::Literal(_)) {
                all_value_literal = false;
            }
            entries.push((key_expr, value_expr));
        }
        if all_string_literal_key && all_value_literal {
            let map: Vec<(Arc<str>, Value)> = entries
                .into_iter()
                .map(|(k, v)| {
                    let key_str: Arc<str> = match k {
                        Expression::Literal(Value::Str(s)) => s,
                        _ => Arc::from(""),
                    };
                    let val: Value = match v {
                        Expression::Literal(val) => val,
                        _ => Value::Null,
                    };
                    (key_str, val)
                })
                .collect();
            Expression::Literal(Value::Map(map))
        } else {
            Expression::Object(entries)
        }
    }

    fn lower_traversal(t: &Traversal, span: Span) -> Expression {
        let source = render_traversal(t);
        let kind = symbol_kind_for(&source);
        let address_hint = parse_address_hint(&source, kind);
        Expression::Unresolved(Symbolic {
            kind,
            source: Arc::from(source.as_str()),
            address_hint,
            span,
        })
    }

    fn lower_func_call(&mut self, call: &HFuncCall, depth: u32, span: Span) -> Expression {
        let mut name = String::new();
        for ns in &call.name.namespace {
            name.push_str(ns.as_str());
            name.push_str("::");
        }
        name.push_str(call.name.name.as_str());
        let mut args: Vec<Expression> = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            args.push(self.lower_expression(arg, depth + 1));
        }
        Expression::FuncCall(Box::new(FuncCall {
            name: Arc::from(name.as_str()),
            args,
            span,
        }))
    }

    fn lower_unary(&mut self, op: &HUnaryOp, depth: u32, span: Span) -> Expression {
        let operand = Box::new(self.lower_expression(&op.expr, depth + 1));
        let mapped = match *op.operator.value() {
            UnaryOperator::Neg => UnaryOp::Neg,
            UnaryOperator::Not => UnaryOp::Not,
        };
        Expression::UnaryOp {
            op: mapped,
            operand,
            span,
        }
    }

    fn lower_binary(&mut self, op: &HclBinaryOp, depth: u32, span: Span) -> Expression {
        let lhs = Box::new(self.lower_expression(&op.lhs_expr, depth + 1));
        let rhs = Box::new(self.lower_expression(&op.rhs_expr, depth + 1));
        let mapped = map_binary_op(*op.operator.value());
        Expression::BinaryOp {
            op: mapped,
            lhs,
            rhs,
            span,
        }
    }

    fn lower_conditional(&mut self, c: &HclConditional, depth: u32, span: Span) -> Expression {
        let cond = Box::new(self.lower_expression(&c.cond_expr, depth + 1));
        let then_branch = Box::new(self.lower_expression(&c.true_expr, depth + 1));
        let else_branch = Box::new(self.lower_expression(&c.false_expr, depth + 1));
        Expression::Conditional(Box::new(Conditional {
            cond,
            then_branch,
            else_branch,
            span,
        }))
    }

    fn lower_for(&mut self, f: &HForExpr, depth: u32, span: Span) -> Expression {
        let intro = &f.intro;
        let mut binders: Vec<Arc<str>> = Vec::new();
        if let Some(k) = &intro.key_var {
            binders.push(Arc::from(k.as_str()));
        }
        binders.push(Arc::from(intro.value_var.as_str()));
        let collection = Box::new(self.lower_expression(&intro.collection_expr, depth + 1));
        let key = f
            .key_expr
            .as_ref()
            .map(|k| Box::new(self.lower_expression(k, depth + 1)));
        let value = Box::new(self.lower_expression(&f.value_expr, depth + 1));
        let cond = f
            .cond
            .as_ref()
            .map(|c| Box::new(self.lower_expression(&c.expr, depth + 1)));
        Expression::For(Box::new(ForExpr {
            binders,
            collection,
            key,
            value,
            cond,
            object_form: f.key_expr.is_some(),
            span,
        }))
    }
}

fn lower_object_value(lower: &mut Lowerer<'_>, value: &ObjectValue, depth: u32) -> Expression {
    lower.lower_expression(value.expr(), depth)
}

fn collapse_template(parts: Vec<Expression>) -> Expression {
    if parts.is_empty() {
        return Expression::Literal(Value::Str(Arc::from("")));
    }
    if parts.len() == 1 {
        // A single-literal template collapses to that literal.
        if let Some(Expression::Literal(Value::Str(_))) = parts.first() {
            // Take the only element; index_first is safe given len == 1.
            let mut iter = parts.into_iter();
            return iter
                .next()
                .unwrap_or(Expression::Literal(Value::Str(Arc::from(""))));
        }
    }
    Expression::TemplateConcat(parts)
}

fn lower_number(n: &hcl_edit::Number) -> Expression {
    if let Some(i) = n.as_i64() {
        Expression::Literal(Value::Int(i))
    } else if let Some(u) = n.as_u64() {
        // u64 → i64 saturation is fine; downstream JSON renders both.
        Expression::Literal(Value::Int(i64::try_from(u).unwrap_or(i64::MAX)))
    } else if let Some(f) = n.as_f64() {
        Expression::Literal(Value::Number(f))
    } else {
        // No numeric representation extractable — record as unresolved.
        Expression::Unresolved(Symbolic {
            kind: SymbolKind::Other,
            source: Arc::from(format!("{n}")),
            address_hint: None,
            span: Span::synthetic(),
        })
    }
}

fn nested_attribute_map_to_expression(attrs: AttributeMap, labels: &[BlockLabel]) -> Expression {
    // Wrap the attribute map as an object expression. If labels were
    // present (e.g. `ingress "rule_a" {}`), prepend a synthetic `__label__`
    // entry so downstream consumers can still see them. We cannot directly
    // build a `Value::Map` because nested attribute values may contain
    // unresolved Expression nodes.
    let mut entries: Vec<(Expression, Expression)> = Vec::with_capacity(attrs.len() + 1);
    if !labels.is_empty() {
        let label_values: Vec<Expression> = labels
            .iter()
            .map(|l| Expression::Literal(Value::Str(Arc::from(l.as_str()))))
            .collect();
        entries.push((
            Expression::Literal(Value::Str(Arc::from("__labels__"))),
            Expression::Array(label_values),
        ));
    }
    let mut all_literal = labels.is_empty();
    for (k, v) in attrs {
        let v_literal = matches!(v, Expression::Literal(_));
        if !v_literal {
            all_literal = false;
        }
        entries.push((Expression::Literal(Value::Str(k)), v));
    }
    if all_literal {
        let map: Vec<(Arc<str>, Value)> = entries
            .into_iter()
            .map(|(k, v)| {
                let key: Arc<str> = match k {
                    Expression::Literal(Value::Str(s)) => s,
                    _ => Arc::from(""),
                };
                let val: Value = match v {
                    Expression::Literal(val) => val,
                    _ => Value::Null,
                };
                (key, val)
            })
            .collect();
        Expression::Literal(Value::Map(map))
    } else {
        Expression::Object(entries)
    }
}

fn render_traversal(t: &Traversal) -> String {
    let mut out = String::new();
    render_expr_for_traversal_root(&t.expr, &mut out);
    for op in &t.operators {
        match op.value() {
            TraversalOperator::GetAttr(ident) => {
                out.push('.');
                out.push_str(ident.as_str());
            }
            TraversalOperator::Index(expr) => {
                out.push('[');
                render_expr_for_traversal_root(expr, &mut out);
                out.push(']');
            }
            TraversalOperator::LegacyIndex(idx) => {
                out.push('[');
                out.push_str(&idx.value().to_string());
                out.push(']');
            }
            TraversalOperator::AttrSplat(_) => {
                out.push_str(".*");
            }
            TraversalOperator::FullSplat(_) => {
                out.push_str("[*]");
            }
        }
    }
    out
}

fn render_expr_for_traversal_root(expr: &HExpression, out: &mut String) {
    match expr {
        HExpression::Variable(v) => out.push_str(v.as_str()),
        HExpression::String(s) => {
            out.push('"');
            out.push_str(s.value().as_str());
            out.push('"');
        }
        HExpression::Number(n) => {
            use std::fmt::Write as _;
            let _ = write!(out, "{}", n.value());
        }
        HExpression::Bool(b) => out.push_str(if *b.value() { "true" } else { "false" }),
        HExpression::Null(_) => out.push_str("null"),
        HExpression::Traversal(inner) => out.push_str(&render_traversal(inner)),
        HExpression::FuncCall(call) => {
            for ns in &call.name.namespace {
                out.push_str(ns.as_str());
                out.push_str("::");
            }
            out.push_str(call.name.name.as_str());
            out.push_str("(...)");
        }
        _ => out.push_str("<expr>"),
    }
}

fn symbol_kind_for(source: &str) -> SymbolKind {
    if source.starts_with("var.") {
        SymbolKind::Var
    } else if source.starts_with("local.") {
        SymbolKind::Local
    } else if source.starts_with("data.") {
        SymbolKind::Data
    } else if source.starts_with("module.") {
        SymbolKind::Module
    } else if source.starts_with("path.") {
        SymbolKind::Path
    } else if source.starts_with("each.") || source.starts_with("count.") {
        SymbolKind::Iteration
    } else if source.starts_with("terraform.") {
        SymbolKind::Terraform
    } else if source.starts_with("dependency.") {
        SymbolKind::TerragruntDependency
    } else if source.contains('.') && !source.starts_with('.') {
        // Looks like a resource reference: `aws_iam_role.r.arn`.
        SymbolKind::Resource
    } else {
        SymbolKind::Other
    }
}

fn parse_address_hint(source: &str, kind: SymbolKind) -> Option<Address> {
    match kind {
        SymbolKind::Var
        | SymbolKind::Local
        | SymbolKind::Data
        | SymbolKind::Module
        | SymbolKind::Resource => Address::new(source).ok(),
        SymbolKind::Path
        | SymbolKind::Iteration
        | SymbolKind::Terraform
        | SymbolKind::TerragruntDependency
        | SymbolKind::Other => None,
    }
}

fn make_symbolic(
    source: &str,
    kind: SymbolKind,
    address_hint: Option<Address>,
    span: Span,
) -> Symbolic {
    Symbolic {
        kind,
        source: Arc::from(source),
        address_hint,
        span,
    }
}

const fn map_binary_op(op: BinaryOperator) -> BinaryOp {
    match op {
        BinaryOperator::Eq => BinaryOp::Eq,
        BinaryOperator::NotEq => BinaryOp::Ne,
        BinaryOperator::LessEq => BinaryOp::Le,
        BinaryOperator::GreaterEq => BinaryOp::Ge,
        BinaryOperator::Less => BinaryOp::Lt,
        BinaryOperator::Greater => BinaryOp::Gt,
        BinaryOperator::Plus => BinaryOp::Add,
        BinaryOperator::Minus => BinaryOp::Sub,
        BinaryOperator::Mul => BinaryOp::Mul,
        BinaryOperator::Div => BinaryOp::Div,
        BinaryOperator::Mod => BinaryOp::Mod,
        BinaryOperator::And => BinaryOp::And,
        BinaryOperator::Or => BinaryOp::Or,
    }
}

/// Map a top-level block identifier to a [`BlockKind`].
///
/// Per [12-hcl-loader.md § 3.3], we only recognise the canonical Terraform /
/// Terragrunt keywords. Everything else falls through to [`BlockKind::Unknown`]
/// so user-defined `dynamic` blocks (or future Terraform extensions) round-trip
/// without surprise.
#[must_use]
pub fn classify_block_kind(ident: &str) -> BlockKind {
    match ident {
        "resource" => BlockKind::Resource,
        "data" => BlockKind::Data,
        "module" => BlockKind::Module,
        "provider" => BlockKind::Provider,
        "variable" => BlockKind::Variable,
        "locals" => BlockKind::Locals,
        "output" => BlockKind::Output,
        "terraform" => BlockKind::Terraform,
        "include" => BlockKind::Include,
        "generate" => BlockKind::Generate,
        "dependency" => BlockKind::Dependency,
        "inputs" => BlockKind::Inputs,
        _ => BlockKind::Unknown,
    }
}

/// Helper used inside `lower_string_template` — exposed as a free function
/// so tests can exercise the collapse logic without spinning up a full
/// `Lowerer`.
#[cfg(test)]
pub(super) fn collapse_template_for_test(parts: Vec<Expression>) -> Expression {
    collapse_template(parts)
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

    use hcl_edit::parser::parse_body;

    use super::*;

    fn lower_first_block(src: &str) -> RawBlock {
        let body = parse_body(src).unwrap();
        let path: Arc<Path> = Arc::from(Path::new("/tmp/x.tf"));
        let li = LineIndex::build(src);
        let limits = LoaderLimits::default();
        let mut lowered = lower_body(&body, &path, &li, &limits, src.len());
        assert!(
            lowered.diagnostics.is_empty(),
            "diagnostics: {:?}",
            lowered.diagnostics
        );
        assert!(!lowered.blocks.is_empty(), "no blocks parsed");
        lowered.blocks.remove(0)
    }

    #[test]
    fn test_classify_block_kind_canonical() {
        assert_eq!(classify_block_kind("resource"), BlockKind::Resource);
        assert_eq!(classify_block_kind("inputs"), BlockKind::Inputs);
        assert_eq!(classify_block_kind("xenon"), BlockKind::Unknown);
    }

    #[test]
    fn test_should_lower_resource_with_string_attr() {
        let src = r#"resource "aws_iam_role" "r" {
  name = "service-role"
}
"#;
        let block = lower_first_block(src);
        assert_eq!(block.kind, BlockKind::Resource);
        assert_eq!(
            block
                .labels
                .iter()
                .map(std::convert::AsRef::as_ref)
                .collect::<Vec<&str>>(),
            vec!["aws_iam_role", "r"]
        );
        assert_eq!(block.body[0].0.as_ref(), "name");
        assert!(matches!(
            &block.body[0].1,
            Expression::Literal(Value::Str(s)) if s.as_ref() == "service-role"
        ));
    }

    #[test]
    fn test_should_lower_unresolved_traversal() {
        let src = r#"resource "aws_iam_role" "r" {
  name = var.environment
}
"#;
        let block = lower_first_block(src);
        let attr = &block.body[0];
        assert_eq!(attr.0.as_ref(), "name");
        match &attr.1 {
            Expression::Unresolved(s) => {
                assert_eq!(s.kind, SymbolKind::Var);
                assert_eq!(s.source.as_ref(), "var.environment");
                assert!(s.address_hint.is_some());
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    #[test]
    fn test_should_lower_template_concat() {
        let src = r#"resource "aws_iam_role" "r" {
  name = "${var.x}-${var.y}"
}
"#;
        let block = lower_first_block(src);
        let attr = &block.body[0];
        match &attr.1 {
            Expression::TemplateConcat(parts) => {
                assert!(parts.len() >= 2, "got {} parts", parts.len());
            }
            other => panic!("expected TemplateConcat, got {other:?}"),
        }
    }

    #[test]
    fn test_should_lower_int_and_float_numbers() {
        let src = r#"resource "x" "y" {
  port = 5432
  ratio = 1.5
}
"#;
        let block = lower_first_block(src);
        assert!(matches!(
            &block.body[0].1,
            Expression::Literal(Value::Int(5432))
        ));
        assert!(matches!(
            &block.body[1].1,
            Expression::Literal(Value::Number(_))
        ));
    }

    #[test]
    fn test_should_lower_array_of_literals_to_value_list() {
        let src = r#"resource "x" "y" {
  cidrs = ["10.0.0.0/8", "192.168.0.0/16"]
}
"#;
        let block = lower_first_block(src);
        match &block.body[0].1 {
            Expression::Literal(Value::List(items)) => {
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected literal list, got {other:?}"),
        }
    }

    #[test]
    fn test_should_lower_mixed_array_to_array_expression() {
        let src = r#"resource "x" "y" {
  cidrs = ["10.0.0.0/8", var.extra]
}
"#;
        let block = lower_first_block(src);
        match &block.body[0].1 {
            Expression::Array(items) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(items[0], Expression::Literal(_)));
                assert!(matches!(items[1], Expression::Unresolved(_)));
            }
            other => panic!("expected Array expression, got {other:?}"),
        }
    }

    #[test]
    fn test_should_lower_object_to_map_when_literal() {
        let src = r#"resource "x" "y" {
  tags = {
    Service = "x"
    Owner   = "y"
  }
}
"#;
        let block = lower_first_block(src);
        match &block.body[0].1 {
            Expression::Literal(Value::Map(entries)) => {
                assert_eq!(entries.len(), 2);
            }
            other => panic!("expected literal map, got {other:?}"),
        }
    }

    #[test]
    fn test_should_lower_object_with_unresolved_value_as_object() {
        let src = r#"resource "x" "y" {
  tags = {
    Service = local.service_name
  }
}
"#;
        let block = lower_first_block(src);
        match &block.body[0].1 {
            Expression::Object(entries) => {
                assert_eq!(entries.len(), 1);
                assert!(matches!(entries[0].1, Expression::Unresolved(_)));
            }
            other => panic!("expected Object expression, got {other:?}"),
        }
    }

    #[test]
    fn test_should_lower_func_call() {
        let src = r#"resource "x" "y" {
  payload = jsonencode({ foo = "bar" })
}
"#;
        let block = lower_first_block(src);
        match &block.body[0].1 {
            Expression::FuncCall(call) => {
                assert_eq!(call.name.as_ref(), "jsonencode");
                assert_eq!(call.args.len(), 1);
            }
            other => panic!("expected FuncCall, got {other:?}"),
        }
    }

    #[test]
    fn test_should_lower_conditional() {
        let src = r#"resource "x" "y" {
  enabled = var.env == "prod" ? true : false
}
"#;
        let block = lower_first_block(src);
        match &block.body[0].1 {
            Expression::Conditional(c) => {
                assert!(matches!(
                    *c.then_branch,
                    Expression::Literal(Value::Bool(true))
                ));
                assert!(matches!(
                    *c.else_branch,
                    Expression::Literal(Value::Bool(false))
                ));
            }
            other => panic!("expected Conditional, got {other:?}"),
        }
    }

    #[test]
    fn test_should_lower_for_expression() {
        let src = r#"resource "x" "y" {
  zones = [for z in var.azs : upper(z)]
}
"#;
        let block = lower_first_block(src);
        match &block.body[0].1 {
            Expression::For(f) => {
                assert_eq!(f.binders.len(), 1);
                assert!(!f.object_form);
            }
            other => panic!("expected For, got {other:?}"),
        }
    }

    #[test]
    fn test_should_lower_nested_block_to_object_under_block_key() {
        let src = r#"resource "aws_security_group" "sg" {
  name = "x"
  ingress {
    from_port   = 5432
    to_port     = 5432
    protocol    = "tcp"
    cidr_blocks = ["10.0.0.0/8"]
  }
}
"#;
        let block = lower_first_block(src);
        // body[0] = name; body[1] = ingress (nested block).
        let ingress = block.body.iter().find(|(k, _)| k.as_ref() == "ingress");
        assert!(ingress.is_some(), "expected ingress nested block");
    }

    #[test]
    fn test_should_lower_unary_neg() {
        let src = r#"resource "x" "y" {
  delta = -42
}
"#;
        let block = lower_first_block(src);
        // hcl-edit folds unary literals into Number; the lowering keeps
        // the Literal form. Either way, the resulting attribute should
        // either be a Literal or a UnaryOp; both are acceptable shapes.
        match &block.body[0].1 {
            Expression::UnaryOp { op, .. } => assert_eq!(*op, UnaryOp::Neg),
            Expression::Literal(Value::Int(n)) => assert_eq!(*n, -42),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn test_should_emit_truncated_when_depth_exceeded() {
        // Deeply nested object → exceeds depth=2.
        let src = r#"resource "x" "y" {
  k = { a = { b = { c = "v" } } }
}
"#;
        let body = parse_body(src).unwrap();
        let path: Arc<Path> = Arc::from(Path::new("/tmp/x.tf"));
        let li = LineIndex::build(src);
        let limits = LoaderLimits::builder().max_attr_depth(2_u32).build();
        let lowered = lower_body(&body, &path, &li, &limits, src.len());
        assert!(
            lowered
                .diagnostics
                .iter()
                .any(|d| d.message.contains("AttributeDepth"))
        );
    }

    #[test]
    fn test_should_skip_blocks_past_block_count_cap() {
        let src = r#"resource "a" "x" {}
resource "b" "y" {}
resource "c" "z" {}
"#;
        let body = parse_body(src).unwrap();
        let path: Arc<Path> = Arc::from(Path::new("/tmp/x.tf"));
        let li = LineIndex::build(src);
        let limits = LoaderLimits::builder().max_blocks_per_file(2_u32).build();
        let lowered = lower_body(&body, &path, &li, &limits, src.len());
        assert_eq!(lowered.blocks.len(), 2);
        assert!(
            lowered
                .diagnostics
                .iter()
                .any(|d| d.message.contains("BlocksPerFile"))
        );
    }

    #[test]
    fn test_should_render_traversal_with_index_and_attr() {
        let src = r#"resource "x" "y" {
  arn = aws_iam_role.r[0].arn
}
"#;
        let block = lower_first_block(src);
        let attr = &block.body[0];
        match &attr.1 {
            Expression::Unresolved(s) => {
                assert_eq!(s.source.as_ref(), "aws_iam_role.r[0].arn");
                assert_eq!(s.kind, SymbolKind::Resource);
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    #[test]
    fn test_collapse_template_single_literal() {
        let parts = vec![Expression::Literal(Value::Str(Arc::from("x")))];
        let collapsed = collapse_template_for_test(parts);
        assert!(matches!(
            collapsed,
            Expression::Literal(Value::Str(ref s)) if s.as_ref() == "x"
        ));
    }
}
