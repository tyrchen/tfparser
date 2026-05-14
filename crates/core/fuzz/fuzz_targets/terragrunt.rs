//! Phase 6 fuzz harness: feed arbitrary bytes through the Terragrunt
//! resolver and assert it never panics or stack-overflows.
//!
//! Per [70-security.md § 6], the resolver is a cross-trust-boundary
//! component: a malicious `terragrunt.hcl` could attempt to escape the
//! workspace root, recurse without bound, or evaluate a function with
//! pathological arguments. This harness stages the bytes inside a
//! tempdir and exercises the full resolve path against them.

#![no_main]

use std::{path::Path, sync::Arc};

use libfuzzer_sys::fuzz_target;
use tfparser_core::{FsTerragruntResolver, TerragruntResolver, TgContext};

fuzz_target!(|data: &[u8]| {
    let Ok(tmp) = tempfile::tempdir() else {
        return;
    };
    let Ok(root) = std::fs::canonicalize(tmp.path()) else {
        return;
    };
    let component_dir = root.join("x");
    if std::fs::create_dir_all(&component_dir).is_err() {
        return;
    }
    if std::fs::write(component_dir.join("terragrunt.hcl"), data).is_err() {
        return;
    }
    let ctx = TgContext::new(Arc::from(root.as_path() as &Path));
    let _ = FsTerragruntResolver::new().resolve(&component_dir, &ctx);
});
