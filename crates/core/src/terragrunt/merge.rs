//! Merge strategies for Terragrunt `include` cascades.
//!
//! Per [14-terragrunt.md § 3.2], an `include` block may declare a
//! `merge_strategy = ...` attribute selecting one of four strategies:
//!
//! - `deep_map_only` (default in Terragrunt ≥ 0.45) — deep-merge maps, leave non-map values from
//!   the child winning.
//! - `deep` — deep-merge everything, list concatenation included.
//! - `shallow` — top-level keys only; child wins on any conflict.
//! - `no_merge` — include the parent for `read_terragrunt_config` access but do not merge it into
//!   the child.
//!
//! [14-terragrunt.md § 3.2]: ../../../specs/14-terragrunt.md

use std::sync::Arc;

use crate::ir::{Map, Value};

/// Which merge strategy an `include` block requested.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum MergeStrategy {
    /// Deep merge for map values; child wins for non-map values.
    #[default]
    DeepMapOnly,
    /// Deep merge for everything; lists concatenate.
    Deep,
    /// Top-level keys only; child wins on any conflict.
    Shallow,
    /// Do not merge the parent into the child.
    NoMerge,
}

impl MergeStrategy {
    /// Parse the literal string surfaced by `include.merge_strategy`.
    ///
    /// Unknown values default to `deep_map_only` and the caller surfaces
    /// a diagnostic.
    pub(super) fn parse(s: &str) -> Option<Self> {
        match s {
            "deep_map_only" => Some(Self::DeepMapOnly),
            "deep" => Some(Self::Deep),
            "shallow" => Some(Self::Shallow),
            "no_merge" => Some(Self::NoMerge),
            _ => None,
        }
    }
}

/// Merge `parent_locals` into `child_locals` per `strategy`.
///
/// Returns the merged `Map`. The cascade rule is "deepest-first": the
/// child's locals already incorporate every deeper include before this
/// call. With `MergeStrategy::NoMerge` the function returns `child_locals`
/// unchanged.
pub(super) fn merge_locals(
    parent_locals: &Map,
    child_locals: &Map,
    strategy: MergeStrategy,
) -> Map {
    match strategy {
        MergeStrategy::NoMerge => child_locals.clone(),
        MergeStrategy::Shallow => merge_shallow(parent_locals, child_locals),
        MergeStrategy::DeepMapOnly => merge_deep_map_only(parent_locals, child_locals),
        MergeStrategy::Deep => merge_deep(parent_locals, child_locals),
    }
}

fn merge_shallow(parent: &Map, child: &Map) -> Map {
    let mut out: Map = parent.clone();
    for (k, v) in child {
        if let Some(slot) = out.iter_mut().find(|(pk, _)| pk == k) {
            slot.1 = v.clone();
        } else {
            out.push((Arc::clone(k), v.clone()));
        }
    }
    out
}

fn merge_deep_map_only(parent: &Map, child: &Map) -> Map {
    let mut out: Map = parent.clone();
    for (k, v) in child {
        if let Some(slot) = out.iter_mut().find(|(pk, _)| pk == k) {
            // For map values, recurse into the maps; for everything else,
            // child wins.
            match (&slot.1, v) {
                (Value::Map(parent_inner), Value::Map(child_inner)) => {
                    slot.1 = Value::Map(merge_deep_map_only(parent_inner, child_inner));
                }
                _ => {
                    slot.1 = v.clone();
                }
            }
        } else {
            out.push((Arc::clone(k), v.clone()));
        }
    }
    out
}

fn merge_deep(parent: &Map, child: &Map) -> Map {
    let mut out: Map = parent.clone();
    for (k, v) in child {
        if let Some(slot) = out.iter_mut().find(|(pk, _)| pk == k) {
            match (&slot.1, v) {
                (Value::Map(parent_inner), Value::Map(child_inner)) => {
                    slot.1 = Value::Map(merge_deep(parent_inner, child_inner));
                }
                (Value::List(parent_inner), Value::List(child_inner)) => {
                    // Concatenate parent + child.
                    let mut merged = parent_inner.clone();
                    merged.extend(child_inner.iter().cloned());
                    slot.1 = Value::List(merged);
                }
                _ => {
                    slot.1 = v.clone();
                }
            }
        } else {
            out.push((Arc::clone(k), v.clone()));
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn map(entries: &[(&str, Value)]) -> Map {
        entries
            .iter()
            .map(|(k, v)| (Arc::<str>::from(*k), v.clone()))
            .collect()
    }

    #[test]
    fn test_no_merge_returns_child_unchanged() {
        let parent = map(&[("a", Value::Int(1))]);
        let child = map(&[("b", Value::Int(2))]);
        let out = merge_locals(&parent, &child, MergeStrategy::NoMerge);
        assert_eq!(out, child);
    }

    #[test]
    fn test_shallow_child_wins_on_conflict() {
        let parent = map(&[("a", Value::Int(1)), ("b", Value::Int(10))]);
        let child = map(&[("a", Value::Int(2))]);
        let out = merge_locals(&parent, &child, MergeStrategy::Shallow);
        assert_eq!(out.len(), 2);
        let a = out.iter().find(|(k, _)| &**k == "a").unwrap();
        assert_eq!(a.1, Value::Int(2));
        let b = out.iter().find(|(k, _)| &**k == "b").unwrap();
        assert_eq!(b.1, Value::Int(10));
    }

    #[test]
    fn test_deep_map_only_recurses_into_maps() {
        let parent = map(&[(
            "tags",
            Value::Map(vec![
                (Arc::from("Org"), Value::Str(Arc::from("acme"))),
                (Arc::from("Env"), Value::Str(Arc::from("dev"))),
            ]),
        )]);
        let child = map(&[(
            "tags",
            Value::Map(vec![(Arc::from("Env"), Value::Str(Arc::from("prod")))]),
        )]);
        let out = merge_locals(&parent, &child, MergeStrategy::DeepMapOnly);
        let tags = out.iter().find(|(k, _)| &**k == "tags").unwrap();
        match &tags.1 {
            Value::Map(m) => {
                assert_eq!(m.len(), 2);
                let org = m.iter().find(|(k, _)| &**k == "Org").unwrap();
                assert_eq!(org.1, Value::Str(Arc::from("acme")));
                let env = m.iter().find(|(k, _)| &**k == "Env").unwrap();
                assert_eq!(env.1, Value::Str(Arc::from("prod")));
            }
            other => panic!("expected Map, got {other:?}"),
        }
    }

    #[test]
    fn test_deep_concatenates_lists() {
        let parent = map(&[("xs", Value::List(vec![Value::Int(1), Value::Int(2)]))]);
        let child = map(&[("xs", Value::List(vec![Value::Int(3)]))]);
        let out = merge_locals(&parent, &child, MergeStrategy::Deep);
        let xs = out.iter().find(|(k, _)| &**k == "xs").unwrap();
        assert_eq!(
            xs.1,
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
        );
    }

    #[test]
    fn test_strategy_parse_known_values() {
        assert_eq!(
            MergeStrategy::parse("deep_map_only"),
            Some(MergeStrategy::DeepMapOnly)
        );
        assert_eq!(MergeStrategy::parse("deep"), Some(MergeStrategy::Deep));
        assert_eq!(
            MergeStrategy::parse("shallow"),
            Some(MergeStrategy::Shallow)
        );
        assert_eq!(
            MergeStrategy::parse("no_merge"),
            Some(MergeStrategy::NoMerge)
        );
        assert!(MergeStrategy::parse("bogus").is_none());
    }
}
