//! Phase 0 spike 0.4 — `hcl-rs::eval::Context` with a custom `find_in_parent_folders`
//! `FuncDef`.
//!
//! Goal: prove we can (a) declare `var.*` so `${var.environment}` reduces,
//! (b) register a custom Terragrunt-shaped function, (c) sandbox it against
//! a workspace root the way [14-terragrunt.md § 3.3] specifies.
//!
//! Run with: `cargo run -p tfparser-core --example spike_eval_context`.
//!
//! [14-terragrunt.md § 3.3]: ../../../specs/14-terragrunt.md

#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use hcl::{
    Expression, Map, Value,
    eval::{Context, Evaluate, FuncArgs, FuncDef, ParamType},
};

/// Per-thread workspace state the custom `FuncDef`s read from.
///
/// `hcl::eval::FuncDef::build` accepts a bare `fn` pointer (no closure
/// captures), so the production Terragrunt resolver — and this spike — pass
/// state through a thread-local that is set right before evaluation. The
/// `WorkspaceCtx::scope` RAII guard guarantees the slot is cleared
/// afterwards so cross-test contamination is impossible.
#[derive(Clone, Debug)]
struct WorkspaceCtx {
    repo_root: PathBuf,
    current_dir: PathBuf,
}

thread_local! {
    static WS: RefCell<Option<WorkspaceCtx>> = const { RefCell::new(None) };
}

struct WorkspaceScope;

impl WorkspaceCtx {
    fn scope(self) -> WorkspaceScope {
        WS.with(|cell| *cell.borrow_mut() = Some(self));
        WorkspaceScope
    }
}

impl Drop for WorkspaceScope {
    fn drop(&mut self) {
        WS.with(|cell| *cell.borrow_mut() = None);
    }
}

fn with_workspace<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&WorkspaceCtx) -> Result<R, String>,
{
    WS.with(|cell| match &*cell.borrow() {
        Some(ws) => f(ws),
        None => Err("find_in_parent_folders called outside a WorkspaceCtx scope".into()),
    })
}

/// Implementation of Terragrunt's `find_in_parent_folders(name)`.
///
/// Sandboxed against [`WorkspaceCtx::repo_root`] per [70-security.md § 3.1].
/// The full Terragrunt signature also accepts a fallback; the spike covers
/// the single-arg form. The optional-arg variant lands in Phase 6.
///
/// `args` is taken by value because that's the signature [`hcl::eval::Func`]
/// (a bare `fn` pointer) requires — see the comment on [`WorkspaceCtx`].
#[allow(clippy::needless_pass_by_value)]
fn find_in_parent_folders_impl(args: FuncArgs) -> Result<Value, String> {
    let Some(Value::String(name)) = args.first() else {
        return Err("find_in_parent_folders: argument must be a string".into());
    };

    if name.is_empty() || name.contains('/') || name.contains('\\') || name.as_str() == ".." {
        return Err(format!(
            "find_in_parent_folders: rejected name {name:?} (must be a bare filename)"
        ));
    }

    with_workspace(|ws| {
        let mut cur = ws.current_dir.clone();
        loop {
            let candidate = cur.join(name.as_str());
            if candidate.exists() {
                let canon = candidate
                    .canonicalize()
                    .map_err(|e| format!("canonicalize {}: {e}", candidate.display()))?;
                if !canon.starts_with(&ws.repo_root) {
                    return Err(format!(
                        "find_in_parent_folders: {} escapes repo_root {}",
                        canon.display(),
                        ws.repo_root.display(),
                    ));
                }
                return Ok(Value::String(canon.display().to_string()));
            }
            if !cur.pop() || !cur.starts_with(&ws.repo_root) {
                return Err(format!(
                    "find_in_parent_folders: no {name:?} found beneath repo_root"
                ));
            }
        }
    })
}

fn build_find_in_parent_folders() -> FuncDef {
    FuncDef::builder()
        .params([ParamType::String])
        .build(find_in_parent_folders_impl)
}

fn build_context() -> Context<'static> {
    let mut ctx = Context::new();

    // Declare `var.*` so `var.environment` reduces. The variable namespace
    // is itself an Object — that's how hcl-rs::eval models traversal.
    let mut vars = Map::new();
    vars.insert("environment".into(), Value::String("staging".into()));
    vars.insert("region".into(), Value::String("us-west-2".into()));
    ctx.declare_var("var", Value::Object(vars));

    // Register the custom function. The workspace state it reads from is
    // injected via `WorkspaceCtx::scope` at evaluation time.
    ctx.declare_func("find_in_parent_folders", build_find_in_parent_folders());

    ctx
}

fn parse_eval(src: &str, ctx: &Context<'_>) -> anyhow::Result<Value> {
    let expr: Expression = src
        .parse()
        .with_context(|| format!("parse `{src}` as HCL expression"))?;
    expr.evaluate(ctx)
        .map_err(|e| anyhow::anyhow!("evaluate `{src}`: {e:?}"))
}

fn main() -> anyhow::Result<()> {
    println!("=== spike_eval_context ===");

    let tmp = tempfile::tempdir().context("create tempdir")?;
    let repo_root = tmp.path().canonicalize().context("canonicalize tempdir")?;
    let deep = repo_root.join("a").join("b").join("c");
    fs::create_dir_all(&deep)?;
    let target = repo_root.join("a").join("target.hcl");
    fs::write(&target, "# target")?;

    let ctx = build_context();
    let _scope = WorkspaceCtx {
        repo_root: repo_root.clone(),
        current_dir: deep.clone(),
    }
    .scope();

    // 1. var.* resolution.
    let env = parse_eval("var.environment", &ctx)?;
    anyhow::ensure!(
        env == Value::String("staging".into()),
        "var.environment did not reduce to 'staging' (got {env:?})"
    );
    println!("var.environment -> {env:?}");

    // 2. Template interpolation.
    let tpl = parse_eval(r#""env=${var.environment} region=${var.region}""#, &ctx)?;
    anyhow::ensure!(
        tpl == Value::String("env=staging region=us-west-2".into()),
        "template interpolation drift (got {tpl:?})"
    );
    println!("template            -> {tpl:?}");

    // 3. find_in_parent_folders walks up from deep/ and finds target.hcl in a/.
    let found = parse_eval(r#"find_in_parent_folders("target.hcl")"#, &ctx)?;
    let Value::String(found_str) = &found else {
        anyhow::bail!("expected string from find_in_parent_folders, got {found:?}");
    };
    let found_path = Path::new(found_str.as_str());
    anyhow::ensure!(
        found_path == target.canonicalize()?,
        "find_in_parent_folders returned wrong path: {found_str}"
    );
    println!("find_in_parent_folders('target.hcl') -> {found_str}");

    // 4. Path escape rejected.
    let escape_attempt = parse_eval(r#"find_in_parent_folders("../etc/passwd")"#, &ctx);
    anyhow::ensure!(
        escape_attempt.is_err(),
        "name with `/` should have been rejected",
    );
    println!("find_in_parent_folders('../etc/passwd') -> rejected ✓");

    // 5. Missing file produces a clean error (not a panic).
    let missing = parse_eval(r#"find_in_parent_folders("absent.hcl")"#, &ctx);
    anyhow::ensure!(
        missing.is_err(),
        "missing file should have produced an error"
    );
    println!("find_in_parent_folders('absent.hcl') -> error ✓");

    println!("OK — eval Context handles var.*, templates, and a sandboxed Terragrunt func.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize tempdir");
        let deep = root.join("a").join("b").join("c");
        fs::create_dir_all(&deep).expect("mkdirs");
        let target = root.join("a").join("target.hcl");
        fs::write(&target, "# target").expect("write target");
        (tmp, root, deep)
    }

    #[test]
    fn test_find_in_parent_folders_walks_up_to_match() {
        let (_tmp, root, deep) = fixture();
        let ctx = build_context();
        let _scope = WorkspaceCtx {
            repo_root: root,
            current_dir: deep,
        }
        .scope();
        let v = parse_eval(r#"find_in_parent_folders("target.hcl")"#, &ctx).unwrap();
        match v {
            Value::String(s) => assert!(
                s.ends_with("a/target.hcl") || s.ends_with("a\\target.hcl"),
                "got {s}, want suffix `a/target.hcl`"
            ),
            other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn test_find_in_parent_folders_rejects_slashes_in_name() {
        let (_tmp, root, deep) = fixture();
        let ctx = build_context();
        let _scope = WorkspaceCtx {
            repo_root: root,
            current_dir: deep,
        }
        .scope();
        let err = parse_eval(r#"find_in_parent_folders("../target.hcl")"#, &ctx).unwrap_err();
        assert!(format!("{err:?}").contains("rejected name"));
    }

    #[test]
    fn test_var_namespace_object_access() {
        let ctx = build_context();
        assert_eq!(
            parse_eval("var.environment", &ctx).unwrap(),
            Value::String("staging".into())
        );
    }
}
