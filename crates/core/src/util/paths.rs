//! Path-safety helpers shared by discovery, loader, evaluator, and Terragrunt
//! phases.
//!
//! All file reads in this crate must be gated through [`canonicalize_inside`]
//! before opening — that's the load-bearing rule of [70-security.md § 3.1].
//! NUL bytes are rejected up front; symlinks are off by default; the resolved
//! path must remain a descendant of the workspace root.
//!
//! The helpers here do **no I/O writes**, never `chmod`, and never resolve
//! through symlinks unless the caller opts in. Returning `Err` is always
//! cheaper than reading a path we should not have read.
//!
//! [70-security.md § 3.1]: ../../../specs/70-security.md

use std::{
    io,
    path::{Component, Path, PathBuf},
};

use thiserror::Error;

/// Errors returned by the path-safety helpers.
///
/// `#[non_exhaustive]` so future phases can add variants (TOCTOU window,
/// excluded-by-policy) without a breaking bump.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PathSafetyError {
    /// The candidate path contained an interior NUL byte. Some kernels accept
    /// these (with truncation); none of them mean what the caller intends.
    #[error("path contains a NUL byte: {0}")]
    NulByte(PathBuf),

    /// The candidate path resolved outside the allowed root. This is the
    /// path-traversal defence — see [70-security.md § 3.1 P1].
    #[error("path escape: `{candidate}` resolves outside `{root}`")]
    Escape {
        /// The candidate as supplied.
        candidate: PathBuf,
        /// The root the candidate must remain underneath.
        root: PathBuf,
    },

    /// The candidate is a symlink and `follow_symlinks` was `false`.
    #[error("path is a symlink and follow_symlinks=false: {0}")]
    UnexpectedSymlink(PathBuf),

    /// I/O error while metadata-querying the candidate. Wrapped, never
    /// converted to `PathSafetyError::Escape` (do not silently treat I/O
    /// failures as path escapes).
    #[error("i/o error resolving {path}: {source}")]
    Io {
        /// Path that triggered the error.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// Whether [`canonicalize_inside`] should follow symbolic links.
///
/// Default is [`SymlinkPolicy::Reject`] per [70-security.md § 3.1 P2]: even
/// inside a vetted workspace, a symlink can point at `/etc/passwd` after a
/// commit lands; off-by-default keeps the parser honest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymlinkPolicy {
    /// Reject any symlink encountered — including the candidate itself and
    /// any ancestor in its resolution chain.
    Reject,
    /// Resolve the symlink and re-check that the target remains under the
    /// allowed root.
    Follow,
}

/// Reject any path containing an interior NUL byte. Discovery and loader call
/// this before any other validation.
///
/// # Errors
///
/// [`PathSafetyError::NulByte`] when an interior NUL is present. Caller must
/// not retry — there's no semantically reasonable repair.
pub fn reject_nul(candidate: &Path) -> Result<(), PathSafetyError> {
    let bytes = candidate.as_os_str().as_encoded_bytes();
    if bytes.contains(&0) {
        return Err(PathSafetyError::NulByte(candidate.to_path_buf()));
    }
    Ok(())
}

/// Canonicalise `candidate` and verify it remains under `root`.
///
/// `root` should already be canonicalised by the caller (typically discovery
/// at workspace-root binding time). The function:
///
/// 1. Rejects NUL bytes in `candidate`.
/// 2. Joins `candidate` onto `root` if it is relative — relative paths are interpreted *relative to
///    the root*, not to the process CWD.
/// 3. Resolves the result via [`std::fs::canonicalize`] when `policy == SymlinkPolicy::Follow`, or
///    via lexical normalisation when `policy == SymlinkPolicy::Reject`.
/// 4. Asserts the resolved path is `root` itself or a descendant.
///
/// # Errors
///
/// - [`PathSafetyError::NulByte`] for NUL in `candidate`.
/// - [`PathSafetyError::Escape`] when the resolved path is not under `root`.
/// - [`PathSafetyError::UnexpectedSymlink`] under [`SymlinkPolicy::Reject`] when the candidate or
///   any ancestor is a symlink.
/// - [`PathSafetyError::Io`] for metadata / canonicalisation failures.
pub fn canonicalize_inside(
    candidate: &Path,
    root: &Path,
    policy: SymlinkPolicy,
) -> Result<PathBuf, PathSafetyError> {
    reject_nul(candidate)?;
    reject_nul(root)?;

    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };

    let resolved = match policy {
        SymlinkPolicy::Follow => {
            std::fs::canonicalize(&joined).map_err(|source| PathSafetyError::Io {
                path: joined.clone(),
                source,
            })?
        }
        SymlinkPolicy::Reject => {
            check_no_symlink_ancestors(&joined)?;
            lexically_normalize(&joined)
        }
    };

    if !is_descendant(&resolved, root) {
        return Err(PathSafetyError::Escape {
            candidate: candidate.to_path_buf(),
            root: root.to_path_buf(),
        });
    }
    Ok(resolved)
}

/// Whether `path` is `root` or a descendant of `root`. Both inputs should be
/// canonical-form paths (no `.` or `..` components); the comparison is a
/// component-wise prefix match, not a string `starts_with`, so
/// `/repo-2` does not match `/repo`.
#[must_use]
pub fn is_descendant(path: &Path, root: &Path) -> bool {
    let mut path_components = path.components();
    for root_component in root.components() {
        match path_components.next() {
            Some(c) if c == root_component => {}
            _ => return false,
        }
    }
    true
}

/// Walk every ancestor of `candidate` from the candidate itself up to the
/// filesystem root, returning [`PathSafetyError::UnexpectedSymlink`] if any
/// step is a symlink.
///
/// Used inside [`canonicalize_inside`] when [`SymlinkPolicy::Reject`] is in
/// effect. We *want* to fail when the input path crosses a symlink at any
/// point, not just at the leaf — symlink-ancestor escapes are the classic
/// container-breakout vector.
fn check_no_symlink_ancestors(candidate: &Path) -> Result<(), PathSafetyError> {
    let mut probe: PathBuf = PathBuf::new();
    for component in candidate.components() {
        probe.push(component.as_os_str());
        if matches!(component, Component::RootDir | Component::Prefix(_)) {
            continue;
        }
        match std::fs::symlink_metadata(&probe) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(PathSafetyError::UnexpectedSymlink(probe));
            }
            Ok(_) | Err(_) => {
                // Missing leaf is fine — caller may be probing for
                // existence. Non-symlink intermediate dirs are fine.
                // Errors on ancestors mean we cannot prove safety
                // ourselves; the descendant-of-root check downstream
                // remains the load-bearing escape defence.
            }
        }
    }
    Ok(())
}

/// Lexically normalise `path` by collapsing `.` / `..` components without
/// touching the filesystem. Used in the [`SymlinkPolicy::Reject`] path so we
/// don't accidentally follow any symlink on the way to canonical form.
///
/// `..` at the root is dropped (cannot escape lexically); a non-absolute
/// input keeps its leading components intact.
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    // Refuse to climb above the start. The caller's escape
                    // check will then fail against `root`, which is the
                    // correct outcome.
                }
            }
        }
    }
    out
}

/// Convert a canonical absolute path to one relative to `root`, returning the
/// original path if it does not lie under `root`. Helper for the discovery
/// stage (which records relative paths in `DiscoveredDir.path`).
#[cfg(test)]
#[must_use]
pub fn relative_to(path: &Path, root: &Path) -> std::sync::Arc<Path> {
    use std::sync::Arc;
    path.strip_prefix(root)
        .map_or_else(|_| Arc::from(path), Arc::from)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_should_reject_nul_in_path() {
        let raw = OsString::from_vec(vec![b'/', b't', 0, b'p']);
        let p = PathBuf::from(raw);
        let err = reject_nul(&p).unwrap_err();
        assert!(matches!(err, PathSafetyError::NulByte(_)));
    }

    #[test]
    fn test_should_accept_descendant_path() {
        let root = tempdir().unwrap();
        let canonical_root = std::fs::canonicalize(root.path()).unwrap();
        let child = canonical_root.join("a/b");
        std::fs::create_dir_all(&child).unwrap();
        let resolved = canonicalize_inside(&child, &canonical_root, SymlinkPolicy::Reject).unwrap();
        assert!(is_descendant(&resolved, &canonical_root));
    }

    #[test]
    fn test_should_reject_path_escape() {
        let root = tempdir().unwrap();
        let canonical_root = std::fs::canonicalize(root.path()).unwrap();
        let escape = canonical_root.join("../escape");
        let err = canonicalize_inside(&escape, &canonical_root, SymlinkPolicy::Reject).unwrap_err();
        assert!(matches!(err, PathSafetyError::Escape { .. }));
    }

    #[test]
    fn test_should_reject_symlink_with_reject_policy() {
        let root = tempdir().unwrap();
        let canonical_root = std::fs::canonicalize(root.path()).unwrap();
        let real = canonical_root.join("real");
        std::fs::create_dir(&real).unwrap();
        let link = canonical_root.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = canonicalize_inside(&link, &canonical_root, SymlinkPolicy::Reject).unwrap_err();
        assert!(matches!(err, PathSafetyError::UnexpectedSymlink(_)));
    }

    #[test]
    fn test_should_resolve_symlink_with_follow_policy_inside_root() {
        let root = tempdir().unwrap();
        let canonical_root = std::fs::canonicalize(root.path()).unwrap();
        let real = canonical_root.join("real");
        std::fs::create_dir(&real).unwrap();
        let link = canonical_root.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let resolved = canonicalize_inside(&link, &canonical_root, SymlinkPolicy::Follow).unwrap();
        assert_eq!(resolved, real);
    }

    #[test]
    fn test_should_reject_symlink_pointing_outside_root_with_follow_policy() {
        let outside = tempdir().unwrap();
        let outside_canonical = std::fs::canonicalize(outside.path()).unwrap();
        let outside_target = outside_canonical.join("hostage");
        std::fs::create_dir(&outside_target).unwrap();

        let root = tempdir().unwrap();
        let canonical_root = std::fs::canonicalize(root.path()).unwrap();
        let link = canonical_root.join("escape");
        std::os::unix::fs::symlink(&outside_target, &link).unwrap();

        let err = canonicalize_inside(&link, &canonical_root, SymlinkPolicy::Follow).unwrap_err();
        assert!(matches!(err, PathSafetyError::Escape { .. }));
    }

    #[test]
    fn test_is_descendant_does_not_match_string_prefixes() {
        let root = Path::new("/tmp/repo");
        let sibling = Path::new("/tmp/repo-2/x");
        assert!(!is_descendant(sibling, root));
        let inside = Path::new("/tmp/repo/x");
        assert!(is_descendant(inside, root));
    }

    #[test]
    fn test_lexically_normalize_collapses_dot_dot() {
        let p = Path::new("/a/b/../c/./d");
        assert_eq!(lexically_normalize(p), PathBuf::from("/a/c/d"));
    }

    #[test]
    fn test_lexically_normalize_does_not_escape_root() {
        let p = Path::new("/../../etc/passwd");
        let n = lexically_normalize(p);
        // After collapsing, the result is under `/`; the escape check at the
        // call site is what prevents using this path. Here we only verify
        // the lexical pass does not produce a `..` outside the input.
        assert!(!n.to_string_lossy().contains(".."));
    }

    #[test]
    fn test_relative_to_strips_root() {
        let root = Path::new("/tmp/repo");
        let path = Path::new("/tmp/repo/a/b");
        let rel = relative_to(path, root);
        assert_eq!(&*rel, Path::new("a/b"));
    }
}
