//! Inter-resource and component-to-component dependency edges.
//!
//! Phase 8 (M5) introduces the dependency-graph projection. An [`Edge`]
//! links a [`Resource`](crate::ir::Resource) (or [`Component`](crate::ir::Component))
//! address to the address it references — either explicitly via
//! `depends_on`, implicitly via a symbolic attribute reference, or across
//! components via a Terragrunt `dependency` block.
//!
//! Per [10-data-model.md § 5.1] (the `dependencies.parquet` schema) and
//! [15-resource-graph.md § 4].
//!
//! [10-data-model.md § 5.1]: ../../specs/10-data-model.md
//! [15-resource-graph.md § 4]: ../../specs/15-resource-graph.md

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::ir::{Address, Span};

/// How an edge was discovered.
///
/// `#[non_exhaustive]` per CLAUDE.md § Type Design — a future
/// `EdgeKind::LifecycleReference` lands additively without breaking the
/// secondary-table schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EdgeKind {
    /// `depends_on = [aws_x.y]` — explicit dependency declared in source.
    ExplicitDependsOn,
    /// An attribute body referenced the target (e.g. `policy = aws_iam_policy.p.arn`).
    AttrRef,
    /// Module call input contained a reference to a sibling resource / module.
    ModuleInput,
    /// Terragrunt `dependency "x" { config_path = "..." }` — points at a
    /// **component**, not a resource address.
    TerragruntDependency,
}

impl EdgeKind {
    /// Stable string discriminator used for the `edge_kind` Parquet column.
    /// Spec 10 § 5.1 pins the exact tokens.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitDependsOn => "explicit_depends_on",
            Self::AttrRef => "attr_ref",
            Self::ModuleInput => "module_input",
            Self::TerragruntDependency => "terragrunt_dependency",
        }
    }
}

/// One dependency edge.
///
/// Field order matches [10-data-model.md § 5.1]. Build via the generated
/// [`EdgeBuilder`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Edge {
    /// Address holding the reference (the "from" / source of the arrow).
    pub from: Address,
    /// Address being referenced.
    pub to: Address,
    /// How the edge was discovered.
    pub kind: EdgeKind,
    /// Attribute path that introduced the edge (e.g. `"policy"`,
    /// `"subnets[0].id"`). `None` for explicit `depends_on` edges and
    /// Terragrunt dependencies.
    #[builder(default)]
    pub attr: Option<Arc<str>>,
    /// Where the reference appears in source.
    pub span: Span,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ir::Span;

    #[test]
    fn test_edge_kind_string_form_is_stable() {
        assert_eq!(EdgeKind::ExplicitDependsOn.as_str(), "explicit_depends_on");
        assert_eq!(EdgeKind::AttrRef.as_str(), "attr_ref");
        assert_eq!(EdgeKind::ModuleInput.as_str(), "module_input");
        assert_eq!(
            EdgeKind::TerragruntDependency.as_str(),
            "terragrunt_dependency"
        );
    }

    #[test]
    fn test_should_build_minimal_edge() {
        let e = Edge::builder()
            .from(Address::new("aws_iam_role.r").unwrap())
            .to(Address::new("aws_iam_policy.p").unwrap())
            .kind(EdgeKind::AttrRef)
            .attr(Some(Arc::<str>::from("policy")))
            .span(Span::synthetic())
            .build();
        assert_eq!(e.from.as_str(), "aws_iam_role.r");
        assert_eq!(e.kind, EdgeKind::AttrRef);
    }

    #[test]
    fn test_should_serde_round_trip_edge() {
        let e = Edge::builder()
            .from(Address::new("aws_iam_role.r").unwrap())
            .to(Address::new("aws_iam_policy.p").unwrap())
            .kind(EdgeKind::AttrRef)
            .span(Span::synthetic())
            .build();
        let json = serde_json::to_string(&e).unwrap();
        let back: Edge = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
