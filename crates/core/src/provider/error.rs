//! Provider-resolver errors.
//!
//! Per [16-provider-resolver.md § 6]. Fatal failures (file I/O, validation
//! that breaks an invariant) bubble through [`ProviderError`]. Recoverable
//! anomalies (a profile that simply isn't in the map, an unrecognised role
//! ARN) surface as [`crate::Diagnostic`]s on the workspace.
//!
//! [16-provider-resolver.md § 6]: ../../../specs/16-provider-resolver.md

use std::{path::PathBuf, sync::Arc};

use crate::error::ValidationError;

/// Errors raised by the provider-resolver layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// I/O failure reading `~/.aws/config` or a profile-map YAML file.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// Path the operation was attempting.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The configured profile-map file exceeded the size cap.
    #[error("profile-map file at {path} is {observed} bytes, exceeds cap {limit}")]
    FileTooLarge {
        /// Offending path.
        path: PathBuf,
        /// Observed byte count.
        observed: u64,
        /// Limit in bytes.
        limit: u64,
    },

    /// INI parse error (`~/.aws/config`).
    #[error("ini parse error at {path}: {source}")]
    Ini {
        /// File the parser was reading.
        path: PathBuf,
        /// Underlying parser error.
        #[source]
        source: ini::ParseError,
    },

    /// YAML deserialise error.
    #[error("yaml deserialise error at {path}: {source}")]
    Yaml {
        /// File path.
        path: PathBuf,
        /// Underlying parser error.
        #[source]
        source: serde_yaml::Error,
    },

    /// `validator` rejected a field in the profile-map YAML.
    #[error("profile-map validation error in {path}: {source}")]
    Validation {
        /// File the field came from.
        path: PathBuf,
        /// Aggregated validation errors.
        #[source]
        source: validator::ValidationErrors,
    },

    /// An entry passed `validator` but failed our newtype check
    /// (account-id / region length-charset). Carries the offending
    /// profile name so the operator can fix the YAML.
    #[error("profile-map entry `{profile}` rejected: {source}")]
    InvalidEntry {
        /// Offending profile name.
        profile: Arc<str>,
        /// Why it was rejected.
        #[source]
        source: ValidationError,
    },

    /// `source_profile` chain exceeded the hop cap (8 per spec § 3.1).
    #[error(
        "aws-config source_profile chain at `{profile}` exceeded {limit} hops (probable cycle)"
    )]
    ChainTooLong {
        /// The profile that started the over-long chain.
        profile: Arc<str>,
        /// Configured hop cap.
        limit: usize,
    },

    /// Resolver was configured in strict mode and one or more profiles
    /// referenced from a `Workspace` had no entry in the profile map.
    #[error("{count} unresolved profile(s) in strict mode (first: `{first}`)")]
    StrictUnresolved {
        /// Total unique unresolved profiles.
        count: usize,
        /// First profile encountered without a mapping.
        first: Arc<str>,
    },
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, ProviderError>;
