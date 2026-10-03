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
use miette::{IntoDiagnostic, Result, WrapErr};
use weaveffi_model::ir::CURRENT_SCHEMA_VERSION;

const INPUT_HELP: &str = "Annotated Rust source (.rs) or an IDL document (yaml|yml|json|toml); \
                          defaults to `[project] input` from the nearest weaveffi.toml";

#[derive(Parser, Debug)]
#[command(
    name = "weaveffi",
    version,
    about = "Generate idiomatic bindings for 11 languages from one API definition over a stable C ABI"
)]
struct Cli {
    /// Print only errors and warnings
    #[arg(long, short, global = true)]
    quiet: bool,
    #[command(subcommand)]
    command: Commands,
}

/// Output format for `validate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ReportFormat {
    /// Human-readable text.
    Human,
    /// One JSON object on stdout.
    Json,
}

/// An IDL serialization format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum IdlFormat {
    /// YAML.
    Yaml,
    /// JSON.
    Json,
    /// TOML.
    Toml,
}

/// A schema export format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SchemaFormat {
    /// JSON Schema (draft 7) for the IDL document.
    JsonSchema,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create a weaveffi.toml (and a starter IDL outside a Rust crate)
    Init {
        /// Project directory
        #[arg(default_value = ".")]
        dir: String,
        /// Package name for a new IDL project (default: the directory name)
        #[arg(long)]
        name: Option<String>,
        /// Overwrite an existing weaveffi.toml
        #[arg(long)]
        force: bool,
    },
    /// Generate bindings for every (or the selected) language target
    Generate {
        #[arg(help = INPUT_HELP)]
        input: Option<String>,
        /// Output directory (default: `[project] out`, else ./generated)
        #[arg(short, long)]
        out: Option<String>,
        /// Comma-separated targets: c, cpp, swift, kotlin, node, wasm, python, dotnet, dart, go, ruby (default: `[project] targets`, else all)
        #[arg(short, long)]
        target: Option<String>,
        /// Path to weaveffi.toml (default: the nearest one at or above the input)
        #[arg(long)]
        config: Option<String>,
        /// Print advisory lints after validation
        #[arg(long)]
        warn: bool,
        /// Regenerate every target even if its inputs and files are unchanged
        #[arg(long)]
        force: bool,
        /// Validate and list the files that would be written, without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Validate an API definition without generating anything
    Validate {
        #[arg(help = INPUT_HELP)]
        input: Option<String>,
        /// Path to weaveffi.toml (default: the nearest one at or above the input)
        #[arg(long)]
        config: Option<String>,
        /// Also report advisory lints
        #[arg(long)]
        warn: bool,
        /// Output format
        #[arg(long, value_enum, default_value_t = ReportFormat::Human)]
        format: ReportFormat,
    },
    /// Show how regenerating would change the output directory (writes nothing, runs no hooks)
    Diff {
        #[arg(help = INPUT_HELP)]
        input: Option<String>,
        /// Output directory to compare against (default: `[project] out`, else ./generated)
        #[arg(short, long)]
        out: Option<String>,
        /// Comma-separated targets to compare (default: `[project] targets`, else all)
        #[arg(short, long)]
        target: Option<String>,
        /// Path to weaveffi.toml (default: the nearest one at or above the input)
        #[arg(long)]
        config: Option<String>,
        /// Print only a summary and exit 2 if files differ, 3 if files would be added or removed
        #[arg(long)]
        check: bool,
    },
    /// Assemble publishable packages that bundle prebuilt native libraries
    Package {
        #[arg(help = INPUT_HELP)]
        input: Option<String>,
        /// Output directory for the packaged artifacts
        #[arg(short, long, default_value = "./dist")]
        out: String,
        /// Comma-separated targets to package (default: `[project] targets`, else all)
        #[arg(short, long)]
        target: Option<String>,
        /// Path to weaveffi.toml (default: the nearest one at or above the input)
        #[arg(long)]
        config: Option<String>,
        /// Directory of prebuilt native libraries laid out as `<dir>/<platform>/<lib>`
        #[arg(long)]
        binaries: Option<String>,
        /// Cargo package to build as the native producer, once per platform
        #[arg(long)]
        build: Option<String>,
        /// Comma-separated platform ids (default: the host): darwin-arm64, darwin-x64, linux-x64, linux-arm64, windows-x64, android-arm64, android-x64, wasm32
        #[arg(long)]
        platforms: Option<String>,
        /// Print advisory lints after validation
        #[arg(long)]
        warn: bool,
    },
    /// Extract an IDL document from annotated Rust source
    Extract {
        /// Rust source file to extract the API from
        input: String,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
        /// Output format
        #[arg(short, long, value_enum, default_value_t = IdlFormat::Yaml)]
        format: IdlFormat,
        /// Emit the IDL even if it does not validate (for example, it
        /// references types declared elsewhere)
        #[arg(long)]
        lenient: bool,
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
        /// Schema export format
        #[arg(long, value_enum, default_value_t = SchemaFormat::JsonSchema)]
        format: SchemaFormat,
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
    let quiet = cli.quiet;
    match cli.command {
        Commands::Init { dir, name, force } => {
            commands::init::cmd_init(&commands::init::InitArgs {
                dir: &dir,
                name: name.as_deref(),
                force,
                quiet,
            })?
        }
        Commands::Generate {
            input,
            out,
            target,
            config,
            warn,
            force,
            dry_run,
        } => commands::generate::cmd_generate(&commands::generate::GenerateArgs {
            input: input.as_deref(),
            out: out.as_deref(),
            targets: target.as_deref(),
            config: config.as_deref(),
            warn,
            force,
            dry_run,
            quiet,
        })?,
        Commands::Validate {
            input,
            config,
            warn,
            format,
        } => commands::validate::cmd_validate(
            input.as_deref(),
            config.as_deref(),
            warn,
            format == ReportFormat::Json,
            quiet,
        )?,
        Commands::Diff {
            input,
            out,
            target,
            config,
            check,
        } => commands::diff::cmd_diff(&commands::diff::DiffArgs {
            input: input.as_deref(),
            out: out.as_deref(),
            targets: target.as_deref(),
            config: config.as_deref(),
            check,
            quiet,
        })?,
        Commands::Package {
            input,
            out,
            target,
            config,
            binaries,
            build,
            platforms,
            warn,
        } => commands::package::cmd_package(&commands::package::PackageArgs {
            input: input.as_deref(),
            out: &out,
            targets: target.as_deref(),
            config: config.as_deref(),
            binaries: binaries.as_deref(),
            build: build.as_deref(),
            platforms: platforms.as_deref(),
            warn,
            quiet,
        })?,
        Commands::Extract {
            input,
            output,
            format,
            lenient,
        } => extract::cmd_extract(&input, output.as_deref(), format, lenient, quiet)?,
        Commands::Completions { shell } => cmd_completions(shell),
        Commands::SchemaVersion => println!("{CURRENT_SCHEMA_VERSION}"),
        Commands::Schema { format } => cmd_schema(format)?,
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

fn cmd_schema(format: SchemaFormat) -> Result<()> {
    match format {
        SchemaFormat::JsonSchema => {
            let schema = schemars::schema_for!(weaveffi_model::ir::Api);
            let json = serde_json::to_string_pretty(&schema)
                .into_diagnostic()
                .wrap_err("failed to serialize JSON Schema")?;
            println!("{json}");
        }
    }
    Ok(())
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
