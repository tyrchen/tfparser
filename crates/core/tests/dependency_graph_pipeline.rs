//! Phase 8 integration: dependency-graph collection + secondary-table
//! emission. Spec 91 § 11 exit criteria — secondary tables emit alongside
//! `resources.parquet`; edge counts match a hand-curated oracle on a small
//! synthesised workspace.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::doc_overindented_list_items
)]

use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};

use arrow::array::{Array, StringArray, UInt32Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use tfparser_core::{
    Address, AttributeMap, Component, ComponentId, ComponentKind, DefaultGraphBuilder, Edge,
    EdgeKind, Expression, GraphBuilder, GraphContext, ModuleCall, ModuleRegistry, ModuleSource,
    Resource, ResourceKind, Span, SymbolKind, Symbolic, Workspace,
    eval::EvaluatedComponent,
    exporter::{ExportOptions, Exporter, ParquetExporter, SecondaryTable},
};

fn span() -> Span {
    Span::synthetic()
}

fn arc_path<P: AsRef<Path>>(p: P) -> Arc<Path> {
    Arc::from(p.as_ref())
}

fn symbolic_resource(source: &str) -> Symbolic {
    Symbolic::builder()
        .kind(SymbolKind::Resource)
        .source(Arc::<str>::from(source))
        .span(span())
        .build()
}

fn read_strings(path: &Path, column: &str) -> Vec<String> {
    let file = File::open(path).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.unwrap();
        let idx = batch.schema().index_of(column).unwrap();
        let col = batch
            .column(idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for i in 0..col.len() {
            out.push(col.value(i).to_string());
        }
    }
    out
}

fn read_u32(path: &Path, column: &str) -> Vec<u32> {
    let file = File::open(path).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.unwrap();
        let idx = batch.schema().index_of(column).unwrap();
        let col = batch
            .column(idx)
            .as_any()
            .downcast_ref::<UInt32Array>()
            .unwrap();
        for i in 0..col.len() {
            out.push(col.value(i));
        }
    }
    out
}

/// Synthesise a small workspace with a deterministic edge oracle:
///
/// component `svc/api-gw`:
///   - `aws_iam_role.r1` attributes: { policy = aws_iam_policy.p.arn, subnet = data.aws_subnet.s.id
///     } depends_on: [aws_iam_role.r2]
///   - `aws_iam_role.r2`
///   - `aws_iam_policy.p`
///   - `data.aws_subnet.s` (data source)
///   - module call `app` (no inputs that reference siblings)
///
/// Expected edge oracle on this fixture:
///   3 attr_ref + 1 explicit_depends_on = 4 edges total.
fn build_oracle_workspace() -> Workspace {
    let r1_attrs: AttributeMap = vec![
        (
            Arc::from("policy"),
            Expression::Unresolved(symbolic_resource("aws_iam_policy.p.arn")),
        ),
        (
            Arc::from("subnet"),
            Expression::Unresolved(
                Symbolic::builder()
                    .kind(SymbolKind::Data)
                    .source(Arc::<str>::from("data.aws_subnet.s.id"))
                    .span(span())
                    .build(),
            ),
        ),
    ];
    let r1 = Resource::builder()
        .address(Address::new("aws_iam_role.r1").unwrap())
        .kind(ResourceKind::Managed)
        .type_(Arc::<str>::from("aws_iam_role"))
        .name(Arc::<str>::from("r1"))
        .attributes(r1_attrs)
        .depends_on(vec![Address::new("aws_iam_role.r2").unwrap()])
        .span(span())
        .build();
    let r2 = Resource::builder()
        .address(Address::new("aws_iam_role.r2").unwrap())
        .kind(ResourceKind::Managed)
        .type_(Arc::<str>::from("aws_iam_role"))
        .name(Arc::<str>::from("r2"))
        .span(span())
        .build();
    let p = Resource::builder()
        .address(Address::new("aws_iam_policy.p").unwrap())
        .kind(ResourceKind::Managed)
        .type_(Arc::<str>::from("aws_iam_policy"))
        .name(Arc::<str>::from("p"))
        .span(span())
        .build();
    let s = Resource::builder()
        .address(Address::new("data.aws_subnet.s").unwrap())
        .kind(ResourceKind::Data)
        .type_(Arc::<str>::from("aws_subnet"))
        .name(Arc::<str>::from("s"))
        .span(span())
        .build();
    let mc = ModuleCall::builder()
        .address(Address::new("module.app").unwrap())
        .source_raw(Arc::<str>::from("./modules/app"))
        .source(ModuleSource::Local(Arc::from("./modules/app")))
        .span(span())
        .build();
    let component = Component::builder()
        .id(ComponentId::from_index(0))
        .path(arc_path(PathBuf::from("svc/api-gw")))
        .kind(ComponentKind::Component)
        .resources(vec![r1, r2, p, s])
        .modules(vec![mc])
        .build();

    let evaluated = EvaluatedComponent::from_component(component);
    let registry = ModuleRegistry::new();
    let ctx = GraphContext::new(arc_path(PathBuf::from("/repo")));
    DefaultGraphBuilder::new()
        .build(vec![evaluated], &registry, &ctx)
        .unwrap()
}

#[test]
fn test_oracle_edges_match_expected_counts() {
    let ws = build_oracle_workspace();
    let attr_refs: Vec<&Edge> = ws
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::AttrRef)
        .collect();
    let explicit: Vec<&Edge> = ws
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::ExplicitDependsOn)
        .collect();
    assert_eq!(attr_refs.len(), 2, "{:?}", ws.edges);
    assert_eq!(explicit.len(), 1, "{:?}", ws.edges);
}

#[test]
fn test_three_tables_emit_alongside_resources() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = build_oracle_workspace();
    let opts = ExportOptions::builder()
        .out_dir(arc_path(tmp.path()))
        .parsed_at_ms(Some(1_700_000_000_000))
        .tables(vec![
            SecondaryTable::Dependencies,
            SecondaryTable::Components,
            SecondaryTable::Modules,
        ])
        .build();
    let report = ParquetExporter::new().export(&ws, &opts).unwrap();

    for name in [
        "resources.parquet",
        "dependencies.parquet",
        "components.parquet",
        "modules.parquet",
        "workspace.manifest.json",
    ] {
        assert!(
            report
                .files
                .iter()
                .any(|f| f.path.file_name().and_then(|n| n.to_str()) == Some(name)),
            "missing {name} in {:?}",
            report
                .files
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>()
        );
    }

    let deps_path = tmp.path().join("dependencies.parquet");
    let froms = read_strings(&deps_path, "from_address");
    let tos = read_strings(&deps_path, "to_address");
    let kinds = read_strings(&deps_path, "edge_kind");
    assert_eq!(froms.len(), 3, "rows={froms:?}");
    // Sorted by (from, to, kind); explicit_depends_on appears alongside
    // attr_ref edges in deterministic order.
    let triples: Vec<(String, String, String)> = froms
        .iter()
        .zip(&tos)
        .zip(&kinds)
        .map(|((a, b), c)| (a.clone(), b.clone(), c.clone()))
        .collect();
    assert_eq!(
        triples,
        vec![
            (
                "aws_iam_role.r1".into(),
                "aws_iam_policy.p".into(),
                "attr_ref".into(),
            ),
            (
                "aws_iam_role.r1".into(),
                "aws_iam_role.r2".into(),
                "explicit_depends_on".into(),
            ),
            (
                "aws_iam_role.r1".into(),
                "data.aws_subnet.s".into(),
                "attr_ref".into(),
            ),
        ]
    );
}

#[test]
fn test_components_parquet_summary_row_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = build_oracle_workspace();
    let opts = ExportOptions::builder()
        .out_dir(arc_path(tmp.path()))
        .parsed_at_ms(Some(1))
        .tables(vec![SecondaryTable::Components])
        .build();
    ParquetExporter::new().export(&ws, &opts).unwrap();
    let path = tmp.path().join("components.parquet");

    let component_paths = read_strings(&path, "component_path");
    let res_counts = read_u32(&path, "resource_count");
    let data_counts = read_u32(&path, "data_count");
    let module_call_counts = read_u32(&path, "module_call_count");

    assert_eq!(component_paths, vec!["svc/api-gw".to_string()]);
    assert_eq!(res_counts, vec![3]); // r1, r2, p (managed)
    assert_eq!(data_counts, vec![1]); // s (data)
    assert_eq!(module_call_counts, vec![1]);
}

#[test]
fn test_three_table_join_simulates_duckdb() {
    // DuckDB is not a Rust dep here; we re-create the same join in-memory
    // by reading the three parquet files and asserting the join key
    // (`address` / `from_address` / `component_path`) lines up. This is
    // the structural cousin of the DuckDB 3-table join the spec calls
    // out under impl-plan § 11.
    let tmp = tempfile::tempdir().unwrap();
    let ws = build_oracle_workspace();
    let opts = ExportOptions::builder()
        .out_dir(arc_path(tmp.path()))
        .parsed_at_ms(Some(1))
        .tables(vec![
            SecondaryTable::Dependencies,
            SecondaryTable::Components,
            SecondaryTable::Modules,
        ])
        .build();
    ParquetExporter::new().export(&ws, &opts).unwrap();

    let res_addrs = read_strings(&tmp.path().join("resources.parquet"), "address");
    let dep_froms = read_strings(&tmp.path().join("dependencies.parquet"), "from_address");
    let component_paths = read_strings(&tmp.path().join("components.parquet"), "component_path");

    // Every `from_address` in dependencies.parquet matches a row in
    // resources.parquet (post-expansion address namespace).
    for from in &dep_froms {
        assert!(
            res_addrs.iter().any(|a| a == from),
            "join failure: dependency row `{from}` has no resources match: {res_addrs:?}"
        );
    }
    // Each component in components.parquet has at least one resources row.
    for cp in &component_paths {
        // resources.parquet has its own component_path column.
        let rcp = read_strings(&tmp.path().join("resources.parquet"), "component_path");
        assert!(
            rcp.iter().any(|p| p == cp),
            "component `{cp}` has no resources rows"
        );
    }
}
