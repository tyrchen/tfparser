//! Phase 9 / task 9.1–9.2 — `criterion` micro-benches for the four
//! hot-path components plus an end-to-end `parse_large_monorepo` macro.
//!
//! Run with `cargo bench -p tfparser-core --bench pipeline`. Use
//! `cargo bench -- --save-baseline main && cargo bench -- --baseline main`
//! to gate regression at 10 % per spec 71 § 7.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs
)]

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use criterion::{Criterion, criterion_group, criterion_main};
use tfparser_core::{
    DefaultPipeline, Evaluator, FuncRegistry, HclEvaluator, Pipeline, PipelineOptions,
    discovery::{Discoverer, DiscoveryOptions, FsDiscoverer},
    eval::{EnvVarMode, EvalContext, EvalLimits},
    exporter::{ExportOptions, Exporter, ParquetExporter, SecondaryTable},
    ir::{Component, ComponentId, ComponentKind, Map},
    loader::{HclEditLoader, LoadContext, Loader, LoaderLimits, RawComponent, SourceMap},
    projection::project_component,
};

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.ancestors().nth(2).map(Path::to_path_buf).unwrap()
}

fn fixture(name: &str) -> PathBuf {
    workspace_root().join("fixtures").join(name)
}

fn load_fixture_components(root: &Path) -> Vec<Component> {
    let canonical = std::fs::canonicalize(root).unwrap();
    let discovered = FsDiscoverer
        .discover(&canonical, &DiscoveryOptions::defaults())
        .unwrap();
    let sources = SourceMap::new();
    let limits = LoaderLimits::default();
    let ctx = LoadContext::new(&discovered.root, &sources, &limits);

    let mut out = Vec::new();
    for (next, dir) in discovered
        .components
        .iter()
        .chain(discovered.modules.iter())
        .enumerate()
    {
        let raw: RawComponent = HclEditLoader.load(dir, &ctx).unwrap();
        let mut diag = Vec::new();
        let c = project_component(&raw, ComponentId::from_index(next), &mut diag);
        out.push(c);
    }
    out
}

fn bench_discovery(c: &mut Criterion) {
    let root = fixture("large-monorepo");
    if !root.exists() {
        return;
    }
    c.bench_function("discovery_large_monorepo", |b| {
        b.iter(|| {
            let canonical = std::fs::canonicalize(&root).unwrap();
            let discovered = FsDiscoverer
                .discover(&canonical, &DiscoveryOptions::defaults())
                .unwrap();
            std::hint::black_box(discovered);
        });
    });
}

fn bench_loader(c: &mut Criterion) {
    let root = fixture("large-monorepo");
    if !root.exists() {
        return;
    }
    let canonical = std::fs::canonicalize(&root).unwrap();
    let discovered = FsDiscoverer
        .discover(&canonical, &DiscoveryOptions::defaults())
        .unwrap();
    c.bench_function("loader_large_monorepo", |b| {
        b.iter(|| {
            let sources = SourceMap::new();
            let limits = LoaderLimits::default();
            let ctx = LoadContext::new(&discovered.root, &sources, &limits);
            let mut total = 0usize;
            for dir in discovered
                .components
                .iter()
                .chain(discovered.modules.iter())
            {
                let raw = HclEditLoader.load(dir, &ctx).unwrap();
                total += raw.raw_blocks.len();
            }
            std::hint::black_box(total);
        });
    });
}

fn bench_evaluator(c: &mut Criterion) {
    let root = fixture("large-monorepo");
    if !root.exists() {
        return;
    }
    let canonical = std::fs::canonicalize(&root).unwrap();
    let components = load_fixture_components(&root);
    let funcs = Arc::new(FuncRegistry::default_with_stdlib());
    c.bench_function("evaluator_large_monorepo", |b| {
        b.iter(|| {
            for component in &components {
                if !matches!(
                    component.kind,
                    ComponentKind::Component | ComponentKind::Module
                ) {
                    continue;
                }
                let ctx = EvalContext::new(
                    Arc::from(canonical.clone()),
                    None,
                    EnvVarMode::default(),
                    Map::new(),
                    Map::new(),
                    Arc::clone(&funcs),
                    EvalLimits::default(),
                );
                let evald = HclEvaluator::new().evaluate(component, &ctx).unwrap();
                std::hint::black_box(evald);
            }
        });
    });
}

fn bench_exporter(c: &mut Criterion) {
    let root = fixture("large-monorepo");
    if !root.exists() {
        return;
    }
    let opts = PipelineOptions::new(Arc::<Path>::from(root.as_path()));
    let ws = DefaultPipeline::new().run(&opts).unwrap();
    c.bench_function("exporter_large_monorepo", |b| {
        b.iter(|| {
            let tmp = tempfile::tempdir().unwrap();
            let opts = ExportOptions::builder()
                .out_dir(Arc::<Path>::from(tmp.path()))
                .parsed_at_ms(Some(1_700_000_000_000))
                .tables(vec![
                    SecondaryTable::Dependencies,
                    SecondaryTable::Components,
                    SecondaryTable::Modules,
                ])
                .build();
            let report = ParquetExporter::new().export(&ws, &opts).unwrap();
            std::hint::black_box(report);
        });
    });
}

fn bench_e2e_pipeline(c: &mut Criterion) {
    let root = fixture("large-monorepo");
    if !root.exists() {
        return;
    }
    let opts = PipelineOptions::new(Arc::<Path>::from(root.as_path()));
    c.bench_function("parse_large_monorepo", |b| {
        b.iter(|| {
            let ws = DefaultPipeline::new().run(&opts).unwrap();
            std::hint::black_box(ws);
        });
    });
}

criterion_group!(
    benches,
    bench_discovery,
    bench_loader,
    bench_evaluator,
    bench_exporter,
    bench_e2e_pipeline,
);
criterion_main!(benches);
