//! The top-level [`Workspace`] IR — what every component spec consumes.

use std::{path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::{
    diagnostic::Diagnostic,
    ir::{Component, Edge, Environment, Module},
};

/// The fully-parsed workspace, ready for export.
///
/// Per [10-data-model.md § 2.1], the workspace owns:
///
/// - the set of components (apply-able roots),
/// - the set of modules referenced from those components,
/// - the discovered environments, and
/// - any non-fatal diagnostics produced anywhere in the pipeline.
///
/// Construct via [`WorkspaceBuilder`].
///
/// [10-data-model.md § 2.1]: ../../specs/10-data-model.md
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Workspace {
    /// Absolute canonical path of the workspace root.
    #[serde(with = "crate::ir::path_serde::arc_path")]
    pub root: Arc<Path>,

    /// Components in path-ascending order (deterministic).
    #[builder(default)]
    pub components: Vec<Component>,

    /// Referenced modules (whether the body was walked or not).
    #[builder(default)]
    pub modules: Vec<Module>,

    /// Discovered environments.
    #[builder(default)]
    pub environments: Vec<Environment>,

    /// Workspace-wide non-fatal diagnostics. Per-component diagnostics
    /// (e.g. from the Terragrunt resolver) hang off their owning
    /// `TerragruntConfig.diagnostics` instead.
    #[builder(default)]
    pub diagnostics: Vec<Diagnostic>,

    /// Dependency edges populated by the graph phase (Phase 8 / M5). The
    /// list is sorted by `(from, to, kind)` for deterministic Parquet
    /// output (see [15-resource-graph.md § 4]).
    ///
    /// [15-resource-graph.md § 4]: ../../specs/15-resource-graph.md
    #[builder(default)]
    pub edges: Vec<Edge>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_should_build_minimal_workspace() {
        let w = Workspace::builder()
            .root(Arc::<Path>::from(PathBuf::from("/tmp/repo")))
            .build();
        assert!(w.components.is_empty());
        assert!(w.modules.is_empty());
        assert!(w.environments.is_empty());
        assert!(w.diagnostics.is_empty());
    }

    #[test]
    fn test_should_serde_round_trip_workspace() {
        let w = Workspace::builder()
            .root(Arc::<Path>::from(PathBuf::from("/tmp/repo")))
            .build();
        let json = serde_json::to_string(&w).unwrap();
        let back: Workspace = serde_json::from_str(&json).unwrap();
        assert_eq!(w, back);
    }
}
