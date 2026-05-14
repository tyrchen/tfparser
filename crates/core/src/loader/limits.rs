//! Loader resource limits.
//!
//! Defaults match [12-hcl-loader.md § 3.5] and [70-security.md § 3.2]. The
//! loader checks each cap as it walks; breaching one emits a
//! [`crate::Diagnostic`] (file skipped) for the per-file caps and
//! [`crate::Error::Limit`] (fatal) for caps the caller could only have
//! noticed by allowing unbounded work first.
//!
//! [12-hcl-loader.md § 3.5]: ../../../specs/12-hcl-loader.md
//! [70-security.md § 3.2]: ../../../specs/70-security.md

use typed_builder::TypedBuilder;

/// Per-file caps enforced by the loader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TypedBuilder)]
#[non_exhaustive]
pub struct LoaderLimits {
    /// Maximum byte size for any individual file. Default: 4 MiB.
    #[builder(default = 4 * 1024 * 1024)]
    pub max_file_bytes: u32,

    /// Maximum top-level-or-nested block count per file. Default: 10 000.
    #[builder(default = 10_000)]
    pub max_blocks_per_file: u32,

    /// Maximum nested expression depth (objects, lists, conditionals,
    /// templates). Default: 64.
    #[builder(default = 64)]
    pub max_attr_depth: u32,

    /// Maximum number of parts in a `TemplateConcat` lowering output.
    /// Default: 1024.
    #[builder(default = 1024)]
    pub max_template_parts: u32,
}

impl Default for LoaderLimits {
    fn default() -> Self {
        Self::builder().build()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults_match_spec() {
        let l = LoaderLimits::default();
        assert_eq!(l.max_file_bytes, 4 * 1024 * 1024);
        assert_eq!(l.max_blocks_per_file, 10_000);
        assert_eq!(l.max_attr_depth, 64);
        assert_eq!(l.max_template_parts, 1024);
    }

    #[test]
    fn test_builder_overrides() {
        let l = LoaderLimits::builder()
            .max_file_bytes(1024_u32)
            .max_blocks_per_file(2_u32)
            .build();
        assert_eq!(l.max_file_bytes, 1024);
        assert_eq!(l.max_blocks_per_file, 2);
        // Other fields keep their defaults.
        assert_eq!(l.max_attr_depth, 64);
    }
}
