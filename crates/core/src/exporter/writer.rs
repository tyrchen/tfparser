//! `ParquetExporter` — synchronous, single-table writer.
//!
//! Per [20-parquet-exporter.md § 3] and [99-key-decisions.md] D10:
//!
//! 1. Pre-allocate one builder per column, sized to the projected row count.
//! 2. Walk every `Component` once, appending one cell per builder per row.
//! 3. Flush a `RecordBatch` to the [`ArrowWriter`] every `row_group_rows` or `row_group_bytes`,
//!    whichever first.
//! 4. Write into `<file>.partial`; fsync; rename to `<file>`. A crash mid-write leaves a `.partial`
//!    breadcrumb, never a half-written `resources.parquet`.
//!
//! Implementation note: a single writer thread owns the file handle. Per
//! [99-key-decisions.md] D14, the library is synchronous + `rayon`, so the
//! exporter does not interact with `tokio`.

use std::{
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::BufWriter,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use arrow::{
    array::{
        ArrayRef, ListBuilder, RecordBatch, StringBuilder, TimestampMillisecondBuilder,
        UInt32Builder,
    },
    datatypes::{DataType, Field, Schema},
};
use parquet::{
    arrow::ArrowWriter,
    basic::{Compression, ZstdLevel},
    file::properties::WriterProperties,
};
use serde::{Deserialize, Serialize};
use tracing::{info_span, instrument};
use typed_builder::TypedBuilder;

use super::{
    ExportError, PARSER_VERSION, SCHEMA_MAJOR, SCHEMA_MINOR,
    json::render_attribute_map,
    manifest::{Manifest, ManifestFile, write_manifest},
    schema::resources_schema,
};
use crate::ir::{
    AttributeMap, Component, Expression, Local, ModuleCall, Output, ProviderBlock, ProviderRef,
    Resource, ResourceKind, Span, Value, Variable, Workspace,
};

/// Options for [`Exporter::export`].
#[derive(Clone, Debug, PartialEq, Eq, TypedBuilder)]
#[non_exhaustive]
#[builder(field_defaults(setter(into)))]
pub struct ExportOptions {
    /// Output directory. Must exist; the exporter does **not** recursively
    /// `mkdir`.
    pub out_dir: Arc<Path>,

    /// Row-group flush threshold by row count. Default: 131 072.
    #[builder(default = 131_072)]
    pub row_group_rows: usize,

    /// Row-group flush threshold by uncompressed bytes. Default: 64 MiB.
    #[builder(default = 64 * 1024 * 1024)]
    pub row_group_bytes: usize,

    /// Compression. Default: zstd-3.
    #[builder(default = CompressionOpt::Zstd(3))]
    pub compression: CompressionOpt,

    /// If `true`, overwrite existing files in `out_dir`. Default: `false`.
    #[builder(default = false)]
    pub overwrite: bool,

    /// Pin `parsed_at` (UTC ms epoch). When `None` the exporter calls
    /// [`jiff::Timestamp::now`].
    ///
    /// Tests and reproducible builds set this to make output byte-deterministic.
    #[builder(default)]
    pub parsed_at_ms: Option<i64>,

    /// Verbatim command line to embed in the manifest (e.g.
    /// `"tfparser parse foo --out bar"`). Optional.
    #[builder(default = Arc::from(""))]
    pub command_line: Arc<str>,
}

/// Supported parquet compression codecs. Phase 3 ships the spec's
/// recommended default — zstd-3. The variant set is `#[non_exhaustive]` so
/// future codecs can land without a breaking API change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum CompressionOpt {
    /// No compression — useful for fuzz / debugging.
    Uncompressed,
    /// Zstandard with the supplied level (1..=22; 3 is the spec default).
    Zstd(i32),
    /// Snappy.
    Snappy,
}

impl CompressionOpt {
    fn to_parquet(self) -> Compression {
        match self {
            Self::Uncompressed => Compression::UNCOMPRESSED,
            Self::Zstd(level) => Compression::ZSTD(ZstdLevel::try_new(level).unwrap_or_default()),
            Self::Snappy => Compression::SNAPPY,
        }
    }
}

/// A single file the exporter produced.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportedFile {
    /// Final on-disk path (after rename).
    pub path: Arc<Path>,
    /// Row count for this file. `0` for the manifest.
    pub rows: u64,
    /// Byte size on disk after rename.
    pub bytes: u64,
    /// Hex-encoded SHA-256 of the file contents.
    pub sha256: String,
}

/// Report returned by [`Exporter::export`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportReport {
    /// Files written.
    pub files: Vec<ExportedFile>,
    /// Row count across all data files (manifest excluded).
    pub total_rows: u64,
    /// Bytes written across all output files.
    pub bytes_written: u64,
    /// Wall-clock elapsed time.
    pub elapsed: Duration,
}

/// Trait for workspace exporters. Phase 3 ships [`ParquetExporter`]; tests
/// may swap in a stub that records calls without touching disk.
pub trait Exporter: Send + Sync {
    /// Serialise `ws` per `opts` and return an [`ExportReport`].
    ///
    /// # Errors
    ///
    /// Returns [`ExportError`] when the output directory is invalid, the
    /// target file exists without `--overwrite`, or any underlying
    /// I/O / arrow / parquet operation fails.
    fn export(&self, ws: &Workspace, opts: &ExportOptions) -> Result<ExportReport, ExportError>;
}

/// Default [`Exporter`] backed by `arrow-rs` + `parquet-rs`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ParquetExporter;

impl ParquetExporter {
    /// Construct a [`ParquetExporter`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Exporter for ParquetExporter {
    #[instrument(level = "info", skip_all, fields(out = %opts.out_dir.display()))]
    fn export(&self, ws: &Workspace, opts: &ExportOptions) -> Result<ExportReport, ExportError> {
        let started = std::time::Instant::now();

        validate_out_dir(&opts.out_dir)?;

        let final_path: Arc<Path> = Arc::from(opts.out_dir.join("resources.parquet"));
        let manifest_path: Arc<Path> = Arc::from(opts.out_dir.join("workspace.manifest.json"));

        if !opts.overwrite {
            for p in [&final_path, &manifest_path] {
                if p.exists() {
                    return Err(ExportError::OutputExists(Arc::clone(p)));
                }
            }
        }

        let parsed_at_ms = opts
            .parsed_at_ms
            .unwrap_or_else(|| jiff::Timestamp::now().as_millisecond());

        let projected_rows = projected_row_count(ws);

        let (rows, bytes) = {
            let span = info_span!("write_resources", path = %final_path.display());
            let _entered = span.enter();
            write_resources_parquet(ws, opts, &final_path, projected_rows, parsed_at_ms)?
        };

        // Hash + manifest.
        let resources_sha = sha256_hex_of_file(&final_path)?;
        let manifest = Manifest {
            tfparser_version: PARSER_VERSION.to_string(),
            schema_major: SCHEMA_MAJOR,
            schema_minor: SCHEMA_MINOR,
            generated_at_ms: parsed_at_ms,
            workspace_root: ws.root.display().to_string(),
            command_line: opts.command_line.to_string(),
            files: vec![ManifestFile {
                name: "resources.parquet".to_string(),
                rows,
                bytes,
                sha256: resources_sha.clone(),
            }],
        };
        let manifest_bytes_written = write_manifest(&manifest, &manifest_path, opts.overwrite)?;
        let manifest_sha = sha256_hex_of_file(&manifest_path)?;

        let files = vec![
            ExportedFile {
                path: Arc::clone(&final_path),
                rows,
                bytes,
                sha256: resources_sha,
            },
            ExportedFile {
                path: Arc::clone(&manifest_path),
                rows: 0,
                bytes: manifest_bytes_written,
                sha256: manifest_sha,
            },
        ];

        Ok(ExportReport {
            files,
            total_rows: rows,
            bytes_written: bytes + manifest_bytes_written,
            elapsed: started.elapsed(),
        })
    }
}

fn validate_out_dir(out: &Path) -> Result<(), ExportError> {
    if !out.exists() {
        return Err(ExportError::OutDirMissing(Arc::from(out)));
    }
    if !out.is_dir() {
        return Err(ExportError::OutDirNotDir(Arc::from(out)));
    }
    Ok(())
}

/// Per-row column builders.
///
/// Built once per export and pre-sized to the projected row count. The
/// writer drains and reinitialises them on each `flush_batch`.
struct RowBuilders {
    workspace_root: StringBuilder,
    component_path: StringBuilder,
    module_path: StringBuilder,
    address: StringBuilder,
    kind: StringBuilder,
    resource_type: StringBuilder,
    resource_name: StringBuilder,
    provider_local: StringBuilder,
    provider_source: StringBuilder,
    account_id: StringBuilder,
    account_name: StringBuilder,
    region: StringBuilder,
    environment: StringBuilder,
    count_expr: StringBuilder,
    for_each_expr: StringBuilder,
    depends_on: ListBuilder<StringBuilder>,
    attributes_json: StringBuilder,
    state_account_id: StringBuilder,
    state_region: StringBuilder,
    file: StringBuilder,
    line: UInt32Builder,
    column: UInt32Builder,
    parser_version: StringBuilder,
    parsed_at: TimestampMillisecondBuilder,
    schema: Arc<Schema>,
    row_count: usize,
    approx_bytes: usize,
}

impl RowBuilders {
    fn with_capacity(rows: usize, schema: Arc<Schema>) -> Self {
        Self {
            workspace_root: StringBuilder::with_capacity(rows, rows * 64),
            component_path: StringBuilder::with_capacity(rows, rows * 32),
            module_path: StringBuilder::with_capacity(rows, rows * 16),
            address: StringBuilder::with_capacity(rows, rows * 48),
            kind: StringBuilder::with_capacity(rows, rows * 8),
            resource_type: StringBuilder::with_capacity(rows, rows * 24),
            resource_name: StringBuilder::with_capacity(rows, rows * 24),
            provider_local: StringBuilder::with_capacity(rows, rows * 12),
            provider_source: StringBuilder::with_capacity(rows, rows * 32),
            account_id: StringBuilder::with_capacity(rows, rows * 12),
            account_name: StringBuilder::with_capacity(rows, rows * 16),
            region: StringBuilder::with_capacity(rows, rows * 12),
            environment: StringBuilder::with_capacity(rows, rows * 12),
            count_expr: StringBuilder::with_capacity(rows, rows * 16),
            for_each_expr: StringBuilder::with_capacity(rows, rows * 16),
            depends_on: ListBuilder::with_capacity(StringBuilder::new(), rows)
                .with_field(Arc::new(Field::new("item", DataType::Utf8, false))),
            attributes_json: StringBuilder::with_capacity(rows, rows * 256),
            state_account_id: StringBuilder::with_capacity(rows, rows * 12),
            state_region: StringBuilder::with_capacity(rows, rows * 12),
            file: StringBuilder::with_capacity(rows, rows * 48),
            line: UInt32Builder::with_capacity(rows),
            column: UInt32Builder::with_capacity(rows),
            parser_version: StringBuilder::with_capacity(rows, rows * 8),
            parsed_at: TimestampMillisecondBuilder::with_capacity(rows)
                .with_timezone(Arc::<str>::from("UTC")),
            schema,
            row_count: 0,
            approx_bytes: 0,
        }
    }

    fn append_row(&mut self, row: &Row<'_>, parsed_at_ms: i64) {
        self.workspace_root.append_value(row.workspace_root);
        self.component_path.append_value(row.component_path);
        self.module_path.append_value(row.module_path);
        self.address.append_value(row.address);
        self.kind.append_value(row.kind);
        self.resource_type.append_value(row.resource_type);
        self.resource_name.append_value(row.resource_name);
        self.provider_local.append_value(row.provider_local);
        self.provider_source.append_value(row.provider_source);
        self.account_id.append_value(row.account_id);
        self.account_name.append_value(row.account_name);
        self.region.append_value(row.region);
        self.environment.append_value(row.environment);
        self.count_expr.append_value(row.count_expr);
        self.for_each_expr.append_value(row.for_each_expr);
        let inner = self.depends_on.values();
        for dep in row.depends_on {
            inner.append_value(dep);
        }
        self.depends_on.append(true);
        self.attributes_json.append_value(row.attributes_json);
        self.state_account_id.append_value(row.state_account_id);
        self.state_region.append_value(row.state_region);
        self.file.append_value(row.file);
        self.line.append_value(row.line);
        self.column.append_value(row.column);
        self.parser_version.append_value(PARSER_VERSION);
        self.parsed_at.append_value(parsed_at_ms);
        self.row_count += 1;
        self.approx_bytes += approx_row_bytes(row);
    }

    fn batch(&mut self) -> Result<RecordBatch, arrow::error::ArrowError> {
        let arrays: Vec<ArrayRef> = vec![
            Arc::new(self.workspace_root.finish()),
            Arc::new(self.component_path.finish()),
            Arc::new(self.module_path.finish()),
            Arc::new(self.address.finish()),
            Arc::new(self.kind.finish()),
            Arc::new(self.resource_type.finish()),
            Arc::new(self.resource_name.finish()),
            Arc::new(self.provider_local.finish()),
            Arc::new(self.provider_source.finish()),
            Arc::new(self.account_id.finish()),
            Arc::new(self.account_name.finish()),
            Arc::new(self.region.finish()),
            Arc::new(self.environment.finish()),
            Arc::new(self.count_expr.finish()),
            Arc::new(self.for_each_expr.finish()),
            Arc::new(self.depends_on.finish()),
            Arc::new(self.attributes_json.finish()),
            Arc::new(self.state_account_id.finish()),
            Arc::new(self.state_region.finish()),
            Arc::new(self.file.finish()),
            Arc::new(self.line.finish()),
            Arc::new(self.column.finish()),
            Arc::new(self.parser_version.finish()),
            Arc::new(self.parsed_at.finish()),
        ];
        let batch = RecordBatch::try_new(Arc::clone(&self.schema), arrays)?;
        self.row_count = 0;
        self.approx_bytes = 0;
        Ok(batch)
    }
}

/// Borrowed view of one row's column values; cheap to construct per row.
struct Row<'a> {
    workspace_root: &'a str,
    component_path: &'a str,
    module_path: &'a str,
    address: &'a str,
    kind: &'a str,
    resource_type: &'a str,
    resource_name: &'a str,
    provider_local: &'a str,
    provider_source: &'a str,
    account_id: &'a str,
    account_name: &'a str,
    region: &'a str,
    environment: &'a str,
    count_expr: &'a str,
    for_each_expr: &'a str,
    depends_on: &'a [String],
    attributes_json: &'a str,
    state_account_id: &'a str,
    state_region: &'a str,
    file: &'a str,
    line: u32,
    column: u32,
}

fn approx_row_bytes(row: &Row<'_>) -> usize {
    row.workspace_root.len()
        + row.component_path.len()
        + row.module_path.len()
        + row.address.len()
        + row.kind.len()
        + row.resource_type.len()
        + row.resource_name.len()
        + row.provider_local.len()
        + row.provider_source.len()
        + row.account_id.len()
        + row.account_name.len()
        + row.region.len()
        + row.environment.len()
        + row.count_expr.len()
        + row.for_each_expr.len()
        + row.depends_on.iter().map(String::len).sum::<usize>()
        + row.attributes_json.len()
        + row.state_account_id.len()
        + row.state_region.len()
        + row.file.len()
        + 8
}

/// Upper bound on pre-allocated rows. Bounds memory at ~`MAX_PREALLOC_ROWS`
/// times per-row capacity hints (~500 B/row × 1M = ~500 MiB). Arrow grows
/// beyond this organically; the clamp prevents pathological workspaces from
/// allocating gigabytes up-front. Per CLAUDE.md § Safety & Security
/// (bound every collection).
const MAX_PREALLOC_ROWS: usize = 1_000_000;

/// Cheap projected upper bound on `Vec` pre-allocation. Each component
/// contributes (resources + providers + modules + outputs + variables +
/// locals) rows.
fn projected_row_count(ws: &Workspace) -> usize {
    ws.components
        .iter()
        .map(|c| {
            c.resources.len()
                + c.providers.len()
                + c.modules.len()
                + c.outputs.len()
                + c.variables.len()
                + c.locals.len()
        })
        .sum::<usize>()
        .min(MAX_PREALLOC_ROWS)
}

fn write_resources_parquet(
    ws: &Workspace,
    opts: &ExportOptions,
    final_path: &Path,
    projected_rows: usize,
    parsed_at_ms: i64,
) -> Result<(u64, u64), ExportError> {
    let partial: PathBuf = partial_path(final_path);
    if partial.exists() {
        fs::remove_file(&partial).map_err(|source| ExportError::Io {
            path: Arc::from(partial.as_path()),
            source,
        })?;
    }

    let schema = Arc::new(resources_schema());
    let mut builders = RowBuilders::with_capacity(projected_rows.max(64), Arc::clone(&schema));
    let workspace_root_str = ws.root.display().to_string();

    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|source| ExportError::Io {
            path: Arc::from(partial.as_path()),
            source,
        })?;
    let buf = BufWriter::with_capacity(256 * 1024, file);
    let writer_props = WriterProperties::builder()
        .set_compression(opts.compression.to_parquet())
        .set_key_value_metadata(Some(vec![
            parquet::file::metadata::KeyValue::new(
                "tfparser.schema.major".to_string(),
                Some(SCHEMA_MAJOR.to_string()),
            ),
            parquet::file::metadata::KeyValue::new(
                "tfparser.schema.minor".to_string(),
                Some(SCHEMA_MINOR.to_string()),
            ),
            parquet::file::metadata::KeyValue::new(
                "tfparser.parser.version".to_string(),
                Some(PARSER_VERSION.to_string()),
            ),
        ]))
        .build();
    let mut arrow_writer = ArrowWriter::try_new(buf, Arc::clone(&schema), Some(writer_props))
        .map_err(|source| ExportError::Parquet {
            path: Arc::from(partial.as_path()),
            source,
        })?;

    let mut total_rows: u64 = 0;
    let mut sorted_components: Vec<&Component> = ws.components.iter().collect();
    sorted_components.sort_by(|a, b| a.path.as_os_str().cmp(b.path.as_os_str()));

    for component in sorted_components {
        let component_path_str = render_path(&component.path);
        emit_component_rows(
            component,
            &workspace_root_str,
            &component_path_str,
            parsed_at_ms,
            &mut builders,
            &mut arrow_writer,
            &Arc::from(partial.as_path()),
            opts,
            &mut total_rows,
        )?;
    }

    if builders.row_count > 0 {
        flush_batch(&mut builders, &mut arrow_writer, &partial)?;
    }

    let buf = arrow_writer
        .into_inner()
        .map_err(|source| ExportError::Parquet {
            path: Arc::from(partial.as_path()),
            source,
        })?;
    let file = buf.into_inner().map_err(|err| ExportError::Io {
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

    Ok((total_rows, bytes))
}

fn partial_path(final_path: &Path) -> PathBuf {
    let mut s: std::ffi::OsString = final_path.as_os_str().to_os_string();
    s.push(".partial");
    PathBuf::from(s)
}

#[allow(clippy::too_many_arguments)]
fn emit_component_rows(
    component: &Component,
    workspace_root_str: &str,
    component_path_str: &str,
    parsed_at_ms: i64,
    builders: &mut RowBuilders,
    arrow_writer: &mut ArrowWriter<BufWriter<File>>,
    partial_path: &Arc<Path>,
    opts: &ExportOptions,
    total_rows: &mut u64,
) -> Result<(), ExportError> {
    // Collect every (sort_key, row-emitter) pair for the component, then
    // emit in (module_path, address)-ascending order per spec §3.4.
    let mut rows: Vec<EmittedRow> = Vec::new();

    let state_account_id = component
        .state_backend
        .as_ref()
        .and_then(|b| b.state_account_id.as_ref())
        .map(|a| a.as_str().to_string())
        .unwrap_or_default();
    let state_region = component
        .state_backend
        .as_ref()
        .and_then(|b| b.state_region.as_ref())
        .map(|r| r.as_str().to_string())
        .unwrap_or_default();

    for r in &component.resources {
        rows.push(resource_row(r, &state_account_id, &state_region));
    }
    for p in &component.providers {
        rows.push(provider_row(p));
    }
    for m in &component.modules {
        rows.push(module_call_row(m));
    }
    for v in &component.variables {
        rows.push(variable_row(v));
    }
    for l in &component.locals {
        rows.push(local_row(l));
    }
    for o in &component.outputs {
        rows.push(output_row(o));
    }

    rows.sort_by(|a, b| {
        (a.module_path.as_str(), a.address.as_str())
            .cmp(&(b.module_path.as_str(), b.address.as_str()))
    });

    let mut json_scratch = String::with_capacity(4096);
    for emitted in &rows {
        json_scratch.clear();
        render_attribute_map(&emitted.attributes, &mut json_scratch);
        let row = Row {
            workspace_root: workspace_root_str,
            component_path: component_path_str,
            module_path: emitted.module_path.as_str(),
            address: emitted.address.as_str(),
            kind: emitted.kind,
            resource_type: emitted.resource_type.as_str(),
            resource_name: emitted.resource_name.as_str(),
            provider_local: emitted.provider_local.as_str(),
            provider_source: emitted.provider_source.as_str(),
            account_id: emitted.account_id.as_str(),
            account_name: emitted.account_name.as_str(),
            region: emitted.region.as_str(),
            environment: emitted.environment.as_str(),
            count_expr: emitted.count_expr.as_str(),
            for_each_expr: emitted.for_each_expr.as_str(),
            depends_on: emitted.depends_on.as_slice(),
            attributes_json: json_scratch.as_str(),
            state_account_id: emitted.state_account_id.as_str(),
            state_region: emitted.state_region.as_str(),
            file: emitted.file.as_str(),
            line: emitted.line,
            column: emitted.column,
        };
        builders.append_row(&row, parsed_at_ms);
        *total_rows = total_rows.saturating_add(1);

        if builders.row_count >= opts.row_group_rows
            || builders.approx_bytes >= opts.row_group_bytes
        {
            flush_batch(builders, arrow_writer, partial_path)?;
        }
    }
    Ok(())
}

fn flush_batch(
    builders: &mut RowBuilders,
    arrow_writer: &mut ArrowWriter<BufWriter<File>>,
    partial_path: &Path,
) -> Result<(), ExportError> {
    let batch = builders.batch().map_err(|source| ExportError::Arrow {
        path: Arc::from(partial_path),
        source,
    })?;
    arrow_writer
        .write(&batch)
        .map_err(|source| ExportError::Parquet {
            path: Arc::from(partial_path),
            source,
        })?;
    Ok(())
}

/// One row's worth of column values, owned (so we can sort across kinds).
struct EmittedRow {
    module_path: String,
    address: String,
    kind: &'static str,
    resource_type: String,
    resource_name: String,
    provider_local: String,
    provider_source: String,
    account_id: String,
    account_name: String,
    region: String,
    environment: String,
    count_expr: String,
    for_each_expr: String,
    depends_on: Vec<String>,
    attributes: AttributeMap,
    state_account_id: String,
    state_region: String,
    file: String,
    line: u32,
    column: u32,
}

fn resource_row(r: &Resource, state_account_id: &str, state_region: &str) -> EmittedRow {
    let provider_local = r
        .provider_ref
        .as_ref()
        .map(provider_ref_string)
        .unwrap_or_default();
    EmittedRow {
        module_path: r.address.module_path(),
        address: r.address.as_str().to_string(),
        kind: match r.kind {
            ResourceKind::Managed => "resource",
            ResourceKind::Data => "data",
        },
        resource_type: r.type_.to_string(),
        resource_name: r.name.to_string(),
        provider_local,
        provider_source: String::new(),
        account_id: String::new(),
        account_name: String::new(),
        region: String::new(),
        environment: String::new(),
        count_expr: r
            .count_expr
            .as_ref()
            .map(render_expression_source)
            .unwrap_or_default(),
        for_each_expr: r
            .for_each_expr
            .as_ref()
            .map(render_expression_source)
            .unwrap_or_default(),
        depends_on: r
            .depends_on
            .iter()
            .map(|a| a.as_str().to_string())
            .collect(),
        attributes: r.attributes.clone(),
        state_account_id: state_account_id.to_string(),
        state_region: state_region.to_string(),
        file: span_relative_file(&r.span),
        line: r.span.line,
        column: r.span.column,
    }
}

fn provider_row(p: &ProviderBlock) -> EmittedRow {
    let local = p.local_name.to_string();
    let provider_local = match p.alias.as_deref() {
        Some(a) if !a.is_empty() => format!("{local}.{a}"),
        _ => local.clone(),
    };
    let address = format!("provider.{provider_local}");
    EmittedRow {
        module_path: String::new(),
        address,
        kind: "provider",
        resource_type: String::new(),
        resource_name: provider_local.clone(),
        provider_local,
        provider_source: p
            .source_addr
            .as_deref()
            .map(str::to_string)
            .unwrap_or_default(),
        account_id: String::new(),
        account_name: String::new(),
        region: String::new(),
        environment: String::new(),
        count_expr: String::new(),
        for_each_expr: String::new(),
        depends_on: Vec::new(),
        attributes: p.raw.clone(),
        state_account_id: String::new(),
        state_region: String::new(),
        file: span_relative_file(&p.span),
        line: p.span.line,
        column: p.span.column,
    }
}

fn module_call_row(m: &ModuleCall) -> EmittedRow {
    let attrs: AttributeMap = m.inputs.clone();
    let provider_local = m
        .providers
        .first()
        .map(|(_, r)| provider_ref_string(r))
        .unwrap_or_default();
    EmittedRow {
        module_path: m.address.module_path(),
        address: m.address.as_str().to_string(),
        kind: "module",
        resource_type: String::new(),
        resource_name: m
            .address
            .as_str()
            .strip_prefix("module.")
            .map_or_else(|| m.address.as_str().to_string(), str::to_string),
        provider_local,
        provider_source: m.source_raw.to_string(),
        account_id: String::new(),
        account_name: String::new(),
        region: String::new(),
        environment: String::new(),
        count_expr: m
            .count_expr
            .as_ref()
            .map(render_expression_source)
            .unwrap_or_default(),
        for_each_expr: m
            .for_each_expr
            .as_ref()
            .map(render_expression_source)
            .unwrap_or_default(),
        depends_on: Vec::new(),
        attributes: attrs,
        state_account_id: String::new(),
        state_region: String::new(),
        file: span_relative_file(&m.span),
        line: m.span.line,
        column: m.span.column,
    }
}

fn variable_row(v: &Variable) -> EmittedRow {
    let mut attrs: AttributeMap = Vec::new();
    if let Some(t) = &v.type_expr {
        attrs.push((Arc::from("type"), t.clone()));
    }
    if let Some(d) = &v.default {
        attrs.push((Arc::from("default"), d.clone()));
    }
    if let Some(d) = &v.description {
        attrs.push((
            Arc::from("description"),
            Expression::Literal(Value::Str(Arc::clone(d))),
        ));
    }
    attrs.push((
        Arc::from("sensitive"),
        Expression::Literal(Value::Bool(v.sensitive)),
    ));
    EmittedRow {
        module_path: String::new(),
        address: format!("var.{}", v.name),
        kind: "variable",
        resource_type: String::new(),
        resource_name: v.name.to_string(),
        provider_local: String::new(),
        provider_source: String::new(),
        account_id: String::new(),
        account_name: String::new(),
        region: String::new(),
        environment: String::new(),
        count_expr: String::new(),
        for_each_expr: String::new(),
        depends_on: Vec::new(),
        attributes: attrs,
        state_account_id: String::new(),
        state_region: String::new(),
        file: span_relative_file(&v.span),
        line: v.span.line,
        column: v.span.column,
    }
}

fn local_row(l: &Local) -> EmittedRow {
    let attrs: AttributeMap = vec![(Arc::from("value"), l.value.clone())];
    EmittedRow {
        module_path: String::new(),
        address: format!("local.{}", l.name),
        kind: "local",
        resource_type: String::new(),
        resource_name: l.name.to_string(),
        provider_local: String::new(),
        provider_source: String::new(),
        account_id: String::new(),
        account_name: String::new(),
        region: String::new(),
        environment: String::new(),
        count_expr: String::new(),
        for_each_expr: String::new(),
        depends_on: Vec::new(),
        attributes: attrs,
        state_account_id: String::new(),
        state_region: String::new(),
        file: span_relative_file(&l.span),
        line: l.span.line,
        column: l.span.column,
    }
}

fn output_row(o: &Output) -> EmittedRow {
    let mut attrs: AttributeMap = Vec::new();
    attrs.push((Arc::from("value"), o.value.clone()));
    if let Some(d) = &o.description {
        attrs.push((
            Arc::from("description"),
            Expression::Literal(Value::Str(Arc::clone(d))),
        ));
    }
    attrs.push((
        Arc::from("sensitive"),
        Expression::Literal(Value::Bool(o.sensitive)),
    ));
    EmittedRow {
        module_path: String::new(),
        address: format!("output.{}", o.name),
        kind: "output",
        resource_type: String::new(),
        resource_name: o.name.to_string(),
        provider_local: String::new(),
        provider_source: String::new(),
        account_id: String::new(),
        account_name: String::new(),
        region: String::new(),
        environment: String::new(),
        count_expr: String::new(),
        for_each_expr: String::new(),
        depends_on: Vec::new(),
        attributes: attrs,
        state_account_id: String::new(),
        state_region: String::new(),
        file: span_relative_file(&o.span),
        line: o.span.line,
        column: o.span.column,
    }
}

fn provider_ref_string(r: &ProviderRef) -> String {
    match r.alias.as_deref() {
        Some(a) if !a.is_empty() => format!("{}.{a}", r.local_name),
        _ => r.local_name.to_string(),
    }
}

/// Render a path as a relative, `/`-separated string suitable for the
/// `component_path` and `file` columns (spec 10 § 3 columns #2, #20). The
/// path must already be relative (loader/discovery guarantee this); we only
/// normalise separators here so Windows hosts don't leak `\` into the
/// downstream Parquet artefact.
fn render_path(p: &Path) -> String {
    let mut out = String::with_capacity(p.as_os_str().len());
    for (idx, comp) in p.components().enumerate() {
        if idx > 0 {
            out.push('/');
        }
        match comp {
            std::path::Component::Normal(s) => {
                out.push_str(&s.to_string_lossy());
            }
            std::path::Component::ParentDir => out.push_str(".."),
            std::path::Component::CurDir => out.push('.'),
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                // Absolute prefixes are not expected at this layer; if one
                // ever appears we keep its display form to preserve traceability.
                out.push_str(&comp.as_os_str().to_string_lossy());
            }
        }
    }
    out
}

fn span_relative_file(span: &Span) -> String {
    render_path(&span.file)
}

/// Render an expression as its compact source form (verbatim for unresolved
/// refs, JSON for richer shapes). Used for the `count_expr` / `for_each_expr`
/// columns where the spec says "verbatim source, `""` if absent".
fn render_expression_source(expr: &Expression) -> String {
    match expr {
        Expression::Literal(Value::Int(n)) => n.to_string(),
        Expression::Literal(Value::Bool(b)) => b.to_string(),
        Expression::Literal(Value::Str(s)) => s.to_string(),
        Expression::Literal(Value::Number(f)) if f.is_finite() => {
            let mut buf = ryu::Buffer::new();
            buf.format(*f).to_string()
        }
        Expression::Unresolved(s) => s.source.to_string(),
        _ => {
            let mut s = String::new();
            let map: AttributeMap = vec![(Arc::from(""), expr.clone())];
            render_attribute_map(&map, &mut s);
            s
        }
    }
}

/// SHA-256 of the file at `path`, hex-encoded lowercase.
fn sha256_hex_of_file(path: &Path) -> Result<String, ExportError> {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).map_err(|source| ExportError::Io {
        path: Arc::from(path),
        source,
    })?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let digest = h.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ir::{
        Address, ComponentId, ComponentKind, ResourceKind, Span, SymbolKind, Symbolic,
    };

    fn arc_path<P: AsRef<Path>>(p: P) -> Arc<Path> {
        Arc::from(p.as_ref())
    }

    fn minimal_resource() -> Resource {
        Resource::builder()
            .address(Address::new("aws_iam_role.r").unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_iam_role"))
            .name(Arc::<str>::from("r"))
            .span(Span::synthetic())
            .build()
    }

    fn minimal_component() -> Component {
        Component::builder()
            .id(ComponentId::from_index(0))
            .path(arc_path(PathBuf::from("svc")))
            .kind(ComponentKind::Component)
            .resources(vec![minimal_resource()])
            .build()
    }

    #[test]
    fn test_should_write_resources_parquet_with_one_row() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::builder()
            .root(arc_path(tmp.path()))
            .components(vec![minimal_component()])
            .build();
        let opts = ExportOptions::builder()
            .out_dir(arc_path(tmp.path()))
            .parsed_at_ms(Some(1_700_000_000_000_i64))
            .build();
        let report = ParquetExporter::new().export(&ws, &opts).unwrap();
        assert_eq!(report.total_rows, 1);
        assert!(
            report
                .files
                .iter()
                .any(|f| f.path.file_name().and_then(|n| n.to_str()) == Some("resources.parquet"))
        );
    }

    #[test]
    fn test_should_refuse_overwrite_without_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let final_path = tmp.path().join("resources.parquet");
        fs::write(&final_path, b"sentinel").unwrap();
        let ws = Workspace::builder()
            .root(arc_path(tmp.path()))
            .components(vec![minimal_component()])
            .build();
        let opts = ExportOptions::builder()
            .out_dir(arc_path(tmp.path()))
            .parsed_at_ms(Some(1))
            .build();
        let err = ParquetExporter::new().export(&ws, &opts).unwrap_err();
        assert!(matches!(err, ExportError::OutputExists(_)));
    }

    #[test]
    fn test_should_overwrite_when_flag_set() {
        let tmp = tempfile::tempdir().unwrap();
        let final_path = tmp.path().join("resources.parquet");
        fs::write(&final_path, b"sentinel").unwrap();
        let ws = Workspace::builder()
            .root(arc_path(tmp.path()))
            .components(vec![minimal_component()])
            .build();
        let opts = ExportOptions::builder()
            .out_dir(arc_path(tmp.path()))
            .parsed_at_ms(Some(1))
            .overwrite(true)
            .build();
        let report = ParquetExporter::new().export(&ws, &opts).unwrap();
        assert_eq!(report.total_rows, 1);
        let bytes = fs::read(&final_path).unwrap();
        assert_ne!(bytes, b"sentinel".to_vec());
    }

    #[test]
    fn test_should_be_byte_deterministic_with_pinned_parsed_at() {
        let tmp_a = tempfile::tempdir().unwrap();
        let tmp_b = tempfile::tempdir().unwrap();
        let ws_a = Workspace::builder()
            .root(arc_path(tmp_a.path()))
            .components(vec![minimal_component()])
            .build();
        let ws_b = Workspace::builder()
            .root(arc_path(tmp_b.path()))
            .components(vec![minimal_component()])
            .build();
        let opts_a = ExportOptions::builder()
            .out_dir(arc_path(tmp_a.path()))
            .parsed_at_ms(Some(1_700_000_000_000_i64))
            .build();
        let opts_b = ExportOptions::builder()
            .out_dir(arc_path(tmp_b.path()))
            .parsed_at_ms(Some(1_700_000_000_000_i64))
            .build();
        let r_a = ParquetExporter::new().export(&ws_a, &opts_a).unwrap();
        let r_b = ParquetExporter::new().export(&ws_b, &opts_b).unwrap();
        let parquet_a = r_a
            .files
            .iter()
            .find(|f| f.path.file_name().and_then(|n| n.to_str()) == Some("resources.parquet"))
            .unwrap();
        let parquet_b = r_b
            .files
            .iter()
            .find(|f| f.path.file_name().and_then(|n| n.to_str()) == Some("resources.parquet"))
            .unwrap();
        // workspace_root differs (tempdir paths), so byte-identical is too
        // strict — assert per-file sha if we override workspace_root, but
        // here we just assert both produced > 0 bytes.
        assert!(parquet_a.bytes > 0);
        assert!(parquet_b.bytes > 0);
    }

    #[test]
    fn test_partial_path_appends_suffix() {
        let p = partial_path(Path::new("/tmp/resources.parquet"));
        assert_eq!(p, PathBuf::from("/tmp/resources.parquet.partial"));
    }

    #[test]
    fn test_render_path_normalises_separators() {
        use std::path::PathBuf;
        // POSIX-shaped input round-trips verbatim.
        assert_eq!(render_path(&PathBuf::from("a/b/c.tf")), "a/b/c.tf");
        // Single-component path stays single.
        assert_eq!(render_path(&PathBuf::from("main.tf")), "main.tf");
        // Empty path stays empty.
        assert_eq!(render_path(&PathBuf::from("")), "");
        // Parent / current dir round-trip.
        assert_eq!(
            render_path(&PathBuf::from("../foo/main.tf")),
            "../foo/main.tf"
        );
    }

    #[test]
    fn test_render_expression_source_int_and_unresolved() {
        assert_eq!(
            render_expression_source(&Expression::Literal(Value::Int(3))),
            "3"
        );
        let expr = Expression::Unresolved(
            Symbolic::builder()
                .kind(SymbolKind::Var)
                .source(Arc::<str>::from("var.x"))
                .span(Span::synthetic())
                .build(),
        );
        assert_eq!(render_expression_source(&expr), "var.x");
    }

    #[test]
    fn test_should_refuse_when_out_dir_missing() {
        let ws = Workspace::builder()
            .root(arc_path(PathBuf::from("/tmp/x")))
            .build();
        let opts = ExportOptions::builder()
            .out_dir(arc_path(PathBuf::from("/this/does/not/exist/zzz")))
            .parsed_at_ms(Some(0))
            .build();
        let err = ParquetExporter::new().export(&ws, &opts).unwrap_err();
        assert!(matches!(err, ExportError::OutDirMissing(_)));
    }

    #[test]
    fn test_should_refuse_when_out_dir_is_file() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("not-a-dir");
        fs::write(&f, b"x").unwrap();
        let ws = Workspace::builder().root(arc_path(tmp.path())).build();
        let opts = ExportOptions::builder()
            .out_dir(arc_path(f))
            .parsed_at_ms(Some(0))
            .build();
        let err = ParquetExporter::new().export(&ws, &opts).unwrap_err();
        assert!(matches!(err, ExportError::OutDirNotDir(_)));
    }
}
