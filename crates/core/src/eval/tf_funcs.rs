//! Terraform-only functions implemented for Phase 4.
//!
//! Per [13-evaluator.md § 5], Terraform ships builtins beyond the HCL stdlib.
//! Phase 4 implements the ones that:
//!
//! - the M1 exit criteria reach (`get_env`, `strcontains`, `formatdate`, `timestamp`), and
//! - have safe, deterministic implementations using the dependencies already in the workspace
//!   (`sha256`/`sha512` via `sha2`, no new crates).
//!
//! Functions intentionally **not** shipped in Phase 4:
//!
//! - `md5`, `sha1` — broken hash functions per CLAUDE.md § Cryptography. Real-world resource
//!   attributes rarely contain literal `md5("…")` calls; on the rare occasion they do, the call
//!   survives as [`crate::ir::Expression::FuncCall`], which is the correct best-effort outcome per
//!   [99-key-decisions.md] D4.
//! - `bcrypt`, `uuid` — non-deterministic / cryptographic.
//! - `base64gzip` — requires flate2 for a single function that is almost always wrapped around a
//!   `templatefile(...)` (Unresolved anyway).
//! - `urlencode` — rare in resource attributes.
//!
//! Each deferred function is recorded as a tracked outcome in
//! [93-improvements-review.md] and may be picked up in Phase 9 hardening if
//! a real fixture forces the issue.
//!
//! [13-evaluator.md § 5]: ../../../specs/13-evaluator.md
//! [99-key-decisions.md]: ../../../specs/99-key-decisions.md
//! [93-improvements-review.md]: ../../../specs/93-improvements-review.md

use std::sync::Arc;

use jiff::{Timestamp, Zoned, civil::Date, fmt::strtime, tz::TimeZone};
use sha2::{Digest, Sha256, Sha512};

use crate::{
    diagnostic::LimitKind,
    eval::{
        registry::{CallCx, FuncError, FuncRegistryBuilder, HclFunc},
        stdlib::type_name,
    },
    ir::Value,
};

/// Register the Phase 4 Terraform-only functions into `b`.
pub fn register(b: &mut FuncRegistryBuilder) {
    b.register("sha256", Arc::new(Sha256Fn));
    b.register("sha512", Arc::new(Sha512Fn));
    b.register("formatdate", Arc::new(FormatdateFn));
    b.register("timestamp", Arc::new(TimestampFn));
    b.register("strcontains", Arc::new(StrcontainsFn));
    b.register("get_env", Arc::new(GetEnvFn));
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

fn check_str_size(name: &'static str, s: &str, cx: &CallCx<'_>) -> Result<(), FuncError> {
    let observed = u64::try_from(s.len()).unwrap_or(u64::MAX);
    let limit = u64::from(cx.limits.max_str_size);
    if observed > limit {
        Err(FuncError::Limit {
            name: Arc::from(name),
            kind: LimitKind::StringSize,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

#[derive(Debug)]
struct Sha256Fn;
impl HclFunc for Sha256Fn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let s = require_str("sha256", args, 0)?;
        let digest = Sha256::digest(s.as_bytes());
        let hex = bytes_to_hex(&digest);
        check_str_size("sha256", &hex, cx)?;
        Ok(Value::Str(Arc::from(hex)))
    }
}

#[derive(Debug)]
struct Sha512Fn;
impl HclFunc for Sha512Fn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let s = require_str("sha512", args, 0)?;
        let digest = Sha512::digest(s.as_bytes());
        let hex = bytes_to_hex(&digest);
        check_str_size("sha512", &hex, cx)?;
        Ok(Value::Str(Arc::from(hex)))
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[derive(Debug)]
struct FormatdateFn;
impl HclFunc for FormatdateFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let spec = require_str("formatdate", args, 0)?;
        let raw = require_str("formatdate", args, 1)?;
        let ts = parse_timestamp("formatdate", raw)?;
        // Terraform-style format spec subset: YYYY, MM, DD, hh, mm, ss.
        // Anything else stays verbatim. Conversion is "find-and-replace"
        // rather than strtime because Terraform's letters do not match
        // strftime tokens.
        let zoned = ts.to_zoned(TimeZone::UTC);
        let out = render_formatdate(spec, &zoned);
        check_str_size("formatdate", &out, cx)?;
        Ok(Value::Str(Arc::from(out)))
    }
}

fn render_formatdate(spec: &str, zoned: &Zoned) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(spec.len());
    let bytes = spec.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = bytes.get(i..).unwrap_or_default();
        if rest.starts_with(b"YYYY") {
            let _ = write!(&mut out, "{:04}", zoned.year());
            i += 4;
        } else if rest.starts_with(b"MM") {
            let _ = write!(&mut out, "{:02}", zoned.month());
            i += 2;
        } else if rest.starts_with(b"DD") {
            let _ = write!(&mut out, "{:02}", zoned.day());
            i += 2;
        } else if rest.starts_with(b"hh") {
            let _ = write!(&mut out, "{:02}", zoned.hour());
            i += 2;
        } else if rest.starts_with(b"mm") {
            let _ = write!(&mut out, "{:02}", zoned.minute());
            i += 2;
        } else if rest.starts_with(b"ss") {
            let _ = write!(&mut out, "{:02}", zoned.second());
            i += 2;
        } else {
            // Step a single UTF-8 code point so multi-byte chars stay
            // intact in the output.
            match spec.get(i..).and_then(|s| s.chars().next()) {
                Some(ch) => {
                    out.push(ch);
                    i += ch.len_utf8();
                }
                None => break,
            }
        }
    }
    out
}

fn parse_timestamp(name: &'static str, s: &str) -> Result<Timestamp, FuncError> {
    if let Ok(ts) = s.parse::<Timestamp>() {
        return Ok(ts);
    }
    if let Ok(z) = s.parse::<Zoned>() {
        return Ok(z.timestamp());
    }
    if let Ok(d) = s.parse::<Date>()
        && let Ok(z) = d.to_zoned(TimeZone::UTC)
    {
        return Ok(z.timestamp());
    }
    if let Ok(t) = strtime::parse("%Y-%m-%dT%H:%M:%S%z", s)
        .and_then(|p| p.to_zoned())
        .map(|z| z.timestamp())
    {
        return Ok(t);
    }
    Err(FuncError::Other {
        name: Arc::from(name),
        message: Arc::from(format!("cannot parse timestamp `{s}`")),
    })
}

#[derive(Debug)]
struct TimestampFn;
impl HclFunc for TimestampFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        // Terraform's `timestamp()` returns the current UTC time in RFC3339.
        // The parser is *source-only* and best-effort — embedding the
        // current wall clock would make output non-deterministic and burn
        // golden snapshots. So we leave the call symbolic: surface a Func
        // error so the call site keeps the unresolved expression.
        let _ = args;
        let _ = cx;
        Err(FuncError::Other {
            name: Arc::from("timestamp"),
            message: Arc::from(
                "non-deterministic: source-only parser does not embed wall-clock time",
            ),
        })
    }
}

#[derive(Debug)]
struct StrcontainsFn;
impl HclFunc for StrcontainsFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let haystack = require_str("strcontains", args, 0)?;
        let needle = require_str("strcontains", args, 1)?;
        Ok(Value::Bool(haystack.contains(needle)))
    }
}

/// Source-only `get_env`. Reads the process environment **only** when the
/// configured [`EnvVarMode`](super::context::EnvVarMode) allows the name;
/// otherwise returns the supplied default or `""`. Never leaks `HOME`,
/// `AWS_SECRET_*`, etc. (invariant I-EVAL-3, [13-evaluator.md § 7]).
///
/// [13-evaluator.md § 7]: ../../../specs/13-evaluator.md
#[derive(Debug)]
struct GetEnvFn;
impl HclFunc for GetEnvFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let name = require_str("get_env", args, 0)?;
        let default = match args.get(1) {
            None | Some(Value::Null) => "",
            Some(Value::Str(s)) => s.as_ref(),
            Some(other) => {
                return Err(FuncError::Type {
                    name: Arc::from("get_env"),
                    index: 1,
                    expected: "string",
                    got: type_name(other),
                });
            }
        };

        if cx.env_vars.is_mock() {
            return Ok(Value::Str(Arc::from(default)));
        }
        if !cx.env_vars.allows(name) {
            return Ok(Value::Str(Arc::from(default)));
        }
        Ok(Value::Str(Arc::from(
            std::env::var(name).unwrap_or_else(|_| default.into()),
        )))
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
    use std::{collections::BTreeSet, path::Path, sync::Arc};

    use super::*;
    use crate::eval::{
        context::{EnvVarMode, EvalLimits},
        registry::FuncRegistry,
    };

    fn registry() -> FuncRegistry {
        let mut b = FuncRegistry::builder();
        register(&mut b);
        b.build()
    }

    fn call(name: &str, args: &[Value], env: &EnvVarMode) -> Result<Value, FuncError> {
        let limits = EvalLimits::default();
        let cx = CallCx {
            workspace_root: Path::new("/tmp/repo"),
            env_vars: env,
            limits: &limits,
        };
        registry()
            .get(name)
            .unwrap_or_else(|| panic!("function `{name}` not registered"))
            .call(args, &cx)
    }

    #[test]
    fn test_sha256_hex_lowercase() {
        let out = call(
            "sha256",
            &[Value::Str(Arc::from("hello"))],
            &EnvVarMode::default(),
        )
        .unwrap();
        assert_eq!(
            out,
            Value::Str(Arc::from(
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
            ))
        );
    }

    #[test]
    fn test_sha512_hex_lowercase() {
        let out = call(
            "sha512",
            &[Value::Str(Arc::from("hello"))],
            &EnvVarMode::default(),
        )
        .unwrap();
        let Value::Str(s) = out else {
            panic!("expected string");
        };
        assert_eq!(s.len(), 128);
        assert!(
            s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
    }

    #[test]
    fn test_formatdate_renders_yyyy_mm_dd() {
        let out = call(
            "formatdate",
            &[
                Value::Str(Arc::from("YYYY-MM-DD hh:mm:ss")),
                Value::Str(Arc::from("2026-05-13T10:20:30Z")),
            ],
            &EnvVarMode::default(),
        )
        .unwrap();
        assert_eq!(out, Value::Str(Arc::from("2026-05-13 10:20:30")));
    }

    #[test]
    fn test_formatdate_passthrough_literal() {
        let out = call(
            "formatdate",
            &[
                Value::Str(Arc::from("hello YYYY world")),
                Value::Str(Arc::from("2026-05-13T00:00:00Z")),
            ],
            &EnvVarMode::default(),
        )
        .unwrap();
        assert_eq!(out, Value::Str(Arc::from("hello 2026 world")));
    }

    #[test]
    fn test_timestamp_is_unresolved() {
        let err = call("timestamp", &[], &EnvVarMode::default()).unwrap_err();
        assert!(matches!(err, FuncError::Other { .. }));
    }

    #[test]
    fn test_strcontains_substring() {
        assert_eq!(
            call(
                "strcontains",
                &[
                    Value::Str(Arc::from("hello world")),
                    Value::Str(Arc::from("world")),
                ],
                &EnvVarMode::default(),
            )
            .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call(
                "strcontains",
                &[Value::Str(Arc::from("hello")), Value::Str(Arc::from("x")),],
                &EnvVarMode::default(),
            )
            .unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_get_env_strict_returns_default_when_not_allowed() {
        let env = EnvVarMode::Strict {
            allowed: BTreeSet::new(),
        };
        let out = call(
            "get_env",
            &[
                Value::Str(Arc::from("HOME")),
                Value::Str(Arc::from("default-x")),
            ],
            &env,
        )
        .unwrap();
        assert_eq!(out, Value::Str(Arc::from("default-x")));
    }

    #[test]
    fn test_get_env_strict_returns_real_value_when_allowed() {
        // SAFETY: integration tests do not depend on this env var; pin it
        // explicitly so the run is deterministic. The set is process-wide
        // but the test runs serially per cargo's per-test process model.
        // SAFETY (unsafe): mutating process env is `unsafe` in Rust 2024
        // because other threads may race; this test has no other threads
        // touching `TF_PARSER_TEST_VAR` so the call is sound.
        // We must not use `#![forbid(unsafe_code)]` here — the crate
        // forbids `unsafe`. Instead we avoid mutating the env entirely:
        // use a known-stable variable.
        let name = "PATH";
        let mut allowed: BTreeSet<Arc<str>> = BTreeSet::new();
        allowed.insert(Arc::from(name));
        let env = EnvVarMode::Strict { allowed };
        let real = std::env::var(name).unwrap_or_default();
        let out = call(
            "get_env",
            &[
                Value::Str(Arc::from(name)),
                Value::Str(Arc::from("fallback")),
            ],
            &env,
        )
        .unwrap();
        assert_eq!(out, Value::Str(Arc::from(real)));
    }

    #[test]
    fn test_get_env_mock_always_returns_default() {
        let env = EnvVarMode::Mock;
        let out = call(
            "get_env",
            &[Value::Str(Arc::from("HOME")), Value::Str(Arc::from("x"))],
            &env,
        )
        .unwrap();
        assert_eq!(out, Value::Str(Arc::from("x")));
    }

    #[test]
    fn test_get_env_passthrough_reads_real_env() {
        let env = EnvVarMode::Passthrough;
        let real = std::env::var("PATH").unwrap_or_default();
        let out = call(
            "get_env",
            &[
                Value::Str(Arc::from("PATH")),
                Value::Str(Arc::from("fallback")),
            ],
            &env,
        )
        .unwrap();
        assert_eq!(out, Value::Str(Arc::from(real)));
    }
}
