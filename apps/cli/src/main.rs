//! `tfparser` CLI — end-to-end wrapper around [`tfparser_core::DefaultPipeline`].
//!
//! Subcommands:
//!
//! - `tfparser parse <root> --out <DIR>` — run the full pipeline (discovery → loader → projection →
//!   terragrunt → evaluator → graph → provider → exporter). Emits `resources.parquet`,
//!   `dependencies.parquet`, `components.parquet`, `modules.parquet`, and
//!   `workspace.manifest.json`.
//! - `tfparser schema` — dump the canonical Arrow schema as JSON.
//! - `tfparser verify` — re-hash every file in a manifest.
//! - `tfparser version` — print build info.
//!
//! The CLI is a *thin* wrapper: every flag maps to a `tfparser-core`
//! option; no business logic lives here (per [50-cli.md § 1](../../../specs/50-cli.md)).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::{io::Write as _, path::PathBuf, process::ExitCode, sync::Arc};

use anyhow::{Context, Result};
use clap::{Parser as ClapParser, Subcommand, ValueEnum};
use tfparser_core::{
    CompressionOpt, EnvVarMode, ExportOptions, Parser as TfParser, SecondaryTable,
    exporter::schema_field_names,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, ClapParser)]
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
    /// matches the manifest's stored SHA-256. Spec 50 § 2; spec 91 § 11.
    Verify(VerifyArgs),
}

#[derive(Debug, ClapParser)]
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

    /// Environment to pin (`terraform.workspace` / Terragrunt cascade choice).
    #[arg(long)]
    environment: Option<String>,

    /// Default AWS region used when neither provider blocks nor Terragrunt
    /// cascade supply one.
    #[arg(long)]
    region: Option<String>,

    /// Path to a YAML profile-map file (per spec 16 § 3.2).
    #[arg(long, value_name = "PATH", conflicts_with = "aws_config")]
    profile_map: Option<PathBuf>,

    /// Path to an `~/.aws/config`-shaped INI file (per spec 16 § 3.1).
    #[arg(long, value_name = "PATH", conflicts_with = "profile_map")]
    aws_config: Option<PathBuf>,

    /// `key=value` repo-level variable bindings (repeatable). Strings only;
    /// every value is parsed as `Value::Str`.
    #[arg(long = "var", value_name = "K=V", action = clap::ArgAction::Append)]
    vars: Vec<String>,

    /// Allowlisted environment variable names visible to `get_env(...)`
    /// (repeatable).
    #[arg(long = "allow-env", value_name = "NAME", action = clap::ArgAction::Append)]
    allow_env: Vec<String>,

    /// How `get_env(...)` reads the process env.
    #[arg(long, value_enum, default_value_t = EnvMode::Strict)]
    env_mode: EnvMode,

    /// Fail if any provider profile referenced by source is not in the
    /// profile map (per spec 16 § 6).
    #[arg(long)]
    strict_providers: bool,

    /// Compression codec for parquet output.
    #[arg(long, value_enum, default_value_t = CompressionKind::Zstd)]
    compression: CompressionKind,

    /// Zstandard level (1..=22; ignored when `--compression` is not `zstd`).
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(i32).range(1..=22))]
    zstd_level: i32,

    /// Which secondary tables to emit. Default: all four
    /// (`dependencies`, `components`, `modules`); pass `none` for the
    /// M0-only `resources.parquet`.
    #[arg(long, value_enum, value_name = "TABLES", default_value = "all")]
    tables: TablesArg,
}

#[derive(Debug, ClapParser)]
struct VerifyArgs {
    /// Path to a `workspace.manifest.json`. Defaults to
    /// `<DIR>/workspace.manifest.json` when `--dir` is supplied.
    #[arg(long, conflicts_with = "dir")]
    manifest: Option<PathBuf>,

    /// Directory containing a `workspace.manifest.json` to verify.
    #[arg(long)]
    dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EnvMode {
    /// `get_env` returns `Unresolved` unless `--allow-env NAME` opts the name in.
    Strict,
    /// `get_env` reads from the process environment with no allowlist
    /// (useful for `TF_VAR_*`-shaped workflows). Use with care — leakages
    /// land in `Value` and the manifest's `attributes_json`.
    Passthrough,
    /// `get_env` always returns the caller's default (or `""`). Useful for
    /// hermetic local runs.
    Mock,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum CompressionKind {
    /// No compression.
    Uncompressed,
    /// Zstandard (level set by `--zstd-level`, default 3).
    Zstd,
    /// Snappy.
    Snappy,
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum TablesArg {
    /// Emit all of `dependencies`, `components`, `modules`.
    All,
    /// Emit no secondary tables — only `resources.parquet`.
    None,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
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
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "tfparser version");
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

    // ---- Build the parser via the façade ------------------------------
    let parser = build_parser(args, &root)?;

    // ---- Build ExportOptions -----------------------------------------
    let export_opts = build_export_options(args)?;

    // ---- Parse + export in one call ----------------------------------
    let (ws, report) = parser
        .parse_and_export(&export_opts)
        .context("parse + export")?;
    info!(
        components = ws.components.len(),
        modules = ws.modules.len(),
        diagnostics = ws.diagnostics.len(),
        "pipeline complete"
    );
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

/// Translate CLI [`ParseArgs`] into a configured [`TfParser`] via the
/// `tfparser_core::Parser` façade. All flag→option mapping lives here.
fn build_parser(args: &ParseArgs, root: &std::path::Path) -> Result<TfParser> {
    let env_mode = match args.env_mode {
        EnvMode::Strict => EnvVarMode::Strict {
            allowed: args
                .allow_env
                .iter()
                .map(|s| Arc::<str>::from(s.as_str()))
                .collect(),
        },
        EnvMode::Passthrough => EnvVarMode::Passthrough,
        EnvMode::Mock => EnvVarMode::Mock,
    };

    let mut b = TfParser::builder()
        .workspace_root(root)
        .env_var_mode(env_mode)
        .allow_env_many(args.allow_env.iter().map(String::as_str))
        .strict_providers(args.strict_providers);

    if let Some(env) = args.environment.as_deref() {
        b = b.environment(env);
    }
    if let Some(region) = args.region.as_deref() {
        b = b
            .default_region(region)
            .with_context(|| format!("invalid --region: {region}"))?;
    }
    if let Some(path) = &args.profile_map {
        b = b
            .load_profile_map_yaml(path)
            .with_context(|| format!("load profile map at {}", path.display()))?;
    } else if let Some(path) = &args.aws_config {
        b = b
            .load_aws_config(path)
            .with_context(|| format!("load aws-config at {}", path.display()))?;
    }
    for kv in &args.vars {
        let (k, v) = parse_kv(kv)?;
        b = b.var(k, v);
    }

    Ok(b.build()?)
}

/// Translate CLI [`ParseArgs`] into [`ExportOptions`].
fn build_export_options(args: &ParseArgs) -> Result<ExportOptions> {
    let parsed_at_ms = match args.parsed_at.as_deref() {
        Some(s) => Some(parse_rfc3339_ms(s)?),
        None => None,
    };
    let command_line = redact_command_line(std::env::args());
    let compression = match (args.compression, args.zstd_level) {
        (CompressionKind::Uncompressed, _) => CompressionOpt::Uncompressed,
        (CompressionKind::Snappy, _) => CompressionOpt::Snappy,
        (CompressionKind::Zstd, level) => {
            CompressionOpt::zstd(level).with_context(|| format!("invalid zstd level: {level}"))?
        }
    };
    let tables = match args.tables {
        TablesArg::All => vec![
            SecondaryTable::Dependencies,
            SecondaryTable::Components,
            SecondaryTable::Modules,
        ],
        TablesArg::None => Vec::new(),
    };
    Ok(ExportOptions::builder()
        .out_dir(Arc::<std::path::Path>::from(args.out.as_path()))
        .overwrite(args.overwrite)
        .compression(compression)
        .parsed_at_ms(parsed_at_ms)
        .command_line(command_line)
        .tables(tables)
        .build())
}

fn parse_kv(s: &str) -> Result<(String, String)> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("--var expects KEY=VALUE (got `{s}`)"))?;
    if k.is_empty() {
        anyhow::bail!("--var KEY must be non-empty (got `{s}`)");
    }
    Ok((k.to_string(), v.to_string()))
}

/// Cap and redact the command line we record in the manifest. Spec 70
/// § Input Validation: secrets sometimes hide behind `--*-token` /
/// `--*-secret` / `--*-password` flags; redact those values in both the
/// inline form (`--flag=value`) **and** the space-separated form
/// (`--flag value`).
fn redact_command_line<I: IntoIterator<Item = String>>(args: I) -> Arc<str> {
    const CAP_BYTES: usize = 4 * 1024;
    let mut out = String::new();
    let mut redact_next = false;
    for (i, raw) in args.into_iter().enumerate() {
        let arg = if redact_next {
            redact_next = false;
            "<redacted>".to_string()
        } else {
            let (rendered, next) = redact_arg(&raw);
            redact_next = next;
            rendered
        };
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&arg);
        if out.len() > CAP_BYTES {
            // `String::truncate` panics on a non-UTF-8 char boundary;
            // walk back to the nearest one before truncating.
            let snap = floor_char_boundary(&out, CAP_BYTES);
            out.truncate(snap);
            out.push_str("…(truncated)");
            break;
        }
    }
    Arc::from(out)
}

/// Returns `(rendered, redact_next)` — when `redact_next` is true, the
/// caller must replace the *next* argv element with `<redacted>` because
/// the current token is a bare `--secret-flag` whose value lives in
/// the following arg.
fn redact_arg(raw: &str) -> (String, bool) {
    // `--foo=bar` form
    if let Some((flag, _value)) = raw.split_once('=')
        && looks_secret_flag(flag)
    {
        return (format!("{flag}=<redacted>"), false);
    }
    // Bare `--secret-flag` with its value in the next argv slot.
    if raw.starts_with("--") && looks_secret_flag(raw) {
        return (raw.to_string(), true);
    }
    (raw.to_string(), false)
}

fn looks_secret_flag(flag: &str) -> bool {
    let lower = flag.to_ascii_lowercase();
    lower.contains("token") || lower.contains("secret") || lower.contains("password")
}

/// `str::floor_char_boundary` is unstable, so roll the byte-walk by hand.
/// Walks back from `index` until landing on a UTF-8 char boundary. `index`
/// > `s.len()` is clamped to the end; `0` is always a boundary.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut i = index.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn run_verify(args: &VerifyArgs) -> Result<()> {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};
    use tfparser_core::exporter::Manifest;

    const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

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

    let bytes = read_capped(&manifest_path, MAX_MANIFEST_BYTES)
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
        match std::fs::read(&path) {
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

/// Read up to `cap` bytes from `path`. Errors when the file exceeds the cap.
/// Defence-in-depth — the manifest is always small in practice.
fn read_capped(path: &std::path::Path, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.take(cap + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > cap {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("file exceeds {cap} byte cap"),
        ));
    }
    Ok(buf)
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
fn map_exit_code(err: &anyhow::Error) -> u8 {
    use tfparser_core::{
        Error as CoreError, ProviderError, exporter::ExportError, graph::GraphError,
        terragrunt::TerragruntError,
    };

    // CoreError wraps the phase-specific errors as variants, so check the
    // façade's `Error` enum first — the bare `downcast_ref::<ExportError>`
    // etc. below catch the (rarer) case where a phase-specific type is
    // returned without being wrapped.
    if let Some(core) = err.downcast_ref::<CoreError>() {
        return match core {
            CoreError::Validation(_) => 2,
            CoreError::Io { .. } => 3,
            CoreError::Limit { .. } => 4,
            CoreError::Provider(_) => 6,
            CoreError::Export(_) => 7,
            _ => 1,
        };
    }
    if err.downcast_ref::<ExportError>().is_some() {
        return 7;
    }
    if err.downcast_ref::<TerragruntError>().is_some() {
        return 5;
    }
    if err.downcast_ref::<ProviderError>().is_some() {
        return 6;
    }
    // Graph builder failures (address collision, depth/expansion caps) ride
    // the loader-class limit code 4 per `50-cli.md § 4.3` — code 8 is
    // reserved there for `--fail-on-diagnostics` and must not collide.
    if err.downcast_ref::<GraphError>().is_some() {
        return 4;
    }
    1
}

#[cfg(test)]
mod tests {
    use super::{looks_secret_flag, redact_command_line};

    #[test]
    fn test_should_redact_token_flag() {
        let argv = [
            "tfparser".to_string(),
            "parse".to_string(),
            "--token=sk-XXXXXXXXXXXX".to_string(),
            "--out=./out".to_string(),
        ];
        let cmd = redact_command_line(argv.iter().cloned());
        assert!(cmd.contains("--token=<redacted>"), "{}", cmd);
        assert!(!cmd.contains("sk-XXXX"));
        assert!(cmd.contains("--out=./out"));
    }

    #[test]
    fn test_should_truncate_at_cap() {
        let big = "x".repeat(10_000);
        let cmd = redact_command_line(std::iter::once(big));
        assert!(cmd.ends_with("…(truncated)"), "{}", cmd);
    }

    #[test]
    fn test_should_detect_secret_flags() {
        assert!(looks_secret_flag("--token"));
        assert!(looks_secret_flag("--aws-secret-access-key"));
        assert!(looks_secret_flag("--PASSWORD"));
        assert!(!looks_secret_flag("--region"));
    }

    #[test]
    fn test_should_redact_space_separated_secret_flag() {
        // Reviewer P1: `--token sk-…` (separate argv tokens) must redact
        // the *next* arg too, not just `--flag=value`.
        let argv = [
            "tfparser".to_string(),
            "parse".to_string(),
            "--token".to_string(),
            "sk-NOTASECRETBUTLOOKSLIKEONE".to_string(),
            "--out".to_string(),
            "./out".to_string(),
        ];
        let cmd = redact_command_line(argv.iter().cloned());
        assert!(cmd.contains("--token <redacted>"), "{}", cmd);
        assert!(!cmd.contains("sk-NOTASECRET"), "{}", cmd);
        // Surrounding flags must be untouched.
        assert!(cmd.contains("--out ./out"), "{}", cmd);
    }

    #[test]
    fn test_should_truncate_at_utf8_boundary_without_panic() {
        // Reviewer P1: `String::truncate` panics on a non-UTF-8 char
        // boundary. Build a string that overruns the 4 KiB cap with
        // multi-byte characters; ensure we walk back to a boundary.
        let big = "你".repeat(2000); // each char is 3 bytes → 6 KiB total
        let cmd = redact_command_line(std::iter::once(big));
        assert!(cmd.ends_with("…(truncated)"), "{}", cmd);
        // No panic and the string is valid UTF-8 (Arc<str> guarantees it).
        assert!(cmd.len() <= 4 * 1024 + "…(truncated)".len() + 2);
    }

    #[test]
    fn test_floor_char_boundary_clamps_at_string_end() {
        assert_eq!(super::floor_char_boundary("hi", 10), 2);
        // 1-byte index inside a 3-byte char snaps to 0.
        assert_eq!(super::floor_char_boundary("你", 1), 0);
        // Exact boundary returns the same index.
        assert_eq!(super::floor_char_boundary("hi世界", 2), 2);
    }
}
