//! # tfparser-core
//!
//! Parse a Terraform / Terragrunt source repository into a typed in-memory IR
//! that can be exported as Parquet. This crate exposes the **types and traits**
//! the pipeline is built around. Implementations land progressively:
//!
//! | Phase | Module(s) gated | Status (Phase 2) |
//! | ----- | --------------- | ---------------- |
//! | 1 | [`ir`], [`diagnostic`], [`pipeline`], [`error`] | ✅ landed |
//! | 2 | [`discovery`], [`loader`] | ✅ this phase |
//! | 3 | `exporter` | not yet |
//! | 4 | `eval` | not yet |
//! | 5 | `graph` | not yet |
//! | 6 | `terragrunt` | trait surface only |
//! | 7 | `provider` | not yet |
//!
//! See `./specs/91-impl-plan.md` for the build-order rationale and
//! `./specs/10-data-model.md` for the IR contract pinned in this crate.
//!
//! ## Engineering invariants
//!
//! - `#![forbid(unsafe_code)]` at the crate root — no `unsafe`, ever.
//! - No `unwrap`/`expect`/`panic` reachable from external input; per CLAUDE.md § Safety & Security
//!   the workspace lints deny those clippy categories for every member.
//! - Every public type is `#[non_exhaustive]` so future fields are additive.
//! - Public `Debug` impls redact sensitive fields (provider secrets, resolved values that may carry
//!   credentials).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod diagnostic;
pub mod discovery;
pub mod error;
pub mod exporter;
pub mod ir;
pub mod loader;
pub mod pipeline;
pub mod projection;
pub(crate) mod util;

pub use diagnostic::{Diagnostic, LimitKind, Severity};
pub use error::{Error, Result, ValidationError};
pub use ir::{
    AccountId, Address, AssumeRole, AttributeMap, BinaryOp, BlockKind, Component, ComponentId,
    ComponentKind, DependencyBlock, Environment, Expression, FileExt, GenerateBlock, IncludePath,
    Local, Map, Module, ModuleCall, ModuleId, ModuleSource, Output, ProviderBlock, ProviderRef,
    Region, Resource, ResourceKind, SourceFile, Span, StateBackend, SymbolKind, Symbolic,
    TerragruntConfig, UnaryOp, Value, Variable, Workspace,
};
pub use pipeline::{Pipeline, PipelineOptions};

#[cfg(test)]
mod thread_safety {
    //! Static `Send + Sync` assertions for the public surface that crosses
    //! thread boundaries via `rayon` (per [99-key-decisions.md] D14).
    //!
    //! [99-key-decisions.md]: ../../specs/99-key-decisions.md

    use super::*;

    const fn assert_send_sync<T: Send + Sync + 'static>() {}

    #[test]
    fn test_public_types_are_send_sync() {
        assert_send_sync::<Workspace>();
        assert_send_sync::<Component>();
        assert_send_sync::<Module>();
        assert_send_sync::<Resource>();
        assert_send_sync::<Diagnostic>();
        assert_send_sync::<Error>();
        assert_send_sync::<ValidationError>();
        assert_send_sync::<PipelineOptions>();
        // Trait objects of `Pipeline` are the cross-thread shape downstream
        // crates (server, future CLI) will hold.
        assert_send_sync::<Box<dyn Pipeline>>();
    }
}
