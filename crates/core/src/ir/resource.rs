//! Resource / data-source IR nodes — the unit of analysis the exporter
//! emits one row per.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::ir::{AccountId, Address, AttributeMap, Expression, ProviderRef, Region, Span};

/// Whether an IR node was declared as `resource` or `data`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ResourceKind {
    /// `resource "type" "name" {}` — a managed resource.
    Managed,
    /// `data "type" "name" {}` — a data source.
    Data,
}

/// Top-level HCL block kinds the loader distinguishes.
///
/// Mirrors [12-hcl-loader.md § 2]'s `BlockKind`.
///
/// [12-hcl-loader.md § 2]: ../../specs/12-hcl-loader.md
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum BlockKind {
    /// `resource "type" "name" {}`
    Resource,
    /// `data "type" "name" {}`
    Data,
    /// `module "name" { source = ... }`
    Module,
    /// `provider "name" { alias = ..., ... }`
    Provider,
    /// `variable "name" {}`
    Variable,
    /// `locals { ... }`
    Locals,
    /// `output "name" {}`
    Output,
    /// `terraform { ... }`
    Terraform,
    /// `include "label" { ... }` (Terragrunt)
    Include,
    /// `generate "label" { ... }` (Terragrunt)
    Generate,
    /// `dependency "label" { ... }` (Terragrunt)
    Dependency,
    /// `inputs { ... }` (Terragrunt)
    Inputs,
    /// Anything else (e.g. user-defined `dynamic` block).
    Unknown,
}

/// A `resource` or `data` block, post-evaluator. Phase 1 only defines the
/// shape; population happens in Phase 3 (loader → IR) and onwards.
///
/// Field order matches [10-data-model.md § 2.2]. Build with the generated
/// [`ResourceBuilder`] (typed-builder) — `Resource { … }` is rejected
/// outside this crate by `#[non_exhaustive]`.
///
/// [10-data-model.md § 2.2]: ../../specs/10-data-model.md
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Resource {
    /// Full TF address (e.g. `module.pacer_db.aws_db_instance.this`).
    pub address: Address,

    /// `resource` vs `data`.
    pub kind: ResourceKind,

    /// Block label 1 (e.g. `aws_db_instance`).
    pub type_: Arc<str>,

    /// Block label 2 (e.g. `this`).
    pub name: Arc<str>,

    /// Per-resource provider selection (`provider = aws.<alias>` attribute).
    #[builder(default)]
    pub provider_ref: Option<ProviderRef>,

    /// `count = ...` expression.
    #[builder(default)]
    pub count_expr: Option<Expression>,

    /// `for_each = ...` expression.
    #[builder(default)]
    pub for_each_expr: Option<Expression>,

    /// Explicit + inferred dependencies (graph phase fills inferred).
    #[builder(default)]
    pub depends_on: Vec<Address>,

    /// Top-level attributes of the body. Nested blocks land here as
    /// [`crate::ir::Value::Map`] structures inside an
    /// [`Expression::Literal`] once the loader has lowered them.
    #[builder(default)]
    pub attributes: AttributeMap,

    /// Resolved AWS account id (12 digits). Filled by the provider resolver
    /// (Phase 7) per [16-provider-resolver.md § 4]. `None` until resolution
    /// runs or when no profile mapping could be inferred (the column then
    /// emits `""` per spec 10 § 3).
    ///
    /// [16-provider-resolver.md § 4]: ../../specs/16-provider-resolver.md
    #[builder(default)]
    pub account_id: Option<AccountId>,

    /// Human-friendly account name, from the profile-map entry the resolver
    /// matched. `None` when unresolved or absent.
    #[builder(default)]
    pub account_name: Option<Arc<str>>,

    /// Resolved AWS region. Filled by the provider resolver (Phase 7) per
    /// [16-provider-resolver.md § 4]. `None` until resolution runs.
    #[builder(default)]
    pub region: Option<Region>,

    /// Source position of the opening `resource`/`data` keyword.
    pub span: Span,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ir::{Address, Span};

    #[test]
    fn test_should_build_minimal_resource() {
        let r = Resource::builder()
            .address(Address::new("aws_db_instance.x").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_db_instance"))
            .name(Arc::<str>::from("x"))
            .span(Span::synthetic())
            .build();
        assert_eq!(r.address.as_str(), "aws_db_instance.x");
        assert!(r.depends_on.is_empty());
        assert!(r.attributes.is_empty());
        assert!(r.provider_ref.is_none());
    }

    #[test]
    fn test_should_serde_round_trip_resource() {
        let r = Resource::builder()
            .address(Address::new("aws_iam_role.r").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_iam_role"))
            .name(Arc::<str>::from("r"))
            .span(Span::synthetic())
            .build();
        let json = serde_json::to_string(&r).unwrap();
        let back: Resource = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
