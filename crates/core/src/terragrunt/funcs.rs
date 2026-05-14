//! Terragrunt-specific functions.
//!
//! Each function is an [`HclFunc`] trait object that closes over an
//! [`Arc<TgState>`] carrying the workspace root, env-var policy, memo, and
//! include stack. Trait objects (not `fn` pointers) are required: every TG
//! function is **stateful**, and the spec text in
//! [14-terragrunt.md § 3.3] explicitly relies on per-call context
//! (`get_terragrunt_dir`, `path_relative_to_include`, …).
//!
//! [14-terragrunt.md § 3.3]: ../../../specs/14-terragrunt.md
//! [`HclFunc`]: crate::eval::HclFunc

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    eval::{CallCx, FuncError, HclFunc},
    ir::Value,
    util::paths::{self, SymlinkPolicy},
};

/// Per-resolution shared state the TG funcs close over.
///
/// Every field is read-mostly. The single mutable spot is the include
/// stack, which lives in a per-thread [`std::cell::RefCell`] (TG resolution
/// runs single-threaded *per component*; cross-component parallelism is
/// handled at the outer [`crate::terragrunt::TerragruntResolver`] layer).
#[derive(Debug)]
pub(super) struct TgState {
    /// Canonical workspace root for path-sandboxing.
    pub workspace_root: Arc<Path>,
    /// The current component's directory (parent of `terragrunt.hcl`).
    pub component_dir: Arc<Path>,
    /// Currently-active include path — `path_relative_to_include` /
    /// `path_relative_from_include` consult this. `None` when the
    /// resolver is processing the component's own `terragrunt.hcl`.
    ///
    /// The cell uses a `RefCell` so the resolver can swap it as the
    /// include chain descends. Per [14-terragrunt.md § 5 I-TG-5], TG
    /// resolution is single-threaded per call; the [`std::cell::RefCell`]
    /// is sound here.
    pub active_include: std::sync::Mutex<Option<Arc<Path>>>,
}

impl TgState {
    pub(super) fn new(workspace_root: Arc<Path>, component_dir: Arc<Path>) -> Self {
        Self {
            workspace_root,
            component_dir,
            active_include: std::sync::Mutex::new(None),
        }
    }
}

// ---------------------------------------------------------------------------
// get_terragrunt_dir() — returns the directory containing the *current*
// terragrunt.hcl. Used both by user code and by other TG funcs.
// ---------------------------------------------------------------------------

/// `get_terragrunt_dir() -> string`
#[derive(Debug)]
pub(super) struct GetTerragruntDirFn {
    pub state: Arc<TgState>,
}

impl HclFunc for GetTerragruntDirFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        if !args.is_empty() {
            return Err(FuncError::Arity {
                name: Arc::from("get_terragrunt_dir"),
                expected: 0,
                got: args.len(),
            });
        }
        Ok(Value::Str(Arc::from(
            self.state.component_dir.to_string_lossy().as_ref(),
        )))
    }
}

// ---------------------------------------------------------------------------
// get_repo_root() — returns the configured workspace_root, never shells
// out to `git rev-parse` (per spec § 3.3 closing rule).
// ---------------------------------------------------------------------------

/// `get_repo_root() -> string`
#[derive(Debug)]
pub(super) struct GetRepoRootFn {
    pub state: Arc<TgState>,
}

impl HclFunc for GetRepoRootFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        if !args.is_empty() {
            return Err(FuncError::Arity {
                name: Arc::from("get_repo_root"),
                expected: 0,
                got: args.len(),
            });
        }
        Ok(Value::Str(Arc::from(
            self.state.workspace_root.to_string_lossy().as_ref(),
        )))
    }
}

// ---------------------------------------------------------------------------
// get_parent_terragrunt_dir() — like get_terragrunt_dir() but for the
// *most recently active include*. Falls back to the component dir.
// ---------------------------------------------------------------------------

/// `get_parent_terragrunt_dir() -> string`
#[derive(Debug)]
pub(super) struct GetParentTerragruntDirFn {
    pub state: Arc<TgState>,
}

impl HclFunc for GetParentTerragruntDirFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        if !args.is_empty() {
            return Err(FuncError::Arity {
                name: Arc::from("get_parent_terragrunt_dir"),
                expected: 0,
                got: args.len(),
            });
        }
        let path = match self.state.active_include.lock() {
            Ok(guard) => guard
                .as_ref()
                .and_then(|p| p.parent().map(Path::to_path_buf)),
            Err(poisoned) => poisoned
                .into_inner()
                .as_ref()
                .and_then(|p| p.parent().map(Path::to_path_buf)),
        };
        let display = path.unwrap_or_else(|| self.state.component_dir.to_path_buf());
        Ok(Value::Str(Arc::from(display.to_string_lossy().as_ref())))
    }
}

// ---------------------------------------------------------------------------
// find_in_parent_folders(name = "terragrunt.hcl", fallback?) — walk up the
// directory tree starting at the current terragrunt.hcl's parent, return
// the first absolute path whose basename matches `name`.
// ---------------------------------------------------------------------------

/// `find_in_parent_folders(name = "terragrunt.hcl", fallback?) -> string`
#[derive(Debug)]
pub(super) struct FindInParentFoldersFn {
    pub state: Arc<TgState>,
}

impl HclFunc for FindInParentFoldersFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let name: &str = match args.first() {
            Some(Value::Str(s)) => s.as_ref(),
            None => "terragrunt.hcl",
            Some(other) => {
                return Err(FuncError::Type {
                    name: Arc::from("find_in_parent_folders"),
                    index: 0,
                    expected: "string",
                    got: type_name(other),
                });
            }
        };
        let fallback: Option<&str> = match args.get(1) {
            Some(Value::Str(s)) => Some(s.as_ref()),
            _ => None,
        };
        match find_in_parents(&self.state.component_dir, name, &self.state.workspace_root) {
            Some(path) => Ok(Value::Str(Arc::from(path.to_string_lossy().as_ref()))),
            None => match fallback {
                Some(fb) => Ok(Value::Str(Arc::from(fb))),
                None => Err(FuncError::Other {
                    name: Arc::from("find_in_parent_folders"),
                    message: Arc::from(format!(
                        "no `{name}` found above `{}`",
                        self.state.component_dir.display()
                    )),
                }),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// find_in_parent_folders_from(start, name?) — same, with explicit start.
// ---------------------------------------------------------------------------

/// `find_in_parent_folders_from(start, name = "terragrunt.hcl") -> string`
#[derive(Debug)]
pub(super) struct FindInParentFoldersFromFn {
    pub state: Arc<TgState>,
}

impl HclFunc for FindInParentFoldersFromFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        if args.is_empty() {
            return Err(FuncError::Arity {
                name: Arc::from("find_in_parent_folders_from"),
                expected: 1,
                got: 0,
            });
        }
        let start = match args.first() {
            Some(Value::Str(s)) => Path::new(s.as_ref()).to_path_buf(),
            Some(other) => {
                return Err(FuncError::Type {
                    name: Arc::from("find_in_parent_folders_from"),
                    index: 0,
                    expected: "string",
                    got: type_name(other),
                });
            }
            None => {
                return Err(FuncError::Arity {
                    name: Arc::from("find_in_parent_folders_from"),
                    expected: 1,
                    got: 0,
                });
            }
        };
        let name: &str = match args.get(1) {
            Some(Value::Str(s)) => s.as_ref(),
            None => "terragrunt.hcl",
            Some(other) => {
                return Err(FuncError::Type {
                    name: Arc::from("find_in_parent_folders_from"),
                    index: 1,
                    expected: "string",
                    got: type_name(other),
                });
            }
        };
        match find_in_parents(&start, name, &self.state.workspace_root) {
            Some(path) => Ok(Value::Str(Arc::from(path.to_string_lossy().as_ref()))),
            None => Err(FuncError::Other {
                name: Arc::from("find_in_parent_folders_from"),
                message: Arc::from(format!("no `{name}` found above `{}`", start.display())),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// path_relative_to_include / path_relative_from_include
// ---------------------------------------------------------------------------

/// `path_relative_to_include() -> string`
#[derive(Debug)]
pub(super) struct PathRelativeToIncludeFn {
    pub state: Arc<TgState>,
}

impl HclFunc for PathRelativeToIncludeFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        if !args.is_empty() {
            return Err(FuncError::Arity {
                name: Arc::from("path_relative_to_include"),
                expected: 0,
                got: args.len(),
            });
        }
        // Path of the *component dir* relative to the most recent include's
        // dir. With no active include, that's `.` per Terragrunt's docs.
        let active = match self.state.active_include.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        };
        let active_dir = active
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| self.state.component_dir.to_path_buf());
        let rel = match self.state.component_dir.strip_prefix(&active_dir) {
            Ok(r) => r.to_path_buf(),
            Err(_) => PathBuf::from("."),
        };
        let rendered = if rel.as_os_str().is_empty() {
            ".".to_string()
        } else {
            rel.to_string_lossy().into_owned()
        };
        Ok(Value::Str(Arc::from(rendered)))
    }
}

/// `path_relative_from_include() -> string`
///
/// Per Terragrunt docs: the inverse — path of the include dir relative to
/// the component dir.
#[derive(Debug)]
pub(super) struct PathRelativeFromIncludeFn {
    pub state: Arc<TgState>,
}

impl HclFunc for PathRelativeFromIncludeFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        if !args.is_empty() {
            return Err(FuncError::Arity {
                name: Arc::from("path_relative_from_include"),
                expected: 0,
                got: args.len(),
            });
        }
        let active = match self.state.active_include.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        };
        let active_dir = active
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| self.state.component_dir.to_path_buf());
        let rel = match active_dir.strip_prefix(&*self.state.component_dir) {
            Ok(r) => r.to_path_buf(),
            Err(_) => PathBuf::from("."),
        };
        let rendered = if rel.as_os_str().is_empty() {
            ".".to_string()
        } else {
            rel.to_string_lossy().into_owned()
        };
        Ok(Value::Str(Arc::from(rendered)))
    }
}

// ---------------------------------------------------------------------------
// `try(expr, fallback)` — Terraform-and-Terragrunt builtin: returns `expr`
// when it's a successfully-reduced literal, else `fallback`. Without
// expression-tree access at the function layer we can only act on the
// already-reduced [`Value`] args. The reducer pre-substitutes both args;
// when the first is `Value::Null` (and never actually resolved) we fall
// through. The closer we can get with a value-only interface is to treat
// the first arg as the result and never return fallback — but for the
// cascade pattern the caller wraps `try(read_terragrunt_config(...), { locals = {} })`,
// and the inner `read_terragrunt_config` falls back to `{}` itself when
// the file is missing. So this implementation simply returns the first
// arg verbatim.
// ---------------------------------------------------------------------------

/// `try(value, fallback) -> value`
#[derive(Debug)]
pub(super) struct TryFn;

impl HclFunc for TryFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        match args.first() {
            Some(v) => Ok(v.clone()),
            None => match args.get(1) {
                Some(fb) => Ok(fb.clone()),
                None => Err(FuncError::Arity {
                    name: Arc::from("try"),
                    expected: 1,
                    got: 0,
                }),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Number(_) => "number",
        Value::Str(_) => "string",
        Value::List(_) => "list",
        Value::Map(_) => "map",
    }
}

/// Walk up from `start_dir` (parent at each step) looking for `name`.
/// Bounded by the canonical workspace root — stops if we'd leave it.
/// Returns `None` when no candidate exists at any level.
fn find_in_parents(start_dir: &Path, name: &str, workspace_root: &Path) -> Option<PathBuf> {
    let abs_start = if start_dir.is_absolute() {
        start_dir.to_path_buf()
    } else {
        workspace_root.join(start_dir)
    };
    let mut cursor: PathBuf = abs_start;
    loop {
        let candidate = cursor.join(name);
        if candidate.exists() {
            // Honour I-TG-1: candidate must canonicalise inside the workspace root.
            if let Ok(canonical) =
                paths::canonicalize_inside(&candidate, workspace_root, SymlinkPolicy::Follow)
            {
                return Some(canonical);
            }
        }
        // Walk up; stop if we leave the workspace root or hit the FS root.
        if !paths::is_descendant(&cursor, workspace_root) {
            return None;
        }
        if cursor == workspace_root {
            return None;
        }
        if !cursor.pop() {
            return None;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use crate::eval::{EnvVarMode, EvalLimits};

    fn cx<'a>(root: &'a Path, env: &'a EnvVarMode, limits: &'a EvalLimits) -> CallCx<'a> {
        CallCx {
            workspace_root: root,
            env_vars: env,
            limits,
        }
    }

    #[test]
    fn test_get_terragrunt_dir_returns_component_dir() {
        let root: Arc<Path> = Arc::from(Path::new("/tmp/repo"));
        let comp: Arc<Path> = Arc::from(Path::new("/tmp/repo/services/a"));
        let state = Arc::new(TgState::new(Arc::clone(&root), Arc::clone(&comp)));
        let fn_ = GetTerragruntDirFn {
            state: Arc::clone(&state),
        };
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let v = fn_.call(&[], &cx(&root, &env, &limits)).unwrap();
        assert_eq!(v, Value::Str(Arc::from("/tmp/repo/services/a")));
    }

    #[test]
    fn test_get_repo_root_returns_workspace_root() {
        let root: Arc<Path> = Arc::from(Path::new("/tmp/repo"));
        let comp: Arc<Path> = Arc::from(Path::new("/tmp/repo/x"));
        let state = Arc::new(TgState::new(Arc::clone(&root), comp));
        let fn_ = GetRepoRootFn { state };
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let v = fn_.call(&[], &cx(&root, &env, &limits)).unwrap();
        assert_eq!(v, Value::Str(Arc::from("/tmp/repo")));
    }

    #[test]
    fn test_find_in_parent_folders_returns_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root: Arc<Path> = Arc::from(std::fs::canonicalize(tmp.path()).unwrap());
        let nested = root.join("services/a");
        std::fs::create_dir_all(&nested).unwrap();
        // Plant a `root.hcl` at the workspace root.
        std::fs::write(root.join("root.hcl"), "").unwrap();
        let state = Arc::new(TgState::new(Arc::clone(&root), Arc::from(nested)));
        let fn_ = FindInParentFoldersFn { state };
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let v = fn_
            .call(
                &[Value::Str(Arc::from("root.hcl"))],
                &cx(&root, &env, &limits),
            )
            .unwrap();
        match v {
            Value::Str(s) => assert!(s.ends_with("root.hcl"), "{s}"),
            other => panic!("expected Str, got {other:?}"),
        }
    }

    #[test]
    fn test_find_in_parent_folders_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let root: Arc<Path> = Arc::from(std::fs::canonicalize(tmp.path()).unwrap());
        let nested = root.join("services/a");
        std::fs::create_dir_all(&nested).unwrap();
        let state = Arc::new(TgState::new(Arc::clone(&root), Arc::from(nested)));
        let fn_ = FindInParentFoldersFn { state };
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let v = fn_
            .call(
                &[
                    Value::Str(Arc::from("missing.hcl")),
                    Value::Str(Arc::from("fallback.hcl")),
                ],
                &cx(&root, &env, &limits),
            )
            .unwrap();
        assert_eq!(v, Value::Str(Arc::from("fallback.hcl")));
    }

    #[test]
    fn test_try_returns_first_arg() {
        let fn_ = TryFn;
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let root = Path::new("/");
        let v = fn_
            .call(&[Value::Int(42), Value::Int(0)], &cx(root, &env, &limits))
            .unwrap();
        assert_eq!(v, Value::Int(42));
    }
}
