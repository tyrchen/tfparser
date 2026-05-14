//! # tfparser-core
//!
//! Parse a Terraform / Terragrunt source repository into a typed in-memory IR
//! that can be exported as Parquet — without running `terraform plan`.
//!
//! ## Quick start
//!
//! One-shot parse with all defaults:
//!
//! ```no_run
//! # fn main() -> tfparser_core::Result<()> {
//! let workspace = tfparser_core::parse("./my-tf-repo")?;
//! println!(
//!     "{} components / {} modules / {} resources",
//!     workspace.components.len(),
//!     workspace.modules.len(),
//!     workspace.components.iter().map(|c| c.resources.len()).sum::<usize>(),
//! );
//! # Ok(()) }
//! ```
//!
//! Builder for full control + Parquet export in one call:
//!
//! ```no_run
//! # fn main() -> tfparser_core::Result<()> {
//! use std::sync::Arc;
//! use std::path::Path;
//! use tfparser_core::{Parser, EnvVarMode, ExportOptions};
//!
//! let parser = Parser::builder()
//!     .workspace_root("./my-tf-repo")
//!     .environment("production")
//!     .default_region("us-west-2")?
//!     .env_var_mode(EnvVarMode::Passthrough)
//!     .allow_env("TF_VAR_environment")
//!     .var("region", "us-east-1")
//!     .strict_providers(true)
//!     .build()?;
//!
//! let export_opts = ExportOptions::builder()
//!     .out_dir(Arc::<Path>::from(Path::new("./out")))
//!     .overwrite(true)
//!     .build();
//! let (workspace, report) = parser.parse_and_export(&export_opts)?;
//! eprintln!("wrote {} rows in {} ms", report.total_rows, report.elapsed.as_millis());
//! # let _ = workspace;
//! # Ok(()) }
//! ```
//!
//! Bring everything in scope with the prelude:
//!
//! ```
//! use tfparser_core::prelude::*;
//! ```
//!
//! ## Surface map
//!
//! | I want to … | Reach for |
//! | ----------- | --------- |
//! | parse a repo with defaults | [`parse`] |
//! | parse + tune env / vars / limits | [`Parser::builder`] |
//! | parse and write Parquet in one call | [`Parser::parse_and_export`] |
//! | inspect the parsed IR | [`ir::Workspace`], [`ir::Component`], [`ir::Resource`] |
//! | swap in a stub for tests | implement [`Pipeline`] / [`Exporter`] |
//! | configure parquet output (compression, manifest, tables) | [`ExportOptions::builder`] + [`ParquetExporter`] |
//! | load an AWS profile map | [`load_aws_config`] / [`load_yaml_profile_map`] |
//!
//! ## Engineering invariants
//!
//! - `#![forbid(unsafe_code)]` at the crate root — no `unsafe`, ever.
//! - No `unwrap` / `expect` / `panic` reachable from external input; the workspace lints deny those
//!   clippy categories for every member.
//! - Every public type is `#[non_exhaustive]` so future fields are additive.
//! - Public `Debug` impls redact sensitive fields (provider secrets, resolved values that may carry
//!   credentials).
//!
//! See `./specs/91-impl-plan.md` for the build-order rationale and
//! `./specs/10-data-model.md` for the IR contract pinned in this crate.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod diagnostic;
pub mod discovery;
pub mod error;
pub mod eval;
pub mod exporter;
pub mod graph;
pub mod ir;
pub mod loader;
pub mod parser;
pub mod pipeline;
pub mod prelude;
pub mod projection;
pub mod provider;
pub mod terragrunt;
pub(crate) mod util;

pub use diagnostic::{Diagnostic, LimitKind, Severity};
pub use error::{Error, Result, ValidationError};
pub use eval::{
    EnvVarMode, EvalContext, EvalError, EvalLimits, EvaluatedComponent, Evaluator, FuncRegistry,
    HclEvaluator,
};
pub use exporter::{
    CompressionOpt, ExportOptions, ExportReport, ExportedFile, Exporter, ParquetExporter,
    SecondaryTable,
};
pub use graph::{
    DefaultGraphBuilder, ExternalModuleRef, GraphBuilder, GraphContext, GraphError, ModuleRegistry,
};
pub use ir::{
    AccountId, Address, AssumeRole, AttributeMap, BinaryOp, BlockKind, Component, ComponentId,
    ComponentKind, DependencyBlock, Edge, EdgeKind, Environment, Expression, FileExt,
    GenerateBlock, IncludePath, Local, Map, Module, ModuleCall, ModuleId, ModuleSource, Output,
    ProviderBlock, ProviderRef, Region, Resource, ResourceKind, SourceFile, Span, StateBackend,
    SymbolKind, Symbolic, TerragruntConfig, UnaryOp, Value, Variable, Workspace,
};
pub use parser::{Parser, ParserBuilder, parse};
pub use pipeline::{DefaultPipeline, Pipeline, PipelineOptions};
pub use provider::{
    DefaultProviderResolver, ProfileEntry, ProfileMap, ProviderContext, ProviderError,
    ProviderResolver, SharedProfileMap, empty_profile_map, extract_account_id, load_aws_config,
    load_yaml_profile_map,
};
pub use terragrunt::{FsTerragruntResolver, TerragruntError, TerragruntResolver, TgContext};

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
        assert_send_sync::<Parser>();
        assert_send_sync::<ParserBuilder>();
        // Trait objects of `Pipeline` are the cross-thread shape downstream
        // crates (server, future CLI) will hold.
        assert_send_sync::<Box<dyn Pipeline>>();
    }
}
