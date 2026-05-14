//! Best-effort HCL expression evaluator.
//!
//! Given a parsed [`Component`] and an [`EvalContext`], the evaluator reduces
//! every [`Expression`] it can statically — variables bound from
//! `*.tfvars`, locals via a worklist fixpoint, stdlib + Terraform-only
//! functions, and sandboxed file functions — leaving references that depend
//! on apply-time data ([`Expression::Unresolved`], `data.*`, resource
//! attributes, module outputs) intact.
//!
//! The evaluator is **best-effort by contract**, per
//! [99-key-decisions.md] D4: an unresolved leaf is not a parse error — it is
//! the correct outcome when the source-only parser does not have apply-time
//! information. The Phase 4 contract is pinned in [13-evaluator.md].
//!
//! # Architecture
//!
//! `eval` is a pure walk over our [`Expression`] tree (`reduce.rs`). The
//! spec ([13-evaluator.md § 4]) describes "feeding our context into the
//! `hcl-rs::eval` evaluator and reading the result back into our IR"; in
//! practice that pattern is partially unreachable because `hcl::eval::FuncDef`
//! accepts a [`fn`-pointer], not a closure, so stateful functions
//! (`file()`, `get_env()`, the Terragrunt helpers) cannot carry sandbox /
//! workspace-root context through it. See [93-improvements-review.md]
//! S-010 / S-011 for the recorded spec defects.
//!
//! The walker keeps the contract the spec actually cares about: every public
//! IR shape that flows through is *ours*. The [`value_to_hcl`] / [`hcl_to_value`]
//! adapters convert at the boundary so future delegations (Phase 6 Terragrunt
//! funcs) can lean on `hcl::Value` without changing this module.
//!
//! [13-evaluator.md]: ../../../specs/13-evaluator.md
//! [13-evaluator.md § 4]: ../../../specs/13-evaluator.md
//! [99-key-decisions.md]: ../../../specs/99-key-decisions.md
//! [93-improvements-review.md]: ../../../specs/93-improvements-review.md
//! [`Component`]: crate::ir::Component
//! [`Expression`]: crate::ir::Expression
//! [`Expression::Unresolved`]: crate::ir::Expression::Unresolved
//! [`fn`-pointer]: https://doc.rust-lang.org/std/primitive.fn.html

mod adapter;
mod component;
mod context;
mod error;
mod files;
mod locals;
mod reduce;
mod registry;
mod stdlib;
mod tf_funcs;

pub use adapter::{hcl_to_value, value_to_hcl};
pub use component::{EvaluatedComponent, Evaluator, HclEvaluator};
pub use context::{EnvVarMode, EvalContext, EvalLimits};
pub use error::EvalError;
pub use locals::CycleParticipant;
pub use registry::{CallCx, FuncError, FuncRegistry, FuncRegistryBuilder, HclFunc};
