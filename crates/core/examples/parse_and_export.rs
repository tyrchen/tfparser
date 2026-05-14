//! Parse a workspace and write the four canonical Parquet tables in a
//! single call, with builder-level configuration.
//!
//! Run with:
//!
//! ```text
//! cargo run -p tfparser-core --example parse_and_export -- \
//!     ./fixtures/large-monorepo ./out
//! ```

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::{collections::BTreeSet, path::Path, process::ExitCode, sync::Arc};

use tfparser_core::{
    EnvVarMode, ExportOptions, Parser, Result, SecondaryTable, exporter::CompressionOpt,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let root = args
        .next()
        .unwrap_or_else(|| "./fixtures/large-monorepo".to_string());
    let out = args.next().unwrap_or_else(|| "./out".to_string());
    std::fs::create_dir_all(&out).map_err(|source| tfparser_core::Error::Io {
        path: out.clone().into(),
        source,
    })?;

    let parser = Parser::builder()
        .workspace_root(&root)
        // Strict env-var sandbox; allowlist a single var so `get_env("TF_VAR_environment")`
        // can resolve when the workspace cascades on it.
        .env_var_mode(EnvVarMode::Strict {
            allowed: BTreeSet::new(),
        })
        .allow_env("TF_VAR_environment")
        // Repo-level `var.environment = "production"`.
        .var("environment", "production")
        .build()?;

    let export = ExportOptions::builder()
        .out_dir(Arc::<Path>::from(Path::new(&out)))
        .overwrite(true)
        .compression(CompressionOpt::Zstd(3))
        .tables(vec![
            SecondaryTable::Dependencies,
            SecondaryTable::Components,
            SecondaryTable::Modules,
        ])
        .build();

    let (ws, report) = parser.parse_and_export(&export)?;
    println!(
        "wrote {} rows / {} bytes in {} ms",
        report.total_rows,
        report.bytes_written,
        report.elapsed.as_millis(),
    );
    for f in &report.files {
        println!("  - {}", f.path.display());
    }
    println!(
        "{} components, {} modules, {} diagnostics",
        ws.components.len(),
        ws.modules.len(),
        ws.diagnostics.len(),
    );
    Ok(())
}
