//! Lossless adapters between our [`Value`](crate::ir::Value) and
//! [`hcl::Value`].
//!
//! Per [13-evaluator.md § 4], the evaluator was meant to "feed our context
//! into the hcl-rs evaluator and read the result back into our IR." The
//! actual Phase 4 walker operates entirely on our IR (see
//! [93-improvements-review.md] S-010 / S-011), but `value_to_hcl` /
//! `hcl_to_value` still earn their keep:
//!
//! - Phase 6's Terragrunt `read_terragrunt_config` returns `hcl::Value`-shaped maps. The adapters
//!   keep that boundary clean.
//! - `proptest` harnesses can round-trip arbitrary `Value`s through `hcl::Value` to verify the
//!   conversion is bijective.
//!
//! Both conversions are pure, total, and panic-free. They preserve numeric
//! fast-path: integer-only `Value::Int` round-trips through `hcl::Number`
//! without floating-point widening.
//!
//! [13-evaluator.md § 4]: ../../../specs/13-evaluator.md
//! [93-improvements-review.md]: ../../../specs/93-improvements-review.md

use std::sync::Arc;

use hcl::{Map as HclMap, Number, Value as HclValue};

use crate::ir::Value;

/// Convert our [`Value`] into the `hcl::Value` shape `hcl::eval` consumes.
///
/// Numeric handling:
/// - [`Value::Int`] → `hcl::Number::from(i64)` (no float widening).
/// - [`Value::Number`] → `hcl::Number::from_f64(f)`, with NaN / ±Inf collapsing to `HclValue::Null`
///   because `hcl::Number` rejects them. The collapse is pinned by a unit test below; callers who
///   care can pre-filter.
///
/// Strings / lists / maps are recursive; insertion order is preserved on
/// `Value::Map → hcl::Value::Object` because [`hcl::Map`] is
/// `indexmap::IndexMap`.
#[must_use]
pub fn value_to_hcl(v: &Value) -> HclValue {
    match v {
        Value::Null => HclValue::Null,
        Value::Bool(b) => HclValue::Bool(*b),
        Value::Int(n) => HclValue::Number(Number::from(*n)),
        Value::Number(f) => Number::from_f64(*f).map_or(HclValue::Null, HclValue::Number),
        Value::Str(s) => HclValue::String(s.to_string()),
        Value::List(items) => HclValue::Array(items.iter().map(value_to_hcl).collect()),
        Value::Map(entries) => {
            let mut out: HclMap<String, HclValue> = HclMap::with_capacity(entries.len());
            for (key, val) in entries {
                out.insert(key.to_string(), value_to_hcl(val));
            }
            HclValue::Object(out)
        }
    }
}

/// Convert an `hcl::Value` back into our [`Value`].
///
/// Inverse of [`value_to_hcl`]. Numbers prefer the [`Value::Int`] fast path
/// when `hcl::Number::as_i64` is `Some`; otherwise [`Value::Number`] with
/// `hcl::Number::as_f64` (which always succeeds for finite numbers because
/// `hcl::Number` rejects NaN / Inf at construction).
#[must_use]
pub fn hcl_to_value(v: &HclValue) -> Value {
    match v {
        HclValue::Null => Value::Null,
        HclValue::Bool(b) => Value::Bool(*b),
        HclValue::Number(n) => n
            .as_i64()
            .map_or_else(|| Value::Number(n.as_f64().unwrap_or(0.0)), Value::Int),
        HclValue::String(s) => Value::Str(Arc::from(s.as_str())),
        HclValue::Array(items) => Value::List(items.iter().map(hcl_to_value).collect()),
        HclValue::Object(entries) => Value::Map(
            entries
                .iter()
                .map(|(k, val)| (Arc::from(k.as_str()), hcl_to_value(val)))
                .collect(),
        ),
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
    use crate::ir::Map;

    #[test]
    fn test_should_round_trip_scalars() {
        for v in [
            Value::Null,
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(0),
            Value::Int(42),
            Value::Int(-1),
            Value::Int(i64::MAX),
            Value::Int(i64::MIN),
            Value::Number(1.5),
            Value::Str(Arc::from("hello")),
        ] {
            let h = value_to_hcl(&v);
            let back = hcl_to_value(&h);
            assert_eq!(v, back, "round trip mismatch: {v:?} → {h:?} → {back:?}");
        }
    }

    #[test]
    fn test_should_round_trip_nested_collections() {
        let v = Value::Map(vec![
            (Arc::from("a"), Value::Int(1)),
            (
                Arc::from("list"),
                Value::List(vec![Value::Bool(true), Value::Str(Arc::from("x"))]),
            ),
            (
                Arc::from("nested"),
                Value::Map(vec![(Arc::from("z"), Value::Number(2.5))]),
            ),
        ]);
        let back = hcl_to_value(&value_to_hcl(&v));
        assert_eq!(v, back);
    }

    #[test]
    fn test_should_preserve_map_insertion_order() {
        let m: Map = vec![
            (Arc::from("z"), Value::Int(1)),
            (Arc::from("a"), Value::Int(2)),
            (Arc::from("m"), Value::Int(3)),
        ];
        let v = Value::Map(m.clone());
        let back = hcl_to_value(&value_to_hcl(&v));
        let Value::Map(out) = back else {
            panic!("expected Map");
        };
        let keys: Vec<&str> = out.iter().map(|(k, _)| k.as_ref()).collect();
        assert_eq!(keys, vec!["z", "a", "m"]);
    }

    #[test]
    fn test_should_collapse_nan_and_infinity_to_null() {
        for f in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let v = Value::Number(f);
            assert_eq!(value_to_hcl(&v), HclValue::Null);
        }
    }

    #[test]
    fn test_should_prefer_int_fast_path_on_back_conversion() {
        let h = HclValue::Number(Number::from(42_i64));
        match hcl_to_value(&h) {
            Value::Int(42) => {}
            other => panic!("expected Value::Int(42), got {other:?}"),
        }
    }
}
