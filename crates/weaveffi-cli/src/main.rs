//! `weaveffi` command-line entry point: `clap` definitions and dispatch.
//!
//! Each subcommand's implementation lives in `commands`, on top of the
//! library's `project` (locating a project and loading its API) and
//! `config` (`weaveffi.toml` and the generator registry).

mod commands;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use commands::Locate;
use miette::{IntoDiagnostic, Result, WrapErr};
use weaveffi_model::ir::CURRENT_SCHEMA_VERSION;

const PLATFORMS_HELP: &str = "Comma-separated platform ids: darwin-arm64, darwin-x64, linux-x64, \
                              linux-arm64, windows-x64, ios-arm64, ios-sim-arm64, ios-sim-x64, \
                              android-arm64, android-x64, wasm32 (default: `[build] platforms`, \
                              else the host)";

const INPUT_HELP: &str = "A Rust producer crate (its directory or Cargo.toml), whose API is read \
                          from its built library, or an IDL document (yaml|yml|json|toml); \
                          defaults to `[project] input` from the nearest weaveffi.toml";

const LIBRARY_HELP: &str = "Read a Rust producer's API from this built library (cdylib, \
                            staticlib, or .wasm) instead of building the crate";

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
        /// Output directory (default: `[project] out`, else ./bindings)
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
        /// Validate and list the files that would be written, without writing
        #[arg(long)]
        dry_run: bool,
        #[arg(long, help = LIBRARY_HELP)]
        library: Option<String>,
        /// Build a Rust producer's library in release mode
        #[arg(long)]
        release: bool,
    },
    /// Build a Rust producer's debug library, generate, and point the bindings at it
    Dev {
        #[arg(help = INPUT_HELP)]
        input: Option<String>,
        /// Output directory (default: `[project] out`, else ./bindings)
        #[arg(short, long)]
        out: Option<String>,
        /// Comma-separated targets (default: `[project] targets`, else all)
        #[arg(short, long)]
        target: Option<String>,
        /// Path to weaveffi.toml (default: the nearest one at or above the input)
        #[arg(long)]
        config: Option<String>,
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
        #[arg(long, help = LIBRARY_HELP)]
        library: Option<String>,
        /// Build a Rust producer's library in release mode
        #[arg(long)]
        release: bool,
    },
    /// Show how regenerating would change the output directory (writes nothing)
    Diff {
        #[arg(help = INPUT_HELP)]
        input: Option<String>,
        /// Output directory to compare against (default: `[project] out`, else ./bindings)
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
        #[arg(long, help = LIBRARY_HELP)]
        library: Option<String>,
        /// Build a Rust producer's library in release mode
        #[arg(long)]
        release: bool,
    },
    /// Cross-compile the Rust producer per platform into target/weaveffi/<platform>/
    Build {
        #[arg(help = INPUT_HELP)]
        input: Option<String>,
        /// Comma-separated platform ids (default: `[build] platforms`, else the host)
        #[arg(long, help = PLATFORMS_HELP)]
        platforms: Option<String>,
        /// Comma-separated targets whose C glue to prebuild (default: `[project] targets`, else all)
        #[arg(short, long)]
        target: Option<String>,
        /// Path to weaveffi.toml (default: the nearest one at or above the input)
        #[arg(long)]
        config: Option<String>,
        /// The producer crate's Cargo.toml (default: `[build] manifest`, else the input's crate)
        #[arg(long)]
        manifest_path: Option<String>,
        /// Build with the dev profile instead of `--release`
        #[arg(long)]
        debug: bool,
        /// Print advisory lints after validation
        #[arg(long)]
        warn: bool,
    },
    /// Build (unless --binaries) and write installable artifacts for each target
    Package {
        #[arg(help = INPUT_HELP)]
        input: Option<String>,
        /// Dist directory for the artifacts (default: `[package] dist`, else ./dist)
        #[arg(short, long)]
        out: Option<String>,
        /// Comma-separated targets to package (default: `[project] targets`, else all)
        #[arg(short, long)]
        target: Option<String>,
        /// Path to weaveffi.toml (default: the nearest one at or above the input)
        #[arg(long)]
        config: Option<String>,
        /// Package existing builds laid out as `<dir>/<platform>/` instead of building
        #[arg(long)]
        binaries: Option<String>,
        /// Comma-separated platform ids (default: `[build] platforms`, else the host; with
        /// --binaries, every platform directory present)
        #[arg(long, help = PLATFORMS_HELP)]
        platforms: Option<String>,
        /// The producer crate's Cargo.toml (default: `[build] manifest`, else the input's crate)
        #[arg(long)]
        manifest_path: Option<String>,
        /// Build with the dev profile instead of `--release`
        #[arg(long)]
        debug: bool,
        /// Print advisory lints after validation
        #[arg(long)]
        warn: bool,
    },
    /// Print the IDL a Rust producer's built library embeds
    Extract {
        /// The producer crate (its directory or Cargo.toml); defaults to `[project] input`
        input: Option<String>,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
        /// Output format
        #[arg(short, long, value_enum, default_value_t = IdlFormat::Yaml)]
        format: IdlFormat,
        /// Path to weaveffi.toml (default: the nearest one at or above the input)
        #[arg(long)]
        config: Option<String>,
        #[arg(long, help = LIBRARY_HELP)]
        library: Option<String>,
        /// Build the library in release mode
        #[arg(long)]
        release: bool,
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
            dry_run,
            library,
            release,
        } => commands::generate::cmd_generate(&commands::generate::GenerateArgs {
            locate: Locate {
                input: input.as_deref(),
                config: config.as_deref(),
                library: library.as_deref(),
                release,
                quiet,
            },
            out: out.as_deref(),
            targets: target.as_deref(),
            warn,
            dry_run,
        })?,
        Commands::Dev {
            input,
            out,
            target,
            config,
        } => commands::dev::cmd_dev(&commands::dev::DevArgs {
            locate: Locate {
                input: input.as_deref(),
                config: config.as_deref(),
                library: None,
                release: false,
                quiet,
            },
            out: out.as_deref(),
            targets: target.as_deref(),
        })?,
        Commands::Validate {
            input,
            config,
            warn,
            format,
            library,
            release,
        } => commands::validate::cmd_validate(
            &Locate {
                input: input.as_deref(),
                config: config.as_deref(),
                library: library.as_deref(),
                release,
                quiet,
            },
            warn,
            format == ReportFormat::Json,
        )?,
        Commands::Diff {
            input,
            out,
            target,
            config,
            check,
            library,
            release,
        } => commands::diff::cmd_diff(&commands::diff::DiffArgs {
            locate: Locate {
                input: input.as_deref(),
                config: config.as_deref(),
                library: library.as_deref(),
                release,
                quiet,
            },
            out: out.as_deref(),
            targets: target.as_deref(),
            check,
        })?,
        Commands::Build {
            input,
            platforms,
            target,
            config,
            manifest_path,
            debug,
            warn,
        } => commands::build::cmd_build(&commands::build::BuildArgs {
            input: input.as_deref(),
            config: config.as_deref(),
            platforms: platforms.as_deref(),
            targets: target.as_deref(),
            debug,
            manifest_path: manifest_path.as_deref(),
            warn,
            quiet,
        })?,
        Commands::Package {
            input,
            out,
            target,
            config,
            binaries,
            platforms,
            manifest_path,
            debug,
            warn,
        } => commands::package::cmd_package(&commands::package::PackageArgs {
            input: input.as_deref(),
            out: out.as_deref(),
            targets: target.as_deref(),
            config: config.as_deref(),
            binaries: binaries.as_deref(),
            platforms: platforms.as_deref(),
            debug,
            manifest_path: manifest_path.as_deref(),
            warn,
            quiet,
        })?,
        Commands::Extract {
            input,
            output,
            format,
            config,
            library,
            release,
        } => commands::extract::cmd_extract(
            &Locate {
                input: input.as_deref(),
                config: config.as_deref(),
                library: library.as_deref(),
                release,
                quiet,
            },
            output.as_deref(),
            format,
        )?,
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
