build:
	@cargo build

test:
	@cargo nextest run --all-features

# Phase 9 bench harness (per [71-performance-budgets.md § 7]).
bench:
	@cargo bench -p tfparser-core --bench pipeline

# Use `make bench-save-baseline` after a known-good commit; subsequent
# `make bench-vs-baseline` compares against it (10 % regression budget).
bench-save-baseline:
	@cargo bench -p tfparser-core --bench pipeline -- --save-baseline main

bench-vs-baseline:
	@cargo bench -p tfparser-core --bench pipeline -- --baseline main

# Per CLAUDE.md § Toolchain & Build — the gates every change must pass.
ci:
	@cargo build --workspace --all-targets
	@cargo test  --workspace --all-targets
	@cargo +nightly fmt --all -- --check
	@cargo clippy --workspace --all-targets -- -D warnings
	@RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
	@cargo deny check

# `cargo +nightly fuzz run hcl_loader -- -max_total_time=600` against
# the Phase 2 harness under crates/core/fuzz/.
fuzz-hcl-loader:
	@cd crates/core/fuzz && cargo +nightly fuzz run hcl_loader -- -max_total_time=600

release:
	@cargo release tag --execute
	@git cliff -o CHANGELOG.md
	@git commit -a -n -m "Update CHANGELOG.md" || true
	@git push origin master
	@cargo release push --execute

update-submodule:
	@git submodule update --init --recursive --remote

.PHONY: build test ci fuzz-hcl-loader release update-submodule bench bench-save-baseline bench-vs-baseline
