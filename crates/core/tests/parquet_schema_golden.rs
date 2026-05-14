//! Schema-drift gate.
//!
//! Per [20-parquet-exporter.md § 7] and [72-testing-strategy.md § 4]: a
//! golden JSON oracle pinned at `tests/golden/resources-schema.json`. CI
//! fails if the code's column list and the spec disagree.
//!
//! The oracle is a tiny, hand-checked summary (not Arrow's native JSON
//! dump) so a reviewer can diff it eyes-only. Adding a column requires:
//!
//! 1. Append the new field at the end of `resources_schema()`.
//! 2. Bump `SCHEMA_MINOR` and update the oracle's `version.minor`.
//! 3. Append the new column entry to the oracle.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use arrow::datatypes::{DataType, TimeUnit};
use serde::Deserialize;
use tfparser_core::exporter::{SCHEMA_MAJOR, SCHEMA_MINOR, resources_schema};

#[derive(Debug, Deserialize)]
struct Golden {
    version: Version,
    columns: Vec<Column>,
}

#[derive(Debug, Deserialize)]
struct Version {
    major: u32,
    minor: u32,
}

#[derive(Debug, Deserialize)]
struct Column {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    nullable: bool,
    #[serde(default)]
    item_nullable: Option<bool>,
}

fn golden() -> Golden {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/resources-schema.json");
    let bytes = std::fs::read(&path).expect("read golden");
    serde_json::from_slice(&bytes).expect("parse golden")
}

fn render_type(dt: &DataType) -> String {
    match dt {
        DataType::Utf8 => "Utf8".into(),
        DataType::UInt32 => "UInt32".into(),
        DataType::List(inner) => match inner.data_type() {
            DataType::Utf8 => "List<Utf8>".into(),
            other => format!("List<{other:?}>"),
        },
        DataType::Timestamp(TimeUnit::Millisecond, tz) => {
            format!("Timestamp(Millisecond, {})", tz.as_deref().unwrap_or(""))
        }
        other => format!("{other:?}"),
    }
}

#[test]
fn test_should_match_golden_schema() {
    let g = golden();
    let s = resources_schema();
    assert_eq!(g.version.major, SCHEMA_MAJOR);
    assert_eq!(g.version.minor, SCHEMA_MINOR);
    assert_eq!(g.columns.len(), s.fields().len());

    for (i, (got, want)) in s.fields().iter().zip(g.columns.iter()).enumerate() {
        assert_eq!(
            got.name(),
            &want.name,
            "column #{i} name diverged (got `{}`, want `{}`)",
            got.name(),
            want.name
        );
        assert_eq!(
            got.is_nullable(),
            want.nullable,
            "column #{i} `{}` nullability diverged",
            want.name
        );
        let rendered = render_type(got.data_type());
        assert_eq!(
            rendered, want.ty,
            "column #{i} `{}` type diverged (got `{rendered}`, want `{}`)",
            want.name, want.ty
        );
        if let DataType::List(inner) = got.data_type() {
            assert_eq!(
                inner.is_nullable(),
                want.item_nullable.unwrap_or(true),
                "column #{i} `{}` list item nullability diverged",
                want.name
            );
        }
    }
}
