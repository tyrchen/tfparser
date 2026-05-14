//! Arrow schema for `resources.parquet`.
//!
//! The schema is **authoritative** — `tests/golden/resources-schema.json`
//! pins it and CI fails on drift. New columns may be appended at the end
//! for additive evolution (per [10-data-model.md § 6]); existing columns
//! cannot be renamed, retyped, or reordered.
//!
//! [10-data-model.md § 6]: ../../../specs/10-data-model.md

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

/// Parser version embedded in every emitted row.
///
/// Bound to `tfparser-core`'s crate version so consumers can branch on
/// upgrades.
pub const PARSER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Parquet schema major version. Bumps require a deprecation cycle.
pub const SCHEMA_MAJOR: u32 = 0;
/// Parquet schema minor version. Bumps are additive.
pub const SCHEMA_MINOR: u32 = 1;

/// Build the canonical `resources.parquet` schema.
#[must_use]
pub fn resources_schema() -> Schema {
    Schema::new(vec![
        utf8_field("workspace_root"),
        utf8_field("component_path"),
        utf8_field("module_path"),
        utf8_field("address"),
        utf8_field("kind"),
        utf8_field("resource_type"),
        utf8_field("resource_name"),
        utf8_field("provider_local"),
        utf8_field("provider_source"),
        utf8_field("account_id"),
        utf8_field("account_name"),
        utf8_field("region"),
        utf8_field("environment"),
        utf8_field("count_expr"),
        utf8_field("for_each_expr"),
        Field::new(
            "depends_on",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, false))),
            false,
        ),
        utf8_field("attributes_json"),
        utf8_field("state_account_id"),
        utf8_field("state_region"),
        utf8_field("file"),
        Field::new("line", DataType::UInt32, false),
        Field::new("column", DataType::UInt32, false),
        utf8_field("parser_version"),
        Field::new(
            "parsed_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some(Arc::from("UTC"))),
            false,
        ),
    ])
}

/// Ordered list of column names for the resources schema.
///
/// Used by the golden-test harness and by `tfparser schema` to dump the
/// schema for downstream tools without round-tripping through Arrow.
#[must_use]
pub fn schema_field_names() -> Vec<&'static str> {
    vec![
        "workspace_root",
        "component_path",
        "module_path",
        "address",
        "kind",
        "resource_type",
        "resource_name",
        "provider_local",
        "provider_source",
        "account_id",
        "account_name",
        "region",
        "environment",
        "count_expr",
        "for_each_expr",
        "depends_on",
        "attributes_json",
        "state_account_id",
        "state_region",
        "file",
        "line",
        "column",
        "parser_version",
        "parsed_at",
    ]
}

fn utf8_field(name: &'static str) -> Field {
    Field::new(name, DataType::Utf8, false)
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
    fn test_schema_has_24_columns() {
        let s = resources_schema();
        assert_eq!(s.fields().len(), 24);
    }

    #[test]
    fn test_schema_columns_match_documented_names() {
        let s = resources_schema();
        let got: Vec<&str> = s.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(got, schema_field_names());
    }

    #[test]
    fn test_depends_on_is_non_null_list_of_utf8() {
        let s = resources_schema();
        let depends_on = s.field_with_name("depends_on").unwrap();
        assert!(!depends_on.is_nullable());
        match depends_on.data_type() {
            DataType::List(item) => {
                assert_eq!(item.data_type(), &DataType::Utf8);
                assert!(!item.is_nullable());
            }
            other => panic!("expected List<Utf8>, got {other:?}"),
        }
    }

    #[test]
    fn test_parsed_at_is_timestamp_ms_utc() {
        let s = resources_schema();
        let f = s.field_with_name("parsed_at").unwrap();
        match f.data_type() {
            DataType::Timestamp(TimeUnit::Millisecond, tz) => {
                assert_eq!(tz.as_deref(), Some("UTC"));
            }
            other => panic!("expected Timestamp(ms, UTC), got {other:?}"),
        }
    }

    #[test]
    fn test_no_nullable_columns() {
        let s = resources_schema();
        for f in s.fields() {
            assert!(!f.is_nullable(), "column `{}` is nullable", f.name());
        }
    }

    #[test]
    fn test_field_names_match_schema_size() {
        let s = resources_schema();
        assert_eq!(s.fields().len(), schema_field_names().len());
    }
}
