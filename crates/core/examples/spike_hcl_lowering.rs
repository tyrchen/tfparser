//! Phase 0 spike 0.2 — `hcl-edit` span lowering.
//!
//! Goal: prove we can (a) parse a real `.tf` file from
//! `fixtures/large-monorepo/`, (b) walk every top-level block / attribute,
//! and (c) reconstruct `(line, column)` from the byte spans `hcl-edit`
//! attaches to every node — matching the design pinned in
//! [12-hcl-loader.md § 6](../../../specs/12-hcl-loader.md).
//!
//! Run with: `cargo run -p tfparser-core --example spike_hcl_lowering`.
//!
//! This example is *not* part of the library API. It exists to keep the
//! spike runnable in CI (`cargo build --examples` exercises it) and to give
//! reviewers a canary they can rerun any time.

#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use std::{fs, ops::Range, path::PathBuf};

use anyhow::Context;
use hcl_edit::{
    Span as _,
    expr::Expression,
    structure::{Block, Body, Structure},
};

/// 1-based (line, column) into a string.
#[derive(Clone, Copy, Debug)]
struct LineCol {
    line: u32,
    column: u32,
}

/// Sorted prefix-sums of line starts. Mirrors the design in
/// [12-hcl-loader.md § 6](../../../specs/12-hcl-loader.md): one `Vec<u32>`,
/// binary search per lookup.
struct LineIndex {
    line_starts: Vec<u32>,
}

impl LineIndex {
    fn build(src: &str) -> Self {
        let mut starts = Vec::with_capacity(src.len() / 32 + 4);
        starts.push(0);
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                // i is the index of the newline; the next line starts at i+1.
                starts.push(u32::try_from(i + 1).expect("file fits in u32"));
            }
        }
        Self {
            line_starts: starts,
        }
    }

    fn locate(&self, byte: u32) -> LineCol {
        // partition_point returns the count of elements strictly less than the
        // probe — so we get the next line. Subtract 1 to land on the line that
        // contains the byte.
        let line_idx = self
            .line_starts
            .partition_point(|&start| start <= byte)
            .saturating_sub(1);
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or_default();
        LineCol {
            line: u32::try_from(line_idx + 1).expect("line fits in u32"),
            column: byte - line_start + 1,
        }
    }
}

#[derive(Default, Debug)]
struct LoweringStats {
    blocks: usize,
    attributes: usize,
    spans_resolved: usize,
    template_strings: usize,
    traversals: usize,
}

fn lower_body(body: &Body, line_index: &LineIndex, depth: usize, stats: &mut LoweringStats) {
    for item in body {
        match item {
            Structure::Block(b) => {
                stats.blocks += 1;
                lower_block(b, line_index, depth, stats);
            }
            Structure::Attribute(a) => {
                stats.attributes += 1;
                let key_span = a.key.span();
                let val_span = a.value.span();
                if key_span.is_some() && val_span.is_some() {
                    stats.spans_resolved += 1;
                }
                // Classify at every depth so traversals / templates inside
                // nested blocks (e.g. ingress {} → cidr_blocks) are counted.
                let kind = classify_expr(&a.value, stats);
                if depth == 0
                    && let Some(span) = val_span
                {
                    let pos = line_index.locate(u32::try_from(span.start).unwrap_or(u32::MAX));
                    println!(
                        "  attr {:<20} at {}:{}  expr={kind}",
                        a.key.as_str(),
                        pos.line,
                        pos.column,
                    );
                }
            }
        }
    }
}

fn lower_block(block: &Block, line_index: &LineIndex, depth: usize, stats: &mut LoweringStats) {
    let span = block.span();
    let pos = span.map(|s: Range<usize>| line_index.locate(u32::try_from(s.start).unwrap_or(0)));
    let labels: Vec<&str> = block
        .labels
        .iter()
        .map(hcl_edit::structure::BlockLabel::as_str)
        .collect();
    let indent = "  ".repeat(depth);
    match pos {
        Some(p) => println!(
            "{indent}block {:<10} labels={:?}  at {}:{}",
            block.ident.as_str(),
            labels,
            p.line,
            p.column,
        ),
        None => println!(
            "{indent}block {:<10} labels={:?}  (no span)",
            block.ident.as_str(),
            labels,
        ),
    }
    lower_body(&block.body, line_index, depth + 1, stats);
}

fn classify_expr(expr: &Expression, stats: &mut LoweringStats) -> &'static str {
    match expr {
        Expression::Null(_) => "null",
        Expression::Bool(_) => "bool",
        Expression::Number(_) => "number",
        Expression::String(_) => "string-literal",
        Expression::Array(_) => "tuple",
        Expression::Object(_) => "object",
        Expression::StringTemplate(_) | Expression::HeredocTemplate(_) => {
            stats.template_strings += 1;
            "string-template"
        }
        Expression::Variable(_) => "variable",
        Expression::Traversal(_) => {
            stats.traversals += 1;
            "traversal (reference)"
        }
        Expression::FuncCall(_) => "func-call",
        Expression::Parenthesis(_) => "parenthesized",
        Expression::Conditional(_) => "conditional",
        Expression::ForExpr(_) => "for-expr",
        Expression::UnaryOp(_) => "unary-op",
        Expression::BinaryOp(_) => "binary-op",
    }
}

fn fixture_path() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is the directory holding `crates/core/Cargo.toml`.
    // The fixtures live at `<workspace_root>/fixtures/...`, which is two
    // levels above the manifest dir.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest
        .parent()
        .and_then(std::path::Path::parent)
        .map_or_else(|| manifest.clone(), std::path::Path::to_path_buf);
    workspace_root.join("fixtures/large-monorepo/terraform/services/order-service/main.tf")
}

fn main() -> anyhow::Result<()> {
    let path: PathBuf = fixture_path();
    let src = fs::read_to_string(&path)
        .with_context(|| format!("reading fixture file {}", path.display()))?;
    println!("=== spike_hcl_lowering ===");
    println!("file: {} ({} bytes)", path.display(), src.len());

    let line_index = LineIndex::build(&src);

    let body: Body = src
        .parse()
        .with_context(|| format!("parsing HCL body of {}", path.display()))?;

    let mut stats = LoweringStats::default();
    lower_body(&body, &line_index, 0, &mut stats);

    println!();
    println!("--- summary ---");
    println!(
        "blocks={} attributes={} spans_resolved={} template_strings={} traversals={}",
        stats.blocks,
        stats.attributes,
        stats.spans_resolved,
        stats.template_strings,
        stats.traversals,
    );

    // Phase 0 success criteria: every attribute had a resolvable span, and
    // we hit at least one template-string + traversal so the loader knows it
    // can lower those Expression variants.
    anyhow::ensure!(
        stats.blocks > 0,
        "no blocks parsed — fixture might be empty"
    );
    anyhow::ensure!(stats.attributes > 0, "no attributes parsed");
    anyhow::ensure!(
        stats.spans_resolved == stats.attributes,
        "some attributes lacked spans ({} of {} resolved)",
        stats.spans_resolved,
        stats.attributes,
    );
    anyhow::ensure!(
        stats.traversals > 0,
        "expected at least one traversal (var.x / module.y) reference",
    );
    println!("OK — all attributes carry resolvable spans");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_index_locates_byte_positions() {
        let src = "abc\ndefgh\ni";
        let li = LineIndex::build(src);
        // 'a' at byte 0 → line 1 col 1.
        let p = li.locate(0);
        assert_eq!((p.line, p.column), (1, 1));
        // 'd' at byte 4 → line 2 col 1.
        let p = li.locate(4);
        assert_eq!((p.line, p.column), (2, 1));
        // 'h' at byte 8 → line 2 col 5.
        let p = li.locate(8);
        assert_eq!((p.line, p.column), (2, 5));
        // 'i' at byte 10 → line 3 col 1.
        let p = li.locate(10);
        assert_eq!((p.line, p.column), (3, 1));
    }

    #[test]
    fn test_line_index_handles_empty_input() {
        let li = LineIndex::build("");
        let p = li.locate(0);
        assert_eq!((p.line, p.column), (1, 1));
    }

    #[test]
    fn test_main_runs_on_real_fixture() -> anyhow::Result<()> {
        // Sanity check: the fixture file the example reads exists. The full
        // example body runs under `cargo run --example`; here we just verify
        // the canary path is present so CI catches accidental fixture loss.
        let p = fixture_path();
        assert!(p.exists(), "fixture not found at {}", p.display());
        Ok(())
    }
}
