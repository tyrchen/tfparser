//! Component IR: the apply-able unit of a Terraform / Terragrunt workspace.

use std::{path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::ir::{
    ComponentId, Expression, ModuleCall, ProviderBlock, Resource, SourceFile, Span, StateBackend,
    TerragruntConfig,
};

/// Whether a [`Component`] is an apply-able root or a reusable module body.
///
/// Set by discovery / module resolution. See [11-discovery.md § 3.2] for the
/// classification heuristics.
///
/// [11-discovery.md § 3.2]: ../../specs/11-discovery.md
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ComponentKind {
    /// An apply-able root directory: contains `terragrunt.hcl`, or
    /// `terraform { backend ... }`, or `resource` blocks at top level.
    Component,
    /// A reusable module body — referenced via `module "x" { source =
    /// "..." }` from a component or another module.
    Module,
}

/// A `variable "name" {}` declaration inside a component.
///
/// `Debug` redacts `default` when `sensitive == true`, per
/// [10-data-model.md § 2.5 (I-IR-8)](../../specs/10-data-model.md).
#[derive(Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Variable {
    /// Variable name.
    pub name: Arc<str>,
    /// Optional `description = "..."`.
    #[builder(default)]
    pub description: Option<Arc<str>>,
    /// Optional `type = ...` expression (kept as-is; we do not parse the
    /// HCL type-constructor mini-language in Phase 1).
    #[builder(default)]
    pub type_expr: Option<Expression>,
    /// Optional `default = ...` value. Pre-evaluation this is an
    /// [`Expression`]; post-evaluation it may be reduced to a
    /// [`Value`](crate::ir::Value).
    #[builder(default)]
    pub default: Option<Expression>,
    /// Whether the variable was declared `sensitive = true`.
    #[builder(default = false)]
    pub sensitive: bool,
    /// Span of the `variable` keyword.
    pub span: Span,
}

impl std::fmt::Debug for Variable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("Variable");
        s.field("name", &self.name)
            .field("description", &self.description)
            .field("type_expr", &self.type_expr.as_ref().map(|_| "<expr>"))
            .field("sensitive", &self.sensitive);
        if self.sensitive {
            s.field("default", &self.default.as_ref().map(|_| "<redacted>"));
        } else {
            s.field("default", &self.default);
        }
        s.field("span", &self.span).finish()
    }
}

/// A `locals { ... }` entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Local {
    /// Local name (left of `=`).
    pub name: Arc<str>,
    /// Right-hand side expression.
    pub value: Expression,
    /// Span of the assignment.
    pub span: Span,
}

/// An `output "name" {}` declaration.
///
/// `Debug` redacts `value` when `sensitive == true`, per
/// [10-data-model.md § 2.5 (I-IR-8)](../../specs/10-data-model.md).
#[derive(Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Output {
    /// Output name.
    pub name: Arc<str>,
    /// `value = ...` expression.
    pub value: Expression,
    /// Optional `description = "..."`.
    #[builder(default)]
    pub description: Option<Arc<str>>,
    /// Whether the output was declared `sensitive = true`.
    #[builder(default = false)]
    pub sensitive: bool,
    /// Span of the `output` keyword.
    pub span: Span,
}

impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("Output");
        s.field("name", &self.name)
            .field("description", &self.description)
            .field("sensitive", &self.sensitive);
        if self.sensitive {
            s.field("value", &"<redacted>");
        } else {
            s.field("value", &self.value);
        }
        s.field("span", &self.span).finish()
    }
}

/// An apply-able component (Terraform "root module").
///
/// Field order matches [10-data-model.md § 2.1]. Build via
/// [`ComponentBuilder`].
///
/// [10-data-model.md § 2.1]: ../../specs/10-data-model.md
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Component {
    /// Stable-within-a-run id.
    pub id: ComponentId,

    /// Path of the component dir, relative to [`crate::ir::Workspace::root`].
    #[serde(with = "crate::ir::path_serde::arc_path")]
    pub path: Arc<Path>,

    /// Whether this is an apply-able root or a reusable module body.
    pub kind: ComponentKind,

    /// Source files belonging to this component (`*.tf`, `*.tfvars`,
    /// `terragrunt.hcl`, …).
    #[builder(default)]
    pub files: Vec<SourceFile>,

    /// `variable "..." {}` blocks declared in the component.
    #[builder(default)]
    pub variables: Vec<Variable>,

    /// `locals { ... }` entries (flattened across multiple `locals` blocks).
    #[builder(default)]
    pub locals: Vec<Local>,

    /// `provider "..." {}` declarations.
    #[builder(default)]
    pub providers: Vec<ProviderBlock>,

    /// `resource` and `data` blocks, post-evaluation. Module-expanded
    /// resources from child modules are appended here at the graph phase.
    #[builder(default)]
    pub resources: Vec<Resource>,

    /// `module "name" { ... }` call sites. Per
    /// [10-data-model.md § 2.1](../../specs/10-data-model.md), the field is
    /// named `modules` even though [`Workspace::modules`](crate::ir::Workspace::modules)
    /// also exists — they are different types (`ModuleCall` here vs `Module`
    /// there) and the local name matches the HCL keyword.
    #[builder(default)]
    pub modules: Vec<ModuleCall>,

    /// `output "..." {}` blocks.
    #[builder(default)]
    pub outputs: Vec<Output>,

    /// Terragrunt-driven configuration, if any.
    #[builder(default)]
    pub terragrunt: Option<TerragruntConfig>,

    /// Resolved state backend from `terraform { backend ... }` or
    /// `generate "backend"`.
    #[builder(default)]
    pub state_backend: Option<StateBackend>,
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
    use crate::ir::{ComponentId, Value};

    #[test]
    fn test_should_build_minimal_component() {
        let c = Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from("services/api-gateway")))
            .kind(ComponentKind::Component)
            .build();
        assert_eq!(c.kind, ComponentKind::Component);
        assert!(c.files.is_empty());
        assert!(c.resources.is_empty());
        assert!(c.terragrunt.is_none());
    }

    #[test]
    fn test_should_redact_sensitive_variable_default_in_debug() {
        let v = Variable::builder()
            .name(Arc::<str>::from("db_password"))
            .default(Some(Expression::Literal(Value::Str(Arc::<str>::from(
                "very-secret",
            )))))
            .sensitive(true)
            .span(Span::synthetic())
            .build();
        let debug = format!("{v:?}");
        assert!(!debug.contains("very-secret"), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
    }

    #[test]
    fn test_should_not_redact_non_sensitive_variable_default() {
        let v = Variable::builder()
            .name(Arc::<str>::from("region"))
            .default(Some(Expression::Literal(Value::Str(Arc::<str>::from(
                "us-west-2",
            )))))
            .span(Span::synthetic())
            .build();
        let debug = format!("{v:?}");
        assert!(debug.contains("us-west-2"), "{debug}");
    }

    #[test]
    fn test_should_redact_sensitive_output_value_in_debug() {
        let o = Output::builder()
            .name(Arc::<str>::from("db_endpoint"))
            .value(Expression::Literal(Value::Str(Arc::<str>::from(
                "very-secret-endpoint",
            ))))
            .sensitive(true)
            .span(Span::synthetic())
            .build();
        let debug = format!("{o:?}");
        assert!(!debug.contains("very-secret-endpoint"), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
    }

    #[test]
    fn test_should_round_trip_local_via_serde() {
        let l = Local::builder()
            .name(Arc::<str>::from("name_prefix"))
            .value(Expression::Literal(Value::Str(Arc::<str>::from(
                "northwind",
            ))))
            .span(Span::synthetic())
            .build();
        let json = serde_json::to_string(&l).unwrap();
        let back: Local = serde_json::from_str(&json).unwrap();
        assert_eq!(l, back);
    }
}
