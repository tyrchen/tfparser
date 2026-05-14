//! [`Loader`] trait and the default [`HclEditLoader`] implementation.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use hcl_edit::parser::parse_body;
use tracing::instrument;

use super::{LoaderLimits, RawBlock, RawComponent, SourceMap, lowering, source_map::LineIndex};
use crate::{
    Diagnostic, LimitKind, Result, Severity,
    diagnostic::Diagnostic as Diag,
    discovery::{DirKind, DiscoveredDir},
    ir::{ComponentKind, FileExt},
    util::paths,
};

/// Per-call context handed to [`Loader::load`].
///
/// The fields are borrowed — the loader is expected to be called many times
/// per workspace and the orchestrator owns the source cache and limits.
#[derive(Debug)]
#[non_exhaustive]
pub struct LoadContext<'a> {
    /// Canonicalised workspace root.
    pub root: &'a Path,
    /// Shared `(path → source)` cache. The loader inserts every file it
    /// reads here so spans render later without re-reading.
    pub sources: &'a SourceMap,
    /// Per-file resource caps.
    pub limits: &'a LoaderLimits,
}

impl<'a> LoadContext<'a> {
    /// Construct a context. All fields are borrowed; the caller owns
    /// lifetime.
    #[must_use]
    pub const fn new(root: &'a Path, sources: &'a SourceMap, limits: &'a LoaderLimits) -> Self {
        Self {
            root,
            sources,
            limits,
        }
    }
}

/// Loader trait. Phase 2 ships a single implementation,
/// [`HclEditLoader`]; downstream may swap an in-memory variant for
/// integration tests.
pub trait Loader: Send + Sync {
    /// Read every HCL-shaped file in `dir` and produce a [`RawComponent`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] for fatal I/O outside per-file scope (e.g. a
    /// directory disappearing mid-walk). Per-file errors / limit breaches
    /// surface as [`Diagnostic`]s on the returned [`RawComponent`], and the
    /// loader continues with the remaining files.
    fn load(&self, dir: &DiscoveredDir, ctx: &LoadContext<'_>) -> Result<RawComponent>;
}

/// Default [`Loader`] implementation backed by `hcl-edit::parser::parse_body`.
///
/// Stateless and `Send + Sync`. Per the spec it's *pure* w.r.t. external
/// state — given the same `(dir, sources, limits)`, the output is byte-for-byte
/// identical (modulo `Arc<str>` identity).
#[derive(Clone, Copy, Debug, Default)]
pub struct HclEditLoader;

impl HclEditLoader {
    /// Construct an [`HclEditLoader`].
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Convenience: parse a single byte slice into [`RawBlock`]s without
    /// touching the filesystem. Used by the fuzz harness and by integration
    /// tests that synthesise inputs in-memory.
    #[must_use]
    pub fn parse_bytes(
        &self,
        bytes: &[u8],
        path: &Arc<Path>,
        limits: &LoaderLimits,
    ) -> ParseBytesResult {
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        if bytes.len() > limits.max_file_bytes as usize {
            diagnostics.push(Diag::limit(
                LimitKind::FileSize,
                "TF1201",
                format!(
                    "file exceeds loader byte cap ({} > {}); skipped",
                    bytes.len(),
                    limits.max_file_bytes
                ),
            ));
            return ParseBytesResult {
                blocks: Vec::new(),
                diagnostics,
            };
        }

        let src = match std::str::from_utf8(bytes) {
            Ok(s) => s,
            Err(err) => {
                diagnostics.push(Diag::new(
                    Severity::Warn,
                    "TF1202",
                    format!("file is not valid UTF-8: {err}"),
                ));
                return ParseBytesResult {
                    blocks: Vec::new(),
                    diagnostics,
                };
            }
        };

        let body = match parse_body(src) {
            Ok(b) => b,
            Err(err) => {
                diagnostics.push(Diag::new(
                    Severity::Warn,
                    "TF1203",
                    format!("HCL parse error: {err}"),
                ));
                return ParseBytesResult {
                    blocks: Vec::new(),
                    diagnostics,
                };
            }
        };

        let line_index = LineIndex::build(src);
        let lowered = lowering::lower_body(&body, path, &line_index, limits, src.len());
        diagnostics.extend(lowered.diagnostics);
        ParseBytesResult {
            blocks: lowered.blocks,
            diagnostics,
        }
    }
}

/// Output of [`HclEditLoader::parse_bytes`]. Cheap value type used by the
/// fuzz harness and synthetic-input tests.
#[derive(Debug, Default)]
pub struct ParseBytesResult {
    /// Lowered blocks.
    pub blocks: Vec<RawBlock>,
    /// Per-file diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

impl Loader for HclEditLoader {
    #[instrument(level = "debug", skip(self, ctx), fields(dir = %dir.path.display()))]
    fn load(&self, dir: &DiscoveredDir, ctx: &LoadContext<'_>) -> Result<RawComponent> {
        let kind = match dir.kind {
            DirKind::Component => ComponentKind::Component,
            DirKind::Module => ComponentKind::Module,
            DirKind::Environments | DirKind::Files | DirKind::Other => {
                // The loader is only useful for Component / Module dirs.
                // Anything else we treat as an empty Module (the orchestrator
                // generally won't even ask).
                ComponentKind::Module
            }
        };
        let mut raw = RawComponent::new(Arc::clone(&dir.path), kind);

        for file in &dir.files {
            if !file.ext.is_hcl() {
                continue;
            }
            // Resolve the absolute path under the root; refuse anything
            // that escapes (defence in depth — discovery should have done
            // the same already).
            let abs = match resolve_under_root(&file.path, ctx.root) {
                Ok(p) => p,
                Err(diag) => {
                    raw.diagnostics.push(diag);
                    continue;
                }
            };

            let bytes = match read_file(&abs, ctx.limits.max_file_bytes) {
                Ok(b) => b,
                Err(LoaderReadError::TooLarge { observed, limit }) => {
                    raw.diagnostics.push(Diag::limit(
                        LimitKind::FileSize,
                        "TF1201",
                        format!(
                            "file exceeds loader byte cap ({observed} > {limit}); skipped: {}",
                            file.path.display()
                        ),
                    ));
                    continue;
                }
                Err(LoaderReadError::Io { source }) => {
                    raw.diagnostics.push(Diag::new(
                        Severity::Warn,
                        "TF1204",
                        format!("i/o error reading {}: {source}", file.path.display()),
                    ));
                    continue;
                }
            };

            let src = match std::str::from_utf8(&bytes) {
                Ok(s) => Arc::<str>::from(s),
                Err(err) => {
                    raw.diagnostics.push(Diag::new(
                        Severity::Warn,
                        "TF1202",
                        format!("file is not valid UTF-8: {} ({err})", file.path.display()),
                    ));
                    continue;
                }
            };

            ctx.sources.insert(&abs, Arc::clone(&src));

            let body = match parse_body(&src) {
                Ok(b) => b,
                Err(err) => {
                    raw.diagnostics.push(Diag::new(
                        Severity::Warn,
                        "TF1203",
                        format!("HCL parse error in {}: {err}", file.path.display()),
                    ));
                    continue;
                }
            };

            let line_index = LineIndex::build(&src);
            let path_arc: Arc<Path> = Arc::clone(&file.path);
            let lowered =
                lowering::lower_body(&body, &path_arc, &line_index, ctx.limits, src.len());
            raw.diagnostics.extend(lowered.diagnostics);
            for block in lowered.blocks {
                if !file_ext_supports_block_kind(file.ext, block.kind) {
                    raw.diagnostics.push(Diag::new(
                        Severity::Trace,
                        "TF1205",
                        format!(
                            "block `{:?}` in unexpected file extension `{:?}`: {}",
                            block.kind,
                            file.ext,
                            file.path.display()
                        ),
                    ));
                }
                raw.raw_blocks.push(block);
            }
        }

        Ok(raw)
    }
}

enum LoaderReadError {
    TooLarge { observed: u64, limit: u32 },
    Io { source: io::Error },
}

fn read_file(path: &Path, max_bytes: u32) -> std::result::Result<Vec<u8>, LoaderReadError> {
    let metadata = std::fs::metadata(path).map_err(|source| LoaderReadError::Io { source })?;
    let len = metadata.len();
    if len > u64::from(max_bytes) {
        return Err(LoaderReadError::TooLarge {
            observed: len,
            limit: max_bytes,
        });
    }
    std::fs::read(path).map_err(|source| LoaderReadError::Io { source })
}

fn resolve_under_root(rel: &Arc<Path>, root: &Path) -> std::result::Result<PathBuf, Diagnostic> {
    let candidate = root.join(rel);
    paths::canonicalize_inside(&candidate, root, paths::SymlinkPolicy::Reject)
        .map_err(|err| Diag::new(Severity::Warn, "TF1206", format!("path safety: {err}")))
}

const fn file_ext_supports_block_kind(ext: FileExt, kind: BlockKind) -> bool {
    use BlockKind as B;
    match (ext, kind) {
        (FileExt::Tfvars, B::Unknown) => true,
        (FileExt::Tf, B::Include | B::Generate | B::Dependency | B::Inputs)
        | (FileExt::Tfvars | FileExt::Json, _) => false,
        (FileExt::Tf | FileExt::Hcl | FileExt::TerragruntHcl, _) => true,
    }
}

// Block kinds appear in messages so we keep the import alive.
use crate::ir::BlockKind;

// Phase-1 sanity: trait objects must be Send + Sync.
#[cfg(test)]
const _ASSERT_LOADER_IS_OBJECT_SAFE: Option<&dyn Loader> = None;
