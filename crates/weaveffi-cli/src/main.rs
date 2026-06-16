//! `weaveffi` command-line entry point: `clap` definitions and dispatch.
//!
//! Each subcommand's implementation lives in `commands` (or its own
//! top-level module for the self-contained ones: `doctor`, `extract`,
//! `scaffold`); the generator registry and config plumbing live in
//! `config`.

mod commands;
mod config;
mod doctor;
mod extract;
mod report;
mod scaffold;

use clap::{CommandFactory, Parser, Subcommand};
use miette::{bail, IntoDiagnostic, Result, WrapErr};
use tracing_subscriber::EnvFilter;
use weaveffi_ir::ir::CURRENT_SCHEMA_VERSION;

#[derive(Parser, Debug)]
#[command(name = "weaveffi", version, about = "WeaveFFI CLI")]
struct Cli {
    #[arg(long, global = true)]
    quiet: bool,
    #[arg(long, short, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    New {
        name: String,
    },
    Generate {
        /// Input IDL/IR file (yaml|yml|json|toml)
        input: String,
        /// Output directory for generated artifacts
        #[arg(short, long, default_value = "./generated")]
        out: String,
        /// Comma-separated list of targets to generate (c, cpp, swift, android, node, wasm, python, dotnet, dart, go, ruby)
        #[arg(short, long)]
        target: Option<String>,
        /// Also generate a scaffold.rs with Rust FFI function stubs
        #[arg(long)]
        scaffold: bool,
        /// Path to a TOML configuration file for generator options
        #[arg(long)]
        config: Option<String>,
        /// Print non-fatal warnings after validation
        #[arg(long)]
        warn: bool,
        /// Force regeneration, bypassing the incremental cache
        #[arg(long)]
        force: bool,
        /// Parse and validate only; print which files would be generated without writing them
        #[arg(long)]
        dry_run: bool,
    },
    Validate {
        /// Input IDL/IR file (yaml|yml|json|toml)
        input: String,
        /// Print non-fatal warnings after validation
        #[arg(long)]
        warn: bool,
        /// Output format: `json` for machine-readable output, otherwise human-readable
        #[arg(long)]
        format: Option<String>,
    },
    Extract {
        /// Path to a Rust source file to extract API definitions from
        input: String,
        /// Output file path (defaults to stdout)
        #[arg(short, long)]
        output: Option<String>,
        /// Output format: yaml (default), json, or toml
        #[arg(short, long, default_value = "yaml")]
        format: Option<String>,
        /// Downgrade validation errors to warnings and emit the IDL anyway.
        /// Useful for bootstrapping from source that references types it does
        /// not yet declare (e.g. opaque handle targets you will define later).
        #[arg(long)]
        warn: bool,
    },
    Lint {
        /// Input IDL/IR file (yaml|yml|json|toml)
        input: String,
        /// Output format: `json` for machine-readable output, otherwise human-readable
        #[arg(long)]
        format: Option<String>,
    },
    Diff {
        /// Input IDL/IR file (yaml|yml|json|toml)
        input: String,
        /// Output directory to compare against (defaults to ./generated)
        #[arg(short, long)]
        out: Option<String>,
        /// Exit non-zero if regeneration would change `out` (2 if files
        /// differ, 3 if files are missing/extra). Prints only a summary,
        /// not per-file diffs.
        #[arg(long)]
        check: bool,
    },
    Doctor {
        /// Only run checks whose `applies_to` includes this target (e.g. `dart`, `swift`)
        #[arg(long)]
        target: Option<String>,
        /// Output format: `json` for machine-readable output, otherwise human-readable
        #[arg(long)]
        format: Option<String>,
    },
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
    SchemaVersion,
    Watch {
        /// Input IDL/IR file (yaml|yml|json|toml)
        input: String,
        /// Output directory for generated artifacts
        #[arg(short, long, default_value = "./generated")]
        out: String,
        /// Comma-separated list of targets to generate
        #[arg(short, long)]
        target: Option<String>,
        /// Path to a TOML configuration file for generator options
        #[arg(long)]
        config: Option<String>,
    },
    Format {
        /// Input IDL/IR file (yaml|yml|json|toml)
        input: String,
        /// Exit non-zero if the file is not already canonically formatted
        #[arg(long)]
        check: bool,
    },
    Schema {
        /// Schema export format (currently only json-schema is supported)
        #[arg(long, default_value = "json-schema")]
        format: String,
    },
}

fn main() -> Result<()> {
    let _ = miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .terminal_links(true)
                .context_lines(3)
                .build(),
        )
    }));

    let cli = Cli::parse();

    let filter = if cli.verbose {
        EnvFilter::new("trace")
    } else if cli.quiet {
        EnvFilter::new("error")
    } else {
        EnvFilter::from_default_env()
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .without_time()
        .init();

    let quiet = cli.quiet;
    match cli.command {
        Commands::New { name } => commands::new::cmd_new(&name, quiet)?,
        Commands::Generate {
            input,
            out,
            target,
            scaffold,
            config,
            warn,
            force,
            dry_run,
        } => commands::generate::cmd_generate(
            &input,
            &out,
            target.as_deref(),
            scaffold,
            config.as_deref(),
            warn,
            force,
            dry_run,
            quiet,
        )?,
        Commands::Validate {
            input,
            warn,
            format,
        } => commands::validate::cmd_validate(&input, warn, format.as_deref(), quiet)?,
        Commands::Extract {
            input,
            output,
            format,
            warn,
        } => extract::cmd_extract(
            &input,
            output.as_deref(),
            format.as_deref().unwrap_or("yaml"),
            warn,
            quiet,
        )?,
        Commands::Lint { input, format } => {
            if !commands::validate::cmd_lint(&input, format.as_deref(), quiet)? {
                std::process::exit(1);
            }
        }
        Commands::Diff { input, out, check } => {
            commands::diff::cmd_diff(&input, out.as_deref(), check, quiet)?
        }
        Commands::Doctor { target, format } => {
            doctor::cmd_doctor(target.as_deref(), format.as_deref())?
        }
        Commands::Completions { shell } => cmd_completions(shell),
        Commands::SchemaVersion => println!("{CURRENT_SCHEMA_VERSION}"),
        Commands::Watch {
            input,
            out,
            target,
            config,
        } => commands::watch::cmd_watch(&input, &out, target.as_deref(), config.as_deref(), quiet)?,
        Commands::Format { input, check } => commands::format::cmd_format(&input, check, quiet)?,
        Commands::Schema { format } => cmd_schema(&format)?,
    }
    Ok(())
}

fn cmd_completions(shell: clap_complete::Shell) {
    clap_complete::generate(
        shell,
        &mut Cli::command(),
        "weaveffi",
        &mut std::io::stdout(),
    );
}

fn cmd_schema(format: &str) -> Result<()> {
    match format {
        "json-schema" => {
            let schema = schemars::schema_for!(weaveffi_ir::ir::Api);
            let json = serde_json::to_string_pretty(&schema)
                .into_diagnostic()
                .wrap_err("failed to serialize JSON Schema")?;
            println!("{json}");
            Ok(())
        }
        other => bail!(
            "unsupported schema format: {} (expected 'json-schema')",
            other
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_checks_cross_targets() {
        let cmd = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .arg("doctor")
            .output()
            .expect("failed to run weaveffi doctor");

        let stdout = String::from_utf8_lossy(&cmd.stdout);
        assert!(cmd.status.success(), "doctor failed: {stdout}");
        assert!(
            stdout.contains("Cross-compilation targets:"),
            "missing cross-target section in doctor output: {stdout}"
        );
        assert!(
            stdout.contains("aarch64-apple-ios"),
            "missing iOS target check: {stdout}"
        );
        assert!(
            stdout.contains("aarch64-linux-android"),
            "missing Android target check: {stdout}"
        );
        assert!(
            stdout.contains("wasm32-unknown-unknown"),
            "missing WASM target check: {stdout}"
        );
        assert!(
            stdout.contains("WebAssembly tools:"),
            "missing wasm tools section: {stdout}"
        );
        assert!(
            stdout.contains("wasm-pack"),
            "missing wasm-pack check: {stdout}"
        );
        assert!(
            stdout.contains("wasm-bindgen-cli"),
            "missing wasm-bindgen-cli check: {stdout}"
        );
    }

    #[test]
    fn completions_bash() {
        let cmd = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .args(["completions", "bash"])
            .output()
            .expect("failed to run weaveffi completions bash");

        let stdout = String::from_utf8_lossy(&cmd.stdout);
        assert!(cmd.status.success(), "completions bash failed: {stdout}");
        assert!(
            stdout.contains("weaveffi"),
            "bash completions should reference weaveffi: {stdout}"
        );
        assert!(
            stdout.contains("complete"),
            "bash completions should contain 'complete': {stdout}"
        );
    }

    #[test]
    fn completions_zsh() {
        let cmd = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .args(["completions", "zsh"])
            .output()
            .expect("failed to run weaveffi completions zsh");

        let stdout = String::from_utf8_lossy(&cmd.stdout);
        assert!(cmd.status.success(), "completions zsh failed: {stdout}");
        assert!(
            stdout.contains("weaveffi"),
            "zsh completions should reference weaveffi: {stdout}"
        );
        assert!(
            stdout.contains("compdef"),
            "zsh completions should contain 'compdef': {stdout}"
        );
    }

    #[test]
    fn schema_version_prints_current() {
        let cmd = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .arg("schema-version")
            .output()
            .expect("failed to run weaveffi schema-version");

        let stdout = String::from_utf8_lossy(&cmd.stdout);
        assert!(cmd.status.success(), "schema-version failed: {stdout}");
        assert_eq!(stdout.trim(), CURRENT_SCHEMA_VERSION);
    }
}
