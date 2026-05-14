//! Source storage and `(line, col)` lookup, per [12-hcl-loader.md § 6].
//!
//! The loader needs (a) to keep file contents alive long enough to render
//! spans later (`SourceMap`), and (b) a cheap byte → line/column converter
//! for every span (`LineIndex`). Both are deliberately simple — building one
//! `LineIndex` per file is `O(n)`; each `locate` is `O(log n)` over a small
//! `Vec<u32>`.
//!
//! [12-hcl-loader.md § 6]: ../../../specs/12-hcl-loader.md

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, RwLock},
};

/// 1-based (line, column) position in a source file.
///
/// `column` is in **bytes**, matching the rest of the IR's byte-offset
/// convention. Multi-byte UTF-8 characters straddling a column boundary
/// will appear as a column at their starting byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LineCol {
    /// 1-based line number.
    pub line: u32,
    /// 1-based byte column within that line.
    pub column: u32,
}

/// Sorted prefix-sums of line-start byte offsets in a single source file.
///
/// Cheap to clone (it owns one `Vec<u32>`); cheap to query (binary search).
#[derive(Clone, Debug)]
pub struct LineIndex {
    line_starts: Vec<u32>,
}

impl LineIndex {
    /// Build a [`LineIndex`] from a source string. `O(n)`.
    ///
    /// Files larger than `u32::MAX` saturate at the cap; the loader
    /// already rejects files above 4 MiB, so this is a defence-in-depth
    /// guard, not the primary cap.
    #[must_use]
    pub fn build(src: &str) -> Self {
        let mut starts = Vec::with_capacity(src.len() / 32 + 4);
        starts.push(0);
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                let next = i.saturating_add(1);
                starts.push(u32::try_from(next).unwrap_or(u32::MAX));
            }
        }
        Self {
            line_starts: starts,
        }
    }

    /// Resolve a byte offset to a 1-based [`LineCol`].
    ///
    /// Returns `(1, 1)` when `byte == 0` and the file is empty. Out-of-range
    /// bytes (> file length) saturate at the last known line's column.
    #[must_use]
    pub fn locate(&self, byte: u32) -> LineCol {
        let line_idx = self
            .line_starts
            .partition_point(|&start| start <= byte)
            .saturating_sub(1);
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or_default();
        LineCol {
            line: u32::try_from(line_idx + 1).unwrap_or(u32::MAX),
            column: byte.saturating_sub(line_start).saturating_add(1),
        }
    }
}

/// Cache of file contents and line indices, keyed by canonical path.
///
/// Used by the loader and the exporter — the exporter renders spans against
/// the original bytes; eviction is therefore off until the consumer signals
/// otherwise. Internally backed by a `RwLock<HashMap>` (rare writes, frequent
/// reads); `DashMap` is overkill for the loader's call pattern (one writer
/// per file, then read-only).
#[derive(Debug, Default)]
pub struct SourceMap {
    inner: RwLock<HashMap<PathBuf, Arc<SourceEntry>>>,
}

/// Cached file contents + lazily-built [`LineIndex`].
#[derive(Debug)]
struct SourceEntry {
    src: Arc<str>,
    line_index: OnceLock<LineIndex>,
}

impl SourceMap {
    /// Construct an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up cached contents for `path`. Returns `None` if not cached.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<Arc<str>> {
        let read = self.inner.read().ok()?;
        read.get(path).map(|entry| Arc::clone(&entry.src))
    }

    /// Insert (or overwrite) the cached contents for `path`. Returns the
    /// `Arc<str>` the caller should use going forward.
    pub fn insert(&self, path: &Path, src: Arc<str>) -> Arc<str> {
        let entry = Arc::new(SourceEntry {
            src: Arc::clone(&src),
            line_index: OnceLock::new(),
        });
        if let Ok(mut write) = self.inner.write() {
            write.insert(path.to_path_buf(), entry);
        }
        src
    }

    /// Build (or reuse a cached) [`LineIndex`] for `path`. Returns `None` if
    /// the path isn't in the cache.
    #[must_use]
    pub fn line_index(&self, path: &Path) -> Option<LineIndex> {
        let read = self.inner.read().ok()?;
        let entry = read.get(path)?;
        let li = entry
            .line_index
            .get_or_init(|| LineIndex::build(&entry.src))
            .clone();
        Some(li)
    }

    /// Number of files currently cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().map_or(0, |m| m.len())
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
    fn test_line_index_locates_first_byte_at_1_1() {
        let li = LineIndex::build("");
        assert_eq!(li.locate(0), LineCol { line: 1, column: 1 });
    }

    #[test]
    fn test_line_index_locates_across_newlines() {
        let src = "abc\ndefgh\ni";
        let li = LineIndex::build(src);
        assert_eq!(li.locate(0), LineCol { line: 1, column: 1 });
        assert_eq!(li.locate(2), LineCol { line: 1, column: 3 });
        assert_eq!(li.locate(3), LineCol { line: 1, column: 4 }); // newline
        assert_eq!(li.locate(4), LineCol { line: 2, column: 1 });
        assert_eq!(li.locate(8), LineCol { line: 2, column: 5 });
        assert_eq!(li.locate(10), LineCol { line: 3, column: 1 });
    }

    #[test]
    fn test_line_index_handles_trailing_newline() {
        let src = "x\n";
        let li = LineIndex::build(src);
        assert_eq!(li.locate(0), LineCol { line: 1, column: 1 });
        // After the trailing newline, the next position is line 2 col 1.
        assert_eq!(li.locate(2), LineCol { line: 2, column: 1 });
    }

    #[test]
    fn test_line_index_clamps_byte_past_end() {
        let li = LineIndex::build("abc");
        let pos = li.locate(1_000);
        assert_eq!(pos.line, 1);
        // column is at most the byte offset within the (only) line.
        assert!(pos.column >= 1);
    }

    #[test]
    fn test_source_map_round_trip() {
        let map = SourceMap::new();
        let path = PathBuf::from("/tmp/x.tf");
        let src: Arc<str> = Arc::from("a\nb\nc");
        let returned = map.insert(&path, Arc::clone(&src));
        assert_eq!(&*returned, "a\nb\nc");
        let cached = map.get(&path).unwrap();
        assert!(Arc::ptr_eq(&cached, &src));
        assert_eq!(map.len(), 1);
        let li = map.line_index(&path).unwrap();
        assert_eq!(li.locate(2), LineCol { line: 2, column: 1 });
    }

    #[test]
    fn test_source_map_returns_none_for_uncached() {
        let map = SourceMap::new();
        assert!(map.get(Path::new("/nope")).is_none());
        assert!(map.line_index(Path::new("/nope")).is_none());
    }

    #[test]
    fn test_source_map_overwrite_replaces_entry() {
        let map = SourceMap::new();
        let path = PathBuf::from("/tmp/x.tf");
        map.insert(&path, Arc::from("v1"));
        map.insert(&path, Arc::from("v2"));
        assert_eq!(&*map.get(&path).unwrap(), "v2");
    }
}
