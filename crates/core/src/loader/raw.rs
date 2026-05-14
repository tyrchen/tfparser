//! Loader output: a [`RawComponent`] containing every block lowered to our
//! own IR.
//!
//! Per [12-hcl-loader.md § 2], the loader's contract is "no `hcl_edit` types
//! escape this struct" (invariant I-LOAD-2). Downstream phases consume only
//! [`RawBlock`] / [`crate::ir::Expression`] / [`crate::ir::Value`].
//!
//! [12-hcl-loader.md § 2]: ../../../specs/12-hcl-loader.md

use std::{path::Path, sync::Arc};

use crate::{
    Diagnostic,
    ir::{AttributeMap, BlockKind, ComponentKind, Span},
};

/// A lowered HCL block.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct RawBlock {
    /// What kind of block (resource, data, variable, …). [`BlockKind::Unknown`]
    /// for anything we did not recognise (e.g. user-defined `dynamic`
    /// blocks).
    pub kind: BlockKind,

    /// Block labels, in source order (`["aws_db_instance", "this"]` for a
    /// resource, `["root"]` for a Terragrunt `include "root"`, etc.).
    pub labels: Vec<Arc<str>>,

    /// Top-level attributes of the block body. Nested blocks land as
    /// [`crate::ir::Value::Map`] entries inside an
    /// [`crate::ir::Expression::Literal`] under a synthetic key (block
    /// identifier).
    pub body: AttributeMap,

    /// Span of the block keyword.
    pub span: Span,

    /// Source file that contained the block. Held as `Arc<Path>` so the
    /// downstream IR does not double-allocate.
    pub source: Arc<Path>,
}

/// One discovered + parsed component, post-loader.
///
/// Carries the **flat** list of every block in every file (preserving file
/// order and within-file order), plus any non-fatal diagnostics surfaced
/// during parse / lowering.
///
/// The downstream phases derive `Component`, `Resource[]`, `ProviderBlock[]`,
/// etc. by walking `raw_blocks` — that projection is Phase 3 work.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct RawComponent {
    /// Path of the component dir, relative to the workspace root.
    pub path: Arc<Path>,

    /// Whether the discoverer classified this as a component or a module.
    pub kind: ComponentKind,

    /// All lowered blocks, file-order then within-file-order.
    pub raw_blocks: Vec<RawBlock>,

    /// Per-file parse / limit diagnostics. Surfaced upward into
    /// `Workspace.diagnostics` by the orchestrator.
    pub diagnostics: Vec<Diagnostic>,
}

impl RawComponent {
    /// Construct an empty [`RawComponent`] for `path`. The loader uses this
    /// as a starting point and pushes blocks / diagnostics during the walk.
    #[must_use]
    pub fn new(path: Arc<Path>, kind: ComponentKind) -> Self {
        Self {
            path,
            kind,
            raw_blocks: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}
