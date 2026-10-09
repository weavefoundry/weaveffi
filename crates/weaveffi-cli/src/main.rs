//! `weaveffi` command-line entry point: `clap` definitions, dispatch, and
//! error rendering.
//!
//! Each subcommand's implementation lives in the library's `commands`
//! module and returns the process exit code; errors come back as `miette`
//! reports, which only this binary renders.

use std::process::ExitCode;

use clap::builder::{PossibleValue, PossibleValuesParser};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use miette::{IntoDiagnostic, Result, WrapErr};
use weaveffi_cli::commands::{self, extract::IdlFormat, Locate};
use weaveffi_cli::targets::REGISTRY;
use weaveffi_model::ir::CURRENT_SCHEMA_VERSION;

const PLATFORMS_HELP: &str = "Comma-separated platform ids: darwin-arm64, darwin-x64, linux-x64, \
                              linux-arm64, windows-x64, ios-arm64, ios-sim-arm64, ios-sim-x64, \
                              android-arm64, android-x64, wasm32 (default: `[build] platforms`, \
                              else the host)";

const INPUT_HELP: &str = "A Rust producer crate (its directory or Cargo.toml), whose API is read \
                          from its built library, or an IDL document (yaml|yml|json); \
                          defaults to `[project] input` from the nearest weaveffi.toml";

const LIBRARY_HELP: &str = "Read a Rust producer's API from this built library (cdylib, \
                            staticlib, or .wasm) instead of building the crate";

const TARGET_HELP: &str =
    "Comma-separated targets (default: `[project] targets`, else every target)";

const DEV_PROFILE_HELP: &str =
    "The Cargo profile to build a Rust producer's library with (default: dev)";

const RELEASE_PROFILE_HELP: &str =
    "The Cargo profile to build with (default: `[build] profile`, else release)";

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

/// Where a command finds its project.
#[derive(Args, Debug)]
struct LocateArgs {
    #[arg(help = INPUT_HELP)]
    input: Option<String>,
    /// Path to weaveffi.toml (default: the nearest one at or above the input)
    #[arg(long)]
    config: Option<String>,
}

/// `--target`: registered target names, validated and listed by `clap`.
#[derive(Args, Debug)]
struct TargetArgs {
    #[arg(
        short,
        long = "target",
        value_name = "TARGETS",
        value_delimiter = ',',
        value_parser = target_names(),
        help = TARGET_HELP
    )]
    targets: Option<Vec<String>>,
}

/// Every registered target, with its description, for `--target`.
fn target_names() -> PossibleValuesParser {
    PossibleValuesParser::new(
        REGISTRY
            .iter()
            .map(|d| PossibleValue::new(d.name).help(d.description)),
    )
}

/// Output format for `validate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ReportFormat {
    /// Human-readable text.
    Human,
    /// One JSON object on stdout.
    Json,
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
        #[command(flatten)]
        locate: LocateArgs,
        #[command(flatten)]
        targets: TargetArgs,
        /// Output directory (default: `[project] out`, else ./bindings)
        #[arg(short, long)]
        out: Option<String>,
        #[arg(long, help = LIBRARY_HELP)]
        library: Option<String>,
        #[arg(long, value_name = "NAME", help = DEV_PROFILE_HELP)]
        profile: Option<String>,
        /// Print advisory lints after validation
        #[arg(long)]
        warn: bool,
        /// List the files the targets render, without writing
        #[arg(long, conflicts_with_all = ["check", "diff"])]
        dry_run: bool,
        /// Write nothing; list the files that would change and exit 1 if any would
        #[arg(long)]
        check: bool,
        /// Write nothing; print a unified diff of what would change
        #[arg(long)]
        diff: bool,
    },
    /// Build a Rust producer's library, generate, and point the bindings at it
    Dev {
        #[command(flatten)]
        locate: LocateArgs,
        #[command(flatten)]
        targets: TargetArgs,
        /// Output directory (default: `[project] out`, else ./bindings)
        #[arg(short, long)]
        out: Option<String>,
        #[arg(long, help = LIBRARY_HELP)]
        library: Option<String>,
        #[arg(long, value_name = "NAME", help = DEV_PROFILE_HELP)]
        profile: Option<String>,
    },
    /// Validate an API definition without generating anything
    Validate {
        #[command(flatten)]
        locate: LocateArgs,
        #[arg(long, help = LIBRARY_HELP)]
        library: Option<String>,
        #[arg(long, value_name = "NAME", help = DEV_PROFILE_HELP)]
        profile: Option<String>,
        /// Also report advisory lints
        #[arg(long)]
        warn: bool,
        /// Output format
        #[arg(long, value_enum, default_value_t = ReportFormat::Human)]
        format: ReportFormat,
    },
    /// Cross-compile the Rust producer per platform into target/weaveffi/<platform>/
    Build {
        #[command(flatten)]
        locate: LocateArgs,
        /// Comma-separated targets whose C glue to prebuild (default: `[project] targets`,
        /// else every target)
        #[arg(
            short,
            long = "target",
            value_name = "TARGETS",
            value_delimiter = ',',
            value_parser = target_names()
        )]
        targets: Option<Vec<String>>,
        #[arg(long, help = PLATFORMS_HELP)]
        platforms: Option<String>,
        #[arg(long, value_name = "NAME", help = RELEASE_PROFILE_HELP)]
        profile: Option<String>,
        /// The producer crate's Cargo.toml (default: `[build] manifest`, else the input's crate)
        #[arg(long)]
        manifest_path: Option<String>,
        /// Print advisory lints after validation
        #[arg(long)]
        warn: bool,
        /// Exit 1 when an artifact is skipped because a tool is missing
        #[arg(long)]
        strict: bool,
    },
    /// Build (unless --binaries) and write installable artifacts for each target
    Package {
        #[command(flatten)]
        locate: LocateArgs,
        #[command(flatten)]
        targets: TargetArgs,
        /// Dist directory for the artifacts (default: `[package] dist`, else ./dist)
        #[arg(short, long)]
        out: Option<String>,
        /// Package existing builds laid out as `<dir>/<platform>/` instead of building
        #[arg(long)]
        binaries: Option<String>,
        /// Comma-separated platform ids (default: `[build] platforms`, else the host; with
        /// --binaries, every platform directory present)
        #[arg(long, help = PLATFORMS_HELP)]
        platforms: Option<String>,
        #[arg(long, value_name = "NAME", help = RELEASE_PROFILE_HELP)]
        profile: Option<String>,
        /// The producer crate's Cargo.toml (default: `[build] manifest`, else the input's crate)
        #[arg(long)]
        manifest_path: Option<String>,
        /// Print advisory lints after validation
        #[arg(long)]
        warn: bool,
        /// Exit 1 when an artifact is skipped because a tool is missing
        #[arg(long)]
        strict: bool,
    },
    /// Print the IDL a Rust producer's built library embeds
    Extract {
        #[command(flatten)]
        locate: LocateArgs,
        #[arg(long, help = LIBRARY_HELP)]
        library: Option<String>,
        #[arg(long, value_name = "NAME", help = DEV_PROFILE_HELP)]
        profile: Option<String>,
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
        /// Output format
        #[arg(short, long, value_enum, default_value_t = IdlFormat::Yaml)]
        format: IdlFormat,
    },
    /// Print the IDL document schema (or, with --version, its version)
    Schema {
        /// Print the IDL schema version this build reads and writes instead
        #[arg(long)]
        version: bool,
        /// Schema export format
        #[arg(long, value_enum, default_value_t = SchemaFormat::JsonSchema)]
        format: SchemaFormat,
    },
    /// Print shell completions
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
}

fn main() -> Result<ExitCode> {
    let _ = miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .terminal_links(true)
                .context_lines(3)
                .build(),
        )
    }));
    let cli = Cli::parse();
    run(cli.command, cli.quiet)
}

/// The [`Locate`] of a command's flags.
fn locate<'a>(
    args: &'a LocateArgs,
    library: Option<&'a String>,
    profile: Option<&'a String>,
    quiet: bool,
) -> Locate<'a> {
    Locate {
        input: args.input.as_deref(),
        config: args.config.as_deref(),
        library: library.map(String::as_str),
        profile: profile.map(String::as_str),
        quiet,
    }
}

fn run(command: Commands, quiet: bool) -> Result<ExitCode> {
    match command {
        Commands::Init { dir, name, force } => {
            commands::init::cmd_init(&commands::init::InitArgs {
                dir: &dir,
                name: name.as_deref(),
                force,
                quiet,
            })
        }
        Commands::Generate {
            locate: l,
            targets,
            out,
            library,
            profile,
            warn,
            dry_run,
            check,
            diff,
        } => commands::generate::cmd_generate(&commands::generate::GenerateArgs {
            locate: locate(&l, library.as_ref(), profile.as_ref(), quiet),
            out: out.as_deref(),
            targets: targets.targets.as_deref(),
            warn,
            dry_run,
            check,
            diff,
        }),
        Commands::Dev {
            locate: l,
            targets,
            out,
            library,
            profile,
        } => commands::dev::cmd_dev(&commands::dev::DevArgs {
            locate: locate(&l, library.as_ref(), profile.as_ref(), quiet),
            out: out.as_deref(),
            targets: targets.targets.as_deref(),
        }),
        Commands::Validate {
            locate: l,
            library,
            profile,
            warn,
            format,
        } => commands::validate::cmd_validate(
            &locate(&l, library.as_ref(), profile.as_ref(), quiet),
            warn,
            format == ReportFormat::Json,
        ),
        Commands::Build {
            locate: l,
            targets,
            platforms,
            profile,
            manifest_path,
            warn,
            strict,
        } => commands::build::cmd_build(&commands::build::BuildArgs {
            locate: locate(&l, None, profile.as_ref(), quiet),
            platforms: platforms.as_deref(),
            targets: targets.as_deref(),
            manifest_path: manifest_path.as_deref(),
            warn,
            strict,
        }),
        Commands::Package {
            locate: l,
            targets,
            out,
            binaries,
            platforms,
            profile,
            manifest_path,
            warn,
            strict,
        } => commands::package::cmd_package(&commands::package::PackageArgs {
            locate: locate(&l, None, profile.as_ref(), quiet),
            out: out.as_deref(),
            targets: targets.targets.as_deref(),
            binaries: binaries.as_deref(),
            platforms: platforms.as_deref(),
            manifest_path: manifest_path.as_deref(),
            warn,
            strict,
        }),
        Commands::Extract {
            locate: l,
            library,
            profile,
            output,
            format,
        } => commands::extract::cmd_extract(
            &locate(&l, library.as_ref(), profile.as_ref(), quiet),
            output.as_deref(),
            format,
        ),
        Commands::Schema { version, format } => cmd_schema(version, format),
        Commands::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "weaveffi",
                &mut std::io::stdout(),
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn cmd_schema(version: bool, format: SchemaFormat) -> Result<ExitCode> {
    if version {
        println!("{CURRENT_SCHEMA_VERSION}");
        return Ok(ExitCode::SUCCESS);
    }
    match format {
        SchemaFormat::JsonSchema => {
            let schema = schemars::schema_for!(weaveffi_model::ir::Api);
            let json = serde_json::to_string_pretty(&schema)
                .into_diagnostic()
                .wrap_err("failed to serialize JSON Schema")?;
            println!("{json}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

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
    fn targets_are_validated_against_the_registry() {
        let cli = Cli::try_parse_from(["weaveffi", "generate", "--target", "c"]).unwrap();
        let Commands::Generate { targets, .. } = cli.command else {
            panic!("generate");
        };
        assert_eq!(targets.targets.unwrap(), ["c"]);
        let error = Cli::try_parse_from(["weaveffi", "generate", "-t", "c,rustlang"])
            .expect_err("unknown target");
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
        assert!(error.to_string().contains("rustlang"), "{error}");
    }

    #[test]
    fn completions_and_schema_version() {
        for (args, needle) in [
            (&["completions", "bash"][..], "complete"),
            (&["completions", "zsh"][..], "compdef"),
            (&["schema", "--version"][..], CURRENT_SCHEMA_VERSION),
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
