//! `workspace.manifest.json` writer.
//!
//! Per [20-parquet-exporter.md § 3.1] — a tiny machine-readable manifest that
//! lets callers tell which Parquet artefacts came from which parse run.
//! It is the *only* non-Parquet output and is always written.
//!
//! [20-parquet-exporter.md § 3.1]: ../../../specs/20-parquet-exporter.md

use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    path::Path,
    sync::Arc,
};

use serde::{Deserialize, Serialize};

use super::ExportError;

/// Top-level manifest structure.
///
/// Wire format is canonical JSON with stable key order. Field names are
/// `snake_case` for downstream tooling compatibility — both consumers
/// (CLI scripts, security audits) parse with `jq`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Manifest {
    /// `tfparser-core` semver.
    pub tfparser_version: String,
    /// Parquet schema major.
    pub schema_major: u32,
    /// Parquet schema minor.
    pub schema_minor: u32,
    /// Timestamp the manifest was produced, ms since UNIX epoch (UTC).
    pub generated_at_ms: i64,
    /// Workspace root the parse ran against (absolute, canonical).
    pub workspace_root: String,
    /// Verbatim CLI command line (or empty when the library was used
    /// directly).
    pub command_line: String,
    /// One entry per file the run produced.
    pub files: Vec<ManifestFile>,
}

/// Per-file entry in the manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ManifestFile {
    /// File name (relative to the output dir).
    pub name: String,
    /// Row count.
    pub rows: u64,
    /// On-disk byte size.
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the file's bytes.
    pub sha256: String,
}

/// Write the manifest atomically (`.partial` → rename). Returns the byte
/// size on disk after the rename.
///
/// # Errors
///
/// Returns [`ExportError::Io`] for I/O failures, [`ExportError::Manifest`]
/// for serialisation failures, and [`ExportError::OutputExists`] when the
/// manifest already exists and `overwrite` is `false`.
pub fn write_manifest(
    manifest: &Manifest,
    final_path: &Path,
    overwrite: bool,
) -> Result<u64, ExportError> {
    if final_path.exists() && !overwrite {
        return Err(ExportError::OutputExists(Arc::from(final_path)));
    }

    let bytes = serde_json::to_vec_pretty(manifest)?;
    let mut partial: std::ffi::OsString = final_path.as_os_str().to_os_string();
    partial.push(".partial");
    let partial = std::path::PathBuf::from(partial);
    if partial.exists() {
        fs::remove_file(&partial).map_err(|source| ExportError::Io {
            path: Arc::from(partial.as_path()),
            source,
        })?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|source| ExportError::Io {
            path: Arc::from(partial.as_path()),
            source,
        })?;
    file.write_all(&bytes).map_err(|source| ExportError::Io {
        path: Arc::from(partial.as_path()),
        source,
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
    let on_disk = fs::metadata(final_path)
        .map(|m| m.len())
        .map_err(|source| ExportError::Io {
            path: Arc::from(final_path),
            source,
        })?;
    Ok(on_disk)
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

    fn fake_manifest() -> Manifest {
        Manifest {
            tfparser_version: "0.1.0".into(),
            schema_major: 0,
            schema_minor: 1,
            generated_at_ms: 1_700_000_000_000,
            workspace_root: "/tmp/repo".into(),
            command_line: "tfparser parse /tmp/repo".into(),
            files: vec![ManifestFile {
                name: "resources.parquet".into(),
                rows: 42,
                bytes: 1024,
                sha256: "deadbeef".into(),
            }],
        }
    }

    #[test]
    fn test_should_round_trip_manifest_via_serde() {
        let m = fake_manifest();
        let s = serde_json::to_string(&m).unwrap();
        let back: Manifest = serde_json::from_str(&s).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn test_should_write_manifest_and_return_byte_count() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("workspace.manifest.json");
        let bytes = write_manifest(&fake_manifest(), &path, false).unwrap();
        let on_disk = fs::read(&path).unwrap();
        assert_eq!(on_disk.len() as u64, bytes);
        let s = String::from_utf8(on_disk).unwrap();
        assert!(s.contains("\"resources.parquet\""), "{s}");
    }

    #[test]
    fn test_should_refuse_overwrite_without_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("workspace.manifest.json");
        fs::write(&path, b"existing").unwrap();
        let err = write_manifest(&fake_manifest(), &path, false).unwrap_err();
        assert!(matches!(err, ExportError::OutputExists(_)));
    }

    #[test]
    fn test_should_overwrite_when_flag_set() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("workspace.manifest.json");
        fs::write(&path, b"existing").unwrap();
        let _bytes = write_manifest(&fake_manifest(), &path, true).unwrap();
        let s = fs::read_to_string(&path).unwrap();
        assert!(s.contains("\"resources.parquet\""), "{s}");
    }
}
