//! Terragrunt-mimicking resolver.
//!
//! Phase 6 lands the subset of Terragrunt that affects **what HCL the
//! component effectively sees**: `include`, `find_in_parent_folders`,
//! `read_terragrunt_config`, locals/inputs cascade, `generate`, `dependency`.
//! Per [99-key-decisions.md] D6, we mimic Terragrunt, never invoke its
//! binary or shell out to `git`.
//!
//! Per [14-terragrunt.md].
//!
//! ## Public surface
//!
//! - [`TerragruntResolver`] / [`FsTerragruntResolver`]: trait + default impl.
//! - [`TgContext`]: per-resolution context (workspace root, env-var policy, depth cap).
//! - [`TerragruntError`]: thiserror enum mirroring spec § 6.
//!
//! [14-terragrunt.md]: ../../../specs/14-terragrunt.md
//! [99-key-decisions.md]: ../../../specs/99-key-decisions.md

mod context;
mod error;
mod funcs;
mod merge;
mod parsed;
mod resolver;

pub use context::TgContext;
pub use error::TerragruntError;
pub use resolver::{FsTerragruntResolver, TerragruntResolver};
