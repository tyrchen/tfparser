//! Discovery configuration.
//!
//! Defaults pin the caps from [70-security.md § 3.2] and the exclude globs
//! from [11-discovery.md § 2]. Callers either call [`DiscoveryOptions::defaults`]
//! (covers every M0 fixture) or override fields via the generated
//! [`DiscoveryOptionsBuilder`].
//!
//! [70-security.md § 3.2]: ../../../specs/70-security.md
//! [11-discovery.md § 2]: ../../../specs/11-discovery.md

use std::sync::Arc;

use globset::{Glob, GlobSet, GlobSetBuilder};
use thiserror::Error;
use typed_builder::TypedBuilder;

/// Hard cap on the byte length of any user-supplied glob pattern, per
/// [70-security.md § 3.4](../../../specs/70-security.md). Beyond this length
/// regex compilation is rejected before reaching `globset`.
pub const MAX_GLOB_PATTERN_BYTES: usize = 256;

/// Cap on walker thread count. Per [11-discovery.md § 3.5], beyond 8 the fs
/// metadata calls saturate on macOS APFS / Linux ext4. The current
/// sequential walker uses one thread; the cap is reserved for the parallel
/// implementation that lands in Phase 9.
pub const MAX_DISCOVERY_THREADS: u32 = 8;

/// Discovery configuration (caps, excludes, classification hints).
///
/// Build via the generated [`DiscoveryOptionsBuilder`].
#[derive(Clone, Debug, TypedBuilder)]
#[non_exhaustive]
pub struct DiscoveryOptions {
    /// Whether the walker follows symlinks. Default: `false`.
    #[builder(default = false)]
    pub follow_symlinks: bool,

    /// Maximum walk depth, measured in directory levels below the workspace
    /// root. Default: `16` per [70-security.md § 3.2].
    #[builder(default = 16)]
    pub max_depth: u32,

    /// Maximum byte size for any individual file. Files exceeding this cap
    /// are skipped with a [`crate::diagnostic::LimitKind::FileSize`]
    /// diagnostic. Default: 8 MiB.
    #[builder(default = 8 * 1024 * 1024)]
    pub max_file_size_bytes: u64,

    /// Workspace-wide cap on the total number of files visited. Breaching
    /// this returns [`crate::Error::Limit`] (fatal). Default: `200_000` per
    /// [70-security.md § 3.2].
    #[builder(default = 200_000)]
    pub max_total_files: u64,

    /// Compiled exclude glob set (defaults plus user-supplied entries).
    /// Anything matching is dropped from the walk *before* classification.
    #[builder(default = default_exclude_globset())]
    pub exclude_globs: Arc<GlobSet>,

    /// Compiled module-glob set. A directory whose path matches is
    /// pre-classified as a module before the file-content heuristics run
    /// (per [11-discovery.md § 3.2 rule 1]).
    #[builder(default = default_module_globset())]
    pub module_globs: Arc<GlobSet>,

    /// Number of walker threads (capped at [`MAX_DISCOVERY_THREADS`]). The
    /// current sequential walker ignores this value but it is reserved for
    /// the parallel implementation that lands in Phase 9.
    #[builder(default = 0)]
    pub threads: u32,
}

impl DiscoveryOptions {
    /// Build the spec defaults: `max_depth=16`, `max_file_size=8 MiB`,
    /// `max_total_files=200_000`, `follow_symlinks=false`, default
    /// excludes (`.git`, `.terraform`, `.terragrunt-cache`), default module
    /// globs (`modules/**`, `**/modules/*`).
    #[must_use]
    pub fn defaults() -> Self {
        Self::builder().build()
    }

    /// The configured `threads` value clamped to [`MAX_DISCOVERY_THREADS`].
    /// Used by the (forthcoming) parallel walker; surfaced now so callers
    /// can introspect the effective value without re-implementing the cap.
    #[must_use]
    pub const fn effective_threads(&self) -> u32 {
        if self.threads > MAX_DISCOVERY_THREADS {
            MAX_DISCOVERY_THREADS
        } else {
            self.threads
        }
    }
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Errors raised while compiling user-supplied glob patterns.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GlobConfigError {
    /// Pattern exceeded [`MAX_GLOB_PATTERN_BYTES`]. Caller should reject the
    /// input — this is a defence against adversarial regex `DoS` via the
    /// underlying `globset` engine.
    #[error("glob pattern too long ({observed} > {limit}): `{pattern}`")]
    TooLong {
        /// The pattern as supplied.
        pattern: String,
        /// Observed byte length.
        observed: usize,
        /// Configured cap.
        limit: usize,
    },

    /// `globset` rejected the pattern as malformed.
    #[error("invalid glob pattern `{pattern}`: {source}")]
    Invalid {
        /// The pattern as supplied.
        pattern: String,
        /// Underlying error.
        #[source]
        source: globset::Error,
    },
}

/// Compile a user-supplied glob list into a [`GlobSet`], applying the length
/// cap from [70-security.md § 3.4](../../../specs/70-security.md).
///
/// # Errors
///
/// [`GlobConfigError::TooLong`] when any pattern exceeds the cap.
/// [`GlobConfigError::Invalid`] when `globset` rejects the syntax.
pub fn compile_glob_set<I, S>(patterns: I) -> Result<GlobSet, GlobConfigError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let pattern_str = pattern.as_ref();
        if pattern_str.len() > MAX_GLOB_PATTERN_BYTES {
            return Err(GlobConfigError::TooLong {
                pattern: pattern_str.to_owned(),
                observed: pattern_str.len(),
                limit: MAX_GLOB_PATTERN_BYTES,
            });
        }
        let glob = Glob::new(pattern_str).map_err(|source| GlobConfigError::Invalid {
            pattern: pattern_str.to_owned(),
            source,
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|source| GlobConfigError::Invalid {
        pattern: String::new(),
        source,
    })
}

/// Default exclude globs — pinned in [11-discovery.md § 2].
fn default_exclude_globset() -> Arc<GlobSet> {
    let patterns = [
        "**/.git",
        "**/.git/**",
        "**/.terraform",
        "**/.terraform/**",
        "**/.terragrunt-cache",
        "**/.terragrunt-cache/**",
    ];
    let set = compile_glob_set(patterns).unwrap_or_else(|_| GlobSet::empty());
    Arc::new(set)
}

/// Default module globs — pinned in [11-discovery.md § 3.2].
fn default_module_globset() -> Arc<GlobSet> {
    let patterns = [
        "modules/*",
        "modules/**",
        "modules-tf12/*",
        "modules-tf12/**",
        "**/modules/*",
        "**/modules-tf12/*",
    ];
    let set = compile_glob_set(patterns).unwrap_or_else(|_| GlobSet::empty());
    Arc::new(set)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn test_should_build_defaults() {
        let opts = DiscoveryOptions::defaults();
        assert!(!opts.follow_symlinks);
        assert_eq!(opts.max_depth, 16);
        assert_eq!(opts.max_file_size_bytes, 8 * 1024 * 1024);
        assert_eq!(opts.max_total_files, 200_000);
        assert!(opts.exclude_globs.is_match(Path::new(".git/HEAD")));
        assert!(
            opts.exclude_globs
                .is_match(Path::new("services/order-service/.terraform/lockfile"))
        );
    }

    #[test]
    fn test_should_match_module_globs_by_default() {
        let opts = DiscoveryOptions::defaults();
        assert!(opts.module_globs.is_match(Path::new("modules/iam-role")));
        assert!(
            opts.module_globs
                .is_match(Path::new("terraform/modules/vpc"))
        );
    }

    #[test]
    fn test_should_compile_user_glob_set() {
        let set = compile_glob_set(["foo/**", "bar/*"]).unwrap();
        assert!(set.is_match(Path::new("foo/x/y")));
        assert!(set.is_match(Path::new("bar/x")));
        assert!(!set.is_match(Path::new("baz")));
    }

    #[test]
    fn test_should_reject_overlong_pattern() {
        let pattern = "a".repeat(MAX_GLOB_PATTERN_BYTES + 1);
        let err = compile_glob_set([pattern]).unwrap_err();
        assert!(matches!(err, GlobConfigError::TooLong { .. }));
    }

    #[test]
    fn test_should_reject_invalid_glob_pattern() {
        let err = compile_glob_set(["[unterminated"]).unwrap_err();
        assert!(matches!(err, GlobConfigError::Invalid { .. }));
    }

    #[test]
    fn test_builder_applies_overrides() {
        let opts: DiscoveryOptions = DiscoveryOptions::builder()
            .max_depth(4_u32)
            .follow_symlinks(true)
            .threads(2_u32)
            .build();
        assert_eq!(opts.max_depth, 4);
        assert!(opts.follow_symlinks);
        assert_eq!(opts.threads, 2);
    }
}
