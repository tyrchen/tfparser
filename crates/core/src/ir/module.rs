//! Module call sites + the module bodies they resolve to.

use std::{path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::ir::{Address, AttributeMap, Component, Expression, ModuleId, ProviderRef, Span};

/// Where a `module "x" { source = "..." }` call points.
///
/// Only [`ModuleSource::Local`] is resolvable source-only by the parser;
/// other variants are captured so the dependency-graph phase can emit a
/// row in `modules.parquet` even when the body is not walked.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum ModuleSource {
    /// Local path source (`./...` or `../...`).
    Local(Arc<str>),

    /// Terraform registry address (e.g. `terraform-aws-modules/eks/aws`).
    Registry(Arc<str>),

    /// Git source (`git::https://...`).
    Git(Arc<str>),

    /// Anything else — captured verbatim.
    External(Arc<str>),
}

impl ModuleSource {
    /// Classify a raw `source = "..."` value.
    #[must_use]
    pub fn classify(raw: &str) -> Self {
        let raw_arc: Arc<str> = Arc::from(raw);
        if raw.starts_with("./") || raw.starts_with("../") {
            Self::Local(raw_arc)
        } else if raw.starts_with("git::")
            || raw.starts_with("git@")
            || raw.contains(".git")
            || raw.starts_with("github.com/")
        {
            Self::Git(raw_arc)
        } else if raw.contains('/') && !raw.starts_with('/') && raw.matches('/').count() >= 2 {
            // `<namespace>/<name>/<provider>` shape.
            Self::Registry(raw_arc)
        } else {
            Self::External(raw_arc)
        }
    }
}

/// A `module "name" { source = "..."; ... }` call site.
///
/// Field order matches [10-data-model.md § 2.2].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct ModuleCall {
    /// Full TF address of the call (e.g. `module.pacer_db`).
    pub address: Address,

    /// Verbatim source string from the HCL.
    pub source_raw: Arc<str>,

    /// Classified source.
    pub source: ModuleSource,

    /// Set by the graph phase once the source has been resolved to a
    /// [`Module`] in the registry.
    #[builder(default)]
    pub resolved: Option<ModuleId>,

    /// `providers = { aws = aws.main }` rewrites the call site applies.
    #[builder(default)]
    pub providers: Vec<(Arc<str>, ProviderRef)>,

    /// Input expressions passed into the module.
    #[builder(default)]
    pub inputs: AttributeMap,

    /// `count = ...` expression.
    #[builder(default)]
    pub count_expr: Option<Expression>,

    /// `for_each = ...` expression.
    #[builder(default)]
    pub for_each_expr: Option<Expression>,

    /// Span of the opening `module` keyword.
    pub span: Span,
}

/// A reusable module body, addressed by [`ModuleId`].
///
/// Modules are referenced via [`ModuleCall::source`]. The graph phase
/// walks local modules and populates [`Module::component`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Module {
    /// Stable-within-a-run id.
    pub id: ModuleId,

    /// Classified source.
    pub source: ModuleSource,

    /// Canonical directory of the module (only set for [`ModuleSource::Local`]).
    #[builder(default)]
    #[serde(with = "crate::ir::path_serde::arc_path_opt")]
    pub canonical_path: Option<Arc<Path>>,

    /// The module body, parsed as a [`Component`] with kind
    /// [`crate::ir::ComponentKind::Module`].
    pub component: Component,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_should_classify_local_source() {
        assert!(matches!(
            ModuleSource::classify("./foo"),
            ModuleSource::Local(_)
        ));
        assert!(matches!(
            ModuleSource::classify("../../modules/rds"),
            ModuleSource::Local(_)
        ));
    }

    #[test]
    fn test_should_classify_git_source() {
        for s in [
            "git::https://github.com/x/y.git",
            "github.com/x/y",
            "git@github.com:x/y.git",
        ] {
            assert!(
                matches!(ModuleSource::classify(s), ModuleSource::Git(_)),
                "expected Git classification for {s}"
            );
        }
    }

    #[test]
    fn test_should_classify_registry_source() {
        assert!(matches!(
            ModuleSource::classify("terraform-aws-modules/eks/aws"),
            ModuleSource::Registry(_)
        ));
    }

    #[test]
    fn test_should_round_trip_module_source_via_serde() {
        let s = ModuleSource::Local(Arc::<str>::from("./foo"));
        let json = serde_json::to_string(&s).unwrap();
        let back: ModuleSource = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn test_should_classify_absolute_unix_path_as_external() {
        // `/abs/path` is not a local-relative source and not a registry
        // address. We capture it as External; the resolver can refuse to
        // walk it.
        assert!(matches!(
            ModuleSource::classify("/abs/path"),
            ModuleSource::External(_)
        ));
    }

    #[test]
    fn test_should_classify_bare_name_as_external() {
        assert!(matches!(
            ModuleSource::classify("just_a_name"),
            ModuleSource::External(_)
        ));
    }

    #[test]
    fn test_should_classify_single_slash_as_external_not_registry() {
        // `foo/bar` has only one slash and is not a 3-segment registry
        // address; treat as External.
        assert!(matches!(
            ModuleSource::classify("foo/bar"),
            ModuleSource::External(_)
        ));
    }
}
