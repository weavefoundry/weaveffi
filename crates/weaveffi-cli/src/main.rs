//! `weaveffi` command-line entry point.
//!
//! Wires together the IR parser, validator, generator orchestrator, and
//! supporting subcommands (`generate`, `validate`, `extract`, `new`,
//! `lint`, `diff`, `doctor`, `completions`, `schema`, `format`, `watch`,
//! `upgrade`).

mod extract;
mod scaffold;

use camino::Utf8Path;
use clap::{CommandFactory, Parser, Subcommand};
use miette::{bail, miette, IntoDiagnostic, NamedSource, Report, Result, WrapErr};
use serde::Deserialize;
use similar::TextDiff;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::process::Command;
use tracing_subscriber::EnvFilter;
use weaveffi_core::codegen::{Generator, Orchestrator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::templates::TemplateEngine;
use weaveffi_core::validate::{collect_warnings, validate_api};
use weaveffi_gen_android::AndroidGenerator;
use weaveffi_gen_c::CGenerator;
use weaveffi_gen_cpp::CppGenerator;
use weaveffi_gen_dart::DartGenerator;
use weaveffi_gen_dotnet::DotnetGenerator;
use weaveffi_gen_go::GoGenerator;
use weaveffi_gen_node::NodeGenerator;
use weaveffi_gen_python::PythonGenerator;
use weaveffi_gen_ruby::RubyGenerator;
use weaveffi_gen_swift::SwiftGenerator;
use weaveffi_gen_wasm::WasmGenerator;
use weaveffi_ir::ir::{CURRENT_SCHEMA_VERSION, SUPPORTED_VERSIONS};
use weaveffi_ir::parse::parse_api_str;

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
        /// Path to a directory containing user template overrides (.tera files)
        #[arg(long)]
        templates: Option<String>,
    },
    Validate {
        /// Input IDL/IR file (yaml|yml|json|toml)
        input: String,
        /// Print non-fatal warnings after validation
        #[arg(long)]
        warn: bool,
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
    },
    Lint {
        /// Input IDL/IR file (yaml|yml|json|toml)
        input: String,
    },
    Diff {
        /// Input IDL/IR file (yaml|yml|json|toml)
        input: String,
        /// Output directory to compare against (defaults to ./generated)
        #[arg(short, long)]
        out: Option<String>,
    },
    Doctor,
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
    SchemaVersion,
    Upgrade {
        /// Input IDL/IR file (yaml|yml|json|toml)
        input: String,
        /// Output file path (defaults to overwriting the input)
        #[arg(short, long)]
        output: Option<String>,
        /// Exit non-zero if migrations would change the file, but do not write
        #[arg(long)]
        check: bool,
    },
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
        Commands::New { name } => cmd_new(&name, quiet)?,
        Commands::Generate {
            input,
            out,
            target,
            scaffold,
            config,
            warn,
            force,
            dry_run,
            templates,
        } => cmd_generate(
            &input,
            &out,
            target.as_deref(),
            scaffold,
            config.as_deref(),
            warn,
            force,
            dry_run,
            templates.as_deref(),
            quiet,
        )?,
        Commands::Validate { input, warn } => cmd_validate(&input, warn, quiet)?,
        Commands::Extract {
            input,
            output,
            format,
        } => cmd_extract(
            &input,
            output.as_deref(),
            format.as_deref().unwrap_or("yaml"),
            quiet,
        )?,
        Commands::Lint { input } => {
            if !cmd_lint(&input, quiet)? {
                std::process::exit(1);
            }
        }
        Commands::Diff { input, out } => cmd_diff(&input, out.as_deref(), quiet)?,
        Commands::Doctor => cmd_doctor()?,
        Commands::Completions { shell } => cmd_completions(shell),
        Commands::SchemaVersion => println!("{CURRENT_SCHEMA_VERSION}"),
        Commands::Upgrade {
            input,
            output,
            check,
        } => cmd_upgrade(&input, output.as_deref(), check, quiet)?,
        Commands::Watch {
            input,
            out,
            target,
            config,
        } => cmd_watch(&input, &out, target.as_deref(), config.as_deref(), quiet)?,
        Commands::Format { input, check } => cmd_format(&input, check, quiet)?,
        Commands::Schema { format } => cmd_schema(&format)?,
    }
    Ok(())
}

fn cmd_new(name: &str, quiet: bool) -> Result<()> {
    let project_dir = Utf8Path::new(name);
    if project_dir.exists() {
        bail!(
            "destination '{}' already exists; choose a new name or remove it",
            name
        );
    }
    std::fs::create_dir_all(project_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create project directory: {}", name))?;

    let module_name = sanitize_module_name(name);

    let idl_path = project_dir.join("weaveffi.yml");
    let idl_contents = format!(
        concat!(
            "version: \"0.3.0\"\n",
            "modules:\n",
            "  - name: {module}\n",
            "    structs:\n",
            "      - name: Item\n",
            "        fields:\n",
            "          - {{ name: id, type: i64 }}\n",
            "          - {{ name: name, type: string }}\n",
            "          - {{ name: description, type: \"string?\" }}\n",
            "    functions:\n",
            "      - name: create_item\n",
            "        params:\n",
            "          - {{ name: name, type: string }}\n",
            "          - {{ name: description, type: \"string?\" }}\n",
            "        return: handle\n",
            "      - name: get_item\n",
            "        params:\n",
            "          - {{ name: id, type: handle }}\n",
            "        return: Item\n",
            "      - name: list_items\n",
            "        params: []\n",
            "        return: \"[Item]\"\n",
            "      - name: delete_item\n",
            "        params:\n",
            "          - {{ name: id, type: handle }}\n",
            "        return: bool\n",
        ),
        module = module_name
    );
    std::fs::write(idl_path.as_std_path(), &idl_contents)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", idl_path))?;

    let mut api = parse_api_str(&idl_contents, "yaml").wrap_err("failed to parse generated IDL")?;
    validate_api(&mut api, None).wrap_err("generated IDL failed validation")?;

    let cargo_toml_path = project_dir.join("Cargo.toml");
    let cargo_toml = format!(
        concat!(
            "[package]\n",
            "name = \"{name}\"\n",
            "version = \"0.1.0\"\n",
            "edition = \"2021\"\n",
            "publish = false\n",
            "\n",
            "[lib]\n",
            "crate-type = [\"cdylib\"]\n",
            "\n",
            "[dependencies]\n",
            "weaveffi-abi = \"0.3\"\n",
            "\n",
            "[lints.rust]\n",
            "unsafe_code = \"allow\"\n",
        ),
        name = name,
    );
    std::fs::write(cargo_toml_path.as_std_path(), &cargo_toml)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", cargo_toml_path))?;

    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(src_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create {}", src_dir))?;
    let lib_rs_path = src_dir.join("lib.rs");
    let lib_contents = scaffold::render_scaffold(&api, "weaveffi");
    std::fs::write(lib_rs_path.as_std_path(), &lib_contents)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", lib_rs_path))?;

    let readme_path = project_dir.join("README.md");
    let readme = format!(
        concat!(
            "# {name}\n\n",
            "A WeaveFFI project.\n\n",
            "## Getting Started\n\n",
            "1. Implement the `todo!()` stubs in `src/lib.rs`.\n",
            "2. Build the shared library:\n\n",
            "   ```sh\n",
            "   cargo build\n",
            "   ```\n\n",
            "3. Generate foreign-language bindings:\n\n",
            "   ```sh\n",
            "   weaveffi generate weaveffi.yml -o generated\n",
            "   ```\n\n",
            "4. Test your library:\n\n",
            "   ```sh\n",
            "   cargo test\n",
            "   ```\n\n",
            "5. Use the generated bindings from Swift, Kotlin, Node.js, Python, .NET, or WASM.\n",
        ),
        name = name,
    );
    std::fs::write(readme_path.as_std_path(), &readme)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", readme_path))?;

    if !quiet {
        println!("Initialized WeaveFFI project at {}", project_dir);
        println!("Next steps:");
        println!("  cd {name}");
        println!("  # Implement the todo!() stubs in src/lib.rs");
        println!("  cargo build");
        println!("  weaveffi generate weaveffi.yml -o generated");
    }
    Ok(())
}

fn load_config(path: Option<&str>) -> Result<GeneratorConfig> {
    match path {
        Some(p) => {
            let contents = std::fs::read_to_string(p)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to read config file: {}", p))?;
            toml::from_str(&contents)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to parse config file: {}", p))
        }
        None => Ok(GeneratorConfig::default()),
    }
}

/// Subset of `GeneratorConfig` fields with every value optional so that inline
/// IDL overrides only touch keys the user actually set.
#[derive(Default, Deserialize)]
struct InlineGeneratorOverrides {
    swift_module_name: Option<String>,
    android_package: Option<String>,
    node_package_name: Option<String>,
    wasm_module_name: Option<String>,
    c_prefix: Option<String>,
    python_package_name: Option<String>,
    dotnet_namespace: Option<String>,
    cpp_namespace: Option<String>,
    cpp_header_name: Option<String>,
    cpp_standard: Option<String>,
    dart_package_name: Option<String>,
    go_module_path: Option<String>,
    ruby_module_name: Option<String>,
    ruby_gem_name: Option<String>,
    strip_module_prefix: Option<bool>,
    template_dir: Option<String>,
    pre_generate: Option<String>,
    post_generate: Option<String>,
}

impl InlineGeneratorOverrides {
    fn apply(self, config: &mut GeneratorConfig) {
        if let Some(v) = self.swift_module_name {
            config.swift_module_name = Some(v);
        }
        if let Some(v) = self.android_package {
            config.android_package = Some(v);
        }
        if let Some(v) = self.node_package_name {
            config.node_package_name = Some(v);
        }
        if let Some(v) = self.wasm_module_name {
            config.wasm_module_name = Some(v);
        }
        if let Some(v) = self.c_prefix {
            config.c_prefix = Some(v);
        }
        if let Some(v) = self.python_package_name {
            config.python_package_name = Some(v);
        }
        if let Some(v) = self.dotnet_namespace {
            config.dotnet_namespace = Some(v);
        }
        if let Some(v) = self.cpp_namespace {
            config.cpp_namespace = Some(v);
        }
        if let Some(v) = self.cpp_header_name {
            config.cpp_header_name = Some(v);
        }
        if let Some(v) = self.cpp_standard {
            config.cpp_standard = Some(v);
        }
        if let Some(v) = self.dart_package_name {
            config.dart_package_name = Some(v);
        }
        if let Some(v) = self.go_module_path {
            config.go_module_path = Some(v);
        }
        if let Some(v) = self.ruby_module_name {
            config.ruby_module_name = Some(v);
        }
        if let Some(v) = self.ruby_gem_name {
            config.ruby_gem_name = Some(v);
        }
        if let Some(v) = self.strip_module_prefix {
            config.strip_module_prefix = v;
        }
        if let Some(v) = self.template_dir {
            config.template_dir = Some(v);
        }
        if let Some(v) = self.pre_generate {
            config.pre_generate = Some(v);
        }
        if let Some(v) = self.post_generate {
            config.post_generate = Some(v);
        }
    }
}

/// Merge `[generators]` overrides from the IDL into `config`.
///
/// Inline IDL values **override** anything supplied via `--config <toml>`,
/// because the IDL is project-local and committed alongside the API
/// definition while a TOML file is typically per-environment.
///
/// Each `(target, table)` pair is interpreted as follows:
///
/// * For per-target sections (`swift`, `android`, `node`, `wasm`, `c`,
///   `python`, `dotnet`, `cpp`, `dart`, `go`, `ruby`), every key in `table`
///   is prefixed with `{target}_` and the resulting flat table is
///   deserialized into [`InlineGeneratorOverrides`]. So `swift.module_name`
///   maps to `swift_module_name`, `cpp.standard` maps to `cpp_standard`,
///   and so on.
/// * For the special section `weaveffi` (or its alias `global`), the table
///   keys are treated as direct [`GeneratorConfig`] field names:
///   `strip_module_prefix`, `template_dir`, `pre_generate`, `post_generate`.
///
/// Unknown target names and unknown keys within a known target are silently
/// ignored so that older CLIs can read newer IDL files without crashing.
fn merge_inline_generators(
    config: &mut GeneratorConfig,
    generators: &BTreeMap<String, toml::Value>,
) {
    const KNOWN_TARGETS: &[&str] = &[
        "swift", "android", "node", "wasm", "c", "python", "dotnet", "cpp", "dart", "go", "ruby",
    ];

    for (target, value) in generators {
        let Some(table) = value.as_table() else {
            continue;
        };

        if target == "weaveffi" || target == "global" {
            if let Ok(overrides) =
                InlineGeneratorOverrides::deserialize(toml::Value::Table(table.clone()))
            {
                overrides.apply(config);
            }
            continue;
        }

        if !KNOWN_TARGETS.contains(&target.as_str()) {
            continue;
        }

        let mut prefixed = toml::value::Table::new();
        for (key, val) in table {
            prefixed.insert(format!("{target}_{key}"), val.clone());
        }

        if let Ok(overrides) = InlineGeneratorOverrides::deserialize(toml::Value::Table(prefixed)) {
            overrides.apply(config);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_generate(
    input: &str,
    out: &str,
    targets: Option<&str>,
    emit_scaffold: bool,
    config_path: Option<&str>,
    warn: bool,
    force: bool,
    dry_run: bool,
    templates_path: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let mut config = load_config(config_path)?;

    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    let format = match ext {
        "yml" | "yaml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        other => bail!(
            "unsupported input format: {} (expected yml|yaml|json|toml)",
            other
        ),
    };
    let contents = std::fs::read_to_string(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    let mut api =
        parse_api_str(&contents, format).map_err(|e| with_named_source(e, input, &contents))?;
    validate_api(&mut api, Some((input, &contents))).map_err(Report::new)?;

    if let Some(ref generators) = api.generators {
        merge_inline_generators(&mut config, generators);
    }

    if warn {
        for w in collect_warnings(&api) {
            eprintln!("warning: {w}");
        }
    }

    let out_dir = Utf8Path::new(out);

    let c = CGenerator;
    let cpp = CppGenerator;
    let swift = SwiftGenerator;
    let android = AndroidGenerator;
    let node = NodeGenerator;
    let wasm = WasmGenerator;
    let python = PythonGenerator;
    let dotnet = DotnetGenerator;
    let dart = DartGenerator;
    let go = GoGenerator;
    let ruby = RubyGenerator;
    let all: Vec<&dyn Generator> = vec![
        &c, &cpp, &swift, &android, &node, &wasm, &python, &dotnet, &dart, &go, &ruby,
    ];

    let filter: Option<Vec<&str>> = targets.map(|t| t.split(',').map(str::trim).collect());

    let selected: Vec<&dyn Generator> = all
        .into_iter()
        .filter(|gen| filter.as_ref().is_none_or(|ts| ts.contains(&gen.name())))
        .collect();

    if dry_run {
        for gen in &selected {
            for path in gen.output_files_with_config(&api, out_dir, &config) {
                println!("{path}");
            }
        }
        return Ok(());
    }

    std::fs::create_dir_all(out_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create output directory: {}", out))?;

    let engine = match templates_path {
        Some(dir) => {
            let mut te = TemplateEngine::new();
            te.load_dir(Utf8Path::new(dir))
                .map_err(|e| miette!("failed to load templates from {}: {:#}", dir, e))?;
            Some(te)
        }
        None => None,
    };

    let mut orchestrator = Orchestrator::new();
    for &gen in &selected {
        orchestrator = orchestrator.with_generator(gen);
    }

    orchestrator
        .run(&api, out_dir, &config, force, engine.as_ref())
        .map_err(|e| miette!("{:#}", e))?;

    if emit_scaffold {
        let scaffold_path = out_dir.join("scaffold.rs");
        let contents = scaffold::render_scaffold(&api, config.c_prefix());
        std::fs::write(scaffold_path.as_std_path(), contents)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to write {}", scaffold_path))?;
        if !quiet {
            println!("Scaffold written to {}", scaffold_path);
        }
    }

    if !quiet {
        println!("Generated artifacts in {}", out);
    }
    Ok(())
}

fn cmd_validate(input: &str, warn: bool, quiet: bool) -> Result<()> {
    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    let format = match ext {
        "yml" | "yaml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        other => bail!(
            "unsupported input format: {} (expected yml|yaml|json|toml)",
            other
        ),
    };
    let contents = std::fs::read_to_string(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    let mut api =
        parse_api_str(&contents, format).map_err(|e| with_named_source(e, input, &contents))?;

    match validate_api(&mut api, Some((input, &contents))) {
        Ok(()) => {
            if warn {
                for w in collect_warnings(&api) {
                    eprintln!("warning: {w}");
                }
            }
            if !quiet {
                let n_modules = api.modules.len();
                let n_functions: usize = api.modules.iter().map(|m| m.functions.len()).sum();
                let n_structs: usize = api.modules.iter().map(|m| m.structs.len()).sum();
                let n_enums: usize = api.modules.iter().map(|m| m.enums.len()).sum();
                println!("Validation passed");
                println!(
                    "  {} modules, {} functions, {} structs, {} enums",
                    n_modules, n_functions, n_structs, n_enums
                );
            }
            Ok(())
        }
        Err(e) => Err(Report::new(e)),
    }
}

/// Returns `Ok(true)` when the file is clean, `Ok(false)` when warnings were found.
fn cmd_lint(input: &str, quiet: bool) -> Result<bool> {
    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    let format = match ext {
        "yml" | "yaml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        other => bail!(
            "unsupported input format: {} (expected yml|yaml|json|toml)",
            other
        ),
    };
    let contents = std::fs::read_to_string(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    let mut api =
        parse_api_str(&contents, format).map_err(|e| with_named_source(e, input, &contents))?;
    validate_api(&mut api, Some((input, &contents))).map_err(Report::new)?;

    let warnings = collect_warnings(&api);
    if warnings.is_empty() {
        if !quiet {
            println!("No warnings.");
        }
        Ok(true)
    } else {
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        Ok(false)
    }
}

fn cmd_diff(input: &str, out: Option<&str>, quiet: bool) -> Result<()> {
    let out = out.unwrap_or("./generated");

    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    let format = match ext {
        "yml" | "yaml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        other => bail!(
            "unsupported input format: {} (expected yml|yaml|json|toml)",
            other
        ),
    };
    let contents = std::fs::read_to_string(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    let mut api =
        parse_api_str(&contents, format).map_err(|e| with_named_source(e, input, &contents))?;
    validate_api(&mut api, Some((input, &contents))).map_err(Report::new)?;

    let tmp = tempfile::tempdir()
        .into_diagnostic()
        .wrap_err("failed to create temp directory")?;
    let tmp_path = Utf8Path::from_path(tmp.path())
        .ok_or_else(|| miette!("temp directory path is not valid UTF-8"))?;

    let c = CGenerator;
    let cpp = CppGenerator;
    let swift = SwiftGenerator;
    let android = AndroidGenerator;
    let node = NodeGenerator;
    let wasm = WasmGenerator;
    let python = PythonGenerator;
    let dotnet = DotnetGenerator;
    let dart = DartGenerator;
    let go = GoGenerator;
    let ruby = RubyGenerator;
    let all: Vec<&dyn Generator> = vec![
        &c, &cpp, &swift, &android, &node, &wasm, &python, &dotnet, &dart, &go, &ruby,
    ];

    let config = GeneratorConfig::default();
    let mut orchestrator = Orchestrator::new();
    for &gen in &all {
        orchestrator = orchestrator.with_generator(gen);
    }
    orchestrator
        .run(&api, tmp_path, &config, true, None)
        .map_err(|e| miette!("{:#}", e))?;

    let out_dir = Utf8Path::new(out);

    let generated = collect_relative_files(tmp_path)?;
    let existing = if out_dir.exists() {
        collect_relative_files(out_dir)?
    } else {
        BTreeSet::new()
    };

    let all_paths: BTreeSet<_> = generated.union(&existing).collect();
    let mut has_diff = false;

    for rel in &all_paths {
        let gen_file = tmp_path.join(rel);
        let out_file = out_dir.join(rel);

        match (gen_file.exists(), out_file.exists()) {
            (true, false) => {
                has_diff = true;
                println!("{rel}: [new file]");
            }
            (false, true) => {
                has_diff = true;
                println!("{rel}: [would be removed]");
            }
            (true, true) => {
                let gen_content =
                    std::fs::read_to_string(gen_file.as_std_path()).into_diagnostic()?;
                let out_content =
                    std::fs::read_to_string(out_file.as_std_path()).into_diagnostic()?;
                if gen_content != out_content {
                    has_diff = true;
                    print_unified_diff(rel, &out_content, &gen_content);
                }
            }
            _ => {}
        }
    }

    if !has_diff && !quiet {
        println!("No differences found.");
    }

    Ok(())
}

fn collect_relative_files(base: &Utf8Path) -> Result<BTreeSet<String>> {
    let mut files = BTreeSet::new();
    walk_dir(base, base, &mut files)?;
    Ok(files)
}

fn walk_dir(base: &Utf8Path, dir: &Utf8Path, out: &mut BTreeSet<String>) -> Result<()> {
    let entries = std::fs::read_dir(dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read directory: {}", dir))?;
    for entry in entries {
        let entry = entry.into_diagnostic()?;
        let path = entry.path();
        let utf8 = Utf8Path::from_path(&path)
            .ok_or_else(|| miette!("non-UTF-8 path: {:?}", path))?
            .to_owned();
        if utf8.is_dir() {
            walk_dir(base, &utf8, out)?;
        } else {
            let rel = utf8
                .strip_prefix(base)
                .into_diagnostic()
                .wrap_err("failed to strip prefix")?
                .to_string();
            if rel != ".weaveffi-cache" {
                out.insert(rel);
            }
        }
    }
    Ok(())
}

fn print_unified_diff(path: &str, old: &str, new: &str) {
    let diff = TextDiff::from_lines(old, new);
    println!("--- {path}");
    println!("+++ {path}");
    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        println!("{hunk}");
    }
}

/// Wrap a [`miette::Diagnostic`] (e.g. a parse error) in a [`Report`] while
/// forcing its source code to be a [`NamedSource`] so the fancy renderer prints
/// the filename in the snippet header. miette's built-in `with_source_code` is
/// a no-op when the inner diagnostic already provides `#[source_code]`, so we
/// use a small wrapper that overrides `source_code()` instead.
fn with_named_source<E>(err: E, filename: &str, contents: &str) -> Report
where
    E: miette::Diagnostic + Send + Sync + 'static,
{
    Report::new(NamedDiagnostic {
        inner: err,
        src: NamedSource::new(filename, contents.to_string()),
    })
}

#[derive(Debug)]
struct NamedDiagnostic<E> {
    inner: E,
    src: NamedSource<String>,
}

impl<E: std::fmt::Display> std::fmt::Display for NamedDiagnostic<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl<E: std::error::Error + 'static> std::error::Error for NamedDiagnostic<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

impl<E: miette::Diagnostic + 'static> miette::Diagnostic for NamedDiagnostic<E> {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.inner.code()
    }
    fn severity(&self) -> Option<miette::Severity> {
        self.inner.severity()
    }
    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.inner.help()
    }
    fn url<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.inner.url()
    }
    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        self.inner.labels()
    }
    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn miette::Diagnostic> + 'a>> {
        self.inner.related()
    }
    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        self.inner.diagnostic_source()
    }
    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        Some(&self.src)
    }
}

fn cmd_extract(input: &str, output: Option<&str>, format: &str, quiet: bool) -> Result<()> {
    let source = std::fs::read_to_string(input)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read source file: {}", input))?;

    let mut api = extract::extract_api_from_rust(&source)
        .map_err(|e| miette!("failed to extract API from Rust source: {:#}", e))?;

    if let Err(e) = validate_api(&mut api, None) {
        eprintln!("warning: {}", e);
    }

    let serialized = match format {
        "yaml" | "yml" => serde_yaml::to_string(&api)
            .into_diagnostic()
            .wrap_err("failed to serialize API as YAML")?,
        "json" => serde_json::to_string_pretty(&api)
            .into_diagnostic()
            .wrap_err("failed to serialize API as JSON")?,
        "toml" => toml::to_string_pretty(&api)
            .into_diagnostic()
            .wrap_err("failed to serialize API as TOML")?,
        other => bail!(
            "unsupported output format: {} (expected yaml, json, or toml)",
            other
        ),
    };

    match output {
        Some(path) => {
            std::fs::write(path, &serialized)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to write output file: {}", path))?;
            if !quiet {
                println!("Extracted API written to {}", path);
            }
        }
        None => print!("{}", serialized),
    }

    Ok(())
}

fn cmd_doctor() -> Result<()> {
    println!("WeaveFFI Doctor: checking toolchain prerequisites\n");

    check_tool(
        "rustc",
        &["--version"],
        "Rust compiler",
        Some("Install via https://rustup.rs"),
    );
    check_tool(
        "cargo",
        &["--version"],
        "Cargo (Rust package manager)",
        Some("Install via https://rustup.rs"),
    );

    if cfg!(target_os = "macos") {
        check_tool(
            "xcodebuild",
            &["-version"],
            "Xcode command-line tools",
            Some("Install Xcode from the App Store, then run `xcode-select --install`"),
        );
    } else {
        println!("- Xcode: skipped (non-macOS)");
    }

    let ndk_hint = if cfg!(target_os = "macos") {
        Some("Install via Android Studio SDK Manager or `brew install android-ndk`. Set ANDROID_NDK_HOME.")
    } else {
        Some("Install via Android Studio SDK Manager. Set ANDROID_NDK_HOME.")
    };
    let ndk_ok = check_tool("ndk-build", &["-v"], "Android NDK (ndk-build)", ndk_hint);
    if !ndk_ok {
        let env_ok = env::var_os("ANDROID_NDK_HOME")
            .map(|p| std::path::Path::new(&p).exists())
            .unwrap_or(false)
            || env::var_os("ANDROID_NDK_ROOT")
                .map(|p| std::path::Path::new(&p).exists())
                .unwrap_or(false);
        if env_ok {
            println!(
                "  note: ANDROID_NDK_HOME/ROOT is set and exists; ensure `ndk-build` is in PATH"
            );
        }
    }

    check_tool(
        "node",
        &["-v"],
        "Node.js",
        Some("Install from https://nodejs.org or with your package manager"),
    );
    check_tool(
        "npm",
        &["-v"],
        "npm",
        Some("Install Node.js which includes npm, or use pnpm/yarn"),
    );

    check_cross_targets();

    println!("\nWebAssembly tools:");
    check_tool(
        "wasm-pack",
        &["--version"],
        "wasm-pack",
        Some("install with `cargo install wasm-pack`"),
    );
    check_tool(
        "wasm-bindgen",
        &["--version"],
        "wasm-bindgen-cli",
        Some("install with `cargo install wasm-bindgen-cli`"),
    );

    println!("\nDoctor completed. Address any missing items above.");
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

fn cmd_upgrade(input: &str, output: Option<&str>, check: bool, quiet: bool) -> Result<()> {
    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    let format = match ext {
        "yml" | "yaml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        other => bail!(
            "unsupported input format: {} (expected yml|yaml|json|toml)",
            other
        ),
    };
    let contents = std::fs::read_to_string(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;

    let outcome = match format {
        "yaml" => upgrade_yaml(&contents, quiet)?,
        "json" => upgrade_json(&contents, quiet)?,
        "toml" => upgrade_toml(&contents, quiet)?,
        _ => unreachable!(),
    };

    match outcome {
        UpgradeOutcome::AlreadyCurrent(v) => {
            if !quiet {
                println!("Already up to date (version {v}).");
            }
            Ok(())
        }
        UpgradeOutcome::Migrated {
            from,
            contents: new,
        } => {
            if check {
                if new != contents {
                    if !quiet {
                        eprintln!(
                            "{input} is outdated (version {from}); run 'weaveffi upgrade {input}' to migrate"
                        );
                    }
                    std::process::exit(2);
                }
                return Ok(());
            }
            let dest = output.unwrap_or(input);
            std::fs::write(dest, &new)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to write output file: {}", dest))?;
            if !quiet {
                println!("Upgraded {dest} from {from} to {CURRENT_SCHEMA_VERSION}");
            }
            Ok(())
        }
    }
}

enum UpgradeOutcome {
    AlreadyCurrent(String),
    Migrated { from: String, contents: String },
}

fn read_version_str<F>(get: F) -> Result<String>
where
    F: FnOnce() -> Option<String>,
{
    get().ok_or_else(|| {
        miette!("missing or non-string 'version' field; cannot determine schema version to migrate from")
    })
}

fn ensure_supported(version: &str) -> Result<()> {
    if !SUPPORTED_VERSIONS.contains(&version) {
        bail!(
            "unsupported source version '{}'; supported: {}",
            version,
            SUPPORTED_VERSIONS.join(", ")
        );
    }
    Ok(())
}

fn upgrade_yaml(input: &str, quiet: bool) -> Result<UpgradeOutcome> {
    let mut value: serde_yaml::Value = serde_yaml::from_str(input)
        .into_diagnostic()
        .wrap_err("failed to parse YAML")?;
    let version = read_version_str(|| {
        value
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })?;
    if version == CURRENT_SCHEMA_VERSION {
        return Ok(UpgradeOutcome::AlreadyCurrent(version));
    }
    ensure_supported(&version)?;
    if let serde_yaml::Value::Mapping(map) = &mut value {
        if let Some(modules) = map.get_mut("modules") {
            yaml_strip_callback_params(modules, "", &version, quiet);
        }
        map.insert(
            serde_yaml::Value::String("version".into()),
            serde_yaml::Value::String(CURRENT_SCHEMA_VERSION.into()),
        );
    }
    let new_contents = serde_yaml::to_string(&value)
        .into_diagnostic()
        .wrap_err("failed to serialize YAML")?;
    Ok(UpgradeOutcome::Migrated {
        from: version,
        contents: new_contents,
    })
}

fn yaml_strip_callback_params(
    modules: &mut serde_yaml::Value,
    parent_path: &str,
    from_version: &str,
    quiet: bool,
) {
    if from_version == "0.1.0" {
        return;
    }
    let serde_yaml::Value::Sequence(mods) = modules else {
        return;
    };
    for module in mods.iter_mut() {
        let serde_yaml::Value::Mapping(map) = module else {
            continue;
        };
        let module_name = map
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let qualified = if parent_path.is_empty() {
            module_name
        } else {
            format!("{parent_path}.{module_name}")
        };
        if let Some(serde_yaml::Value::Sequence(fns)) = map.get_mut("functions") {
            for func in fns.iter_mut() {
                let serde_yaml::Value::Mapping(fmap) = func else {
                    continue;
                };
                let fname = fmap
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unnamed)")
                    .to_string();
                if let Some(serde_yaml::Value::Sequence(p)) = fmap.get_mut("params") {
                    p.retain(|param| {
                        let serde_yaml::Value::Mapping(pmap) = param else {
                            return true;
                        };
                        let ty = pmap.get("type").and_then(|v| v.as_str());
                        if ty == Some("callback") {
                            let pname = pmap
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(unnamed)");
                            if !quiet {
                                eprintln!(
                                    "warning: removed callback-typed param '{pname}' from function '{qualified}::{fname}'"
                                );
                            }
                            return false;
                        }
                        true
                    });
                }
            }
        }
        if let Some(submodules) = map.get_mut("modules") {
            yaml_strip_callback_params(submodules, &qualified, from_version, quiet);
        }
    }
}

fn upgrade_json(input: &str, quiet: bool) -> Result<UpgradeOutcome> {
    let mut value: serde_json::Value = serde_json::from_str(input)
        .into_diagnostic()
        .wrap_err("failed to parse JSON")?;
    let version = read_version_str(|| {
        value
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })?;
    if version == CURRENT_SCHEMA_VERSION {
        return Ok(UpgradeOutcome::AlreadyCurrent(version));
    }
    ensure_supported(&version)?;
    if let serde_json::Value::Object(map) = &mut value {
        if let Some(modules) = map.get_mut("modules") {
            json_strip_callback_params(modules, "", &version, quiet);
        }
        map.insert(
            "version".to_string(),
            serde_json::Value::String(CURRENT_SCHEMA_VERSION.into()),
        );
    }
    let new_contents = serde_json::to_string_pretty(&value)
        .into_diagnostic()
        .wrap_err("failed to serialize JSON")?;
    Ok(UpgradeOutcome::Migrated {
        from: version,
        contents: new_contents,
    })
}

fn json_strip_callback_params(
    modules: &mut serde_json::Value,
    parent_path: &str,
    from_version: &str,
    quiet: bool,
) {
    if from_version == "0.1.0" {
        return;
    }
    let serde_json::Value::Array(mods) = modules else {
        return;
    };
    for module in mods.iter_mut() {
        let serde_json::Value::Object(map) = module else {
            continue;
        };
        let module_name = map
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let qualified = if parent_path.is_empty() {
            module_name
        } else {
            format!("{parent_path}.{module_name}")
        };
        if let Some(serde_json::Value::Array(fns)) = map.get_mut("functions") {
            for func in fns.iter_mut() {
                let serde_json::Value::Object(fmap) = func else {
                    continue;
                };
                let fname = fmap
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unnamed)")
                    .to_string();
                if let Some(serde_json::Value::Array(p)) = fmap.get_mut("params") {
                    p.retain(|param| {
                        let serde_json::Value::Object(pmap) = param else {
                            return true;
                        };
                        let ty = pmap.get("type").and_then(|v| v.as_str());
                        if ty == Some("callback") {
                            let pname = pmap
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(unnamed)");
                            if !quiet {
                                eprintln!(
                                    "warning: removed callback-typed param '{pname}' from function '{qualified}::{fname}'"
                                );
                            }
                            return false;
                        }
                        true
                    });
                }
            }
        }
        if let Some(submodules) = map.get_mut("modules") {
            json_strip_callback_params(submodules, &qualified, from_version, quiet);
        }
    }
}

fn upgrade_toml(input: &str, quiet: bool) -> Result<UpgradeOutcome> {
    let mut value: toml::Value = toml::from_str(input)
        .into_diagnostic()
        .wrap_err("failed to parse TOML")?;
    let version = read_version_str(|| {
        value
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })?;
    if version == CURRENT_SCHEMA_VERSION {
        return Ok(UpgradeOutcome::AlreadyCurrent(version));
    }
    ensure_supported(&version)?;
    if let toml::Value::Table(map) = &mut value {
        if let Some(modules) = map.get_mut("modules") {
            toml_strip_callback_params(modules, "", &version, quiet);
        }
        map.insert(
            "version".to_string(),
            toml::Value::String(CURRENT_SCHEMA_VERSION.into()),
        );
    }
    let new_contents = toml::to_string_pretty(&value)
        .into_diagnostic()
        .wrap_err("failed to serialize TOML")?;
    Ok(UpgradeOutcome::Migrated {
        from: version,
        contents: new_contents,
    })
}

fn toml_strip_callback_params(
    modules: &mut toml::Value,
    parent_path: &str,
    from_version: &str,
    quiet: bool,
) {
    if from_version == "0.1.0" {
        return;
    }
    let toml::Value::Array(mods) = modules else {
        return;
    };
    for module in mods.iter_mut() {
        let toml::Value::Table(map) = module else {
            continue;
        };
        let module_name = map
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let qualified = if parent_path.is_empty() {
            module_name
        } else {
            format!("{parent_path}.{module_name}")
        };
        if let Some(toml::Value::Array(fns)) = map.get_mut("functions") {
            for func in fns.iter_mut() {
                let toml::Value::Table(fmap) = func else {
                    continue;
                };
                let fname = fmap
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unnamed)")
                    .to_string();
                if let Some(toml::Value::Array(p)) = fmap.get_mut("params") {
                    p.retain(|param| {
                        let toml::Value::Table(pmap) = param else {
                            return true;
                        };
                        let ty = pmap.get("type").and_then(|v| v.as_str());
                        if ty == Some("callback") {
                            let pname = pmap
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(unnamed)");
                            if !quiet {
                                eprintln!(
                                    "warning: removed callback-typed param '{pname}' from function '{qualified}::{fname}'"
                                );
                            }
                            return false;
                        }
                        true
                    });
                }
            }
        }
        if let Some(submodules) = map.get_mut("modules") {
            toml_strip_callback_params(submodules, &qualified, from_version, quiet);
        }
    }
}

fn sanitize_module_name(name: &str) -> String {
    let lowered = name.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if matches!(ch, '-' | '_' | ' ') {
            out.push('_');
        }
    }
    if out.is_empty() {
        String::from("module")
    } else {
        out
    }
}

fn check_cross_targets() {
    println!("\nCross-compilation targets:");

    let installed = match Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        _ => {
            println!("- rustup: MISSING (cannot check installed targets)");
            println!("  hint: install via https://rustup.rs");
            return;
        }
    };

    let required = [
        ("aarch64-apple-ios", "iOS"),
        ("aarch64-linux-android", "Android"),
        ("wasm32-unknown-unknown", "WebAssembly"),
    ];

    for (target, label) in &required {
        if installed.lines().any(|line| line.trim() == *target) {
            println!("- {} ({}): installed", label, target);
        } else {
            println!("- {} ({}): MISSING", label, target);
            println!("  hint: install with `rustup target add {}`", target);
        }
    }
}

fn check_tool<S: AsRef<OsStr>>(cmd: &str, args: &[S], label: &str, hint: Option<&str>) -> bool {
    match Command::new(cmd).args(args).output() {
        Ok(out) => {
            if out.status.success() {
                let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if ver.is_empty() {
                    println!("- {}: OK ({})", label, cmd);
                } else {
                    println!("- {}: OK ({}: {})", label, cmd, ver);
                }
                true
            } else {
                println!(
                    "- {}: MISSING ({} exited with status {})",
                    label, cmd, out.status
                );
                if let Some(h) = hint {
                    println!("  hint: {}", h);
                }
                false
            }
        }
        Err(_) => {
            println!("- {}: MISSING ({} not found in PATH)", label, cmd);
            if let Some(h) = hint {
                println!("  hint: {}", h);
            }
            false
        }
    }
}

/// Returns `true` when enough time has elapsed since the most recent file
/// system event for the debounce window to have closed and the generator to
/// fire. Pure function so the watch loop can be unit-tested without real
/// timers or `notify` events.
fn debounce_should_fire(
    last_event: std::time::Instant,
    now: std::time::Instant,
    debounce: std::time::Duration,
) -> bool {
    now.saturating_duration_since(last_event) >= debounce
}

#[allow(clippy::too_many_arguments)]
fn cmd_watch(
    input: &str,
    out: &str,
    targets: Option<&str>,
    config_path: Option<&str>,
    quiet: bool,
) -> Result<()> {
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let in_path = Utf8Path::new(input);
    let abs_input = std::fs::canonicalize(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to resolve input file: {}", input))?;
    let parent = abs_input
        .parent()
        .ok_or_else(|| miette!("input file has no parent directory: {}", input))?
        .to_path_buf();

    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .into_diagnostic()
    .wrap_err("failed to create file watcher")?;
    watcher
        .watch(&parent, RecursiveMode::NonRecursive)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to watch directory: {}", parent.display()))?;

    if let Err(e) = cmd_generate(
        input,
        out,
        targets,
        false,
        config_path,
        false,
        false,
        false,
        None,
        quiet,
    ) {
        eprintln!("error: {e:?}");
    }
    if !quiet {
        println!("Watching...");
    }

    let debounce = Duration::from_millis(500);
    let mut pending: Option<Instant> = None;
    loop {
        let timeout = match pending {
            Some(t) => debounce
                .saturating_sub(t.elapsed())
                .max(Duration::from_millis(10)),
            None => Duration::from_secs(60),
        };
        match rx.recv_timeout(timeout) {
            Ok(Ok(event)) => {
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Any
                ) && event.paths.iter().any(|p| {
                    std::fs::canonicalize(p)
                        .map(|c| c == abs_input)
                        .unwrap_or(false)
                }) {
                    pending = Some(Instant::now());
                }
            }
            Ok(Err(e)) => eprintln!("watch error: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("file watcher disconnected unexpectedly");
            }
        }
        if let Some(t) = pending {
            if debounce_should_fire(t, Instant::now(), debounce) {
                pending = None;
                if let Err(e) = cmd_generate(
                    input,
                    out,
                    targets,
                    false,
                    config_path,
                    false,
                    false,
                    false,
                    None,
                    quiet,
                ) {
                    eprintln!("error: {e:?}");
                } else if !quiet {
                    let now = chrono_local_time_string();
                    println!("Regenerated at {now}");
                }
            }
        }
    }
}

/// Format a `HH:MM:SS` timestamp from the system clock without pulling in a
/// chrono dependency. Computes hours/minutes/seconds from the seconds-since-
/// epoch, applying the local UTC offset by reading `localtime`'s difference.
fn chrono_local_time_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let local_offset = local_utc_offset_seconds();
    let local = now as i64 + local_offset;
    let secs = local.rem_euclid(86_400);
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// Best-effort local timezone offset in seconds. Falls back to UTC (`0`) on
/// platforms without an obvious way to query the offset; the watch command
/// only uses this for the friendly "Regenerated at HH:MM:SS" line.
fn local_utc_offset_seconds() -> i64 {
    if let Ok(out) = Command::new("date").arg("+%z").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.len() == 5 {
                let sign = if s.starts_with('-') { -1 } else { 1 };
                if let (Ok(h), Ok(m)) = (s[1..3].parse::<i64>(), s[3..5].parse::<i64>()) {
                    return sign * (h * 3600 + m * 60);
                }
            }
        }
    }
    0
}

fn cmd_format(input: &str, check: bool, quiet: bool) -> Result<()> {
    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    let format = match ext {
        "yml" | "yaml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        other => bail!(
            "unsupported input format: {} (expected yml|yaml|json|toml)",
            other
        ),
    };
    let contents = std::fs::read_to_string(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    let mut api =
        parse_api_str(&contents, format).map_err(|e| with_named_source(e, input, &contents))?;
    validate_api(&mut api, Some((input, &contents))).map_err(Report::new)?;

    let formatted = match format {
        "yaml" => format_api_yaml(&api)?,
        "json" => format_api_json(&api)?,
        "toml" => toml::to_string_pretty(&api)
            .into_diagnostic()
            .wrap_err("failed to serialize API as TOML")?,
        _ => unreachable!(),
    };

    if check {
        if formatted != contents {
            if !quiet {
                eprintln!(
                    "{input} is not canonically formatted; run 'weaveffi format {input}' to fix"
                );
            }
            std::process::exit(1);
        }
        if !quiet {
            println!("{input} is canonically formatted");
        }
    } else {
        if formatted == contents {
            if !quiet {
                println!("{input} is already canonically formatted");
            }
            return Ok(());
        }
        std::fs::write(in_path.as_std_path(), &formatted)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to write {}", input))?;
        if !quiet {
            println!("Formatted {input}");
        }
    }
    Ok(())
}

/// Serialize `api` as YAML with deterministic key ordering. `serde_yaml`
/// preserves the order yielded by the serializer, so we round-trip through
/// `serde_json::Value` whose object representation is a `BTreeMap` and
/// therefore alphabetically sorted at every level.
fn format_api_yaml(api: &weaveffi_ir::ir::Api) -> Result<String> {
    let json: serde_json::Value = serde_json::to_value(api)
        .into_diagnostic()
        .wrap_err("failed to convert API to JSON value")?;
    serde_yaml::to_string(&json)
        .into_diagnostic()
        .wrap_err("failed to serialize API as YAML")
}

/// Serialize `api` as pretty-printed JSON with sorted keys at every level by
/// going through `serde_json::Value` (whose `Object` is a `BTreeMap`).
fn format_api_json(api: &weaveffi_ir::ir::Api) -> Result<String> {
    let json: serde_json::Value = serde_json::to_value(api)
        .into_diagnostic()
        .wrap_err("failed to convert API to JSON value")?;
    let mut out = serde_json::to_string_pretty(&json)
        .into_diagnostic()
        .wrap_err("failed to serialize API as JSON")?;
    out.push('\n');
    Ok(out)
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
    fn config_file_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("weaveffi.toml");
        std::fs::write(
            &cfg_path,
            concat!(
                "swift_module_name = \"MyApp\"\n",
                "android_package = \"com.example.myapp\"\n",
                "strip_module_prefix = true\n",
            ),
        )
        .unwrap();

        let cfg = load_config(Some(cfg_path.to_str().unwrap())).unwrap();
        assert_eq!(cfg.swift_module_name(), "MyApp");
        assert_eq!(cfg.android_package(), "com.example.myapp");
        assert!(cfg.strip_module_prefix);
        assert_eq!(cfg.c_prefix(), "weaveffi");
    }

    #[test]
    fn lint_clean_file_succeeds() {
        let sample = format!(
            "{}/../../samples/calculator/calculator.yml",
            env!("CARGO_MANIFEST_DIR")
        );
        assert!(
            cmd_lint(&sample, false).unwrap(),
            "calculator sample should be lint-clean"
        );
    }

    #[test]
    fn inline_generator_config() {
        let yaml = concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: math\n",
            "    functions:\n",
            "      - name: add\n",
            "        params:\n",
            "          - { name: a, type: i32 }\n",
            "          - { name: b, type: i32 }\n",
            "        return: i32\n",
            "generators:\n",
            "  swift:\n",
            "    module_name: MySwiftModule\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.generators.is_some());

        let mut config = GeneratorConfig::default();
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap());
        assert_eq!(config.swift_module_name, Some("MySwiftModule".to_string()));
    }

    #[test]
    fn inline_dart_package_name_merges() {
        let yaml = concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: m\n",
            "    functions: []\n",
            "generators:\n",
            "  dart:\n",
            "    package_name: my_dart_pkg\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        let mut config = GeneratorConfig::default();
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap());
        assert_eq!(config.dart_package_name.as_deref(), Some("my_dart_pkg"));
    }

    #[test]
    fn inline_go_module_path_merges() {
        let yaml = concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: m\n",
            "    functions: []\n",
            "generators:\n",
            "  go:\n",
            "    module_path: github.com/me/myffi\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        let mut config = GeneratorConfig::default();
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap());
        assert_eq!(
            config.go_module_path.as_deref(),
            Some("github.com/me/myffi")
        );
    }

    #[test]
    fn inline_ruby_module_name_merges() {
        let yaml = concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: m\n",
            "    functions: []\n",
            "generators:\n",
            "  ruby:\n",
            "    module_name: MyRubyMod\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        let mut config = GeneratorConfig::default();
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap());
        assert_eq!(config.ruby_module_name.as_deref(), Some("MyRubyMod"));
    }

    #[test]
    fn inline_ruby_gem_name_merges() {
        let yaml = concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: m\n",
            "    functions: []\n",
            "generators:\n",
            "  ruby:\n",
            "    gem_name: my_ruby_gem\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        let mut config = GeneratorConfig::default();
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap());
        assert_eq!(config.ruby_gem_name.as_deref(), Some("my_ruby_gem"));
    }

    #[test]
    fn inline_global_strip_module_prefix_merges() {
        let yaml = concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: m\n",
            "    functions: []\n",
            "generators:\n",
            "  weaveffi:\n",
            "    strip_module_prefix: true\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        let mut config = GeneratorConfig::default();
        assert!(!config.strip_module_prefix);
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap());
        assert!(config.strip_module_prefix);
    }

    #[test]
    fn inline_global_template_dir_merges() {
        let yaml = concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: m\n",
            "    functions: []\n",
            "generators:\n",
            "  weaveffi:\n",
            "    template_dir: ./my_templates\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        let mut config = GeneratorConfig::default();
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap());
        assert_eq!(config.template_dir.as_deref(), Some("./my_templates"));
    }

    #[test]
    fn inline_global_pre_generate_merges() {
        let yaml = concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: m\n",
            "    functions: []\n",
            "generators:\n",
            "  global:\n",
            "    pre_generate: \"echo hi\"\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        let mut config = GeneratorConfig::default();
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap());
        assert_eq!(config.pre_generate.as_deref(), Some("echo hi"));
    }

    #[test]
    fn inline_unknown_target_silently_ignored() {
        let yaml = concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: m\n",
            "    functions: []\n",
            "generators:\n",
            "  rustlang:\n",
            "    crate_name: future_target\n",
            "  swift:\n",
            "    module_name: KeptSwift\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        let mut config = GeneratorConfig::default();
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap());
        assert_eq!(config.swift_module_name.as_deref(), Some("KeptSwift"));
    }

    #[test]
    fn inline_unknown_key_silently_ignored() {
        let yaml = concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: m\n",
            "    functions: []\n",
            "generators:\n",
            "  swift:\n",
            "    module_name: KeptSwift\n",
            "    unrecognized_future_key: \"some value\"\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        let mut config = GeneratorConfig::default();
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap());
        assert_eq!(config.swift_module_name.as_deref(), Some("KeptSwift"));
    }

    #[test]
    fn inline_generator_config_overrides_file() {
        let yaml = concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: math\n",
            "    functions:\n",
            "      - name: add\n",
            "        params:\n",
            "          - { name: a, type: i32 }\n",
            "          - { name: b, type: i32 }\n",
            "        return: i32\n",
            "generators:\n",
            "  swift:\n",
            "    module_name: FromIDL\n",
            "  android:\n",
            "    package: com.idl.app\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();

        let mut config = GeneratorConfig {
            swift_module_name: Some("FromFile".into()),
            android_package: Some("com.file.app".into()),
            ..Default::default()
        };
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap());
        assert_eq!(config.swift_module_name, Some("FromIDL".to_string()));
        assert_eq!(config.android_package, Some("com.idl.app".to_string()));
    }

    #[test]
    fn config_default_when_no_file() {
        let cfg = load_config(None).unwrap();
        assert_eq!(cfg.swift_module_name(), "WeaveFFI");
        assert_eq!(cfg.android_package(), "com.weaveffi");
        assert!(!cfg.strip_module_prefix);
    }

    #[test]
    fn dry_run_lists_files() {
        let dir = tempfile::tempdir().unwrap();
        let yml = dir.path().join("api.yml");
        std::fs::write(
            &yml,
            concat!(
                "version: \"0.1.0\"\n",
                "modules:\n",
                "  - name: math\n",
                "    functions:\n",
                "      - name: add\n",
                "        params:\n",
                "          - { name: a, type: i32 }\n",
                "          - { name: b, type: i32 }\n",
                "        return: i32\n",
            ),
        )
        .unwrap();

        let out = dir.path().join("out");
        let input = yml.to_str().unwrap();
        let out_str = out.to_str().unwrap();

        cmd_generate(
            input, out_str, None, false, None, false, false, true, None, false,
        )
        .unwrap();

        assert!(!out.exists(), "dry-run should not create output directory");

        let api = {
            let contents = std::fs::read_to_string(&yml).unwrap();
            let mut api = weaveffi_ir::parse::parse_api_str(&contents, "yaml").unwrap();
            weaveffi_core::validate::validate_api(&mut api, None).unwrap();
            api
        };
        let out_dir = Utf8Path::new(out_str);

        let c = CGenerator;
        let cpp = CppGenerator;
        let swift = SwiftGenerator;
        let android = AndroidGenerator;
        let node = NodeGenerator;
        let wasm = WasmGenerator;
        let python = PythonGenerator;
        let dotnet = DotnetGenerator;
        let dart = DartGenerator;
        let go = GoGenerator;
        let ruby = RubyGenerator;
        let all: Vec<&dyn Generator> = vec![
            &c, &cpp, &swift, &android, &node, &wasm, &python, &dotnet, &dart, &go, &ruby,
        ];

        let mut files: Vec<String> = Vec::new();
        for gen in &all {
            files.extend(gen.output_files(&api, out_dir));
        }

        assert!(
            files.iter().any(|f| f.contains("c/weaveffi.h")),
            "missing c header: {files:?}"
        );
        assert!(
            files.iter().any(|f| f.contains("swift/Package.swift")),
            "missing swift package: {files:?}"
        );
        assert!(
            files.iter().any(|f| f.contains("android/build.gradle")),
            "missing android gradle: {files:?}"
        );
        assert!(
            files.iter().any(|f| f.contains("node/types.d.ts")),
            "missing node types: {files:?}"
        );
        assert!(
            files.iter().any(|f| f.contains("wasm/weaveffi_wasm.js")),
            "missing wasm js: {files:?}"
        );
        assert!(
            files
                .iter()
                .any(|f| f.contains("python/weaveffi/__init__.py")),
            "missing python init: {files:?}"
        );
    }

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
    fn new_creates_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .current_dir(dir.path())
            .args(["new", "test_proj"])
            .output()
            .expect("failed to run weaveffi new");

        assert!(
            cmd.status.success(),
            "weaveffi new failed: {}",
            String::from_utf8_lossy(&cmd.stderr)
        );
        let cargo_toml = dir.path().join("test_proj/Cargo.toml");
        assert!(cargo_toml.exists(), "Cargo.toml should exist");
        let contents = std::fs::read_to_string(&cargo_toml).unwrap();
        assert!(
            contents.contains("cdylib"),
            "Cargo.toml should contain cdylib: {contents}"
        );
    }

    #[test]
    fn new_creates_lib_rs() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .current_dir(dir.path())
            .args(["new", "test_proj"])
            .output()
            .expect("failed to run weaveffi new");

        assert!(
            cmd.status.success(),
            "weaveffi new failed: {}",
            String::from_utf8_lossy(&cmd.stderr)
        );
        let lib_rs = dir.path().join("test_proj/src/lib.rs");
        assert!(lib_rs.exists(), "src/lib.rs should exist");
        let contents = std::fs::read_to_string(&lib_rs).unwrap();
        assert!(
            contents.contains("todo!()"),
            "lib.rs should contain todo!() stubs: {contents}"
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

    #[test]
    fn generate_cpp_target_filter() {
        let dir = tempfile::tempdir().unwrap();
        let sample = format!(
            "{}/../../samples/calculator/calculator.yml",
            env!("CARGO_MANIFEST_DIR")
        );
        let out = dir.path().join("out");

        let cmd = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .args([
                "generate",
                &sample,
                "-o",
                out.to_str().unwrap(),
                "--target",
                "cpp",
            ])
            .output()
            .expect("failed to run weaveffi generate --target cpp");

        assert!(
            cmd.status.success(),
            "generate --target cpp failed: {}",
            String::from_utf8_lossy(&cmd.stderr)
        );
        assert!(out.join("cpp").exists(), "cpp/ should exist in output");
        assert!(!out.join("c").exists(), "c/ should NOT exist in output");
    }

    #[test]
    fn diff_shows_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let yml = dir.path().join("api.yml");
        std::fs::write(
            &yml,
            concat!(
                "version: \"0.1.0\"\n",
                "modules:\n",
                "  - name: math\n",
                "    functions:\n",
                "      - name: add\n",
                "        params:\n",
                "          - { name: a, type: i32 }\n",
                "          - { name: b, type: i32 }\n",
                "        return: i32\n",
            ),
        )
        .unwrap();

        let empty_out = dir.path().join("empty_out");
        std::fs::create_dir_all(&empty_out).unwrap();
        let input = yml.to_str().unwrap();
        let out_str = empty_out.to_str().unwrap();

        let cmd = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .args(["diff", input, "--out", out_str])
            .output()
            .expect("failed to run weaveffi diff");

        let stdout = String::from_utf8_lossy(&cmd.stdout);
        assert!(cmd.status.success(), "diff failed: {stdout}");
        assert!(
            !stdout.is_empty(),
            "diff output should not be empty for an empty output dir"
        );
        for line in stdout.lines() {
            assert!(
                line.contains("[new file]"),
                "expected [new file] in every line, got: {line}"
            );
        }
    }

    #[test]
    fn debounce_should_fire_after_window_elapses() {
        use std::time::{Duration, Instant};
        let last = Instant::now();
        let debounce = Duration::from_millis(500);
        assert!(!debounce_should_fire(last, last, debounce));
        assert!(!debounce_should_fire(
            last,
            last + Duration::from_millis(499),
            debounce
        ));
        assert!(debounce_should_fire(
            last,
            last + Duration::from_millis(500),
            debounce
        ));
        assert!(debounce_should_fire(
            last,
            last + Duration::from_secs(1),
            debounce
        ));
    }

    #[test]
    fn debounce_handles_now_before_last_event() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        assert!(!debounce_should_fire(
            later,
            now,
            Duration::from_millis(500)
        ));
    }

    #[test]
    fn debounce_collapses_burst_to_single_fire() {
        use std::time::{Duration, Instant};
        let debounce = Duration::from_millis(500);
        let t0 = Instant::now();
        let burst = [
            t0,
            t0 + Duration::from_millis(50),
            t0 + Duration::from_millis(120),
            t0 + Duration::from_millis(200),
        ];
        let last = *burst.last().unwrap();
        for &t in &burst {
            assert!(
                !debounce_should_fire(last, t, debounce),
                "burst event at {:?} after last must not fire",
                t.duration_since(t0)
            );
        }
        assert!(debounce_should_fire(
            last,
            last + Duration::from_millis(500),
            debounce
        ));
    }
}
