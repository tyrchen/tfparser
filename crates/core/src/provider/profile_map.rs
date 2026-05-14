//! `ProfileMap` — the AWS-profile → account mapping the resolver consults.
//!
//! Three concrete loaders are provided, mirroring [16-provider-resolver.md § 3]:
//!
//! - [`load_yaml_profile_map`]: a user-supplied YAML file (validated via the `validator` derive —
//!   CLAUDE.md § Input Validation, "rejects, doesn't sanitize").
//! - [`load_aws_config`]: parses `~/.aws/config` with `rust-ini`, following `source_profile` chains
//!   up to a hop cap.
//! - [`empty`]: returns an empty map (the `none` loader from spec § 3.3).
//!
//! All loaders return an [`Arc<ProfileMap>`] so the resolver hot path can
//! `Arc::clone` cheaply, and downstream callers can wrap the value behind
//! an [`SharedProfileMap`] (`ArcSwap<ProfileMap>`) for lock-free swap on
//! re-runs (CLAUDE.md § Async & Concurrency).
//!
//! ## Size cap
//!
//! Per spec 16 § 9, both loaders enforce a hard 256 KiB byte cap on the
//! source file. Anything larger is rejected with
//! [`ProviderError::FileTooLarge`]. This is defence against
//! decompression-style amplification when the loader is fed an
//! adversarial file.

use std::{collections::HashMap, fs, io::Read as _, path::Path, sync::Arc};

use arc_swap::ArcSwap;
use ini::Ini;
use regex::Regex;
use serde::Deserialize;
use validator::{Validate, ValidationError as VrError};

use crate::{
    ir::{AccountId, Region},
    provider::error::{ProviderError, Result},
};

/// Spec 16 § 9: hard size cap for the source file (`~/.aws/config` or
/// `profile-map.yaml`).
pub const PROFILE_MAP_FILE_MAX_BYTES: u64 = 256 * 1024;

/// Spec 16 § 3.1: maximum `source_profile` chain hop count.
pub const AWS_CONFIG_MAX_CHAIN_HOPS: usize = 8;

/// One entry in the [`ProfileMap`].
///
/// Per [16-provider-resolver.md § 2]. Account id and region are stored as
/// validated newtypes; `role_arn` stays as `Arc<str>` because it is only
/// ever passed back into [`extract_account_id`](super::resolver::extract_account_id)
/// downstream.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProfileEntry {
    /// AWS account id (12 digits).
    pub account_id: AccountId,
    /// Human-friendly label; may equal `account_id` when no other name is
    /// available.
    pub account_name: Arc<str>,
    /// Region declared in the source, if any.
    pub region: Option<Region>,
    /// Raw `role_arn` if the profile assumed a role.
    pub role_arn: Option<Arc<str>>,
}

/// Profile → entry index used by the provider resolver.
///
/// Construction goes through one of the loaders in this module so
/// validation is enforced once and `ProfileMap` exposes only resolved data.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ProfileMap {
    entries: HashMap<Arc<str>, ProfileEntry>,
}

impl ProfileMap {
    /// Look up an entry by profile name. `O(1)`.
    #[must_use]
    pub fn lookup(&self, profile: &str) -> Option<&ProfileEntry> {
        self.entries.get(profile)
    }

    /// Number of profiles in the map.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if the map contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate `(profile, entry)` pairs. Iteration order is unspecified;
    /// the consumer must sort if a deterministic walk is required.
    pub fn iter(&self) -> impl Iterator<Item = (&Arc<str>, &ProfileEntry)> {
        self.entries.iter()
    }
}

/// Atomically-swappable handle for re-runs. Per CLAUDE.md § Async &
/// Concurrency: `ArcSwap` for infrequently-updated shared data; lock-free
/// reads on the hot resolver path.
pub type SharedProfileMap = ArcSwap<ProfileMap>;

/// Build a [`SharedProfileMap`] seeded with `initial`.
#[must_use]
pub fn shared(initial: Arc<ProfileMap>) -> SharedProfileMap {
    ArcSwap::from(initial)
}

/// Return an empty [`ProfileMap`] (spec § 3.3 `none` loader).
#[must_use]
pub fn empty() -> Arc<ProfileMap> {
    Arc::new(ProfileMap::default())
}

// ----------------------------------------------------------------------------
// YAML loader (spec § 3.2)
// ----------------------------------------------------------------------------

/// Raw YAML body. `#[serde(deny_unknown_fields)]` per CLAUDE.md § Type
/// Design — fails fast on typos.
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
struct YamlBody {
    #[validate(nested)]
    profiles: HashMap<String, YamlEntry>,
}

#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
struct YamlEntry {
    /// 12-digit AWS account id.
    #[validate(regex(path = *ACCOUNT_ID_RE))]
    account_id: String,
    /// Human-friendly account name; bounded.
    #[validate(length(min = 1, max = 64), regex(path = *NAME_RE))]
    account_name: String,
    /// Optional region (`a-z0-9-`).
    #[serde(default)]
    #[validate(custom(function = "validate_region_opt"))]
    region: Option<String>,
    /// Optional `role_arn` (loose check — `extract_account_id` does the
    /// strict parse downstream).
    #[serde(default)]
    #[validate(length(max = 2048))]
    role_arn: Option<String>,
}

// SAFETY (clippy::unwrap_used): the literal patterns are exercised by
// `test_static_regexes_compile` and the validator-derived integration
// tests; a regression in the literal would fail those tests at build
// time. There is no observable runtime recovery path, so the unwrap is
// the simplest sound choice.
#[allow(clippy::unwrap_used)]
static ACCOUNT_ID_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^\d{12}$").unwrap());

#[allow(clippy::unwrap_used)]
static NAME_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._\-/ ]{1,64}$").unwrap());

fn validate_region_opt(value: &str) -> std::result::Result<(), VrError> {
    Region::new(value).map(|_| ()).map_err(|_| {
        let mut e = VrError::new("region");
        e.message = Some("region must match ^[a-z0-9-]{1,32}$".into());
        e
    })
}

/// Load a profile map from a YAML file at `path`.
///
/// # Errors
///
/// Returns [`ProviderError`] on I/O failure, size-cap breach, YAML parse,
/// or validation rejection.
pub fn load_yaml_profile_map(path: &Path) -> Result<Arc<ProfileMap>> {
    let bytes = read_capped(path)?;
    let body: YamlBody = serde_yaml::from_slice(&bytes).map_err(|source| ProviderError::Yaml {
        path: path.to_path_buf(),
        source,
    })?;
    body.validate()
        .map_err(|source| ProviderError::Validation {
            path: path.to_path_buf(),
            source,
        })?;

    let mut entries = HashMap::with_capacity(body.profiles.len());
    for (name, raw) in body.profiles {
        let profile_arc: Arc<str> = Arc::from(name.as_str());
        let account_id =
            AccountId::new(&raw.account_id).map_err(|source| ProviderError::InvalidEntry {
                profile: Arc::clone(&profile_arc),
                source,
            })?;
        let region = match raw.region {
            Some(r) => Some(
                Region::new(&r).map_err(|source| ProviderError::InvalidEntry {
                    profile: Arc::clone(&profile_arc),
                    source,
                })?,
            ),
            None => None,
        };
        let role_arn = raw.role_arn.map(Arc::<str>::from);
        let account_name = Arc::<str>::from(raw.account_name);
        entries.insert(
            profile_arc,
            ProfileEntry {
                account_id,
                account_name,
                region,
                role_arn,
            },
        );
    }
    Ok(Arc::new(ProfileMap { entries }))
}

// ----------------------------------------------------------------------------
// AWS-config loader (spec § 3.1)
// ----------------------------------------------------------------------------

/// Load a profile map from `~/.aws/config` (or any user-supplied INI-shaped
/// file).
///
/// Per spec § 3.1: for each `[profile <name>]` section we extract
/// `sso_account_id`, `role_arn`, and `region`. When the section has none,
/// we follow `source_profile = ...` up to [`AWS_CONFIG_MAX_CHAIN_HOPS`] hops.
///
/// # Errors
///
/// Returns [`ProviderError`] on I/O failure, size-cap breach, INI parse, or
/// chain-cycle detection.
pub fn load_aws_config(path: &Path) -> Result<Arc<ProfileMap>> {
    let bytes = read_capped(path)?;
    // CLAUDE.md § Input Validation — reject, don't sanitize. Refuse the
    // file outright on invalid UTF-8 rather than silently swap in U+FFFD
    // replacement characters.
    let text = std::str::from_utf8(&bytes).map_err(|source| ProviderError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
    })?;
    let ini = Ini::load_from_str(text).map_err(|source| ProviderError::Ini {
        path: path.to_path_buf(),
        source,
    })?;

    // First pass: index every section by its profile name (the section
    // label after `profile ` for `[profile xxx]`, plain `default` for
    // `[default]`, ignored otherwise).
    let mut raw_sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    for (section, props) in &ini {
        let Some(s) = section else { continue };
        let name = if s == "default" {
            "default".to_owned()
        } else if let Some(stripped) = s.strip_prefix("profile ") {
            stripped.trim().to_owned()
        } else {
            continue;
        };
        let mut bucket: HashMap<String, String> = HashMap::new();
        for (k, v) in props {
            bucket.insert(k.to_owned(), v.to_owned());
        }
        raw_sections.insert(name, bucket);
    }

    // Second pass: resolve each profile, following `source_profile` chains.
    let mut entries: HashMap<Arc<str>, ProfileEntry> = HashMap::new();
    for name in raw_sections.keys().cloned().collect::<Vec<_>>() {
        let resolved = resolve_aws_profile(&name, &raw_sections)?;
        if let Some(entry) = resolved {
            entries.insert(Arc::from(name.as_str()), entry);
        }
    }
    Ok(Arc::new(ProfileMap { entries }))
}

/// Walk the `source_profile` chain for `name` and produce a [`ProfileEntry`]
/// when an account id can be inferred.
///
/// Caps the chain at [`AWS_CONFIG_MAX_CHAIN_HOPS`] traversals: the starting
/// profile is hop 0, the next is hop 1, and so on. If the chain has not
/// resolved an account by hop `MAX` **and** the next profile is still a
/// `source_profile` pointer, the chain is too long and we surface
/// [`ProviderError::ChainTooLong`] rather than silently truncate (spec § 3.1).
fn resolve_aws_profile(
    name: &str,
    sections: &HashMap<String, HashMap<String, String>>,
) -> Result<Option<ProfileEntry>> {
    let mut visited: Vec<String> = Vec::new();
    let mut cur = name.to_string();
    let mut last_region: Option<Region> = None;
    let mut last_role_arn: Option<Arc<str>> = None;
    let mut account: Option<AccountId> = None;
    let mut hops: usize = 0;

    loop {
        if visited.iter().any(|v| v == &cur) {
            return Err(ProviderError::ChainTooLong {
                profile: Arc::from(cur.as_str()),
                limit: AWS_CONFIG_MAX_CHAIN_HOPS,
            });
        }
        visited.push(cur.clone());

        let Some(props) = sections.get(&cur) else {
            // Chain points at an undefined profile — stop here.
            break;
        };

        if last_region.is_none()
            && let Some(r) = props.get("region")
            && let Ok(parsed) = Region::new(r)
        {
            last_region = Some(parsed);
        }
        if last_role_arn.is_none()
            && let Some(arn) = props.get("role_arn")
        {
            last_role_arn = Some(Arc::from(arn.as_str()));
        }

        if account.is_none()
            && let Some(sso) = props.get("sso_account_id")
            && let Ok(id) = AccountId::new(sso)
        {
            account = Some(id);
        }
        if account.is_none()
            && let Some(arn) = props.get("role_arn")
        {
            account = super::resolver::extract_account_id(arn);
        }

        if account.is_some() {
            break;
        }

        match props.get("source_profile") {
            Some(next) if !next.is_empty() => {
                if hops >= AWS_CONFIG_MAX_CHAIN_HOPS {
                    return Err(ProviderError::ChainTooLong {
                        profile: Arc::from(name),
                        limit: AWS_CONFIG_MAX_CHAIN_HOPS,
                    });
                }
                cur = next.clone();
                hops = hops.saturating_add(1);
            }
            _ => break,
        }
    }

    let Some(account_id) = account else {
        return Ok(None);
    };
    Ok(Some(ProfileEntry {
        account_name: Arc::from(name),
        account_id,
        region: last_region,
        role_arn: last_role_arn,
    }))
}

// ----------------------------------------------------------------------------
// Shared helpers
// ----------------------------------------------------------------------------

fn read_capped(path: &Path) -> Result<Vec<u8>> {
    let meta = fs::metadata(path).map_err(|source| ProviderError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if meta.len() > PROFILE_MAP_FILE_MAX_BYTES {
        return Err(ProviderError::FileTooLarge {
            path: path.to_path_buf(),
            observed: meta.len(),
            limit: PROFILE_MAP_FILE_MAX_BYTES,
        });
    }
    let mut buf = Vec::with_capacity(meta.len().try_into().unwrap_or_default());
    fs::File::open(path)
        .and_then(|mut f| f.read_to_end(&mut buf))
        .map_err(|source| ProviderError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(buf)
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

    fn write_tmp(text: &str) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), text).unwrap();
        f
    }

    // -- YAML loader -----------------------------------------------------

    #[test]
    fn test_should_load_minimal_yaml_profile_map() {
        let f = write_tmp(
            r#"
profiles:
  main-developer:
    account_id: "370025973162"
    account_name: "primary"
    region: "us-west-2"
"#,
        );
        let map = load_yaml_profile_map(f.path()).unwrap();
        let entry = map.lookup("main-developer").unwrap();
        assert_eq!(entry.account_id.as_str(), "370025973162");
        assert_eq!(entry.account_name.as_ref(), "primary");
        assert_eq!(entry.region.as_ref().map(Region::as_str), Some("us-west-2"));
    }

    #[test]
    fn test_should_reject_yaml_bad_account_id() {
        let f = write_tmp(
            r#"
profiles:
  bad:
    account_id: "12"
    account_name: "x"
"#,
        );
        let err = load_yaml_profile_map(f.path()).unwrap_err();
        assert!(
            matches!(err, ProviderError::Validation { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn test_should_reject_yaml_unknown_field() {
        let f = write_tmp(
            r#"
profiles:
  good:
    account_id: "370025973162"
    account_name: "primary"
    rogue_field: "value"
"#,
        );
        let err = load_yaml_profile_map(f.path()).unwrap_err();
        assert!(matches!(err, ProviderError::Yaml { .. }), "got {err:?}");
    }

    #[test]
    fn test_should_reject_yaml_file_over_size_cap() {
        let f = tempfile::NamedTempFile::new().unwrap();
        // 257 KiB ASCII content under a "profiles:" header — content has to
        // be ≥ cap to trigger the rejection. The body need not parse.
        let cap_usize: usize = usize::try_from(PROFILE_MAP_FILE_MAX_BYTES).unwrap();
        let payload = "x".repeat(cap_usize + 64);
        std::fs::write(f.path(), payload).unwrap();
        let err = load_yaml_profile_map(f.path()).unwrap_err();
        assert!(
            matches!(err, ProviderError::FileTooLarge { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn test_should_load_yaml_with_role_arn() {
        let f = write_tmp(
            r#"
profiles:
  cross-account:
    account_id: "999999999999"
    account_name: "cross"
    role_arn: "arn:aws:iam::999999999999:role/admin"
"#,
        );
        let map = load_yaml_profile_map(f.path()).unwrap();
        let e = map.lookup("cross-account").unwrap();
        assert_eq!(
            e.role_arn.as_deref(),
            Some("arn:aws:iam::999999999999:role/admin")
        );
    }

    #[test]
    fn test_should_reject_yaml_invalid_region_chars() {
        let f = write_tmp(
            r#"
profiles:
  bad-region:
    account_id: "111111111111"
    account_name: "x"
    region: "US-EAST-1"
"#,
        );
        let err = load_yaml_profile_map(f.path()).unwrap_err();
        assert!(
            matches!(err, ProviderError::Validation { .. }),
            "got {err:?}"
        );
    }

    // -- aws_config loader ----------------------------------------------

    #[test]
    fn test_should_load_aws_config_with_sso_account_id() {
        let f = write_tmp(
            r"
[profile main]
sso_account_id = 370025973162
region = us-west-2
",
        );
        let map = load_aws_config(f.path()).unwrap();
        let e = map.lookup("main").unwrap();
        assert_eq!(e.account_id.as_str(), "370025973162");
        assert_eq!(e.region.as_ref().map(Region::as_str), Some("us-west-2"));
    }

    #[test]
    fn test_should_load_aws_config_role_arn_extracts_account_id() {
        let f = write_tmp(
            r"
[profile cross]
role_arn = arn:aws:iam::123456789012:role/admin
region   = eu-west-1
",
        );
        let map = load_aws_config(f.path()).unwrap();
        let e = map.lookup("cross").unwrap();
        assert_eq!(e.account_id.as_str(), "123456789012");
        assert_eq!(
            e.role_arn.as_deref(),
            Some("arn:aws:iam::123456789012:role/admin")
        );
    }

    #[test]
    fn test_should_follow_source_profile_chain() {
        let f = write_tmp(
            r"
[profile root]
sso_account_id = 111111111111
region = us-east-1

[profile mid]
source_profile = root

[profile leaf]
source_profile = mid
",
        );
        let map = load_aws_config(f.path()).unwrap();
        let leaf = map.lookup("leaf").unwrap();
        assert_eq!(leaf.account_id.as_str(), "111111111111");
        // Region cascades too.
        assert_eq!(leaf.region.as_ref().map(Region::as_str), Some("us-east-1"));
    }

    #[test]
    fn test_should_reject_aws_config_chain_cycle() {
        let f = write_tmp(
            r"
[profile a]
source_profile = b

[profile b]
source_profile = a
",
        );
        let err = load_aws_config(f.path()).unwrap_err();
        assert!(
            matches!(err, ProviderError::ChainTooLong { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn test_should_load_aws_config_default_section() {
        let f = write_tmp(
            r"
[default]
sso_account_id = 222222222222
region = us-west-2
",
        );
        let map = load_aws_config(f.path()).unwrap();
        let e = map.lookup("default").unwrap();
        assert_eq!(e.account_id.as_str(), "222222222222");
    }

    #[test]
    fn test_aws_config_skips_profiles_without_account_signals() {
        // A profile with no `sso_account_id` / `role_arn` / chain into one
        // is silently dropped — we have nothing to map it to.
        let f = write_tmp(
            r"
[profile bare]
region = us-east-1
",
        );
        let map = load_aws_config(f.path()).unwrap();
        assert!(map.lookup("bare").is_none());
    }

    // -- ArcSwap wiring -------------------------------------------------

    #[test]
    fn test_shared_profile_map_swap_replaces_atomically() {
        let initial = empty();
        let shared = shared(Arc::clone(&initial));
        // Replace with a non-empty map and verify the swap is visible.
        let f = write_tmp(
            r#"
profiles:
  p:
    account_id: "100000000001"
    account_name: "x"
"#,
        );
        let next = load_yaml_profile_map(f.path()).unwrap();
        shared.store(next);
        let loaded = shared.load();
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn test_static_regexes_compile() {
        assert!(ACCOUNT_ID_RE.is_match("123456789012"));
        assert!(!ACCOUNT_ID_RE.is_match("12345"));
        assert!(NAME_RE.is_match("primary"));
        assert!(!NAME_RE.is_match(""));
    }
}
