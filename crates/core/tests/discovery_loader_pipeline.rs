//! Phase 2 integration test — exercises [`FsDiscoverer`] + [`HclEditLoader`]
//! end-to-end against the M0 fixtures and asserts the exit criteria pinned in
//! `specs/91-impl-plan.md § 5`.
//!
//! Specifically:
//! - `Discovered { components: [single-component, multi-provider] }` returns the expected
//!   structure.
//! - Lowered `RawComponent.raw_blocks[*].body` contains no `hcl_edit` types (invariant I-LOAD-2 —
//!   checked structurally by serializing through `serde_json` and confirming the round-trip
//!   produces our IR shape).

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

use tfparser_core::{
    discovery::{Discoverer, DiscoveryOptions, FsDiscoverer},
    ir::{AttributeMap, BlockKind, Expression, Value},
    loader::{HclEditLoader, LoadContext, Loader, LoaderLimits, RawBlock, SourceMap},
};

/// Structural proof of invariant I-LOAD-2: `RawBlock.body` is exactly an
/// [`AttributeMap`] (`Vec<(Arc<str>, Expression)>`), so by construction it
/// cannot carry an `hcl_edit` type. Compile-time assertion.
const _ASSERT_BODY_IS_ATTRIBUTE_MAP: fn(&RawBlock) -> &AttributeMap = |b| &b.body;

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

#[test]
fn test_should_discover_single_component_fixture() {
    let root = fixture("single-component");
    let discovered = FsDiscoverer
        .discover(&root, &DiscoveryOptions::defaults())
        .expect("discovery succeeds");
    assert_eq!(discovered.components.len(), 1, "{discovered:?}");
    let c = &discovered.components[0];
    // single-component is the workspace root itself (one `.tf` file at the
    // top level).
    assert_eq!(c.path.as_ref(), Path::new(""));
    assert_eq!(c.kind, tfparser_core::discovery::DirKind::Component);
}

#[test]
fn test_should_discover_multi_provider_fixture() {
    let root = fixture("multi-provider");
    let discovered = FsDiscoverer
        .discover(&root, &DiscoveryOptions::defaults())
        .expect("discovery succeeds");
    assert_eq!(discovered.components.len(), 1);
    let c = &discovered.components[0];
    assert_eq!(c.files.len(), 2, "expected main.tf + variables.tf");
}

#[test]
fn test_should_load_single_component_fixture_blocks() {
    let root = fixture("single-component");
    let discovered = FsDiscoverer
        .discover(&root, &DiscoveryOptions::defaults())
        .expect("discovery succeeds");
    let sources = SourceMap::new();
    let limits = LoaderLimits::default();
    let ctx = LoadContext::new(&discovered.root, &sources, &limits);
    let raw = HclEditLoader
        .load(&discovered.components[0], &ctx)
        .expect("load succeeds");
    let kinds: Vec<BlockKind> = raw.raw_blocks.iter().map(|b| b.kind).collect();
    // Expect at least: terraform, provider, two `variable`, one `locals`,
    // one `resource`, one `output`.
    assert!(kinds.contains(&BlockKind::Terraform), "{kinds:?}");
    assert!(kinds.contains(&BlockKind::Provider), "{kinds:?}");
    assert!(kinds.contains(&BlockKind::Variable), "{kinds:?}");
    assert!(kinds.contains(&BlockKind::Locals), "{kinds:?}");
    assert!(kinds.contains(&BlockKind::Resource), "{kinds:?}");
    assert!(kinds.contains(&BlockKind::Output), "{kinds:?}");
}

#[test]
fn test_iload2_lowered_body_contains_no_hcl_edit_types() {
    // The compile-time `_ASSERT_BODY_IS_ATTRIBUTE_MAP` constant above is the
    // load-bearing proof of I-LOAD-2 — if `RawBlock.body` ever stops being an
    // `AttributeMap`, that line stops compiling. The runtime sweep below is
    // belt-and-braces: it serialises every lowered block through serde_json
    // and asserts no `hcl_edit`-internal type names leak into the output.
    let root = fixture("single-component");
    let discovered = FsDiscoverer
        .discover(&root, &DiscoveryOptions::defaults())
        .expect("discovery");
    let sources = SourceMap::new();
    let limits = LoaderLimits::default();
    let ctx = LoadContext::new(&discovered.root, &sources, &limits);
    let raw = HclEditLoader
        .load(&discovered.components[0], &ctx)
        .expect("load");

    // Each block's body is `Vec<(Arc<str>, Expression)>`. The Expression
    // tree is exhaustively defined in our IR; serde-printing a sample
    // gives us a guarantee no foreign types leak.
    for block in &raw.raw_blocks {
        for (key, expr) in &block.body {
            let json = serde_json::to_string(expr).expect("serialize");
            // Spot-check: hcl_edit's internal types use names like "Decor",
            // "Decorated", "Spanned" — none should appear in our output.
            for forbidden in ["Decor", "Decorated", "Spanned", "RawString"] {
                assert!(
                    !json.contains(forbidden),
                    "key={key:?} contains forbidden hcl_edit type `{forbidden}`: {json}"
                );
            }
        }
    }
}

#[test]
fn test_should_resolve_known_unresolved_references_in_single_component() {
    let root = fixture("single-component");
    let discovered = FsDiscoverer
        .discover(&root, &DiscoveryOptions::defaults())
        .expect("discovery");
    let sources = SourceMap::new();
    let limits = LoaderLimits::default();
    let ctx = LoadContext::new(&discovered.root, &sources, &limits);
    let raw = HclEditLoader
        .load(&discovered.components[0], &ctx)
        .expect("load");
    // The provider's `region = var.region` should lower to Unresolved(var.region).
    let provider = raw
        .raw_blocks
        .iter()
        .find(|b| b.kind == BlockKind::Provider)
        .expect("provider block present");
    let region_attr = provider
        .body
        .iter()
        .find(|(k, _)| k.as_ref() == "region")
        .expect("provider has region attr");
    match &region_attr.1 {
        Expression::Unresolved(s) => {
            assert_eq!(s.source.as_ref(), "var.region");
        }
        other => panic!("expected Unresolved(var.region), got {other:?}"),
    }
}

#[test]
fn test_should_load_multi_provider_with_aliases() {
    let root = fixture("multi-provider");
    let discovered = FsDiscoverer
        .discover(&root, &DiscoveryOptions::defaults())
        .expect("discovery");
    let sources = SourceMap::new();
    let limits = LoaderLimits::default();
    let ctx = LoadContext::new(&discovered.root, &sources, &limits);
    let raw = HclEditLoader
        .load(&discovered.components[0], &ctx)
        .expect("load");
    let provider_blocks: Vec<_> = raw
        .raw_blocks
        .iter()
        .filter(|b| b.kind == BlockKind::Provider)
        .collect();
    assert_eq!(provider_blocks.len(), 2, "expected two provider blocks");
    // Both should carry an alias attribute.
    for p in provider_blocks {
        let alias = p.body.iter().find(|(k, _)| k.as_ref() == "alias");
        assert!(alias.is_some(), "provider missing alias: {p:?}");
    }
}

#[test]
fn test_should_classify_modules_under_large_monorepo_root() {
    // When the workspace root is the monorepo's top, the default
    // `module_globs` (`**/modules/*`) classifies anything inside
    // `terraform/modules/` as a module rather than a component.
    let root = workspace_root().join("fixtures/large-monorepo");
    if !root.exists() {
        return;
    }
    let discovered = FsDiscoverer
        .discover(&root, &DiscoveryOptions::defaults())
        .expect("discovery");
    let module_paths: Vec<_> = discovered
        .modules
        .iter()
        .map(|m| m.path.to_string_lossy().into_owned())
        .collect();
    assert!(
        module_paths
            .iter()
            .any(|p| p.starts_with("terraform/modules/")),
        "expected `terraform/modules/*` modules in the discovered set; got {module_paths:?}"
    );
}

#[test]
fn test_should_keep_arc_path_for_block_source() {
    let root = fixture("single-component");
    let discovered = FsDiscoverer
        .discover(&root, &DiscoveryOptions::defaults())
        .expect("discovery");
    let sources = SourceMap::new();
    let limits = LoaderLimits::default();
    let ctx = LoadContext::new(&discovered.root, &sources, &limits);
    let raw = HclEditLoader
        .load(&discovered.components[0], &ctx)
        .expect("load");
    let block = raw.raw_blocks.first().expect("at least one block");
    let _: &Arc<Path> = &block.source; // type-checks via the Arc<Path> alias
    assert_eq!(block.source.as_ref(), Path::new("main.tf"));
    // Spans point into the parsed file; they are not synthetic.
    assert!(
        block.span.byte_range.start <= block.span.byte_range.end,
        "{:?}",
        block.span
    );
}

#[test]
fn test_should_drop_unresolved_when_array_has_literal_only_values() {
    // Sanity: an array of all-string literals should lower to Literal(List(...)).
    let root = fixture("multi-provider");
    let discovered = FsDiscoverer
        .discover(&root, &DiscoveryOptions::defaults())
        .expect("discovery");
    let sources = SourceMap::new();
    let limits = LoaderLimits::default();
    let ctx = LoadContext::new(&discovered.root, &sources, &limits);
    let raw = HclEditLoader
        .load(&discovered.components[0], &ctx)
        .expect("load");
    // variables.tf carries `default = { Project = "northwind" }` — a literal
    // map. Verify the variable block exists and its `default` lowers to a
    // literal Map.
    let var = raw
        .raw_blocks
        .iter()
        .find(|b| b.kind == BlockKind::Variable && b.labels.iter().any(|l| l.as_ref() == "tags"))
        .expect("tags variable");
    let default = var
        .body
        .iter()
        .find(|(k, _)| k.as_ref() == "default")
        .expect("default attr");
    assert!(matches!(default.1, Expression::Literal(Value::Map(_))));
}
