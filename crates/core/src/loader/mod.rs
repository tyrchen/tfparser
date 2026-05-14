//! HCL loader: read source files for a discovered component, parse them
//! with `hcl-edit`, lower to our IR, and produce a [`RawComponent`].
//!
//! This module is the second cross-trust-boundary phase. Per
//! [12-hcl-loader.md], the loader:
//!
//! - takes file size / block-count / depth caps as input;
//! - never panics on adversarial bytes;
//! - emits per-file diagnostics, never aborts the workspace on a single bad file;
//! - drops the `hcl_edit` parse tree once lowering is complete (I-LOAD-2).
//!
//! [12-hcl-loader.md]: ../../../specs/12-hcl-loader.md

mod limits;
mod lowering;
mod raw;
mod source_map;
mod traits;

pub use limits::{LoaderLimits, LoaderLimitsBuilder};
pub use raw::{RawBlock, RawComponent};
pub use source_map::{LineCol, LineIndex, SourceMap};
pub use traits::{HclEditLoader, LoadContext, Loader, ParseBytesResult};
