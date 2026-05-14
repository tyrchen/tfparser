//! Resource graph: flatten module bodies into their callers, expand
//! `count` / `for_each`, and emit a [`Workspace`](crate::ir::Workspace).
//!
//! Phase 5 lands module expansion (closes M2). Dependency-edge collection
//! and the secondary Parquet tables (`dependencies.parquet`,
//! `components.parquet`, `modules.parquet`) are Phase 8 / M5 work — they
//! consume the same `Workspace`, so the contract pinned here does not need
//! to change when they arrive.
//!
//! Per [15-resource-graph.md].
//!
//! ## Public surface
//!
//! - [`GraphBuilder`] / [`DefaultGraphBuilder`]: the trait + default impl that flattens module
//!   bodies into their callers.
//! - [`ModuleRegistry`]: canonical-path-keyed index of local module bodies; the orchestrator (Phase
//!   5 pipeline wiring) builds this by evaluating every `DirKind::Module` directory the discoverer
//!   emitted.
//! - [`GraphContext`]: per-build context (workspace root, recursion / expansion caps).
//! - [`GraphError`]: errors emitted by the graph phase. Spec § 7 `UnresolvableModuleSource` /
//!   `DepthExceeded` are recorded as [`crate::Diagnostic`]s in practice (the builder method
//!   signature reserves `Err` for fatal IR-construction failures).
//!
//! [15-resource-graph.md]: ../../../specs/15-resource-graph.md

mod builder;
mod edges;
mod error;
mod expand;
mod registry;

pub use builder::{DefaultGraphBuilder, GraphBuilder, GraphContext};
pub use edges::collect_edges_in_place;
pub use error::GraphError;
pub use registry::{ExternalModuleRef, ModuleRegistry};

// `expand::expand_resource` and helpers are crate-private — only `builder`
// drives them. Tests live next to their implementation.
