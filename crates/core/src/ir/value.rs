//! Resolved values: the result of evaluating an [`Expression`] against an
//! evaluator context.
//!
//! [`Expression`]: crate::ir::Expression
//!
//! Per [10-data-model.md § 2.3], we keep `Number` and `Int` as separate
//! variants for the integer-literal fast path; `Number` is the upstream
//! HCL2 representation. The two are *not* numerically interconvertible at
//! the IR level — round-tripping a literal preserves its source form.
//!
//! `Map` is an *ordered* `Vec<(Arc<str>, Value)>` rather than a `HashMap`:
//! insertion order is semantically meaningful for the canonical JSON we
//! emit ([20-parquet-exporter.md § 3.3]).
//!
//! [10-data-model.md § 2.3]: ../../specs/10-data-model.md
//! [20-parquet-exporter.md § 3.3]: ../../specs/20-parquet-exporter.md

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Insertion-ordered map of string key → [`Value`].
pub type Map = Vec<(Arc<str>, Value)>;

/// A fully-resolved HCL value.
///
/// `Value` is what a resolved [`Expression`] reduces to. Anything that
/// cannot resolve statically stays as [`Expression::Unresolved`].
///
/// # Equality
///
/// `Value` is **`PartialEq` but not `Eq` or `Hash`** because [`Value::Number`]
/// wraps an `f64` and `f64::NAN != f64::NAN`. Treat `Value` as a value type,
/// not as a map / set key. If you need a hashable key, hash the canonical
/// JSON form ([20-parquet-exporter.md § 3.3](../../specs/20-parquet-exporter.md))
/// or wrap the number in your own total-order container.
///
/// [`Expression`]: crate::ir::Expression
/// [`Expression::Unresolved`]: crate::ir::Expression::Unresolved
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
#[non_exhaustive]
pub enum Value {
    /// HCL `null`.
    Null,

    /// Boolean.
    Bool(bool),

    /// Integer literal (fast path; preserved verbatim from the source).
    Int(i64),

    /// Floating-point number — the default HCL2 numeric representation.
    Number(f64),

    /// UTF-8 string (interned-friendly via `Arc<str>`).
    Str(Arc<str>),

    /// Heterogeneous list.
    List(Vec<Value>),

    /// Insertion-ordered map.
    Map(Map),
}

impl Value {
    /// Returns the contained string slice, if `self` is a [`Value::Str`].
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the contained boolean, if `self` is a [`Value::Bool`].
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Self::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// Returns the contained integer, if `self` is a [`Value::Int`].
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match *self {
            Self::Int(n) => Some(n),
            _ => None,
        }
    }

    /// Whether this value contains no information.
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
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
    use super::*;

    #[test]
    fn test_should_round_trip_value_via_serde_json() {
        let v = Value::Map(vec![
            (Arc::from("a"), Value::Int(42)),
            (Arc::from("b"), Value::Str(Arc::from("hello"))),
            (
                Arc::from("c"),
                Value::List(vec![Value::Bool(true), Value::Null]),
            ),
        ]);
        let json = serde_json::to_string(&v).unwrap();
        let back: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn test_should_offer_typed_accessors() {
        assert_eq!(Value::Str(Arc::from("x")).as_str(), Some("x"));
        assert_eq!(Value::Bool(true).as_bool(), Some(true));
        assert_eq!(Value::Int(7).as_int(), Some(7));
        assert!(Value::Null.is_null());
    }

    #[test]
    fn test_should_preserve_map_insertion_order() {
        let m: Map = vec![
            (Arc::from("z"), Value::Int(1)),
            (Arc::from("a"), Value::Int(2)),
            (Arc::from("m"), Value::Int(3)),
        ];
        let Value::Map(got) = Value::Map(m) else {
            panic!("constructed a Map, did not get one back");
        };
        let keys: Vec<&str> = got.iter().map(|(k, _)| k.as_ref()).collect();
        assert_eq!(keys, vec!["z", "a", "m"]);
    }
}
