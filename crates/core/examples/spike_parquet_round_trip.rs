//! Phase 0 spike 0.3 — `parquet` schema declaration + round-trip.
//!
//! Goal: prove we can (a) declare the canonical `resources.parquet` Arrow
//! schema from [10-data-model.md § 3](../../../specs/10-data-model.md);
//! (b) write a small batch via the streaming `ArrowWriter`; (c) read it
//! back and confirm every cell + the schema match.
//!
//! Run with: `cargo run -p tfparser-core --example spike_parquet_round_trip`.
//!
//! Schema reproduced verbatim here so the spike depends on nothing beyond
//! `arrow` + `parquet`. Phase 3 lifts this into a real `ParquetExporter`.

#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use anyhow::Context;
use arrow::{
    array::{
        ArrayRef, ListBuilder, RecordBatch, StringBuilder, TimestampMillisecondArray, UInt32Array,
    },
    datatypes::{DataType, Field, Schema, TimeUnit},
};
use parquet::{
    arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder},
    basic::{Compression, ZstdLevel},
    file::properties::WriterProperties,
};
use tempfile::NamedTempFile;

const ROW_COUNT: usize = 10;

fn build_schema() -> Schema {
    // Mirrors [10-data-model.md § 3] — column order and types are the
    // public Parquet contract; do not reorder, retype, or rename.
    Schema::new(vec![
        Field::new("workspace_root", DataType::Utf8, false),
        Field::new("component_path", DataType::Utf8, false),
        Field::new("module_path", DataType::Utf8, false),
        Field::new("address", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("resource_type", DataType::Utf8, false),
        Field::new("resource_name", DataType::Utf8, false),
        Field::new("provider_local", DataType::Utf8, false),
        Field::new("provider_source", DataType::Utf8, false),
        Field::new("account_id", DataType::Utf8, false),
        Field::new("account_name", DataType::Utf8, false),
        Field::new("region", DataType::Utf8, false),
        Field::new("environment", DataType::Utf8, false),
        Field::new("count_expr", DataType::Utf8, false),
        Field::new("for_each_expr", DataType::Utf8, false),
        Field::new(
            "depends_on",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            false,
        ),
        Field::new("attributes_json", DataType::Utf8, false),
        Field::new("state_account_id", DataType::Utf8, false),
        Field::new("state_region", DataType::Utf8, false),
        Field::new("file", DataType::Utf8, false),
        Field::new("line", DataType::UInt32, false),
        Field::new("column", DataType::UInt32, false),
        Field::new("parser_version", DataType::Utf8, false),
        Field::new(
            "parsed_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some(Arc::from("UTC"))),
            false,
        ),
    ])
}

fn build_record_batch() -> RecordBatch {
    let schema = Arc::new(build_schema());

    let strings = |fill: fn(usize) -> String| {
        let mut b = StringBuilder::with_capacity(ROW_COUNT, ROW_COUNT * 32);
        for i in 0..ROW_COUNT {
            b.append_value(fill(i));
        }
        Arc::new(b.finish()) as ArrayRef
    };
    let constant = |s: &str| {
        let mut b = StringBuilder::with_capacity(ROW_COUNT, ROW_COUNT * s.len());
        for _ in 0..ROW_COUNT {
            b.append_value(s);
        }
        Arc::new(b.finish()) as ArrayRef
    };
    let empty_strings = || constant("");

    // depends_on: List<Utf8>; row 0 has 0 deps, row 1 has 1, row 2 has 2, ...
    let mut list_builder = ListBuilder::new(StringBuilder::new());
    for i in 0..ROW_COUNT {
        let values = list_builder.values();
        for j in 0..i {
            values.append_value(format!("dep.{i}.{j}"));
        }
        list_builder.append(true);
    }
    let depends_on = Arc::new(list_builder.finish()) as ArrayRef;

    let line = Arc::new(UInt32Array::from_iter_values(
        (0..ROW_COUNT).map(|i| u32::try_from(i + 1).unwrap_or(u32::MAX)),
    )) as ArrayRef;
    let column = Arc::new(UInt32Array::from_iter_values(std::iter::repeat_n(
        1_u32, ROW_COUNT,
    ))) as ArrayRef;

    let timestamps = TimestampMillisecondArray::from_iter_values(std::iter::repeat_n(
        1_700_000_000_000_i64,
        ROW_COUNT,
    ))
    .with_timezone("UTC");
    let parsed_at = Arc::new(timestamps) as ArrayRef;

    let columns: Vec<ArrayRef> = vec![
        constant("/repo/large-monorepo"),
        strings(|i| format!("services/svc-{i:02}")),
        empty_strings(),
        strings(|i| format!("aws_s3_bucket.b{i}")),
        constant("resource"),
        constant("aws_s3_bucket"),
        strings(|i| format!("b{i}")),
        empty_strings(),
        constant("hashicorp/aws"),
        constant("100000000001"),
        constant("primary"),
        constant("us-west-2"),
        constant("staging"),
        empty_strings(),
        empty_strings(),
        depends_on,
        constant("{}"),
        constant(""),
        constant(""),
        strings(|i| format!("services/svc-{i:02}/main.tf")),
        line,
        column,
        constant(env!("CARGO_PKG_VERSION")),
        parsed_at,
    ];

    RecordBatch::try_new(schema, columns).expect("record batch matches schema")
}

fn main() -> anyhow::Result<()> {
    println!("=== spike_parquet_round_trip ===");

    let schema = Arc::new(build_schema());
    let original = build_record_batch();
    println!(
        "schema: {} columns, batch: {} rows",
        schema.fields().len(),
        original.num_rows()
    );

    // Write to a temp file. CLAUDE.md § Security: tempfile::NamedTempFile
    // cleans up on drop and uses a unique path.
    let tmp = NamedTempFile::new().context("create temp parquet file")?;
    {
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(ZstdLevel::try_new(3)?))
            .build();
        let file = std::fs::File::create(tmp.path())?;
        let mut writer = ArrowWriter::try_new(file, Arc::clone(&schema), Some(props))
            .context("construct ArrowWriter")?;
        writer.write(&original).context("write batch")?;
        writer.close().context("close writer")?;
    }

    let on_disk_bytes = std::fs::metadata(tmp.path())?.len();
    println!(
        "wrote {} ({} bytes on disk)",
        tmp.path().display(),
        on_disk_bytes
    );

    // Read back via the ArrowRecordBatchReaderBuilder. This validates
    // (a) the schema we wrote (b) the rows we wrote.
    let file = std::fs::File::open(tmp.path())?;
    let builder =
        ParquetRecordBatchReaderBuilder::try_new(file).context("open parquet reader builder")?;
    let read_schema: Arc<Schema> = Arc::clone(builder.schema());
    let mut reader = builder.build().context("build parquet reader")?;
    let read_batch = reader
        .next()
        .ok_or_else(|| anyhow::anyhow!("no batches in parquet file"))?
        .context("read batch")?;

    // Schema match.
    anyhow::ensure!(
        read_schema.fields() == schema.fields(),
        "schema drifted during round-trip"
    );

    // Cell-by-cell match.
    anyhow::ensure!(
        read_batch.num_rows() == original.num_rows(),
        "row count differs: wrote {} read {}",
        original.num_rows(),
        read_batch.num_rows()
    );
    for (i, (orig_col, back_col)) in original
        .columns()
        .iter()
        .zip(read_batch.columns())
        .enumerate()
    {
        anyhow::ensure!(
            orig_col.as_ref() == back_col.as_ref(),
            "column {i} ({}) differs after round-trip",
            schema.field(i).name(),
        );
    }
    println!(
        "OK — schema and {} rows round-tripped through parquet (zstd-3)",
        original.num_rows()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_pins_every_phase1_column() {
        let schema = build_schema();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        // Pin the exact column order from spec 10 § 3. Any change here must
        // be a deliberate schema-version bump.
        assert_eq!(
            names,
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
            ],
        );
    }

    #[test]
    fn test_record_batch_matches_schema_arity() {
        let batch = build_record_batch();
        let schema = build_schema();
        assert_eq!(batch.num_columns(), schema.fields().len());
        assert_eq!(batch.num_rows(), ROW_COUNT);
    }
}
