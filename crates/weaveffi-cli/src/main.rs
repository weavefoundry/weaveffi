//! `weaveffi` command-line entry point: `clap` definitions and dispatch.
//!
//! Each subcommand's implementation lives in `commands` (or `extract` for the
//! Rust-source extractor); project configuration and the generator registry
//! live in `config`.

mod commands;
mod config;
mod extract;
mod report;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum HumanJsonFormat {
    Human,
    Json,
}

impl HumanJsonFormat {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Json => "json",
        }
    }
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Generate bindings for every (or the selected) language target
    Generate {
        /// Input: annotated Rust source (.rs) or an IDL document (yaml|yml|json|toml)
        input: String,
        /// Output directory for generated artifacts
        #[arg(short, long, default_value = "./generated")]
        out: String,
        /// Comma-separated list of targets to generate (c, cpp, swift, android, node, wasm, python, dotnet, dart, go, ruby)
        #[arg(short, long)]
        target: Option<String>,
        /// Path to weaveffi.toml (default: the nearest one at or above the input file)
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
    /// Validate an API definition without generating anything
    Validate {
        /// Input: annotated Rust source (.rs) or an IDL document (yaml|yml|json|toml)
        input: String,
        /// Also report non-fatal warnings (advisory lints)
        #[arg(long)]
        warn: bool,
        /// Output format: `json` for machine-readable output, otherwise human-readable
        #[arg(long)]
        format: Option<HumanJsonFormat>,
    },
    /// Assemble publishable packages that bundle prebuilt native libraries
    Package {
        /// Input: annotated Rust source (.rs) or an IDL document (yaml|yml|json|toml)
        input: String,
        /// Output directory for the packaged artifacts
        #[arg(short, long, default_value = "./dist")]
        out: String,
        /// Comma-separated list of targets to package (e.g. node,python,dotnet)
        #[arg(short, long)]
        target: Option<String>,
        /// Path to weaveffi.toml (default: the nearest one at or above the input file)
        #[arg(long)]
        config: Option<String>,
        /// Directory of prebuilt native libraries laid out as `<dir>/<platform>/<lib>`
        /// (platform ids: darwin-arm64, darwin-x64, linux-x64, linux-arm64, windows-x64)
        #[arg(long)]
        binaries: Option<String>,
        /// Cargo package to cross-compile as the native producer (one cdylib per platform)
        #[arg(long)]
        build: Option<String>,
        /// Comma-separated platform ids to target (defaults to the full v1 matrix)
        #[arg(long)]
        platforms: Option<String>,
        /// Print non-fatal warnings after validation
        #[arg(long)]
        warn: bool,
    },
    /// Extract an IDL document from annotated Rust source
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
    /// Show how regenerating would change an existing output directory
    Diff {
        /// Input: annotated Rust source (.rs) or an IDL document (yaml|yml|json|toml)
        input: String,
        /// Output directory to compare against (defaults to ./generated)
        #[arg(short, long)]
        out: Option<String>,
        /// Path to weaveffi.toml (default: the nearest one at or above the input file)
        #[arg(long)]
        config: Option<String>,
        /// Exit non-zero if regeneration would change `out` (2 if files
        /// differ, 3 if files are missing/extra). Prints only a summary,
        /// not per-file diffs.
        #[arg(long)]
        check: bool,
    },
    /// Print shell completions
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
    /// Print the IDL schema version this build reads and writes
    SchemaVersion,
    /// Print the IDL document schema
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
        Commands::Generate {
            input,
            out,
            target,
            config,
            warn,
            force,
            dry_run,
        } => commands::generate::cmd_generate(
            &input,
            &out,
            target.as_deref(),
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
        } => commands::validate::cmd_validate(
            &input,
            warn,
            format.map(HumanJsonFormat::as_str),
            quiet,
        )?,
        Commands::Package {
            input,
            out,
            target,
            config,
            binaries,
            build,
            platforms,
            warn,
        } => commands::package::cmd_package(
            &input,
            &out,
            target.as_deref(),
            config.as_deref(),
            binaries.as_deref(),
            build.as_deref(),
            platforms.as_deref(),
            warn,
            quiet,
        )?,
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
        Commands::Diff {
            input,
            out,
            config,
            check,
        } => commands::diff::cmd_diff(&input, out.as_deref(), config.as_deref(), check, quiet)?,
        Commands::Completions { shell } => cmd_completions(shell),
        Commands::SchemaVersion => println!("{CURRENT_SCHEMA_VERSION}"),
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
    fn rejects_unknown_human_json_output_formats() {
        let args = ["weaveffi", "validate", "input.yml", "--format", "jsonn"];
        let error = Cli::try_parse_from(args).expect_err("unknown format should be rejected");
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
        assert!(
            error.to_string().contains("possible values: human, json"),
            "{error}"
        );
    }

    #[test]
    fn removed_subcommands_are_gone() {
        for cmd in ["new", "lint", "doctor", "man", "watch", "format"] {
            assert!(
                Cli::try_parse_from(["weaveffi", cmd]).is_err(),
                "`{cmd}` should no longer parse"
            );
        }
    }

    #[test]
    fn completions_and_schema_version() {
        for (args, needle) in [
            (&["completions", "bash"][..], "complete"),
            (&["completions", "zsh"][..], "compdef"),
            (&["schema-version"][..], CURRENT_SCHEMA_VERSION),
        ] {
            let cmd = assert_cmd::Command::cargo_bin("weaveffi")
                .expect("binary not found")
                .args(args)
                .output()
                .expect("failed to run weaveffi");
            let stdout = String::from_utf8_lossy(&cmd.stdout);
            assert!(cmd.status.success(), "{args:?} failed: {stdout}");
            assert!(stdout.contains(needle), "{args:?}: {stdout}");
        }
    }
}
