//! Terragrunt configuration attached to a [`Component`](crate::ir::Component).
//!
//! Phase 1 only defines the shape; population happens in Phase 6 per
//! [14-terragrunt.md](../../specs/14-terragrunt.md). The diagnostic field is
//! intentionally a flat `Vec` (not the workspace-level [`Diagnostic`]
//! `Vec`) — Terragrunt-resolution diagnostics are reported per component.

use std::{path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::{
    diagnostic::Diagnostic,
    ir::{AccountId, AttributeMap, Map, Region, Span},
};

/// One entry on the include load chain (deepest last).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct IncludePath {
    /// Resolved absolute path of the included Terragrunt file.
    #[serde(with = "crate::ir::path_serde::arc_path")]
    pub path: Arc<Path>,
    /// Optional `name` label on the `include` block.
    #[builder(default)]
    pub label: Option<Arc<str>>,
    /// Span of the `include` block in the consumer file.
    pub span: Span,
}

/// `generate "label" { ... }` block, captured verbatim. The parser **does
/// not** write the file; consumers can synthesize it if needed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct GenerateBlock {
    /// Label on the `generate` block (e.g. `"backend"`).
    pub label: Arc<str>,
    /// Target path that would be written (relative to the component dir).
    #[serde(with = "crate::ir::path_serde::arc_path")]
    pub path: Arc<Path>,
    /// `if_exists` policy as declared (e.g. `"overwrite_terragrunt"`).
    pub if_exists: Arc<str>,
    /// Verbatim contents.
    pub contents: Arc<str>,
    /// Span of the `generate` keyword.
    pub span: Span,
}

/// `dependency "label" { config_path = "..."; mock_outputs = ... }` block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct DependencyBlock {
    /// Dependency name (e.g. `"vpc"`).
    pub name: Arc<str>,
    /// Resolved absolute path of the dependency component dir.
    #[serde(with = "crate::ir::path_serde::arc_path")]
    pub config_path: Arc<Path>,
    /// Optional `mock_outputs = { ... }` attribute body, kept verbatim.
    #[builder(default)]
    pub mock_outputs: AttributeMap,
    /// Span of the `dependency` block.
    pub span: Span,
}

/// Terraform state backend description.
///
/// Source can be `terraform { backend "s3" {} }` declared in the component
/// directly, or extracted from a `generate "backend"` block's `contents`.
/// `state_account_id` is derived later from `profile` / `role_arn` by the
/// provider resolver.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct StateBackend {
    /// Backend kind (e.g. `"s3"`, `"local"`, `"remote"`).
    pub kind: Arc<str>,
    /// Verbatim attribute body of the backend block.
    pub attributes: AttributeMap,
    /// Account id derived from the backend profile / `role_arn`, if any.
    #[builder(default)]
    pub state_account_id: Option<AccountId>,
    /// Region declared in the backend block, if any.
    #[builder(default)]
    pub state_region: Option<Region>,
    /// Span of the backend block.
    pub span: Span,
}

/// Per-component Terragrunt configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct TerragruntConfig {
    /// Component directory (absolute path).
    #[serde(with = "crate::ir::path_serde::arc_path")]
    pub component_dir: Arc<Path>,

    /// Locals post-cascade merge. Drives the evaluator context for the
    /// component's `.tf` files.
    #[builder(default)]
    pub effective_locals: Map,

    /// Resolved `inputs { ... }` from the component's terragrunt.hcl.
    #[builder(default)]
    pub inputs: Map,

    /// Include chain (deepest last).
    #[builder(default)]
    pub includes: Vec<IncludePath>,

    /// `generate "label" { ... }` blocks captured for downstream consumers.
    #[builder(default)]
    pub generates: Vec<GenerateBlock>,

    /// `dependency "label" { ... }` blocks.
    #[builder(default)]
    pub dependencies: Vec<DependencyBlock>,

    /// State backend extracted from `terraform { backend ... }` or
    /// `generate "backend"`.
    #[builder(default)]
    pub state_backend: Option<StateBackend>,

    /// Non-fatal diagnostics emitted during resolution.
    #[builder(default)]
    pub diagnostics: Vec<Diagnostic>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ir::Span;

    #[test]
    fn test_should_build_minimal_terragrunt_config() {
        let cfg = TerragruntConfig::builder()
            .component_dir(Arc::<Path>::from(PathBuf::from("/repo/services/x")))
            .build();
        assert!(cfg.effective_locals.is_empty());
        assert!(cfg.includes.is_empty());
    }

    #[test]
    fn test_should_serde_round_trip_generate_block() {
        let g = GenerateBlock {
            label: Arc::<str>::from("backend"),
            path: Arc::<Path>::from(PathBuf::from("generated_backend.tf")),
            if_exists: Arc::<str>::from("overwrite_terragrunt"),
            contents: Arc::<str>::from("terraform { backend \"s3\" {} }"),
            span: Span::synthetic(),
        };
        let json = serde_json::to_string(&g).unwrap();
        let back: GenerateBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(g, back);
    }
}
