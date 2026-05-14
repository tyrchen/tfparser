//! Possibly-unresolved HCL expressions.
//!
//! Per [10-data-model.md § 2.3], the evaluator reduces each `Expression` to
//! either a fully-resolved [`Value`] (via [`Expression::Literal`]) or leaves
//! it as a symbolic node that survives intact to the exporter.
//! [`Expression::Unresolved`] is the **only** variant that may carry a
//! symbolic reference after evaluation; all other variants are reduced or
//! left as partial subtrees per the propagation rules in
//! [13-evaluator.md § 6].
//!
//! [10-data-model.md § 2.3]: ../../specs/10-data-model.md
//! [13-evaluator.md § 6]: ../../specs/13-evaluator.md

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::ir::{Address, Span, Value};

/// Insertion-ordered association list of attribute name → expression. Used
/// for every HCL body that has attributes (resource bodies, provider
/// configs, module inputs, etc.).
pub type AttributeMap = Vec<(Arc<str>, Expression)>;

/// HCL binary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum BinaryOp {
    /// `a + b`
    Add,
    /// `a - b`
    Sub,
    /// `a * b`
    Mul,
    /// `a / b`
    Div,
    /// `a % b`
    Mod,
    /// `a == b`
    Eq,
    /// `a != b`
    Ne,
    /// `a < b`
    Lt,
    /// `a <= b`
    Le,
    /// `a > b`
    Gt,
    /// `a >= b`
    Ge,
    /// `a && b`
    And,
    /// `a || b`
    Or,
}

/// HCL unary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum UnaryOp {
    /// `-a`
    Neg,
    /// `!a`
    Not,
}

/// What an [`Expression::Unresolved`] refers to syntactically. Used by the
/// dependency-graph phase to derive edges and by the exporter to emit the
/// `__kind__` discriminator in canonical JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[non_exhaustive]
pub enum SymbolKind {
    /// `var.<name>`
    Var,
    /// `local.<name>`
    Local,
    /// `<type>.<name>[.<attr>...]`
    Resource,
    /// `data.<type>.<name>[.<attr>...]`
    Data,
    /// `module.<name>[.<output>]`
    Module,
    /// `path.module`, `path.root`, etc.
    Path,
    /// `each.key`, `each.value`, `count.index`
    Iteration,
    /// `terraform.workspace`, etc.
    Terraform,
    /// `dependency.<name>.outputs.<x>` (Terragrunt)
    TerragruntDependency,
    /// Anything we recognised as a reference but cannot place in a more
    /// specific bucket.
    Other,
}

/// A symbolic reference left unresolved by the evaluator.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Symbolic {
    /// What syntactic shape this reference has.
    pub kind: SymbolKind,

    /// Verbatim source of the reference (e.g. `"var.environment"`,
    /// `"aws_iam_role.r.arn"`).
    pub source: Arc<str>,

    /// Parsed address form when the reference resolves to a Terraform
    /// address. `None` for `Path`, `Iteration`, `Terraform`, etc.
    #[builder(default)]
    pub address_hint: Option<Address>,

    /// Where this reference was written.
    pub span: Span,
}

/// An HCL function call — used both for unevaluated calls (when the
/// evaluator cannot reduce the call, e.g. due to unresolved arguments) and
/// as an intermediate representation during evaluation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct FuncCall {
    /// Function name (e.g. `"jsonencode"`).
    pub name: Arc<str>,
    /// Argument expressions, in order.
    pub args: Vec<Expression>,
    /// Where the call appears.
    pub span: Span,
}

/// An HCL conditional `cond ? then : else`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct Conditional {
    /// Condition expression.
    pub cond: Box<Expression>,
    /// Branch evaluated when `cond` is true.
    pub then_branch: Box<Expression>,
    /// Branch evaluated when `cond` is false.
    pub else_branch: Box<Expression>,
    /// Span of the whole conditional.
    pub span: Span,
}

/// An HCL `for` comprehension — captured verbatim when the evaluator cannot
/// reduce it (typically because the source collection is unresolved).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct ForExpr {
    /// Iteration variable name(s): `[key, value]` if a key was bound, else
    /// `[value]`.
    pub binders: Vec<Arc<str>>,
    /// Source collection.
    pub collection: Box<Expression>,
    /// Yielded key expression (object form only).
    #[builder(default)]
    pub key: Option<Box<Expression>>,
    /// Yielded value expression.
    pub value: Box<Expression>,
    /// Optional `if` clause.
    #[builder(default)]
    pub cond: Option<Box<Expression>>,
    /// Whether this is an object comprehension (`{...}`) vs. a list one (`[...]`).
    #[builder(default = false)]
    pub object_form: bool,
    /// Span.
    pub span: Span,
}

/// A possibly-resolved HCL expression.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase", tag = "kind", content = "node")]
pub enum Expression {
    /// A fully-resolved value.
    Literal(Value),

    /// A symbolic reference the evaluator could not resolve (`var.x`,
    /// `local.y`, `data.z.w`, `aws_iam_role.r.arn`, etc.).
    Unresolved(Symbolic),

    /// Binary operation. Subtrees may themselves be `Unresolved`.
    BinaryOp {
        /// Operator.
        op: BinaryOp,
        /// Left-hand side.
        lhs: Box<Expression>,
        /// Right-hand side.
        rhs: Box<Expression>,
        /// Span of the whole operation.
        span: Span,
    },

    /// Unary operation.
    UnaryOp {
        /// Operator.
        op: UnaryOp,
        /// Operand.
        operand: Box<Expression>,
        /// Span.
        span: Span,
    },

    /// Template concatenation, e.g. `"foo-${var.x}-bar"` → parts.
    TemplateConcat(Vec<Expression>),

    /// Function call.
    FuncCall(Box<FuncCall>),

    /// `cond ? a : b`.
    Conditional(Box<Conditional>),

    /// `[for ... in ...]` / `{for ... in ...}`.
    For(Box<ForExpr>),
}

impl Expression {
    /// Returns the resolved [`Value`] if this expression is a
    /// [`Expression::Literal`].
    #[must_use]
    pub fn as_literal(&self) -> Option<&Value> {
        match self {
            Self::Literal(v) => Some(v),
            _ => None,
        }
    }

    /// `true` iff every leaf in the expression tree is a [`Value`] (no
    /// [`Symbolic`] anywhere).
    #[must_use]
    pub fn is_fully_resolved(&self) -> bool {
        match self {
            Self::Literal(_) => true,
            Self::Unresolved(_) => false,
            Self::BinaryOp { lhs, rhs, .. } => lhs.is_fully_resolved() && rhs.is_fully_resolved(),
            Self::UnaryOp { operand, .. } => operand.is_fully_resolved(),
            Self::TemplateConcat(parts) => parts.iter().all(Self::is_fully_resolved),
            Self::FuncCall(call) => call.args.iter().all(Self::is_fully_resolved),
            Self::Conditional(c) => {
                c.cond.is_fully_resolved()
                    && c.then_branch.is_fully_resolved()
                    && c.else_branch.is_fully_resolved()
            }
            Self::For(f) => {
                f.collection.is_fully_resolved()
                    && f.value.is_fully_resolved()
                    && f.key.as_ref().is_none_or(|k| k.is_fully_resolved())
                    && f.cond.as_ref().is_none_or(|c| c.is_fully_resolved())
            }
        }
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
    use std::{path::Path, sync::Arc};

    use super::*;

    fn fake_span() -> Span {
        Span::synthetic()
    }

    #[test]
    fn test_should_classify_literal_as_resolved() {
        let e = Expression::Literal(Value::Int(42));
        assert!(e.is_fully_resolved());
        assert_eq!(e.as_literal(), Some(&Value::Int(42)));
    }

    #[test]
    fn test_should_classify_unresolved_as_not_resolved() {
        let e = Expression::Unresolved(Symbolic {
            kind: SymbolKind::Var,
            source: Arc::from("var.environment"),
            address_hint: Some(Address::new("var.environment").unwrap()),
            span: fake_span(),
        });
        assert!(!e.is_fully_resolved());
    }

    #[test]
    fn test_should_recurse_into_binary_op() {
        let e = Expression::BinaryOp {
            op: BinaryOp::Add,
            lhs: Box::new(Expression::Literal(Value::Int(1))),
            rhs: Box::new(Expression::Unresolved(Symbolic {
                kind: SymbolKind::Local,
                source: Arc::from("local.x"),
                address_hint: None,
                span: fake_span(),
            })),
            span: fake_span(),
        };
        assert!(!e.is_fully_resolved());
    }

    #[test]
    fn test_should_serde_round_trip_expression_tree() {
        let span = Span::new(Arc::from(Path::new("/tmp/x.tf")), 0..1, 1, 1).unwrap();
        let e = Expression::TemplateConcat(vec![
            Expression::Literal(Value::Str(Arc::from("prefix-"))),
            Expression::Unresolved(Symbolic {
                kind: SymbolKind::Var,
                source: Arc::from("var.environment"),
                address_hint: None,
                span: span.clone(),
            }),
        ]);
        let json = serde_json::to_string(&e).unwrap();
        let back: Expression = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn test_should_serde_round_trip_func_call() {
        let call = FuncCall {
            name: Arc::from("jsonencode"),
            args: vec![Expression::Literal(Value::Str(Arc::from("hi")))],
            span: fake_span(),
        };
        let e = Expression::FuncCall(Box::new(call));
        let json = serde_json::to_string(&e).unwrap();
        let back: Expression = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn test_func_call_with_unresolved_argument_is_unresolved() {
        let call = FuncCall {
            name: Arc::from("jsonencode"),
            args: vec![Expression::Unresolved(Symbolic {
                kind: SymbolKind::Var,
                source: Arc::from("var.x"),
                address_hint: None,
                span: fake_span(),
            })],
            span: fake_span(),
        };
        let e = Expression::FuncCall(Box::new(call));
        assert!(!e.is_fully_resolved());
    }

    #[test]
    fn test_should_serde_round_trip_conditional() {
        let cond = Conditional {
            cond: Box::new(Expression::Literal(Value::Bool(true))),
            then_branch: Box::new(Expression::Literal(Value::Int(1))),
            else_branch: Box::new(Expression::Literal(Value::Int(2))),
            span: fake_span(),
        };
        let e = Expression::Conditional(Box::new(cond));
        assert!(e.is_fully_resolved());
        let json = serde_json::to_string(&e).unwrap();
        let back: Expression = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn test_conditional_with_unresolved_branch_is_unresolved() {
        let cond = Conditional {
            cond: Box::new(Expression::Literal(Value::Bool(true))),
            then_branch: Box::new(Expression::Literal(Value::Int(1))),
            else_branch: Box::new(Expression::Unresolved(Symbolic {
                kind: SymbolKind::Var,
                source: Arc::from("var.x"),
                address_hint: None,
                span: fake_span(),
            })),
            span: fake_span(),
        };
        let e = Expression::Conditional(Box::new(cond));
        assert!(!e.is_fully_resolved());
    }

    #[test]
    fn test_should_serde_round_trip_for_expr() {
        let f = ForExpr {
            binders: vec![Arc::from("k"), Arc::from("v")],
            collection: Box::new(Expression::Literal(Value::List(vec![Value::Int(1)]))),
            key: Some(Box::new(Expression::Unresolved(Symbolic {
                kind: SymbolKind::Iteration,
                source: Arc::from("k"),
                address_hint: None,
                span: fake_span(),
            }))),
            value: Box::new(Expression::Literal(Value::Bool(true))),
            cond: None,
            object_form: true,
            span: fake_span(),
        };
        let e = Expression::For(Box::new(f));
        let json = serde_json::to_string(&e).unwrap();
        let back: Expression = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
        assert!(!e.is_fully_resolved(), "Iteration ref keeps it unresolved");
    }
}
