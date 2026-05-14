//! Parquet exporter — turns a [`Workspace`](crate::ir::Workspace) into
//! `resources.parquet` (+ `workspace.manifest.json`) on disk per
//! [20-parquet-exporter.md].
//!
//! Phase 3 closes M0 by shipping the **single-table** flat schema pinned in
//! [10-data-model.md § 3] (24 columns, all non-null with `""` / empty-list
//! as the "missing" sentinel). Future phases extend the output with
//! `dependencies.parquet`, `components.parquet`, `modules.parquet`, etc. —
//! same writer pattern, separate file, same atomic-rename guarantee.
//!
//! ## Trust boundary
//!
//! The exporter consumes only IR — `Workspace` values are already trusted
//! (they crossed validation at discovery / loader / evaluator land). The
//! one trust boundary the exporter polices is the *output path*: it
//! refuses to overwrite existing files unless `--overwrite` is set, and
//! writes atomically via `<file>.partial` → `rename` (per
//! [99-key-decisions.md] D10).
//!
//! [20-parquet-exporter.md]: ../../../specs/20-parquet-exporter.md
//! [10-data-model.md § 3]: ../../../specs/10-data-model.md

pub mod json;
mod manifest;
mod schema;
mod secondary;
mod writer;

use std::sync::Arc;

pub use manifest::{Manifest, ManifestFile, write_manifest};
pub use schema::{
    PARSER_VERSION, SCHEMA_MAJOR, SCHEMA_MINOR, resources_schema, schema_field_names,
};
pub use secondary::{
    components_field_names, components_schema, dependencies_field_names, dependencies_schema,
    modules_field_names, modules_schema,
};
pub use writer::{
    CompressionOpt, ExportOptions, ExportReport, ExportedFile, Exporter, ParquetExporter,
    SecondaryTable,
};

/// Errors the exporter can raise.
///
/// Each variant carries the offending path when known, so the user can
/// jump straight to it — see [20-parquet-exporter.md § 5].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExportError {
    /// The output path exists and `--overwrite` was not set.
    #[error("output exists and --overwrite not set: {0}")]
    OutputExists(Arc<std::path::Path>),

    /// Output directory does not exist.
    #[error("output directory does not exist: {0}")]
    OutDirMissing(Arc<std::path::Path>),

    /// Output path exists but is not a directory.
    #[error("output path is not a directory: {0}")]
    OutDirNotDir(Arc<std::path::Path>),

    /// I/O failure while writing.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// Path that triggered the error.
        path: Arc<std::path::Path>,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Arrow / Parquet writer raised.
    #[error("parquet writer error at {path}: {source}")]
    Parquet {
        /// Target file.
        path: Arc<std::path::Path>,
        /// Underlying parquet error.
        #[source]
        source: parquet::errors::ParquetError,
    },

    /// Arrow batch construction raised.
    #[error("arrow error at {path}: {source}")]
    Arrow {
        /// Target file.
        path: Arc<std::path::Path>,
        /// Underlying arrow error.
        #[source]
        source: arrow::error::ArrowError,
    },

    /// JSON manifest serialisation raised.
    #[error("manifest serialisation error at {path}: {source}")]
    Manifest {
        /// Manifest path.
        path: Arc<std::path::Path>,
        /// Underlying serialisation error.
        #[source]
        source: serde_json::Error,
    },
}
