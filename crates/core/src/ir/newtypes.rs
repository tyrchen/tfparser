//! Validated newtypes for IR boundary values.
//!
//! Per CLAUDE.md § Input Validation — "Newtype every domain primitive":
//! every value crossing a trust boundary is wrapped in a type whose private
//! constructor enforces the invariants. Downstream code never re-validates;
//! the type system guarantees the shape.
//!
//! Three boundary newtypes live here:
//!
//! - [`Address`]: a Terraform-style resource address. Validated against the charset and length cap
//!   pinned in [70-security.md § 4].
//! - [`AccountId`]: a 12-digit AWS account id.
//! - [`Region`]: an AWS region name (e.g. `us-west-2`).
//!
//! [70-security.md § 4]: ../../specs/70-security.md

use std::{borrow::Borrow, fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::error::ValidationError;

// ----------------------------------------------------------------------------
// Address
// ----------------------------------------------------------------------------

/// Maximum byte length for any [`Address`]. Per [70-security.md § 4].
pub const ADDRESS_MAX_BYTES: usize = 1024;

/// Terraform-style resource address.
///
/// Examples: `aws_db_instance.x`, `module.pacer_db.aws_db_instance.this`,
/// `module.foo.module.bar.aws_iam_role.r[0]`.
///
/// # Validation
///
/// Each candidate must:
///
/// - be non-empty;
/// - be at most [`ADDRESS_MAX_BYTES`] bytes;
/// - contain only characters in the allowlist `A-Z`, `a-z`, `0-9`, `_`, `.`, `/`, `-`, `[`, `]`,
///   `"`;
/// - have balanced `[]` and `"`.
///
/// Use [`Address::new`] or `TryFrom<&str>` / `FromStr` to construct.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Address(Box<str>);

impl Address {
    /// Construct an [`Address`], running every validation rule.
    ///
    /// # Errors
    ///
    /// Returns a [`ValidationError`] variant naming the rule that failed.
    pub fn new(s: impl AsRef<str>) -> Result<Self, ValidationError> {
        let s = s.as_ref();
        if s.is_empty() {
            return Err(ValidationError::Empty { field: "Address" });
        }
        if s.len() > ADDRESS_MAX_BYTES {
            return Err(ValidationError::TooLong {
                field: "Address",
                observed: s.len(),
                limit: ADDRESS_MAX_BYTES,
            });
        }
        // Charset allowlist. Validating byte-by-byte (ASCII-safe; the
        // allowed set is entirely ASCII so every disallowed Unicode byte
        // falls naturally into the BadChar branch).
        for (offset, &byte) in s.as_bytes().iter().enumerate() {
            if !is_address_byte(byte) {
                return Err(ValidationError::BadChar {
                    field: "Address",
                    byte,
                    offset,
                });
            }
        }
        if !brackets_and_quotes_balanced(s.as_bytes()) {
            return Err(ValidationError::Shape {
                field: "Address",
                rule: "unbalanced-brackets-or-quotes",
            });
        }
        Ok(Self(Box::from(s)))
    }

    /// Borrow the address as `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Iterate the module name segments of this address, in order.
    ///
    /// Examples:
    ///
    /// - `aws_iam_role.r` → empty.
    /// - `module.pacer_db.aws_db_instance.this` → `["pacer_db"]`.
    /// - `module.outer.module.inner.aws_iam_role.r` → `["outer", "inner"]`.
    #[must_use]
    pub fn module_segments(&self) -> ModuleSegments<'_> {
        ModuleSegments { rest: &self.0 }
    }

    /// Module-path prefix of this address, dot-joined.
    ///
    /// `""` for top-level resources, `"pacer_db"` for one-level, `"outer.inner"` for nested.
    /// This mirrors the [`module_path` Parquet column shape pinned in
    /// 10-data-model.md § 3](../../specs/10-data-model.md).
    #[must_use]
    pub fn module_path(&self) -> String {
        let mut out = String::new();
        for seg in self.module_segments() {
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(seg);
        }
        out
    }

    /// The non-module suffix of this address (e.g. `"aws_db_instance.this"`).
    ///
    /// Returns `""` if the address is malformed (every leading `module.X` segment
    /// consumed but nothing left). Such input cannot reach this code path because
    /// the constructor rejects it via the charset / balance check.
    #[must_use]
    pub fn resource_part(&self) -> &str {
        let mut iter = self.module_segments();
        // Drain the iterator to advance `rest` past the module prefix.
        while iter.next().is_some() {}
        iter.rest
    }
}

/// Iterator over the module name segments of an [`Address`].
#[derive(Clone, Debug)]
pub struct ModuleSegments<'a> {
    rest: &'a str,
}

impl<'a> Iterator for ModuleSegments<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        const MODULE: &str = "module.";
        let rest = self.rest.strip_prefix(MODULE)?;
        let dot = rest.find('.')?;
        let (name, after_dot) = (rest.get(..dot)?, rest.get(dot + 1..)?);
        self.rest = after_dot;
        Some(name)
    }
}

impl AsRef<str> for Address {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for Address {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Address {
    type Err = ValidationError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<&str> for Address {
    type Error = ValidationError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl TryFrom<String> for Address {
    type Error = ValidationError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<Address> for String {
    fn from(a: Address) -> Self {
        a.0.into_string()
    }
}

const fn is_address_byte(b: u8) -> bool {
    matches!(b,
        b'A'..=b'Z'
        | b'a'..=b'z'
        | b'0'..=b'9'
        | b'_'
        | b'.'
        | b'/'
        | b'-'
        | b'['
        | b']'
        | b'"'
    )
}

fn brackets_and_quotes_balanced(bytes: &[u8]) -> bool {
    let mut bracket_depth: i32 = 0;
    let mut in_quotes = false;
    for &byte in bytes {
        match byte {
            b'[' if !in_quotes => bracket_depth += 1,
            b']' if !in_quotes => {
                bracket_depth -= 1;
                if bracket_depth < 0 {
                    return false;
                }
            }
            b'"' => in_quotes = !in_quotes,
            _ => {}
        }
    }
    bracket_depth == 0 && !in_quotes
}

// ----------------------------------------------------------------------------
// AccountId
// ----------------------------------------------------------------------------

/// AWS account id — exactly 12 ASCII digits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AccountId(Box<str>);

impl AccountId {
    /// Construct an [`AccountId`].
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::Shape`] for any length other than 12 or for
    /// non-digit characters.
    pub fn new(s: impl AsRef<str>) -> Result<Self, ValidationError> {
        let s = s.as_ref();
        if s.len() != 12 {
            return Err(ValidationError::Shape {
                field: "AccountId",
                rule: "must-be-12-digits",
            });
        }
        if !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ValidationError::Shape {
                field: "AccountId",
                rule: "must-be-ascii-digits",
            });
        }
        Ok(Self(Box::from(s)))
    }

    /// Borrow as `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for AccountId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for AccountId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AccountId {
    type Err = ValidationError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<&str> for AccountId {
    type Error = ValidationError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl TryFrom<String> for AccountId {
    type Error = ValidationError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<AccountId> for String {
    fn from(a: AccountId) -> Self {
        a.0.into_string()
    }
}

// ----------------------------------------------------------------------------
// Region
// ----------------------------------------------------------------------------

/// Maximum byte length for any [`Region`]. Per [70-security.md § 4].
pub const REGION_MAX_BYTES: usize = 32;

/// AWS region — matches `^[a-z0-9-]{1,32}$`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Region(Box<str>);

impl Region {
    /// Construct a [`Region`].
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] for empty input, over-long input, or any
    /// disallowed character.
    pub fn new(s: impl AsRef<str>) -> Result<Self, ValidationError> {
        let s = s.as_ref();
        if s.is_empty() {
            return Err(ValidationError::Empty { field: "Region" });
        }
        if s.len() > REGION_MAX_BYTES {
            return Err(ValidationError::TooLong {
                field: "Region",
                observed: s.len(),
                limit: REGION_MAX_BYTES,
            });
        }
        for (offset, &byte) in s.as_bytes().iter().enumerate() {
            if !matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-') {
                return Err(ValidationError::BadChar {
                    field: "Region",
                    byte,
                    offset,
                });
            }
        }
        Ok(Self(Box::from(s)))
    }

    /// Borrow as `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Region {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for Region {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Region {
    type Err = ValidationError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<&str> for Region {
    type Error = ValidationError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl TryFrom<String> for Region {
    type Error = ValidationError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<Region> for String {
    fn from(r: Region) -> Self {
        r.0.into_string()
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::needless_raw_string_hashes
)]
mod tests {
    use super::*;

    // -- Address ---------------------------------------------------------

    #[test]
    fn test_should_accept_simple_resource_address() {
        let a = Address::new("aws_iam_role.r").unwrap();
        assert_eq!(a.as_str(), "aws_iam_role.r");
        assert_eq!(a.module_segments().count(), 0);
        assert_eq!(a.resource_part(), "aws_iam_role.r");
    }

    #[test]
    fn test_should_accept_module_prefixed_address() {
        let a = Address::new("module.pacer_db.aws_db_instance.this").unwrap();
        let segs: Vec<&str> = a.module_segments().collect();
        assert_eq!(segs, vec!["pacer_db"]);
        assert_eq!(a.resource_part(), "aws_db_instance.this");
    }

    #[test]
    fn test_should_accept_nested_module_address() {
        let a = Address::new("module.outer.module.inner.aws_iam_role.r").unwrap();
        let segs: Vec<&str> = a.module_segments().collect();
        assert_eq!(segs, vec!["outer", "inner"]);
        assert_eq!(a.resource_part(), "aws_iam_role.r");
    }

    #[test]
    fn test_should_accept_indexed_address() {
        let a = Address::new(r#"aws_subnet.s[0]"#).unwrap();
        assert_eq!(a.as_str(), "aws_subnet.s[0]");
    }

    #[test]
    fn test_should_accept_string_indexed_address() {
        let a = Address::new(r#"aws_route53_record.rec["api.example.com"]"#).unwrap();
        assert!(a.as_str().contains(r#"["api.example.com"]"#));
    }

    #[test]
    fn test_should_reject_empty_address() {
        let err = Address::new("").unwrap_err();
        assert!(matches!(err, ValidationError::Empty { field: "Address" }));
    }

    #[test]
    fn test_should_reject_overlong_address() {
        let long = "a".repeat(ADDRESS_MAX_BYTES + 1);
        let err = Address::new(long).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::TooLong {
                field: "Address",
                ..
            }
        ));
    }

    #[test]
    fn test_should_reject_space_in_address() {
        let err = Address::new("aws_iam_role with_space").unwrap_err();
        assert!(matches!(err, ValidationError::BadChar { byte: b' ', .. }));
    }

    #[test]
    fn test_should_reject_nul_byte_in_address() {
        let err = Address::new("aws_iam_role\0bad").unwrap_err();
        assert!(matches!(err, ValidationError::BadChar { byte: 0, .. }));
    }

    #[test]
    fn test_should_reject_unicode_outside_allowlist() {
        let err = Address::new("aws_iam_role.café").unwrap_err();
        assert!(matches!(err, ValidationError::BadChar { .. }));
    }

    #[test]
    fn test_should_reject_unbalanced_brackets() {
        let err = Address::new("aws_subnet.s[").unwrap_err();
        assert!(matches!(
            err,
            ValidationError::Shape {
                rule: "unbalanced-brackets-or-quotes",
                ..
            }
        ));
        let err2 = Address::new("aws_subnet.s]").unwrap_err();
        assert!(matches!(err2, ValidationError::Shape { .. }));
    }

    #[test]
    fn test_should_reject_unbalanced_quotes() {
        let err = Address::new(r#"aws_subnet.s["abc]"#).unwrap_err();
        assert!(matches!(err, ValidationError::Shape { .. }));
    }

    #[test]
    fn test_should_accept_brackets_inside_quotes() {
        // A real Terraform address can have `[` and `]` inside the quoted
        // key. Bracket counting must respect quote state.
        let a = Address::new(r#"r["foo[bar]baz"]"#).unwrap();
        assert!(a.as_str().contains("[bar]"));
    }

    #[test]
    fn test_should_accept_nested_bracket_groups() {
        let a = Address::new("a[0][1]").unwrap();
        assert_eq!(a.as_str(), "a[0][1]");
    }

    #[test]
    fn test_should_produce_dotted_module_path() {
        let a = Address::new("module.outer.module.inner.aws_iam_role.r").unwrap();
        assert_eq!(a.module_path(), "outer.inner");
        let b = Address::new("aws_iam_role.r").unwrap();
        assert_eq!(b.module_path(), "");
    }

    #[test]
    fn test_should_round_trip_address_via_fromstr() {
        let a: Address = "module.x.aws_iam_role.r".parse().unwrap();
        assert_eq!(a.as_str(), "module.x.aws_iam_role.r");
    }

    #[test]
    fn test_should_serde_round_trip_address() {
        let a = Address::new("module.x.aws_iam_role.r").unwrap();
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, "\"module.x.aws_iam_role.r\"");
        let back: Address = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn test_should_reject_invalid_serde_input() {
        let r: Result<Address, _> = serde_json::from_str("\"with space\"");
        assert!(r.is_err());
    }

    // -- AccountId -------------------------------------------------------

    #[test]
    fn test_should_accept_12_digit_account_id() {
        let a = AccountId::new("100000000001").unwrap();
        assert_eq!(a.as_str(), "100000000001");
    }

    #[test]
    fn test_should_reject_account_id_length_mismatch() {
        for bad in ["", "1", "12345", "0000000000001"] {
            let err = AccountId::new(bad).unwrap_err();
            assert!(matches!(
                err,
                ValidationError::Shape {
                    rule: "must-be-12-digits",
                    ..
                }
            ));
        }
    }

    #[test]
    fn test_should_reject_account_id_with_letters() {
        let err = AccountId::new("0000000000ab").unwrap_err();
        assert!(matches!(
            err,
            ValidationError::Shape {
                rule: "must-be-ascii-digits",
                ..
            }
        ));
    }

    #[test]
    fn test_should_reject_account_id_with_unicode_digits() {
        // U+0660 ARABIC-INDIC DIGIT ZERO — not ASCII
        let err = AccountId::new("00000000000٠").unwrap_err();
        assert!(matches!(err, ValidationError::Shape { .. }));
    }

    #[test]
    fn test_should_serde_round_trip_account_id() {
        let a = AccountId::new("370025973162").unwrap();
        let json = serde_json::to_string(&a).unwrap();
        let back: AccountId = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    // -- Region ----------------------------------------------------------

    #[test]
    fn test_should_accept_valid_region() {
        let r = Region::new("us-west-2").unwrap();
        assert_eq!(r.as_str(), "us-west-2");
    }

    #[test]
    fn test_should_reject_empty_region() {
        let err = Region::new("").unwrap_err();
        assert!(matches!(err, ValidationError::Empty { .. }));
    }

    #[test]
    fn test_should_reject_overlong_region() {
        let s = "a".repeat(REGION_MAX_BYTES + 1);
        let err = Region::new(s).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::TooLong {
                field: "Region",
                ..
            }
        ));
    }

    #[test]
    fn test_should_reject_uppercase_region() {
        let err = Region::new("US-WEST-2").unwrap_err();
        assert!(matches!(err, ValidationError::BadChar { .. }));
    }

    #[test]
    fn test_should_reject_underscore_in_region() {
        let err = Region::new("us_west_2").unwrap_err();
        assert!(matches!(err, ValidationError::BadChar { .. }));
    }

    #[test]
    fn test_should_serde_round_trip_region() {
        let r = Region::new("ap-southeast-1").unwrap();
        let json = serde_json::to_string(&r).unwrap();
        let back: Region = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
