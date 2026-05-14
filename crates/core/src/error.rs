//! Top-level error type for `tfparser-core`.
//!
//! Per CLAUDE.md § Error Handling: every fallible API in this crate returns
//! [`Result<T>`] aliased to `std::result::Result<T, Error>`. Phase-specific
//! errors (`DiscoveryError`, `LoaderError`, `EvalError`, …) plug in via
//! `#[from]` once their owning modules land. Until then the variants below
//! are sufficient — none of them are reachable in Phase 1 since no real
//! pipeline runs yet, but they pin the public error contract so downstream
//! crates can match on the shape today.

use std::path::PathBuf;

use thiserror::Error;

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level error returned by every public `tfparser_core` API.
///
/// Per the spec's stability contract ([10-data-model.md § Versioning], the
/// variants below are `#[non_exhaustive]` so future phases can add error
/// kinds without a major bump. Match arms should always include a
/// catch-all.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// A validated newtype rejected its input (length, charset, or shape
    /// invariant). The accompanying [`ValidationError`] names the field and
    /// the rule.
    #[error("validation failed: {0}")]
    Validation(#[from] ValidationError),

    /// I/O error reading or writing a path.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// Path that triggered the error.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A configured resource limit was exceeded. The accompanying
    /// [`crate::diagnostic::LimitKind`] identifies which one.
    #[error("limit exceeded ({kind:?}): observed {observed} > limit {limit}")]
    Limit {
        /// Which limit category fired.
        kind: crate::diagnostic::LimitKind,
        /// Observed value.
        observed: u64,
        /// Configured limit.
        limit: u64,
    },

    /// Provider resolver (Phase 7) raised a fatal error.
    #[error(transparent)]
    Provider(#[from] crate::provider::ProviderError),
}

/// Reasons a validated newtype constructor (e.g. [`crate::Address::new`])
/// can reject its input.
///
/// Field names are stable; new variants are additive.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ValidationError {
    /// The candidate string was empty when a non-empty value was required.
    #[error("`{field}` must not be empty")]
    Empty {
        /// Name of the field being validated.
        field: &'static str,
    },

    /// The candidate exceeds the per-field byte cap.
    #[error("`{field}` exceeds maximum byte length ({observed} > {limit})")]
    TooLong {
        /// Name of the field being validated.
        field: &'static str,
        /// Observed byte length.
        observed: usize,
        /// Configured maximum.
        limit: usize,
    },

    /// A disallowed character appeared in the candidate.
    #[error(
        "`{field}` contains disallowed character {:?} at byte {offset}",
        char::from(*byte)
    )]
    BadChar {
        /// Name of the field being validated.
        field: &'static str,
        /// Offending byte (rendered as a `char` in the message).
        byte: u8,
        /// Byte offset of the offending character.
        offset: usize,
    },

    /// The candidate failed a higher-level structural rule (balanced
    /// quotes/brackets, expected digit count, etc.). `rule` is a short
    /// machine-readable token (e.g. `"unbalanced-brackets"`); the message
    /// embeds the input snippet for diagnostics.
    #[error("`{field}` failed rule `{rule}`")]
    Shape {
        /// Name of the field being validated.
        field: &'static str,
        /// Rule identifier — short machine-readable token.
        rule: &'static str,
    },
}
