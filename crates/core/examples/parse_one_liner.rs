//! Smallest possible consumer: `tfparser_core::parse(path)`.
//!
//! Run with:
//!
//! ```text
//! cargo run -p tfparser-core --example parse_one_liner -- ./fixtures/single-component
//! ```

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

use tfparser_core::{Result, parse, prelude::*};

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
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "./fixtures/single-component".to_string());

    let ws: Workspace = parse(&root)?;

    println!("workspace_root: {}", ws.root.display());
    println!("components:     {}", ws.components.len());
    println!("modules:        {}", ws.modules.len());
    let resources: usize = ws.components.iter().map(|c| c.resources.len()).sum();
    println!("resources:      {resources}");
    println!("diagnostics:    {}", ws.diagnostics.len());
    Ok(())
}
