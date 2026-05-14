//! `tfparser` CLI — Phase 3 happy path.
//!
//! M0 ships only:
//!
//! - `tfparser parse <root> --out <DIR>` — runs discovery → loader → projection → exporter, emits
//!   `resources.parquet` + `workspace.manifest.json`.
//! - `tfparser schema` — dumps the canonical Arrow schema as JSON.
//! - `tfparser version` — prints build info.
//!
//! Later phases extend `parse` with evaluator/terragrunt/graph options
//! and add `tfparser inspect` and `tfparser verify`. The CLI is a *thin*
//! wrapper: every flag maps to a `tfparser-core` option, no business
//! logic lives here (per [50-cli.md § 1](../../../specs/50-cli.md)).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::{io::Write as _, path::PathBuf, process::ExitCode, sync::Arc};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tfparser_core::{
    Workspace,
    discovery::{Discoverer, DiscoveryOptions, FsDiscoverer},
    exporter::{CompressionOpt, ExportOptions, Exporter, ParquetExporter, schema_field_names},
    ir::ComponentId,
    loader::{HclEditLoader, LoadContext, Loader, LoaderLimits, SourceMap},
    projection::project_component,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "tfparser",
    version,
    about = "Parse Terraform / Terragrunt source into Parquet."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Increase log verbosity (`-v` info, `-vv` debug, `-vvv` trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Parse a workspace and write Parquet artefacts.
    Parse(ParseArgs),

    /// Print the canonical Arrow schema as JSON.
    Schema,

    /// Print build/version info.
    Version,

    /// Re-hash every file in a `workspace.manifest.json` and verify it
    /// matches the manifest's stored SHA-256. Spec 50 § 2; spec 91 § 11
    /// (Phase 8.7).
    Verify(VerifyArgs),
}

#[derive(Debug, Parser)]
struct ParseArgs {
    /// Workspace root directory.
    root: PathBuf,

    /// Output directory (created if missing).
    #[arg(short, long, default_value = "./tfparser-out")]
    out: PathBuf,

    /// Overwrite existing files in --out.
    #[arg(long)]
    overwrite: bool,

    /// Pin `parsed_at` to this RFC3339 timestamp (for reproducible builds).
    #[arg(long)]
    parsed_at: Option<String>,
}

#[derive(Debug, Parser)]
struct VerifyArgs {
    /// Path to a `workspace.manifest.json`. Defaults to
    /// `<DIR>/workspace.manifest.json` when `--dir` is supplied.
    #[arg(long, conflicts_with = "dir")]
    manifest: Option<PathBuf>,

    /// Directory containing a `workspace.manifest.json` to verify.
    #[arg(long)]
    dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // anyhow already prints the chain; emit one line for the user.
            eprintln_anyhow(&err);
            ExitCode::from(map_exit_code(&err))
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Parse(args) => run_parse(args),
        Command::Schema => run_schema(),
        Command::Version => {
            // Single tracing::info line so `-v` shows it too.
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "tfparser-cli version");
            std::io::Write::write_all(
                &mut std::io::stdout(),
                format!("tfparser {}\n", env!("CARGO_PKG_VERSION")).as_bytes(),
            )?;
            Ok(())
        }
        Command::Verify(args) => run_verify(args),
    }
}

fn run_parse(args: &ParseArgs) -> Result<()> {
    let root = canonicalize_root(&args.root)?;
    ensure_out_dir(&args.out)?;

    let discovered = FsDiscoverer
        .discover(&root, &DiscoveryOptions::defaults())
        .context("discovery")?;
    info!(
        components = discovered.components.len(),
        modules = discovered.modules.len(),
        "discovery complete"
    );

    let sources = SourceMap::new();
    let limits = LoaderLimits::default();
    let ctx = LoadContext::new(&discovered.root, &sources, &limits);

    let mut components = Vec::new();
    let mut diagnostics = Vec::new();
    for (idx, dir) in discovered.components.iter().enumerate() {
        let raw = HclEditLoader.load(dir, &ctx).context("loader")?;
        diagnostics.extend(raw.diagnostics.iter().cloned());
        let comp = project_component(&raw, ComponentId::from_index(idx), &mut diagnostics);
        components.push(comp);
    }
    info!(components = components.len(), "loaded + projected");

    let ws = Workspace::builder()
        .root(Arc::<std::path::Path>::from(discovered.root.as_ref()))
        .components(components)
        .diagnostics(diagnostics.clone())
        .build();

    let parsed_at_ms = match args.parsed_at.as_deref() {
        Some(s) => Some(parse_rfc3339_ms(s)?),
        None => None,
    };

    let command_line: Arc<str> = Arc::from(std::env::args().collect::<Vec<_>>().join(" "));
    let opts = ExportOptions::builder()
        .out_dir(Arc::<std::path::Path>::from(args.out.as_path()))
        .overwrite(args.overwrite)
        .compression(CompressionOpt::Zstd(3))
        .parsed_at_ms(parsed_at_ms)
        .command_line(command_line)
        .build();
    let report = ParquetExporter::new()
        .export(&ws, &opts)
        .context("export")?;

    info!(
        rows = report.total_rows,
        bytes = report.bytes_written,
        elapsed_ms = report.elapsed.as_millis(),
        "export complete"
    );

    let mut stdout = std::io::stdout().lock();
    writeln!(
        stdout,
        "✓ wrote {} rows ({} bytes) in {} ms",
        report.total_rows,
        report.bytes_written,
        report.elapsed.as_millis()
    )?;
    for f in &report.files {
        writeln!(stdout, "  - {}", f.path.display())?;
    }
    if !ws.diagnostics.is_empty() {
        writeln!(stdout, "{} diagnostic(s)", ws.diagnostics.len())?;
    }
    Ok(())
}

fn run_verify(args: &VerifyArgs) -> Result<()> {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};
    use tfparser_core::exporter::Manifest;

    let manifest_path = match (&args.manifest, &args.dir) {
        (Some(p), _) => p.clone(),
        (None, Some(dir)) => dir.join("workspace.manifest.json"),
        (None, None) => {
            anyhow::bail!("either --manifest <PATH> or --dir <DIR> is required");
        }
    };
    let manifest_dir = manifest_path
        .parent()
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);

    let bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read manifest at {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse manifest at {}", manifest_path.display()))?;

    let mut stdout = std::io::stdout().lock();
    writeln!(
        stdout,
        "verifying {} ({} file(s))",
        manifest_path.display(),
        manifest.files.len()
    )?;

    let mut mismatches: Vec<String> = Vec::new();
    for f in &manifest.files {
        let path = manifest_dir.join(&f.name);
        let read = std::fs::read(&path);
        match read {
            Ok(bytes) => {
                let mut h = Sha256::new();
                h.update(&bytes);
                let digest = h.finalize();
                let mut got = String::with_capacity(64);
                for b in digest {
                    let _ = write!(got, "{b:02x}");
                }
                let observed = bytes.len() as u64;
                let ok = got == f.sha256 && observed == f.bytes;
                if ok {
                    writeln!(stdout, "  ✓ {} ({} bytes)", f.name, observed)?;
                } else {
                    let reason = if got == f.sha256 {
                        format!("byte-size mismatch (expected {}, got {observed})", f.bytes)
                    } else {
                        format!("sha mismatch (expected {}, got {got})", f.sha256)
                    };
                    writeln!(stdout, "  ✗ {} — {reason}", f.name)?;
                    mismatches.push(format!("{}: {reason}", f.name));
                }
            }
            Err(err) => {
                writeln!(stdout, "  ✗ {} — not found: {err}", f.name)?;
                mismatches.push(format!("{}: i/o: {err}", f.name));
            }
        }
    }

    if mismatches.is_empty() {
        writeln!(stdout, "ok — manifest verified")?;
        Ok(())
    } else {
        anyhow::bail!(
            "manifest verification failed: {} error(s)",
            mismatches.len()
        );
    }
}

fn run_schema() -> Result<()> {
    let names = schema_field_names();
    let body = serde_json::json!({
        "schema_major": tfparser_core::exporter::SCHEMA_MAJOR,
        "schema_minor": tfparser_core::exporter::SCHEMA_MINOR,
        "columns": names,
    });
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{}", serde_json::to_string_pretty(&body)?)?;
    Ok(())
}

fn canonicalize_root(root: &std::path::Path) -> Result<std::path::PathBuf> {
    root.canonicalize().map_err(|source| {
        // Surface as a core IO error so map_exit_code routes it to
        // exit code 3 ("Discovery error / root missing") per spec 50 § 4.3.
        anyhow::Error::from(tfparser_core::Error::Io {
            path: root.to_path_buf(),
            source,
        })
    })
}

fn ensure_out_dir(out: &std::path::Path) -> Result<()> {
    if !out.exists() {
        std::fs::create_dir_all(out)
            .with_context(|| format!("create output dir: {}", out.display()))?;
    }
    if !out.is_dir() {
        anyhow::bail!("output path is not a directory: {}", out.display());
    }
    Ok(())
}

fn parse_rfc3339_ms(s: &str) -> Result<i64> {
    let ts: jiff::Timestamp = s
        .parse()
        .with_context(|| format!("invalid RFC3339 timestamp: {s}"))?;
    Ok(ts.as_millisecond())
}

fn init_logging(verbosity: u8) {
    let default_filter = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

fn eprintln_anyhow(err: &anyhow::Error) {
    use std::io::Write as _;
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "error: {err}");
    for cause in err.chain().skip(1) {
        let _ = writeln!(stderr, "  caused by: {cause}");
    }
}

/// Map an error to the exit code table pinned in `50-cli.md § 4.3`.
///
/// Phase 3 only ships codes 0/1/2/3/4/7 — eval (5), terragrunt (5), and
/// provider (6) errors are introduced by later phases.
fn map_exit_code(err: &anyhow::Error) -> u8 {
    use tfparser_core::{Error as CoreError, exporter::ExportError};

    if err.downcast_ref::<ExportError>().is_some() {
        return 7;
    }
    if let Some(core) = err.downcast_ref::<CoreError>() {
        return match core {
            CoreError::Validation(_) => 2,
            CoreError::Io { .. } => 3,
            CoreError::Limit { .. } => 4,
            _ => 1,
        };
    }
    // anyhow wraps io::Error / clap errors — generic failure.
    1
}
