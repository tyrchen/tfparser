//! In-memory intermediate representation.
//!
//! This module is the contract every other phase consumes. Per
//! [10-data-model.md], the shapes are **stable for v0.1** — additive
//! changes only, no renames or retypes.
//!
//! [10-data-model.md]: ../../specs/10-data-model.md

mod component;
mod edge;
mod environment;
mod expression;
mod files;
mod ids;
mod module;
mod newtypes;
pub(crate) mod path_serde;
mod provider;
mod resource;
mod span;
mod terragrunt;
mod value;
mod workspace;

pub use component::{Component, ComponentBuilder, ComponentKind, Local, Output, Variable};
pub use edge::{Edge, EdgeBuilder, EdgeKind};
pub use environment::{Environment, EnvironmentBuilder};
pub use expression::{
    AttributeMap, BinaryOp, Conditional, Expression, ForExpr, FuncCall, SymbolKind, Symbolic,
    UnaryOp,
};
pub use files::{FileExt, SourceFile};
pub use ids::{ComponentId, ModuleId};
pub use module::{Module, ModuleBuilder, ModuleCall, ModuleCallBuilder, ModuleSource};
pub use newtypes::{
    ADDRESS_MAX_BYTES, AccountId, Address, ModuleSegments, REGION_MAX_BYTES, Region,
};
pub use provider::{AssumeRole, ProviderBlock, ProviderBlockBuilder, ProviderRef};
pub use resource::{BlockKind, Resource, ResourceBuilder, ResourceKind};
pub use span::Span;
pub use terragrunt::{
    DependencyBlock, GenerateBlock, IncludePath, StateBackend, StateBackendBuilder,
    TerragruntConfig, TerragruntConfigBuilder,
};
pub use value::{Map, Value};
pub use workspace::{Workspace, WorkspaceBuilder};
