//! Common types bundled for `use tfparser_core::prelude::*;`.
//!
//! Lives next to but separate from the crate root re-exports so consumers
//! can opt in to the broader names without having to spell each import.
//! The included items are deliberately narrow: the façade ([`Parser`],
//! [`parse`]), the workspace IR core ([`Workspace`], [`Component`],
//! [`Resource`], [`Module`]), the error contract ([`Result`], [`Error`]),
//! and the diagnostic / exporter shapes that anyone consuming the parquet
//! tables will eventually need.
//!
//! If you want the lower-level building blocks (pipeline trait, evaluator,
//! terragrunt resolver, …) reach into the module that owns them.
//!
//! ```no_run
//! use tfparser_core::prelude::*;
//!
//! # fn main() -> Result<()> {
//! let workspace: Workspace = parse("./my-tf-repo")?;
//! println!("{} components", workspace.components.len());
//! # Ok(())
//! # }
//! ```

pub use crate::{
    Component, Diagnostic, Error, ExportOptions, ExportReport, Exporter, Module, ParquetExporter,
    Parser, ParserBuilder, Resource, Result, Severity, Workspace, parse,
};
