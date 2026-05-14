//! Module registry — index of [`EvaluatedComponent`] bodies keyed by their
//! canonical directory path.
//!
//! Per [15-resource-graph.md § 2], the graph phase consumes
//! `Vec<EvaluatedComponent>` (per-component evaluator output) plus a
//! [`ModuleRegistry`] that resolves `ModuleCall.source` to a walked module
//! body. The orchestrator (Phase 5 pipeline wiring) builds the registry by
//! evaluating every module dir the discovery phase classified as
//! `DirKind::Module` and keying it by `canonical_path`.
//!
//! Non-local sources (Registry, Git, External) cannot be walked source-only;
//! they live in [`ExternalModuleRef`] so the dependency-graph phase can still
//! emit a row in `modules.parquet`.
//!
//! [15-resource-graph.md § 2]: ../../../specs/15-resource-graph.md

use std::{collections::HashMap, path::Path, sync::Arc};

use crate::{
    eval::EvaluatedComponent,
    ir::{ModuleSource, Span},
};

/// An external module reference captured for the dependency graph but **not**
/// walked source-only (Registry, Git, or generic External source).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ExternalModuleRef {
    /// Verbatim `source = "..."` value.
    pub source_raw: Arc<str>,
    /// Classified source.
    pub source: ModuleSource,
    /// Call site span (one of the call sites; the registry de-dupes by
    /// `source_raw`).
    pub first_seen: Span,
}

/// Map of local module dirs (canonical paths) to their evaluator output.
///
/// The graph phase looks up `ModuleCall.source` after canonicalising it
/// relative to the calling component's dir; a hit drops the module's
/// `EvaluatedComponent` into the expansion pipeline.
///
/// External (non-local) sources land in `external_refs` so the
/// `modules.parquet` writer (Phase 8) can still emit a row for them.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ModuleRegistry {
    /// Canonical-path keyed map. The path is **absolute** and canonical so
    /// `..`-laden sources from different call sites resolve to the same
    /// entry.
    pub local_modules: HashMap<Arc<Path>, EvaluatedComponent>,
    /// External / unwalked references, de-duplicated by `source_raw`.
    pub external_refs: Vec<ExternalModuleRef>,
}

impl ModuleRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a local module body keyed by its canonical absolute path.
    /// Idempotent — re-inserting the same path replaces the value (last
    /// writer wins; orchestrator guarantees uniqueness).
    pub fn insert_local(&mut self, canonical: Arc<Path>, component: EvaluatedComponent) {
        self.local_modules.insert(canonical, component);
    }

    /// Record an external reference if not already present (key:
    /// `source_raw`).
    pub fn record_external(&mut self, source_raw: Arc<str>, source: ModuleSource, span: Span) {
        if self
            .external_refs
            .iter()
            .any(|e| e.source_raw == source_raw)
        {
            return;
        }
        self.external_refs.push(ExternalModuleRef {
            source_raw,
            source,
            first_seen: span,
        });
    }

    /// Look up a local module body by canonical path.
    #[must_use]
    pub fn get_local(&self, canonical: &Path) -> Option<&EvaluatedComponent> {
        self.local_modules.get(canonical)
    }

    /// Count of local modules in the registry.
    #[must_use]
    pub fn local_count(&self) -> usize {
        self.local_modules.len()
    }

    /// Count of external references.
    #[must_use]
    pub fn external_count(&self) -> usize {
        self.external_refs.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{
        eval::EvaluatedComponent,
        ir::{Component, ComponentId, ComponentKind, Span},
    };

    fn evaluated_component(path: &str) -> EvaluatedComponent {
        EvaluatedComponent {
            raw: Arc::new(
                Component::builder()
                    .id(ComponentId::from_index(0))
                    .path(Arc::<Path>::from(PathBuf::from(path)))
                    .kind(ComponentKind::Module)
                    .build(),
            ),
            variables: Vec::new(),
            locals: Vec::new(),
            providers: Vec::new(),
            resources: Vec::new(),
            modules: Vec::new(),
            outputs: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn test_should_insert_and_lookup_local_module() {
        let mut reg = ModuleRegistry::new();
        let canonical: Arc<Path> = Arc::from(PathBuf::from("/repo/modules/s3"));
        reg.insert_local(Arc::clone(&canonical), evaluated_component("modules/s3"));
        assert_eq!(reg.local_count(), 1);
        assert!(reg.get_local(&canonical).is_some());
    }

    #[test]
    fn test_should_dedup_external_refs_by_source_raw() {
        let mut reg = ModuleRegistry::new();
        let span = Span::synthetic();
        reg.record_external(
            Arc::<str>::from("terraform-aws-modules/eks/aws"),
            ModuleSource::Registry(Arc::<str>::from("terraform-aws-modules/eks/aws")),
            span.clone(),
        );
        reg.record_external(
            Arc::<str>::from("terraform-aws-modules/eks/aws"),
            ModuleSource::Registry(Arc::<str>::from("terraform-aws-modules/eks/aws")),
            span,
        );
        assert_eq!(reg.external_count(), 1);
    }
}
