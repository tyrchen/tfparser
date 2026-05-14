//! Secondary Parquet tables — `dependencies.parquet`, `components.parquet`,
//! `modules.parquet` — produced alongside `resources.parquet` per
//! [10-data-model.md § 5] and [15-resource-graph.md § 5].
//!
//! Each writer follows the same pattern as the primary `resources` writer
//! (atomic `<file>.partial → rename`, zstd-3 default, deterministic row
//! order, no nullable columns).
//!
//! [10-data-model.md § 5]: ../../../specs/10-data-model.md
//! [15-resource-graph.md § 5]: ../../../specs/15-resource-graph.md

use std::{
    fs::{self, File, OpenOptions},
    io::BufWriter,
    path::{Path, PathBuf},
    sync::Arc,
};

use arrow::{
    array::{
        ArrayRef, BooleanBuilder, ListBuilder, RecordBatch, StringBuilder, UInt32Builder,
        UInt64Builder,
    },
    datatypes::{DataType, Field, Schema},
};
use parquet::{arrow::ArrowWriter, file::properties::WriterProperties};

use super::{ExportError, schema::PARSER_VERSION, writer::CompressionOpt};
use crate::ir::{AccountId, Component, Edge, Module, ModuleSource, Region, Workspace};

// ----------------------------------------------------------------------------
// dependencies.parquet
// ----------------------------------------------------------------------------

/// Build the canonical `dependencies.parquet` schema (spec 10 § 5.1).
#[must_use]
pub fn dependencies_schema() -> Schema {
    Schema::new(vec![
        utf8("from_address"),
        utf8("to_address"),
        utf8("edge_kind"),
        utf8("source_attr"),
        utf8("file"),
        Field::new("line", DataType::UInt32, false),
        Field::new("column", DataType::UInt32, false),
    ])
}

/// Column names in declaration order.
#[must_use]
pub fn dependencies_field_names() -> Vec<&'static str> {
    vec![
        "from_address",
        "to_address",
        "edge_kind",
        "source_attr",
        "file",
        "line",
        "column",
    ]
}

/// Write `dependencies.parquet` to `final_path`. Returns (rows, bytes).
pub(crate) fn write_dependencies_parquet(
    ws: &Workspace,
    final_path: &Path,
    compression: CompressionOpt,
) -> Result<(u64, u64), ExportError> {
    let schema = Arc::new(dependencies_schema());

    // `graph::edges::collect_edges_in_place` already sorts by
    // `(from, to, kind)`. Borrowing directly avoids the redundant
    // re-sort the original `sorted_edges` helper did (P-094 closed).
    let edges: &[Edge] = ws.edges.as_slice();
    debug_assert!(
        edges.iter().zip(edges.iter().skip(1)).all(|(a, b)| (
            a.from.as_str(),
            a.to.as_str(),
            a.kind.as_str()
        ) <= (
            b.from.as_str(),
            b.to.as_str(),
            b.kind.as_str()
        )),
        "graph::edges::collect_edges_in_place must produce sorted edges"
    );
    let rows = edges.len();

    let mut from = StringBuilder::with_capacity(rows, rows * 48);
    let mut to = StringBuilder::with_capacity(rows, rows * 48);
    let mut kind = StringBuilder::with_capacity(rows, rows * 24);
    let mut attr = StringBuilder::with_capacity(rows, rows * 16);
    let mut file = StringBuilder::with_capacity(rows, rows * 48);
    let mut line = UInt32Builder::with_capacity(rows);
    let mut column = UInt32Builder::with_capacity(rows);

    for e in edges {
        from.append_value(e.from.as_str());
        to.append_value(e.to.as_str());
        kind.append_value(e.kind.as_str());
        attr.append_value(e.attr.as_deref().unwrap_or(""));
        file.append_value(render_path(&e.span.file));
        line.append_value(e.span.line);
        column.append_value(e.span.column);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(from.finish()),
        Arc::new(to.finish()),
        Arc::new(kind.finish()),
        Arc::new(attr.finish()),
        Arc::new(file.finish()),
        Arc::new(line.finish()),
        Arc::new(column.finish()),
    ];
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).map_err(|source| {
        ExportError::Arrow {
            path: Arc::from(final_path),
            source,
        }
    })?;

    write_single_batch(final_path, &schema, &batch, compression, rows as u64)
}

// P-094 closed (2026-05-14): the workspace edges are already sorted by
// `graph::edges::collect_edges_in_place`. The helper that re-sorted them
// here is gone; the writer borrows `ws.edges` directly with a
// `debug_assert!` invariant check (see `write_dependencies_parquet`).

// ----------------------------------------------------------------------------
// components.parquet
// ----------------------------------------------------------------------------

/// Build the canonical `components.parquet` schema (spec 15 § 5).
#[must_use]
pub fn components_schema() -> Schema {
    Schema::new(vec![
        utf8("component_path"),
        utf8("kind"),
        Field::new("resource_count", DataType::UInt32, false),
        Field::new("data_count", DataType::UInt32, false),
        Field::new("module_call_count", DataType::UInt32, false),
        Field::new("output_count", DataType::UInt32, false),
        Field::new("variable_count", DataType::UInt32, false),
        Field::new("local_count", DataType::UInt32, false),
        Field::new("provider_count", DataType::UInt32, false),
        Field::new(
            "environments_seen",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, false))),
            false,
        ),
        Field::new("has_terragrunt", DataType::Boolean, false),
        utf8("state_backend_kind"),
        utf8("state_account_id"),
        utf8("state_region"),
        Field::new("unresolved_count", DataType::UInt64, false),
    ])
}

/// Column names in declaration order.
#[must_use]
pub fn components_field_names() -> Vec<&'static str> {
    vec![
        "component_path",
        "kind",
        "resource_count",
        "data_count",
        "module_call_count",
        "output_count",
        "variable_count",
        "local_count",
        "provider_count",
        "environments_seen",
        "has_terragrunt",
        "state_backend_kind",
        "state_account_id",
        "state_region",
        "unresolved_count",
    ]
}

#[allow(clippy::too_many_lines)] // Mirrors the column-by-column writer pattern from `writer.rs`; splitting is churn.
pub(crate) fn write_components_parquet(
    ws: &Workspace,
    final_path: &Path,
    compression: CompressionOpt,
) -> Result<(u64, u64), ExportError> {
    let schema = Arc::new(components_schema());

    // Deterministic order — sort by path. Note `Workspace.components` is
    // already sorted by path (I-GRAPH-5), but tests pass partial
    // workspaces through the writer too.
    let mut sorted: Vec<&Component> = ws.components.iter().collect();
    sorted.sort_by(|a, b| a.path.as_os_str().cmp(b.path.as_os_str()));

    let rows = sorted.len();
    let mut component_path = StringBuilder::with_capacity(rows, rows * 32);
    let mut kind = StringBuilder::with_capacity(rows, rows * 12);
    let mut resource_count = UInt32Builder::with_capacity(rows);
    let mut data_count = UInt32Builder::with_capacity(rows);
    let mut module_call_count = UInt32Builder::with_capacity(rows);
    let mut output_count = UInt32Builder::with_capacity(rows);
    let mut variable_count = UInt32Builder::with_capacity(rows);
    let mut local_count = UInt32Builder::with_capacity(rows);
    let mut provider_count = UInt32Builder::with_capacity(rows);
    let mut environments_seen: ListBuilder<StringBuilder> = ListBuilder::with_capacity(
        StringBuilder::new(),
        rows,
    )
    .with_field(Arc::new(Field::new("item", DataType::Utf8, false)));
    let mut has_terragrunt = BooleanBuilder::with_capacity(rows);
    let mut state_backend_kind = StringBuilder::with_capacity(rows, rows * 8);
    let mut state_account_id = StringBuilder::with_capacity(rows, rows * 12);
    let mut state_region = StringBuilder::with_capacity(rows, rows * 12);
    let mut unresolved_count = UInt64Builder::with_capacity(rows);

    for c in &sorted {
        component_path.append_value(render_path(&c.path));
        kind.append_value(match c.kind {
            crate::ir::ComponentKind::Component => "component",
            crate::ir::ComponentKind::Module => "module",
        });

        let (n_res, n_data) =
            c.resources
                .iter()
                .fold((0u32, 0u32), |(res, data), r| match r.kind {
                    crate::ir::ResourceKind::Managed => (res.saturating_add(1), data),
                    crate::ir::ResourceKind::Data => (res, data.saturating_add(1)),
                });
        resource_count.append_value(n_res);
        data_count.append_value(n_data);
        module_call_count.append_value(c.modules.len().try_into().unwrap_or(u32::MAX));
        output_count.append_value(c.outputs.len().try_into().unwrap_or(u32::MAX));
        variable_count.append_value(c.variables.len().try_into().unwrap_or(u32::MAX));
        local_count.append_value(c.locals.len().try_into().unwrap_or(u32::MAX));
        provider_count.append_value(c.providers.len().try_into().unwrap_or(u32::MAX));

        // `environments_seen` — Phase 6+ populates this from the
        // Terragrunt include chain (`*.terragrunt.hcl` filename roots).
        // Phase 8 ships an empty list; an `if let Some(tg)` block here
        // would expand it once the cascade exposes the data deterministically.
        let inner = environments_seen.values();
        let mut envs: Vec<String> = Vec::new();
        for env in &ws.environments {
            // Discoverable environments are workspace-wide; surface them
            // for every component until the cascade-narrowing pass lands.
            envs.push(env.name.to_string());
        }
        envs.sort();
        envs.dedup();
        for e in envs {
            inner.append_value(&e);
        }
        environments_seen.append(true);

        has_terragrunt.append_value(c.terragrunt.is_some());

        let backend = c
            .terragrunt
            .as_ref()
            .and_then(|tg| tg.state_backend.as_ref())
            .or(c.state_backend.as_ref());
        state_backend_kind.append_value(backend.map_or("", |b| b.kind.as_ref()));
        state_account_id.append_value(
            backend
                .and_then(|b| b.state_account_id.as_ref())
                .map_or("", AccountId::as_str),
        );
        state_region.append_value(
            backend
                .and_then(|b| b.state_region.as_ref())
                .map_or("", Region::as_str),
        );

        let unres = count_unresolved(c);
        unresolved_count.append_value(unres);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(component_path.finish()),
        Arc::new(kind.finish()),
        Arc::new(resource_count.finish()),
        Arc::new(data_count.finish()),
        Arc::new(module_call_count.finish()),
        Arc::new(output_count.finish()),
        Arc::new(variable_count.finish()),
        Arc::new(local_count.finish()),
        Arc::new(provider_count.finish()),
        Arc::new(environments_seen.finish()),
        Arc::new(has_terragrunt.finish()),
        Arc::new(state_backend_kind.finish()),
        Arc::new(state_account_id.finish()),
        Arc::new(state_region.finish()),
        Arc::new(unresolved_count.finish()),
    ];

    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).map_err(|source| {
        ExportError::Arrow {
            path: Arc::from(final_path),
            source,
        }
    })?;
    write_single_batch(final_path, &schema, &batch, compression, rows as u64)
}

fn count_unresolved(c: &Component) -> u64 {
    use crate::ir::Expression;

    fn walk(expr: &Expression, n: &mut u64) {
        match expr {
            Expression::Literal(_) => {}
            Expression::Unresolved(_) => *n = n.saturating_add(1),
            Expression::BinaryOp { lhs, rhs, .. } => {
                walk(lhs, n);
                walk(rhs, n);
            }
            Expression::UnaryOp { operand, .. } => walk(operand, n),
            Expression::TemplateConcat(parts) | Expression::Array(parts) => {
                for p in parts {
                    walk(p, n);
                }
            }
            Expression::Object(entries) => {
                for (k, v) in entries {
                    walk(k, n);
                    walk(v, n);
                }
            }
            Expression::FuncCall(call) => {
                for a in &call.args {
                    walk(a, n);
                }
            }
            Expression::Conditional(cnd) => {
                walk(&cnd.cond, n);
                walk(&cnd.then_branch, n);
                walk(&cnd.else_branch, n);
            }
            Expression::For(f) => {
                walk(&f.collection, n);
                walk(&f.value, n);
                if let Some(k) = &f.key {
                    walk(k, n);
                }
                if let Some(cd) = &f.cond {
                    walk(cd, n);
                }
            }
        }
    }

    let mut total: u64 = 0;
    for r in &c.resources {
        for (_, e) in &r.attributes {
            walk(e, &mut total);
        }
    }
    for m in &c.modules {
        for (_, e) in &m.inputs {
            walk(e, &mut total);
        }
    }
    for o in &c.outputs {
        walk(&o.value, &mut total);
    }
    for l in &c.locals {
        walk(&l.value, &mut total);
    }
    total
}

// ----------------------------------------------------------------------------
// modules.parquet
// ----------------------------------------------------------------------------

/// Build the canonical `modules.parquet` schema (spec 10 § 5.3).
#[must_use]
pub fn modules_schema() -> Schema {
    Schema::new(vec![
        utf8("module_id"),
        utf8("source_raw"),
        utf8("source_kind"),
        utf8("canonical_path"),
        Field::new("call_count", DataType::UInt32, false),
        Field::new("resolved", DataType::Boolean, false),
    ])
}

/// Column names in declaration order.
#[must_use]
pub fn modules_field_names() -> Vec<&'static str> {
    vec![
        "module_id",
        "source_raw",
        "source_kind",
        "canonical_path",
        "call_count",
        "resolved",
    ]
}

pub(crate) fn write_modules_parquet(
    ws: &Workspace,
    final_path: &Path,
    compression: CompressionOpt,
) -> Result<(u64, u64), ExportError> {
    let schema = Arc::new(modules_schema());

    // Index every distinct `ModuleSource` observed across all components'
    // `module "x" { source = ... }` call sites, then emit one row per
    // source. Walked modules (kind=Module in workspace.modules) supply
    // the canonical path; unwalked external sources still appear with
    // an empty `canonical_path`.
    let mut rows: Vec<ModuleRow> = Vec::new();
    let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for c in &ws.components {
        for m in &c.modules {
            let entry = counts.entry(source_key(&m.source)).or_insert(0);
            *entry = entry.saturating_add(1);
        }
    }
    for module in &ws.modules {
        let key = source_key(&module.source);
        let call_count = counts.get(&key).copied().unwrap_or(0);
        rows.push(ModuleRow::from_walked(module, call_count));
    }

    // Surface sources that appeared in a `module "x"` block but did not
    // get a walked-body entry (registry / git / external).
    let walked: std::collections::HashSet<String> =
        ws.modules.iter().map(|m| source_key(&m.source)).collect();
    for c in &ws.components {
        for m in &c.modules {
            let key = source_key(&m.source);
            if walked.contains(&key) {
                continue;
            }
            let call_count = counts.get(&key).copied().unwrap_or(0);
            rows.push(ModuleRow::from_call_site(m, call_count));
        }
    }

    // Dedup by `source_raw` × `source_kind` (the rows already de-emerge
    // from walked / unwalked split, but a `Local` module might also be
    // declared twice with the same source).
    rows.sort_by(|a, b| {
        (a.source_raw.as_str(), a.source_kind.as_str())
            .cmp(&(b.source_raw.as_str(), b.source_kind.as_str()))
    });
    rows.dedup_by(|a, b| a.source_raw == b.source_raw && a.source_kind == b.source_kind);

    let row_count = rows.len();
    let mut id = StringBuilder::with_capacity(row_count, row_count * 8);
    let mut source_raw = StringBuilder::with_capacity(row_count, row_count * 64);
    let mut source_kind = StringBuilder::with_capacity(row_count, row_count * 12);
    let mut canonical_path = StringBuilder::with_capacity(row_count, row_count * 64);
    let mut call_count = UInt32Builder::with_capacity(row_count);
    let mut resolved = BooleanBuilder::with_capacity(row_count);

    for r in &rows {
        id.append_value(&r.module_id);
        source_raw.append_value(&r.source_raw);
        source_kind.append_value(&r.source_kind);
        canonical_path.append_value(&r.canonical_path);
        call_count.append_value(r.call_count);
        resolved.append_value(r.resolved);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(id.finish()),
        Arc::new(source_raw.finish()),
        Arc::new(source_kind.finish()),
        Arc::new(canonical_path.finish()),
        Arc::new(call_count.finish()),
        Arc::new(resolved.finish()),
    ];
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).map_err(|source| {
        ExportError::Arrow {
            path: Arc::from(final_path),
            source,
        }
    })?;
    write_single_batch(final_path, &schema, &batch, compression, row_count as u64)
}

#[derive(Clone, Debug)]
struct ModuleRow {
    module_id: String,
    source_raw: String,
    source_kind: String,
    canonical_path: String,
    call_count: u32,
    resolved: bool,
}

impl ModuleRow {
    fn from_walked(m: &Module, call_count: u32) -> Self {
        Self {
            module_id: format!("{}", m.id.get()),
            source_raw: source_raw_str(&m.source),
            source_kind: source_kind_str(&m.source).to_string(),
            canonical_path: m
                .canonical_path
                .as_deref()
                .map(render_path)
                .unwrap_or_default(),
            call_count,
            resolved: m.canonical_path.is_some(),
        }
    }

    fn from_call_site(m: &crate::ir::ModuleCall, call_count: u32) -> Self {
        Self {
            module_id: String::new(),
            source_raw: m.source_raw.to_string(),
            source_kind: source_kind_str(&m.source).to_string(),
            canonical_path: String::new(),
            call_count,
            resolved: false,
        }
    }
}

fn source_raw_str(s: &ModuleSource) -> String {
    match s {
        ModuleSource::Local(v)
        | ModuleSource::Registry(v)
        | ModuleSource::Git(v)
        | ModuleSource::External(v) => v.to_string(),
    }
}

fn source_kind_str(s: &ModuleSource) -> &'static str {
    match s {
        ModuleSource::Local(_) => "local",
        ModuleSource::Registry(_) => "registry",
        ModuleSource::Git(_) => "git",
        ModuleSource::External(_) => "external",
    }
}

fn source_key(s: &ModuleSource) -> String {
    format!("{}|{}", source_kind_str(s), source_raw_str(s))
}

// ----------------------------------------------------------------------------
// Shared writer plumbing
// ----------------------------------------------------------------------------

fn utf8(name: &'static str) -> Field {
    Field::new(name, DataType::Utf8, false)
}

fn render_path(p: &Path) -> String {
    let mut out = String::with_capacity(p.as_os_str().len());
    for (idx, c) in p.components().enumerate() {
        if idx > 0 {
            out.push('/');
        }
        match c {
            std::path::Component::Normal(s) => out.push_str(&s.to_string_lossy()),
            std::path::Component::ParentDir => out.push_str(".."),
            std::path::Component::CurDir => out.push('.'),
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                out.push_str(&c.as_os_str().to_string_lossy());
            }
        }
    }
    out
}

fn write_single_batch(
    final_path: &Path,
    schema: &Arc<Schema>,
    batch: &RecordBatch,
    compression: CompressionOpt,
    rows: u64,
) -> Result<(u64, u64), ExportError> {
    let partial = partial_path(final_path);
    if partial.exists() {
        fs::remove_file(&partial).map_err(|source| ExportError::Io {
            path: Arc::from(partial.as_path()),
            source,
        })?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|source| ExportError::Io {
            path: Arc::from(partial.as_path()),
            source,
        })?;
    let buf = BufWriter::with_capacity(64 * 1024, file);

    let props = WriterProperties::builder()
        .set_compression(compression.to_parquet_compression())
        .set_key_value_metadata(Some(vec![
            parquet::file::metadata::KeyValue::new(
                "tfparser.schema.major".to_string(),
                Some(super::SCHEMA_MAJOR.to_string()),
            ),
            parquet::file::metadata::KeyValue::new(
                "tfparser.schema.minor".to_string(),
                Some(super::SCHEMA_MINOR.to_string()),
            ),
            parquet::file::metadata::KeyValue::new(
                "tfparser.parser.version".to_string(),
                Some(PARSER_VERSION.to_string()),
            ),
        ]))
        .build();
    let mut writer =
        ArrowWriter::try_new(buf, Arc::clone(schema), Some(props)).map_err(|source| {
            ExportError::Parquet {
                path: Arc::from(partial.as_path()),
                source,
            }
        })?;
    if rows > 0 {
        writer.write(batch).map_err(|source| ExportError::Parquet {
            path: Arc::from(partial.as_path()),
            source,
        })?;
    }
    let inner: BufWriter<File> = writer.into_inner().map_err(|source| ExportError::Parquet {
        path: Arc::from(partial.as_path()),
        source,
    })?;
    let file = inner.into_inner().map_err(|err| ExportError::Io {
        path: Arc::from(partial.as_path()),
        source: err.into_error(),
    })?;
    file.sync_all().map_err(|source| ExportError::Io {
        path: Arc::from(partial.as_path()),
        source,
    })?;
    drop(file);

    fs::rename(&partial, final_path).map_err(|source| ExportError::Io {
        path: Arc::from(partial.as_path()),
        source,
    })?;
    let bytes = fs::metadata(final_path)
        .map(|m| m.len())
        .map_err(|source| ExportError::Io {
            path: Arc::from(final_path),
            source,
        })?;
    Ok((rows, bytes))
}

fn partial_path(final_path: &Path) -> PathBuf {
    let mut s: std::ffi::OsString = final_path.as_os_str().to_os_string();
    s.push(".partial");
    PathBuf::from(s)
}

// `EdgeKind::as_str` is in the IR; convenience local trait shim for
// CompressionOpt → parquet::basic::Compression mapping (the writer
// module's helper is private).
trait CompressionOptExt {
    fn to_parquet_compression(self) -> parquet::basic::Compression;
}

impl CompressionOptExt for CompressionOpt {
    fn to_parquet_compression(self) -> parquet::basic::Compression {
        use parquet::basic::{Compression, ZstdLevel};
        match self {
            Self::Uncompressed => Compression::UNCOMPRESSED,
            Self::Zstd(level) => Compression::ZSTD(ZstdLevel::try_new(level).unwrap_or_default()),
            Self::Snappy => Compression::SNAPPY,
        }
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
    fn test_dependencies_schema_columns_match_documented() {
        let s = dependencies_schema();
        let got: Vec<&str> = s.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(got, dependencies_field_names());
        assert_eq!(s.fields().len(), 7);
    }

    #[test]
    fn test_components_schema_columns_match_documented() {
        let s = components_schema();
        let got: Vec<&str> = s.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(got, components_field_names());
    }

    #[test]
    fn test_modules_schema_columns_match_documented() {
        let s = modules_schema();
        let got: Vec<&str> = s.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(got, modules_field_names());
    }

    #[test]
    fn test_no_nullable_columns_in_secondary_schemas() {
        for s in [dependencies_schema(), components_schema(), modules_schema()] {
            for f in s.fields() {
                assert!(!f.is_nullable(), "column `{}` nullable", f.name());
            }
        }
    }
}
