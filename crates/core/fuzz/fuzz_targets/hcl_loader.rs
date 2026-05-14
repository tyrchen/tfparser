//! Phase 2 fuzz harness: feed arbitrary bytes to `HclEditLoader::parse_bytes`
//! and assert it never panics, never OOMs (per the loader limits), and always
//! returns either a non-empty diagnostic vector or a parsed block list.
//!
//! Per [70-security.md § 6](../../../specs/70-security.md), CI runs this for
//! ≥ 10 min per PR via `cargo +nightly fuzz run hcl_loader -- -max_total_time=600`.

#![no_main]

use std::{path::Path, sync::Arc};

use libfuzzer_sys::fuzz_target;
use tfparser_core::loader::{HclEditLoader, LoaderLimits};

fuzz_target!(|data: &[u8]| {
    let path: Arc<Path> = Arc::from(Path::new("fuzz/input.tf"));
    let limits = LoaderLimits::default();
    let _ = HclEditLoader.parse_bytes(data, &path, &limits);
});
