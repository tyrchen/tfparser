//! Phase 3 integration: discovery → loader → projection → `ParquetExporter`.
//!
//! Exercises the exit criteria from `specs/91-impl-plan.md § 6`:
//!
//! - `tfparser parse single-component` writes a Parquet file that round-trips through
//!   `parquet::arrow::arrow_reader` and yields the expected rows.
//! - The schema-golden test (see `parquet_schema_golden.rs`) catches drift.
//! - All M0 columns present; `account_id` / `region` / `module_path` empty for every row at M0.
//! - Atomic write: a `.partial` does not survive a successful export.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use arrow::array::AsArray;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use tfparser_core::{
    Workspace,
    discovery::{Discoverer, DiscoveryOptions, FsDiscoverer},
    exporter::{ExportOptions, Exporter, ParquetExporter},
    ir::ComponentId,
    loader::{HclEditLoader, LoadContext, Loader, LoaderLimits, SourceMap},
    projection::project_component,
};

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .expect("workspace root")
}

fn fixture(name: &str) -> PathBuf {
    workspace_root().join("fixtures").join(name)
}

fn build_workspace(fixture_name: &str) -> Workspace {
    let root = fixture(fixture_name);
    let discovered = FsDiscoverer
        .discover(&root, &DiscoveryOptions::defaults())
        .expect("discovery succeeds");
    let sources = SourceMap::new();
    let limits = LoaderLimits::default();
    let ctx = LoadContext::new(&discovered.root, &sources, &limits);

    let mut components = Vec::new();
    let mut workspace_diagnostics = Vec::new();
    for (idx, dir) in discovered.components.iter().enumerate() {
        let raw = HclEditLoader.load(dir, &ctx).expect("load");
        workspace_diagnostics.extend(raw.diagnostics.iter().cloned());
        let comp = project_component(
            &raw,
            ComponentId::from_index(idx),
            &mut workspace_diagnostics,
        );
        components.push(comp);
    }
    Workspace::builder()
        .root(Arc::<Path>::from(discovered.root.as_ref()))
        .components(components)
        .diagnostics(workspace_diagnostics)
        .build()
}

#[test]
fn test_should_export_single_component_fixture_to_parquet() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = build_workspace("single-component");
    let opts = ExportOptions::builder()
        .out_dir(Arc::<Path>::from(tmp.path()))
        .parsed_at_ms(Some(1_700_000_000_000_i64))
        .build();
    let report = ParquetExporter::new().export(&ws, &opts).expect("export");
    assert!(report.total_rows > 0, "expected at least one row");

    let resources = tmp.path().join("resources.parquet");
    assert!(resources.exists(), "resources.parquet not written");

    let manifest = tmp.path().join("workspace.manifest.json");
    assert!(manifest.exists(), "manifest not written");

    // No `.partial` leftovers.
    assert!(
        !resources.with_extension("parquet.partial").exists(),
        "partial leaked"
    );
    assert!(
        !manifest.with_extension("json.partial").exists(),
        "manifest partial leaked"
    );
}

#[test]
fn test_should_emit_expected_columns_and_row_kinds() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = build_workspace("single-component");
    let opts = ExportOptions::builder()
        .out_dir(Arc::<Path>::from(tmp.path()))
        .parsed_at_ms(Some(1_700_000_000_000_i64))
        .build();
    let _ = ParquetExporter::new().export(&ws, &opts).expect("export");

    let file = std::fs::File::open(tmp.path().join("resources.parquet")).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();

    let mut kinds: Vec<String> = Vec::new();
    let mut addresses: Vec<String> = Vec::new();
    let mut module_paths_empty = true;
    let mut account_ids_empty = true;
    let mut regions_empty = true;
    for batch in reader {
        let batch = batch.unwrap();
        let kind_col = batch.column_by_name("kind").unwrap().as_string::<i32>();
        let addr_col = batch.column_by_name("address").unwrap().as_string::<i32>();
        let mp_col = batch
            .column_by_name("module_path")
            .unwrap()
            .as_string::<i32>();
        let acc_col = batch
            .column_by_name("account_id")
            .unwrap()
            .as_string::<i32>();
        let region_col = batch.column_by_name("region").unwrap().as_string::<i32>();
        for i in 0..batch.num_rows() {
            kinds.push(kind_col.value(i).to_string());
            addresses.push(addr_col.value(i).to_string());
            if !mp_col.value(i).is_empty() {
                module_paths_empty = false;
            }
            if !acc_col.value(i).is_empty() {
                account_ids_empty = false;
            }
            if !region_col.value(i).is_empty() {
                regions_empty = false;
            }
        }
    }
    // single-component fixture: a resource, provider, two variables, one
    // local, one output.
    assert!(kinds.contains(&"resource".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"provider".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"variable".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"local".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"output".to_string()), "{kinds:?}");
    assert!(
        addresses.iter().any(|a| a == "aws_iam_role.service"),
        "{addresses:?}"
    );
    // M0: module_path / account_id / region are empty for every row.
    assert!(module_paths_empty, "module_path should be empty at M0");
    assert!(account_ids_empty, "account_id should be empty at M0");
    assert!(regions_empty, "region should be empty at M0");
}

#[test]
fn test_should_emit_attributes_json_for_resource_row() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = build_workspace("single-component");
    let opts = ExportOptions::builder()
        .out_dir(Arc::<Path>::from(tmp.path()))
        .parsed_at_ms(Some(1_700_000_000_000_i64))
        .build();
    let _ = ParquetExporter::new().export(&ws, &opts).expect("export");

    let file = std::fs::File::open(tmp.path().join("resources.parquet")).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    let mut found = false;
    for batch in reader {
        let batch = batch.unwrap();
        let kind_col = batch.column_by_name("kind").unwrap().as_string::<i32>();
        let attrs_col = batch
            .column_by_name("attributes_json")
            .unwrap()
            .as_string::<i32>();
        for i in 0..batch.num_rows() {
            if kind_col.value(i) == "resource" {
                let body = attrs_col.value(i);
                let v: serde_json::Value =
                    serde_json::from_str(body).expect("valid JSON in attributes_json");
                // The single-component fixture's resource has a `name` and
                // `assume_role_policy` attribute. The latter is a function
                // call; the former references `local.full_name` (unresolved).
                assert!(v.is_object(), "attributes_json must be a JSON object");
                found = true;
            }
        }
    }
    assert!(found, "expected at least one resource row");
}

#[test]
fn test_should_emit_manifest_with_sha256_for_resources() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = build_workspace("single-component");
    let opts = ExportOptions::builder()
        .out_dir(Arc::<Path>::from(tmp.path()))
        .parsed_at_ms(Some(1_700_000_000_000_i64))
        .build();
    let _ = ParquetExporter::new().export(&ws, &opts).expect("export");
    let bytes = std::fs::read(tmp.path().join("workspace.manifest.json")).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let files = v.get("files").and_then(|f| f.as_array()).expect("files");
    let resources = files
        .iter()
        .find(|f| f.get("name").and_then(|n| n.as_str()) == Some("resources.parquet"))
        .expect("resources.parquet entry");
    let sha = resources
        .get("sha256")
        .and_then(|s| s.as_str())
        .expect("sha256 string");
    assert_eq!(sha.len(), 64, "SHA-256 hex must be 64 chars: {sha}");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_should_export_multi_provider_with_aliases() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ws = build_workspace("multi-provider");
    let opts = ExportOptions::builder()
        .out_dir(Arc::<Path>::from(tmp.path()))
        .parsed_at_ms(Some(1_700_000_000_000_i64))
        .build();
    let _ = ParquetExporter::new().export(&ws, &opts).expect("export");

    let file = std::fs::File::open(tmp.path().join("resources.parquet")).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    let mut provider_locals: Vec<String> = Vec::new();
    for batch in reader {
        let batch = batch.unwrap();
        let kind_col = batch.column_by_name("kind").unwrap().as_string::<i32>();
        let pl_col = batch
            .column_by_name("provider_local")
            .unwrap()
            .as_string::<i32>();
        for i in 0..batch.num_rows() {
            if kind_col.value(i) == "resource" {
                provider_locals.push(pl_col.value(i).to_string());
            }
        }
    }
    assert!(
        provider_locals.iter().any(|p| p == "aws.main"),
        "{provider_locals:?}"
    );
    assert!(
        provider_locals.iter().any(|p| p == "aws.backup"),
        "{provider_locals:?}"
    );
}

#[test]
fn test_should_produce_byte_identical_parquet_for_pinned_parsed_at() {
    // Two exports of an identical Workspace+opts produce identical bytes.
    let tmp_a = tempfile::tempdir().expect("tempdir");
    let tmp_b = tempfile::tempdir().expect("tempdir");
    let ws = build_workspace("single-component");

    let opts_a = ExportOptions::builder()
        .out_dir(Arc::<Path>::from(tmp_a.path()))
        .parsed_at_ms(Some(1_700_000_000_000_i64))
        .build();
    let opts_b = ExportOptions::builder()
        .out_dir(Arc::<Path>::from(tmp_b.path()))
        .parsed_at_ms(Some(1_700_000_000_000_i64))
        .build();

    let _ = ParquetExporter::new().export(&ws, &opts_a).unwrap();
    let _ = ParquetExporter::new().export(&ws, &opts_b).unwrap();

    let bytes_a = std::fs::read(tmp_a.path().join("resources.parquet")).unwrap();
    let bytes_b = std::fs::read(tmp_b.path().join("resources.parquet")).unwrap();
    assert_eq!(
        bytes_a, bytes_b,
        "exports with pinned parsed_at should be byte-identical"
    );
}
