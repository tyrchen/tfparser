//! Function registry: the set of HCL/Terraform/Terragrunt functions the
//! evaluator can dispatch to.
//!
//! Each function is a small trait object stored under its name in an
//! `Arc<FuncRegistry>`. Trait objects (not `fn` pointers) are used because
//! stateful functions (`file()`, `get_env()`, the Terragrunt helpers in
//! Phase 6) carry per-call context that a bare `fn` pointer cannot capture
//! — see [93-improvements-review.md] S-011.
//!
//! [93-improvements-review.md]: ../../../specs/93-improvements-review.md

use std::{collections::HashMap, fmt, path::Path, sync::Arc};

use thiserror::Error;

use crate::{
    diagnostic::LimitKind,
    eval::context::{EnvVarMode, EvalLimits},
    ir::Value,
};

/// Per-call context handed to each [`HclFunc::call`].
///
/// `CallCx` is intentionally **read-only** so functions cannot mutate the
/// evaluator's state. Path-safety helpers (`canonicalize_inside`) take
/// `&Path`, so `workspace_root` is borrowed; the `EnvVarMode` and
/// `EvalLimits` are likewise references.
#[derive(Debug)]
#[non_exhaustive]
pub struct CallCx<'a> {
    /// Absolute, canonicalised workspace root for path-sandboxing.
    pub workspace_root: &'a Path,
    /// How `get_env(...)` should treat the process environment.
    pub env_vars: &'a EnvVarMode,
    /// Resource limits enforced inside function bodies (string size, list
    /// length, file read size).
    pub limits: &'a EvalLimits,
}

impl<'a> CallCx<'a> {
    /// Construct a new `CallCx` from explicit pieces.
    #[must_use]
    pub const fn new(
        workspace_root: &'a Path,
        env_vars: &'a EnvVarMode,
        limits: &'a EvalLimits,
    ) -> Self {
        Self {
            workspace_root,
            env_vars,
            limits,
        }
    }
}

/// Error returned by an [`HclFunc::call`].
///
/// `FuncError` is the call-site shape; the evaluator converts each variant
/// to an [`crate::eval::EvalError`] or a [`crate::Diagnostic`] before it
/// surfaces in `Workspace.diagnostics`. The variants intentionally mirror
/// the evaluator-level types so downstream tooling sees the same `LimitKind`
/// regardless of whether the breach happened in the walker or inside a
/// function body.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum FuncError {
    /// Wrong number of arguments. `expected` is `usize::MAX` for variadic
    /// minimum-match failures; messages embed the function-specific shape.
    #[error("`{name}`: expected {expected} arg(s), got {got}")]
    Arity {
        /// Function name.
        name: Arc<str>,
        /// Expected arg count (or `usize::MAX` for "variadic ≥ N").
        expected: usize,
        /// Observed arg count.
        got: usize,
    },

    /// Argument failed its type check. `index` is 0-based.
    #[error("`{name}` arg #{index}: expected {expected}, got {got}")]
    Type {
        /// Function name.
        name: Arc<str>,
        /// Argument index (0-based).
        index: usize,
        /// Expected type name (e.g. `"string"`, `"number"`).
        expected: &'static str,
        /// Observed type name.
        got: &'static str,
    },

    /// A function-level limit fired (e.g. result string > `max_str_size`).
    #[error("`{name}` limit ({kind:?}): observed {observed} > {limit}")]
    Limit {
        /// Function name.
        name: Arc<str>,
        /// Which limit category fired.
        kind: LimitKind,
        /// Observed value.
        observed: u64,
        /// Configured limit.
        limit: u64,
    },

    /// File function rejected the path because it resolves outside the
    /// workspace root.
    #[error("`{name}` path escape: `{path}`")]
    PathEscape {
        /// Function name.
        name: &'static str,
        /// Path supplied by the caller.
        path: std::path::PathBuf,
    },

    /// Anything else. The message is rendered verbatim in diagnostics; it
    /// should not embed user-controlled data without escaping.
    #[error("`{name}`: {message}")]
    Other {
        /// Function name.
        name: Arc<str>,
        /// Free-form message; safe to log.
        message: Arc<str>,
    },
}

/// A single function dispatchable by the evaluator.
///
/// Implementations must be `Send + Sync` (the registry is shared across
/// `rayon` worker threads per [99-key-decisions.md] D14) and `Debug` (per
/// CLAUDE.md § Type Design).
///
/// [99-key-decisions.md]: ../../../specs/99-key-decisions.md
pub trait HclFunc: fmt::Debug + Send + Sync + 'static {
    /// Call the function with already-resolved arguments.
    ///
    /// `args` is borrowed from the caller's reduced expression tree.
    /// Returning `Ok(Value)` means the call site collapses to
    /// [`crate::ir::Expression::Literal`]; returning `Err(_)` keeps the
    /// call site as an unresolved
    /// [`crate::ir::Expression::FuncCall`] and the workspace records a
    /// diagnostic.
    ///
    /// # Errors
    ///
    /// See [`FuncError`] variants.
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError>;
}

/// Read-only function registry shared by the evaluator across components.
///
/// Construct via [`FuncRegistryBuilder`]. The registry is intentionally
/// not extendable post-construction so the function table is a stable
/// `&'static`-shaped object — every later operation against it is a
/// concurrent read.
#[derive(Default)]
pub struct FuncRegistry {
    funcs: HashMap<Arc<str>, Arc<dyn HclFunc>>,
}

impl FuncRegistry {
    /// Look up a function by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn HclFunc>> {
        self.funcs.get(name)
    }

    /// Whether a function with this name is registered.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.funcs.contains_key(name)
    }

    /// Iterate over `(name, func)` pairs in arbitrary order. Used by
    /// diagnostics ("here's the function table the evaluator saw").
    pub fn iter(&self) -> impl Iterator<Item = (&Arc<str>, &Arc<dyn HclFunc>)> {
        self.funcs.iter()
    }

    /// Number of registered functions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.funcs.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.funcs.is_empty()
    }

    /// Start a builder from this registry's contents. Used when the spec
    /// wants "stdlib + overrides".
    #[must_use]
    pub fn to_builder(&self) -> FuncRegistryBuilder {
        FuncRegistryBuilder {
            funcs: self.funcs.clone(),
        }
    }

    /// Build a registry pre-loaded with the Phase 4 default function set:
    /// HCL stdlib, Terraform-only functions, and the sandboxed file
    /// functions (`file` / `fileexists` / `templatefile` / `fileset`). The
    /// sandbox helpers operate on the workspace root supplied via
    /// [`CallCx::workspace_root`] at call time — no closure state.
    #[must_use]
    pub fn default_with_stdlib() -> Self {
        let mut b = Self::builder();
        super::stdlib::register(&mut b);
        super::tf_funcs::register(&mut b);
        super::files::register(&mut b);
        b.build()
    }

    /// Start an empty registry builder.
    #[must_use]
    pub fn builder() -> FuncRegistryBuilder {
        FuncRegistryBuilder::default()
    }
}

impl fmt::Debug for FuncRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<&str> = self.funcs.keys().map(Arc::as_ref).collect();
        names.sort_unstable();
        f.debug_struct("FuncRegistry")
            .field("count", &self.funcs.len())
            .field("names", &names)
            .finish()
    }
}

/// Mutable builder for a [`FuncRegistry`].
#[derive(Default)]
pub struct FuncRegistryBuilder {
    funcs: HashMap<Arc<str>, Arc<dyn HclFunc>>,
}

impl FuncRegistryBuilder {
    /// Register a function by name. Re-registering replaces the existing
    /// entry — used by tests and by the Phase 6 Terragrunt overlay.
    pub fn register(&mut self, name: impl Into<Arc<str>>, func: Arc<dyn HclFunc>) -> &mut Self {
        self.funcs.insert(name.into(), func);
        self
    }

    /// Drop an entry by name. No-op if absent.
    pub fn unregister(&mut self, name: &str) -> &mut Self {
        self.funcs.remove(name);
        self
    }

    /// Whether a name is already registered.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.funcs.contains_key(name)
    }

    /// Freeze into a [`FuncRegistry`].
    #[must_use]
    pub fn build(self) -> FuncRegistry {
        FuncRegistry { funcs: self.funcs }
    }
}

impl fmt::Debug for FuncRegistryBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FuncRegistryBuilder")
            .field("count", &self.funcs.len())
            .finish()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use std::path::Path;

    use super::*;

    #[derive(Debug)]
    struct Echo;
    impl HclFunc for Echo {
        fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
            args.first().cloned().ok_or_else(|| FuncError::Arity {
                name: Arc::from("echo"),
                expected: 1,
                got: 0,
            })
        }
    }

    fn fake_cx() -> (EvalLimits, EnvVarMode) {
        (EvalLimits::default(), EnvVarMode::default())
    }

    #[test]
    fn test_registry_builder_registers_and_unregisters() {
        let mut b = FuncRegistry::builder();
        b.register("echo", Arc::new(Echo));
        assert!(b.contains("echo"));
        b.unregister("echo");
        assert!(!b.contains("echo"));
    }

    #[test]
    fn test_registry_dispatch_echo() {
        let mut b = FuncRegistry::builder();
        b.register("echo", Arc::new(Echo));
        let r = b.build();
        let (limits, env_vars) = fake_cx();
        let cx = CallCx {
            workspace_root: Path::new("/tmp"),
            env_vars: &env_vars,
            limits: &limits,
        };
        let v = r.get("echo").unwrap().call(&[Value::Int(7)], &cx).unwrap();
        assert_eq!(v, Value::Int(7));
    }

    #[test]
    fn test_default_with_stdlib_has_known_functions() {
        let r = FuncRegistry::default_with_stdlib();
        // Spot-check a few canonical entries from spec 13 § 5.
        assert!(r.contains("jsonencode"), "{r:?}");
        assert!(r.contains("merge"), "{r:?}");
        assert!(r.contains("sha256"), "{r:?}");
        assert!(r.contains("base64encode"), "{r:?}");
    }

    #[test]
    fn test_func_error_arity_renders_function_name() {
        let e = FuncError::Arity {
            name: Arc::from("merge"),
            expected: 2,
            got: 1,
        };
        let s = format!("{e}");
        assert!(s.contains("merge"));
        assert!(s.contains('2'));
        assert!(s.contains('1'));
    }

    #[test]
    fn test_registry_is_send_sync() {
        const fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<FuncRegistry>();
        assert_send_sync::<Arc<FuncRegistry>>();
        assert_send_sync::<Arc<dyn HclFunc>>();
    }

    #[test]
    fn test_registry_debug_lists_sorted_names() {
        let mut b = FuncRegistry::builder();
        b.register("z_alpha", Arc::new(Echo));
        b.register("a_alpha", Arc::new(Echo));
        let r = b.build();
        let s = format!("{r:?}");
        let z_pos = s.find("z_alpha").expect("z_alpha present");
        let a_pos = s.find("a_alpha").expect("a_alpha present");
        assert!(a_pos < z_pos, "expected sorted names: {s}");
    }
}
