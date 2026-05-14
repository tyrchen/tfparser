//! Sandboxed file functions: `file`, `fileexists`, `templatefile`, `fileset`.
//!
//! Every read is routed through
//! [`crate::util::paths::canonicalize_inside`] with the workspace root the
//! evaluator was bound to. Per [70-security.md § 3.1 P5], **all** file
//! reads in this crate must funnel through the one sandbox helper — no
//! private "I'll just open this path" code paths.
//!
//! [70-security.md § 3.1 P5]: ../../../specs/70-security.md

use std::{io, path::Path, sync::Arc};

use crate::{
    diagnostic::LimitKind,
    eval::{
        registry::{CallCx, FuncError, FuncRegistryBuilder, HclFunc},
        stdlib::type_name,
    },
    ir::Value,
    util::paths::{PathSafetyError, SymlinkPolicy, canonicalize_inside},
};

/// Register the four file functions into `b`.
pub fn register(b: &mut FuncRegistryBuilder) {
    b.register("file", Arc::new(FileFn));
    b.register("fileexists", Arc::new(FileexistsFn));
    b.register("templatefile", Arc::new(TemplatefileFn));
    b.register("fileset", Arc::new(FilesetFn));
}

fn require_str<'a>(
    name: &'static str,
    args: &'a [Value],
    index: usize,
) -> Result<&'a str, FuncError> {
    match args.get(index) {
        Some(Value::Str(s)) => Ok(s.as_ref()),
        Some(other) => Err(FuncError::Type {
            name: Arc::from(name),
            index,
            expected: "string",
            got: type_name(other),
        }),
        None => Err(FuncError::Arity {
            name: Arc::from(name),
            expected: index + 1,
            got: args.len(),
        }),
    }
}

fn resolve_inside(
    name: &'static str,
    root: &Path,
    candidate: &str,
) -> Result<std::path::PathBuf, FuncError> {
    match canonicalize_inside(Path::new(candidate), root, SymlinkPolicy::Reject) {
        Ok(p) => Ok(p),
        Err(PathSafetyError::Escape { .. }) => Err(FuncError::PathEscape {
            name,
            path: std::path::PathBuf::from(candidate),
        }),
        Err(PathSafetyError::UnexpectedSymlink(p) | PathSafetyError::NulByte(p)) => {
            Err(FuncError::PathEscape { name, path: p })
        }
        Err(PathSafetyError::Io { path, source }) => Err(FuncError::Other {
            name: Arc::from(name),
            message: Arc::from(format!("i/o resolving {}: {source}", path.display())),
        }),
    }
}

fn read_capped(name: &'static str, path: &Path, cap: u32) -> Result<Vec<u8>, FuncError> {
    let meta = std::fs::metadata(path).map_err(|e| io_err(name, path, &e))?;
    let len = meta.len();
    let limit = u64::from(cap);
    if len > limit {
        return Err(FuncError::Limit {
            name: Arc::from(name),
            kind: LimitKind::FileSize,
            observed: len,
            limit,
        });
    }
    std::fs::read(path).map_err(|e| io_err(name, path, &e))
}

fn io_err(name: &'static str, path: &Path, err: &io::Error) -> FuncError {
    FuncError::Other {
        name: Arc::from(name),
        message: Arc::from(format!("i/o reading {}: {err}", path.display())),
    }
}

#[derive(Debug)]
struct FileFn;
impl HclFunc for FileFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let raw = require_str("file", args, 0)?;
        let path = resolve_inside("file", cx.workspace_root, raw)?;
        let bytes = read_capped("file", &path, cx.limits.max_file_bytes)?;
        let text = String::from_utf8(bytes).map_err(|e| FuncError::Other {
            name: Arc::from("file"),
            message: Arc::from(format!("invalid utf-8 in {}: {e}", path.display())),
        })?;
        Ok(Value::Str(Arc::from(text)))
    }
}

#[derive(Debug)]
struct FileexistsFn;
impl HclFunc for FileexistsFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let raw = require_str("fileexists", args, 0)?;
        match canonicalize_inside(Path::new(raw), cx.workspace_root, SymlinkPolicy::Reject) {
            Ok(p) => Ok(Value::Bool(std::fs::metadata(p).is_ok())),
            // Path escapes: surface the escape. A bare "is it inside?" is
            // not enough — silently returning `false` would mask a
            // misconfigured config that expected an outside-root file.
            Err(PathSafetyError::Escape { .. }) => Err(FuncError::PathEscape {
                name: "fileexists",
                path: std::path::PathBuf::from(raw),
            }),
            // Symlink rejection / NUL bytes: same shape.
            Err(PathSafetyError::UnexpectedSymlink(p) | PathSafetyError::NulByte(p)) => {
                Err(FuncError::PathEscape {
                    name: "fileexists",
                    path: p,
                })
            }
            // I/O resolving (e.g. permission denied on an ancestor):
            // Terraform's fileexists returns false on any I/O error, but
            // surfacing the error here as a Func failure keeps the
            // diagnostic loop intact. The call site then keeps the
            // unresolved expression.
            Err(PathSafetyError::Io { .. }) => Ok(Value::Bool(false)),
        }
    }
}

#[derive(Debug)]
struct TemplatefileFn;
impl HclFunc for TemplatefileFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let raw = require_str("templatefile", args, 0)?;
        let vars = match args.get(1) {
            Some(Value::Map(m)) => m,
            Some(other) => {
                return Err(FuncError::Type {
                    name: Arc::from("templatefile"),
                    index: 1,
                    expected: "map",
                    got: type_name(other),
                });
            }
            None => {
                return Err(FuncError::Arity {
                    name: Arc::from("templatefile"),
                    expected: 2,
                    got: args.len(),
                });
            }
        };
        let path = resolve_inside("templatefile", cx.workspace_root, raw)?;
        let bytes = read_capped("templatefile", &path, cx.limits.max_file_bytes)?;
        let template = String::from_utf8(bytes).map_err(|e| FuncError::Other {
            name: Arc::from("templatefile"),
            message: Arc::from(format!("invalid utf-8 in {}: {e}", path.display())),
        })?;
        let out = render_template(&template, vars)?;
        let observed = u64::try_from(out.len()).unwrap_or(u64::MAX);
        let limit = u64::from(cx.limits.max_str_size);
        if observed > limit {
            return Err(FuncError::Limit {
                name: Arc::from("templatefile"),
                kind: LimitKind::StringSize,
                observed,
                limit,
            });
        }
        Ok(Value::Str(Arc::from(out)))
    }
}

/// Render a Terraform template with a minimal substitution pass.
///
/// Supports only `${name}` interpolations whose binding is a string in
/// `vars`. Any unresolved reference (`${var.x}`, `${each.value}`,
/// `${trimspace(...)}`, …) leaves the template unresolved by surfacing a
/// `FuncError::Other`, which keeps the call site as a
/// [`crate::ir::Expression::FuncCall`] — the correct best-effort outcome
/// per spec 13 § 5 closing rule. Phase 9 hardening may swap in a full
/// template engine; for Phase 4 the simple substitution covers the
/// "load a `user_data` file and inject `${region}`" pattern that
/// fixtures reach for.
fn render_template(src: &str, vars: &crate::ir::Map) -> Result<String, FuncError> {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = bytes.get(i..).unwrap_or_default();
        if rest.starts_with(b"$${") {
            // Escaped sequence: emit literal `${`.
            out.push_str("${");
            i += 3;
        } else if rest.starts_with(b"${") {
            let after_open = bytes.get(i + 2..).unwrap_or_default();
            let close =
                after_open
                    .iter()
                    .position(|c| *c == b'}')
                    .ok_or_else(|| FuncError::Other {
                        name: Arc::from("templatefile"),
                        message: Arc::from("unterminated `${` in template"),
                    })?;
            let name_range_start = i + 2;
            let name_range_end = name_range_start + close;
            let name = src
                .get(name_range_start..name_range_end)
                .map(str::trim)
                .ok_or_else(|| FuncError::Other {
                    name: Arc::from("templatefile"),
                    message: Arc::from("malformed `${...}` in template"),
                })?;
            // Only plain identifier substitution; anything fancier is left
            // unresolved.
            if !is_plain_ident(name) {
                return Err(FuncError::Other {
                    name: Arc::from("templatefile"),
                    message: Arc::from(format!("unresolvable template ref `{name}`")),
                });
            }
            match vars.iter().find(|(k, _)| k.as_ref() == name) {
                Some((_, Value::Str(s))) => out.push_str(s),
                Some((_, other)) => {
                    return Err(FuncError::Type {
                        name: Arc::from("templatefile"),
                        index: 1,
                        expected: "string-valued binding",
                        got: type_name(other),
                    });
                }
                None => {
                    return Err(FuncError::Other {
                        name: Arc::from("templatefile"),
                        message: Arc::from(format!("template ref `{name}` not bound")),
                    });
                }
            }
            i = name_range_end + 1;
        } else {
            match src.get(i..).and_then(|s| s.chars().next()) {
                Some(ch) => {
                    out.push(ch);
                    i += ch.len_utf8();
                }
                None => break,
            }
        }
    }
    Ok(out)
}

fn is_plain_ident(s: &str) -> bool {
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_alphabetic() || b == b'_' => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[derive(Debug)]
struct FilesetFn;
impl HclFunc for FilesetFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let raw_dir = require_str("fileset", args, 0)?;
        let pattern = require_str("fileset", args, 1)?;

        // Cap the pattern length per [70-security.md § 3.4]: globs from
        // arbitrary user input cannot exceed 256 bytes.
        if pattern.len() > 256 {
            return Err(FuncError::Other {
                name: Arc::from("fileset"),
                message: Arc::from("glob pattern exceeds 256-byte cap"),
            });
        }

        let dir = resolve_inside("fileset", cx.workspace_root, raw_dir)?;
        let meta = std::fs::metadata(&dir).map_err(|e| io_err("fileset", &dir, &e))?;
        if !meta.is_dir() {
            return Err(FuncError::Other {
                name: Arc::from("fileset"),
                message: Arc::from(format!(
                    "`fileset` base is not a directory: {}",
                    dir.display()
                )),
            });
        }

        let glob = globset::GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|e| FuncError::Other {
                name: Arc::from("fileset"),
                message: Arc::from(format!("invalid glob `{pattern}`: {e}")),
            })?
            .compile_matcher();

        let mut matches: Vec<String> = Vec::new();
        for entry in ignore::WalkBuilder::new(&dir)
            .standard_filters(false)
            .hidden(false)
            .build()
        {
            let entry = entry.map_err(|e| FuncError::Other {
                name: Arc::from("fileset"),
                message: Arc::from(format!("walk error: {e}")),
            })?;
            if entry.file_type().is_none_or(|ft| !ft.is_file()) {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(&dir) else {
                continue;
            };
            // Skip the root directory itself.
            if rel.as_os_str().is_empty() {
                continue;
            }
            // Normalise to forward slashes (per Component.path I-IR-2).
            let s = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if glob.is_match(&s) {
                matches.push(s);
                let observed = u64::try_from(matches.len()).unwrap_or(u64::MAX);
                let limit = u64::from(cx.limits.max_list_len);
                if observed > limit {
                    return Err(FuncError::Limit {
                        name: Arc::from("fileset"),
                        kind: LimitKind::ListLength,
                        observed,
                        limit,
                    });
                }
            }
        }
        // Terraform's fileset returns a *set* — we represent it as a sorted
        // list for byte-deterministic output.
        matches.sort();
        Ok(Value::List(
            matches
                .into_iter()
                .map(|s| Value::Str(Arc::from(s)))
                .collect(),
        ))
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
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::*;
    use crate::eval::context::{EnvVarMode, EvalLimits};

    fn cx_for<'a>(root: &'a Path, env: &'a EnvVarMode, limits: &'a EvalLimits) -> CallCx<'a> {
        CallCx {
            workspace_root: root,
            env_vars: env,
            limits,
        }
    }

    fn write(root: &Path, rel: &str, contents: &[u8]) -> PathBuf {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, contents).unwrap();
        p
    }

    #[test]
    fn test_file_reads_inside_root() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        write(&root, "hello.txt", b"world");
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let cx = cx_for(&root, &env, &limits);
        let f = FileFn;
        let v = f.call(&[Value::Str(Arc::from("hello.txt"))], &cx).unwrap();
        assert_eq!(v, Value::Str(Arc::from("world")));
    }

    #[test]
    fn test_file_rejects_path_escape() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let cx = cx_for(&root, &env, &limits);
        let f = FileFn;
        let err = f
            .call(&[Value::Str(Arc::from("../../etc/passwd"))], &cx)
            .unwrap_err();
        assert!(
            matches!(err, FuncError::PathEscape { name: "file", .. }),
            "{err:?}"
        );
    }

    #[test]
    fn test_file_enforces_byte_cap() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        write(&root, "big.txt", &vec![0u8; 1024]);
        let env = EnvVarMode::default();
        let limits = EvalLimits {
            max_file_bytes: 64,
            ..EvalLimits::default()
        };
        let cx = cx_for(&root, &env, &limits);
        let f = FileFn;
        let err = f
            .call(&[Value::Str(Arc::from("big.txt"))], &cx)
            .unwrap_err();
        assert!(
            matches!(
                err,
                FuncError::Limit {
                    kind: LimitKind::FileSize,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn test_fileexists_true_then_false() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        write(&root, "exists.txt", b"x");
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let cx = cx_for(&root, &env, &limits);
        let f = FileexistsFn;
        assert_eq!(
            f.call(&[Value::Str(Arc::from("exists.txt"))], &cx).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            f.call(&[Value::Str(Arc::from("missing.txt"))], &cx)
                .unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_fileexists_rejects_escape() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let cx = cx_for(&root, &env, &limits);
        let f = FileexistsFn;
        let err = f
            .call(&[Value::Str(Arc::from("../../etc/passwd"))], &cx)
            .unwrap_err();
        assert!(matches!(err, FuncError::PathEscape { .. }), "{err:?}");
    }

    #[test]
    fn test_templatefile_substitutes_plain_identifier() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        write(&root, "tmpl.txt", b"hello ${name}!");
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let cx = cx_for(&root, &env, &limits);
        let f = TemplatefileFn;
        let v = f
            .call(
                &[
                    Value::Str(Arc::from("tmpl.txt")),
                    Value::Map(vec![(Arc::from("name"), Value::Str(Arc::from("world")))]),
                ],
                &cx,
            )
            .unwrap();
        assert_eq!(v, Value::Str(Arc::from("hello world!")));
    }

    #[test]
    fn test_templatefile_escape_sequence() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        write(&root, "tmpl.txt", b"price is $${price}");
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let cx = cx_for(&root, &env, &limits);
        let f = TemplatefileFn;
        let v = f
            .call(
                &[Value::Str(Arc::from("tmpl.txt")), Value::Map(Vec::new())],
                &cx,
            )
            .unwrap();
        assert_eq!(v, Value::Str(Arc::from("price is ${price}")));
    }

    #[test]
    fn test_templatefile_unresolvable_ref_errors() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        write(&root, "tmpl.txt", b"hi ${trimspace(name)}");
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let cx = cx_for(&root, &env, &limits);
        let f = TemplatefileFn;
        let err = f
            .call(
                &[Value::Str(Arc::from("tmpl.txt")), Value::Map(Vec::new())],
                &cx,
            )
            .unwrap_err();
        assert!(matches!(err, FuncError::Other { .. }));
    }

    #[test]
    fn test_fileset_lists_matches_sorted() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        write(&root, "a/b.tf", b"");
        write(&root, "a/c.tf", b"");
        write(&root, "a/skip.txt", b"");
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let cx = cx_for(&root, &env, &limits);
        let f = FilesetFn;
        let v = f
            .call(
                &[Value::Str(Arc::from("a")), Value::Str(Arc::from("*.tf"))],
                &cx,
            )
            .unwrap();
        assert_eq!(
            v,
            Value::List(vec![
                Value::Str(Arc::from("b.tf")),
                Value::Str(Arc::from("c.tf")),
            ])
        );
    }

    #[test]
    fn test_fileset_rejects_long_pattern() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("a")).unwrap();
        let env = EnvVarMode::default();
        let limits = EvalLimits::default();
        let cx = cx_for(&root, &env, &limits);
        let pattern = "*".repeat(300);
        let f = FilesetFn;
        let err = f
            .call(
                &[
                    Value::Str(Arc::from("a")),
                    Value::Str(Arc::from(pattern.as_str())),
                ],
                &cx,
            )
            .unwrap_err();
        assert!(matches!(err, FuncError::Other { .. }), "{err:?}");
    }
}
