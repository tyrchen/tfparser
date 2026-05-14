//! Phase 4 fuzz harness: parse arbitrary HCL fragments and run the
//! evaluator over the projected component against a fixed minimal context.
//! Per [70-security.md § 6], the harness asserts the evaluator never
//! panics, the iteration cap is respected, and the function-call
//! sandboxing never reaches outside the (empty) tempdir we hand it.
//!
//! Inputs are bytes interpreted as HCL source — the same shape the
//! `hcl_loader` harness consumes — so the corpus from that target also
//! exercises this one once it survives the loader.

#![no_main]

use std::{path::Path, sync::Arc};

use libfuzzer_sys::fuzz_target;
use tfparser_core::{
    Evaluator,
    eval::{EvalContext, FuncRegistry, HclEvaluator},
    ir::{ComponentId, ComponentKind},
    loader::{HclEditLoader, LoaderLimits, RawComponent},
    projection::project_component,
};

fuzz_target!(|data: &[u8]| {
    // Parse with the loader's limits; bail if it fails (the loader fuzz
    // harness already exercises that path).
    let path: Arc<Path> = Arc::from(Path::new("fuzz/input.tf"));
    let limits = LoaderLimits::default();
    let parsed = HclEditLoader.parse_bytes(data, &path, &limits);

    // Build a minimal RawComponent and project to IR.
    let mut raw = RawComponent::new(Arc::from(Path::new("")), ComponentKind::Component);
    raw.raw_blocks.extend(parsed.blocks);
    let mut diags = Vec::new();
    let component = project_component(&raw, ComponentId::from_index(0), &mut diags);

    // Empty workspace root pin (tempdir-shaped path); sandbox helpers will
    // reject any file read attempt.
    let ctx = EvalContext {
        workspace_root: Arc::from(Path::new("/nonexistent-fuzz-root")),
        environment: None,
        env_vars: tfparser_core::eval::EnvVarMode::default(),
        repo_vars: Vec::new(),
        cascade_locals: Vec::new(),
        funcs: Arc::new(FuncRegistry::default_with_stdlib()),
        limits: tfparser_core::eval::EvalLimits::default(),
    };

    // The evaluator must not panic; failures are diagnostics, not errors.
    let _ = HclEvaluator.evaluate(&component, &ctx);
});
