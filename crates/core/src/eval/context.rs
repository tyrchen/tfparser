//! Evaluator context: workspace root, variable + cascade bindings, function
//! registry, env-var mode, and resource limits.
//!
//! Per [13-evaluator.md § 2], the [`EvalContext`] is the **read-only** input
//! to every call into [`crate::eval::Evaluator::evaluate`]. The same context
//! object is shared across components inside a `rayon::par_iter` per
//! [99-key-decisions.md] D14 — `EvalContext: Send + Sync` (asserted in
//! [`crate::eval::component`]).
//!
//! [13-evaluator.md § 2]: ../../../specs/13-evaluator.md
//! [99-key-decisions.md]: ../../../specs/99-key-decisions.md

use std::{collections::BTreeSet, path::Path, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{
    error::ValidationError,
    eval::registry::FuncRegistry,
    ir::{Map, Value},
};

/// How [`get_env`](super::registry::HclFunc) calls treat the process
/// environment.
///
/// Default is [`EnvVarMode::Strict`] with an empty allowlist — `get_env` is
/// off until the operator opts in. See [70-security.md § 3.3].
///
/// [70-security.md § 3.3]: ../../../specs/70-security.md
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "mode")]
#[non_exhaustive]
pub enum EnvVarMode {
    /// Pass the full process environment to the evaluator. The CLI prints a
    /// startup warning before this mode goes live.
    Passthrough,
    /// Only the names in `allowed` are visible. Any other `get_env(...)`
    /// returns the supplied default or `""`.
    Strict {
        /// Allowed env var names. The set is a `BTreeSet` for deterministic
        /// ordering (and dedup) per [13-evaluator.md § 2].
        ///
        /// [13-evaluator.md § 2]: ../../../specs/13-evaluator.md
        allowed: BTreeSet<Arc<str>>,
    },
    /// `get_env` always returns the supplied default (or `""`). Useful for
    /// hermetic tests.
    Mock,
}

impl Default for EnvVarMode {
    fn default() -> Self {
        Self::Strict {
            allowed: BTreeSet::new(),
        }
    }
}

impl EnvVarMode {
    /// Whether the given env-var name may be read.
    #[must_use]
    pub fn allows(&self, name: &str) -> bool {
        match self {
            Self::Passthrough => true,
            Self::Strict { allowed } => allowed.iter().any(|s| s.as_ref() == name),
            Self::Mock => false,
        }
    }

    /// Whether to consult the process environment or always return the
    /// caller's default. `Mock` short-circuits even when the name is empty.
    #[must_use]
    pub const fn is_mock(&self) -> bool {
        matches!(self, Self::Mock)
    }
}

/// Per-call resource limits enforced by the evaluator and its registered
/// functions.
///
/// Per [70-security.md § 3.2] every cap is configurable; the defaults match
/// the spec. Breaching any returns `Err(EvalError::Limit { kind, ... })` —
/// never a panic.
///
/// [70-security.md § 3.2]: ../../../specs/70-security.md
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct EvalLimits {
    /// Maximum positional+variadic arg count per function call. Default: 64.
    pub max_func_args: u32,
    /// Maximum rendered string size returned by a function (bytes).
    /// Default: 1 MiB.
    pub max_str_size: u32,
    /// Maximum list/array size returned by a function. Default: 100 000.
    pub max_list_len: u32,
    /// Maximum reduction iterations across a single `evaluate` call.
    /// Default: 1 000 000. Anti-DoS bound for nested `for` / recursive
    /// templates.
    pub max_iterations: u32,
    /// Maximum read size for sandboxed file functions (bytes). Default:
    /// 4 MiB (matches the loader's per-file cap).
    pub max_file_bytes: u32,
}

impl Default for EvalLimits {
    fn default() -> Self {
        Self {
            max_func_args: 64,
            max_str_size: 1 << 20,
            max_list_len: 100_000,
            max_iterations: 1_000_000,
            max_file_bytes: 4 << 20,
        }
    }
}

/// Read-only inputs to a single [`crate::eval::Evaluator::evaluate`] call.
///
/// `repo_vars` and `cascade_locals` are insertion-ordered [`Map`]s (see
/// [10-data-model.md § 2.3]) — order preservation matters for the canonical
/// JSON in error diagnostics. `funcs` is an `Arc<FuncRegistry>` so the same
/// table is shared across all components in a workspace without cloning.
///
/// [10-data-model.md § 2.3]: ../../../specs/10-data-model.md
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct EvalContext {
    /// Absolute, canonicalised workspace root. All sandboxed file functions
    /// validate paths against this root.
    pub workspace_root: Arc<Path>,

    /// Optional environment name (e.g. `Some("staging")`). Bound as the
    /// `terraform.workspace` placeholder for the evaluator pass; downstream
    /// resource rows surface it in the `environment` column.
    pub environment: Option<Arc<str>>,

    /// How `get_env(...)` reads the process environment.
    pub env_vars: EnvVarMode,

    /// `var.*` bindings from `.tfvars` and CLI `--var k=v` flags.
    pub repo_vars: Map,

    /// `local.*` shadows injected from a Terragrunt cascade (Phase 6 will
    /// populate this; Phase 4 accepts an empty map and a synthetic one for
    /// tests).
    pub cascade_locals: Map,

    /// Registered functions (stdlib + Terraform-only + sandboxed file +
    /// Terragrunt helpers).
    pub funcs: Arc<FuncRegistry>,

    /// Per-call resource limits.
    pub limits: EvalLimits,
}

impl EvalContext {
    /// Construct a fully-specified [`EvalContext`]. Public because
    /// downstream crates (the CLI, the future server) build contexts
    /// from their own config sources; the struct is `#[non_exhaustive]`
    /// so the constructor is the stable surface even when fields evolve.
    #[must_use]
    pub fn new(
        workspace_root: Arc<Path>,
        environment: Option<Arc<str>>,
        env_vars: EnvVarMode,
        repo_vars: Map,
        cascade_locals: Map,
        funcs: Arc<FuncRegistry>,
        limits: EvalLimits,
    ) -> Self {
        Self {
            workspace_root,
            environment,
            env_vars,
            repo_vars,
            cascade_locals,
            funcs,
            limits,
        }
    }

    /// Construct a minimal context pinned to `workspace_root` with default
    /// limits, empty `repo_vars` / `cascade_locals`, an empty registry,
    /// and strict env-var mode (empty allowlist). Convenience for tests
    /// and the default CLI invocation.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::Empty`] when `workspace_root` is an empty
    /// path — the evaluator requires a non-empty root for the sandboxed
    /// path helpers to reject escapes meaningfully.
    pub fn minimal(workspace_root: Arc<Path>) -> Result<Self, ValidationError> {
        if workspace_root.as_os_str().is_empty() {
            return Err(ValidationError::Empty {
                field: "EvalContext.workspace_root",
            });
        }
        Ok(Self {
            workspace_root,
            environment: None,
            env_vars: EnvVarMode::default(),
            repo_vars: Map::new(),
            cascade_locals: Map::new(),
            funcs: Arc::new(FuncRegistry::default()),
            limits: EvalLimits::default(),
        })
    }

    /// Read a binding for `name` from `repo_vars`.
    #[must_use]
    pub fn lookup_repo_var(&self, name: &str) -> Option<&Value> {
        self.repo_vars
            .iter()
            .find_map(|(k, v)| if k.as_ref() == name { Some(v) } else { None })
    }

    /// Read a binding for `name` from `cascade_locals`.
    #[must_use]
    pub fn lookup_cascade_local(&self, name: &str) -> Option<&Value> {
        self.cascade_locals.iter().find_map(
            |(k, v)| {
                if k.as_ref() == name { Some(v) } else { None }
            },
        )
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
    use super::*;

    #[test]
    fn test_default_env_var_mode_is_strict_empty() {
        let m = EnvVarMode::default();
        assert!(!m.allows("HOME"));
        assert!(!m.is_mock());
    }

    #[test]
    fn test_strict_mode_allows_listed_names_only() {
        let mut allowed = BTreeSet::new();
        allowed.insert(Arc::<str>::from("TF_VAR_environment"));
        let m = EnvVarMode::Strict { allowed };
        assert!(m.allows("TF_VAR_environment"));
        assert!(!m.allows("HOME"));
    }

    #[test]
    fn test_passthrough_allows_everything() {
        let m = EnvVarMode::Passthrough;
        assert!(m.allows("ANY"));
    }

    #[test]
    fn test_mock_rejects_all_and_is_mock() {
        let m = EnvVarMode::Mock;
        assert!(!m.allows("HOME"));
        assert!(m.is_mock());
    }

    #[test]
    fn test_eval_limits_defaults_match_spec() {
        let l = EvalLimits::default();
        assert_eq!(l.max_func_args, 64);
        assert_eq!(l.max_str_size, 1 << 20);
        assert_eq!(l.max_list_len, 100_000);
        assert_eq!(l.max_iterations, 1_000_000);
    }

    #[test]
    fn test_minimal_context_rejects_empty_root() {
        let err = EvalContext::minimal(Arc::from(Path::new(""))).unwrap_err();
        assert!(matches!(err, ValidationError::Empty { .. }));
    }

    #[test]
    fn test_minimal_context_round_trips_vars() {
        let mut ctx = EvalContext::minimal(Arc::from(Path::new("/tmp/repo"))).unwrap();
        ctx.repo_vars
            .push((Arc::from("region"), Value::Str(Arc::from("us-east-2"))));
        assert_eq!(
            ctx.lookup_repo_var("region"),
            Some(&Value::Str(Arc::from("us-east-2")))
        );
        assert_eq!(ctx.lookup_repo_var("missing"), None);
    }

    #[test]
    fn test_env_mode_serde_round_trip() {
        let m = EnvVarMode::Strict {
            allowed: [Arc::<str>::from("AWS_REGION")].into_iter().collect(),
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: EnvVarMode = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}
