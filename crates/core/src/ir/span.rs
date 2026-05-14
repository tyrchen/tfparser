//! Source-position metadata attached to every IR node that originated in a
//! parsed file.
//!
//! Per [10-data-model.md § 2.4]: `file` is an `Arc<Path>` (shared across all
//! spans pointing into the same source); `byte_range` is `Range<u32>` to keep
//! the on-disk footprint small at reference scale (4 GB per file is more than
//! enough); `line` / `column` are 1-based.
//!
//! [10-data-model.md § 2.4]: ../../specs/10-data-model.md
//!
//! ## Invariants
//!
//! - `byte_range.start <= byte_range.end`. Enforced by [`Span::new`]; direct field access bypasses
//!   the check (the field is private).
//! - `line >= 1`, `column >= 1`. Zero values are nonsensical for 1-based positions and would mask
//!   bugs downstream; rejected by the constructor.

use std::{ops::Range, path::Path, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::error::ValidationError;

/// Byte-offset + line/column span into a source file.
///
/// Spans are cheap to clone (`Arc<Path>` is a single ref-count bump). Per
/// [10-data-model.md § 2.5 (I-IR-1)], every `Span` in the workspace IR
/// resolves to a path *underneath* the workspace root — discovery enforces
/// this; `Span` itself does not validate the path (it accepts already-trusted
/// values from the loader).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct Span {
    /// Path of the source file the span points into.
    #[serde(with = "crate::ir::path_serde::arc_path")]
    pub file: Arc<Path>,

    /// Half-open byte range `[start, end)` into the file's contents.
    pub byte_range: Range<u32>,

    /// 1-based line number of `byte_range.start`.
    pub line: u32,

    /// 1-based column (in bytes) of `byte_range.start`.
    pub column: u32,
}

impl Span {
    /// Construct a span, validating that `byte_range` is well-ordered and
    /// `line`/`column` are 1-based.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::Shape`] if any invariant is violated.
    pub fn new(
        file: Arc<Path>,
        byte_range: Range<u32>,
        line: u32,
        column: u32,
    ) -> Result<Self, ValidationError> {
        if byte_range.start > byte_range.end {
            return Err(ValidationError::Shape {
                field: "Span.byte_range",
                rule: "start-after-end",
            });
        }
        if line == 0 {
            return Err(ValidationError::Shape {
                field: "Span.line",
                rule: "must-be-1-based",
            });
        }
        if column == 0 {
            return Err(ValidationError::Shape {
                field: "Span.column",
                rule: "must-be-1-based",
            });
        }
        Ok(Self {
            file,
            byte_range,
            line,
            column,
        })
    }

    /// A synthetic 1-byte span used for IR nodes that did not originate in a
    /// file (e.g. defaults injected by the evaluator). Carries an empty path.
    #[must_use]
    pub fn synthetic() -> Self {
        Self {
            file: Arc::from(Path::new("")),
            byte_range: 0..0,
            line: 1,
            column: 1,
        }
    }

    /// Convenience accessor for the byte range as a `usize` slice into a
    /// source buffer.
    #[must_use]
    pub fn byte_range_usize(&self) -> Range<usize> {
        self.byte_range.start as usize..self.byte_range.end as usize
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::reversed_empty_ranges
)]
mod tests {
    use super::*;

    fn p(s: &str) -> Arc<Path> {
        Arc::from(Path::new(s))
    }

    fn must_err(r: Result<Span, ValidationError>) -> ValidationError {
        match r {
            Ok(s) => panic!("expected Err, got Ok({s:?})"),
            Err(e) => e,
        }
    }

    #[test]
    fn test_should_construct_span_with_valid_inputs() {
        let s = Span::new(p("/tmp/x.tf"), 10..20, 3, 5).unwrap();
        assert_eq!(s.byte_range, 10..20);
        assert_eq!(s.line, 3);
        assert_eq!(s.column, 5);
        assert_eq!(s.byte_range_usize(), 10usize..20usize);
    }

    #[test]
    fn test_should_reject_reversed_byte_range() {
        // Constructed manually so clippy doesn't flag the literal as
        // `reversed_empty_ranges` — the IR allows the field type to express
        // any range; we want to assert the constructor rejects it.
        let range = Range::<u32> { start: 30, end: 10 };
        let err = must_err(Span::new(p("/tmp/x.tf"), range, 1, 1));
        assert!(matches!(
            err,
            ValidationError::Shape {
                rule: "start-after-end",
                ..
            }
        ));
    }

    #[test]
    fn test_should_reject_zero_line() {
        let err = must_err(Span::new(p("/tmp/x.tf"), 0..1, 0, 1));
        assert!(matches!(
            err,
            ValidationError::Shape {
                field: "Span.line",
                ..
            }
        ));
    }

    #[test]
    fn test_should_reject_zero_column() {
        let err = must_err(Span::new(p("/tmp/x.tf"), 0..1, 1, 0));
        assert!(matches!(
            err,
            ValidationError::Shape {
                field: "Span.column",
                ..
            }
        ));
    }

    #[test]
    fn test_should_serde_round_trip_span() {
        let s = Span::new(p("foo/bar.tf"), 5..15, 2, 4).unwrap();
        let json = serde_json::to_string(&s).unwrap();
        let back: Span = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn test_should_serialize_span_in_camel_case() {
        let s = Span::new(p("a.tf"), 0..1, 1, 1).unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("byteRange"), "got: {json}");
    }

    #[test]
    fn test_should_serde_round_trip_synthetic_span() {
        let s = Span::synthetic();
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains("\"file\":\"\""),
            "expected empty file: {json}"
        );
        assert!(json.contains("\"line\":1"), "{json}");
        let back: Span = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
