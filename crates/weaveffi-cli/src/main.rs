mod extract;
mod scaffold;

use camino::Utf8Path;
use clap::{CommandFactory, Parser, Subcommand};
use color_eyre::eyre::{bail, eyre, Report, Result, WrapErr};
use color_eyre::Section;
use serde::Deserialize;
use similar::TextDiff;
use std::collections::{BTreeSet, HashMap};
use std::env;
use std::ffi::OsStr;
use std::process::Command;
use tracing_subscriber::EnvFilter;
use weaveffi_core::codegen::{Capability, Generator, Orchestrator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::templates::TemplateEngine;
use weaveffi_core::validate::{
    collect_warnings, validate_api, validate_capabilities, ValidationError,
};
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
use weaveffi_ir::ir::CURRENT_SCHEMA_VERSION;
use weaveffi_ir::parse::{parse_api_str, ParseError};

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
}

fn main() -> Result<()> {
    let _ = color_eyre::install();

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
        .wrap_err_with(|| format!("failed to create project directory: {}", name))?;

    let module_name = sanitize_module_name(name);

    let idl_path = project_dir.join("weaveffi.yml");
    let idl_contents = format!(
        concat!(
            "version: \"0.1.0\"\n",
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
        .wrap_err_with(|| format!("failed to write {}", idl_path))?;

    let mut api = parse_api_str(&idl_contents, "yaml").wrap_err("failed to parse generated IDL")?;
    validate_api(&mut api).wrap_err("generated IDL failed validation")?;

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
            "weaveffi-abi = \"0.2\"\n",
            "\n",
            "[lints.rust]\n",
            "unsafe_code = \"allow\"\n",
        ),
        name = name,
    );
    std::fs::write(cargo_toml_path.as_std_path(), &cargo_toml)
        .wrap_err_with(|| format!("failed to write {}", cargo_toml_path))?;

    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(src_dir.as_std_path())
        .wrap_err_with(|| format!("failed to create {}", src_dir))?;
    let lib_rs_path = src_dir.join("lib.rs");
    let lib_contents = scaffold::render_scaffold(&api);
    std::fs::write(lib_rs_path.as_std_path(), &lib_contents)
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
                .wrap_err_with(|| format!("failed to read config file: {}", p))?;
            toml::from_str(&contents)
                .wrap_err_with(|| format!("failed to parse config file: {}", p))
        }
        None => Ok(GeneratorConfig::default()),
    }
}

/// Typed representation of the inline `[generators]` section.
///
/// Each nested struct uses `deny_unknown_fields` so serde surfaces any
/// unsupported key; we convert those errors into
/// [`ValidationError::UnknownGeneratorConfigKey`] with the offending target
/// so the CLI can print a focused suggestion.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineGeneratorsSection {
    #[serde(default)]
    swift: Option<InlineSwiftSection>,
    #[serde(default)]
    android: Option<InlineAndroidSection>,
    #[serde(default)]
    node: Option<InlineNodeSection>,
    #[serde(default)]
    wasm: Option<InlineWasmSection>,
    #[serde(default)]
    c: Option<InlineCSection>,
    #[serde(default)]
    python: Option<InlinePythonSection>,
    #[serde(default)]
    dotnet: Option<InlineDotnetSection>,
    #[serde(default)]
    cpp: Option<InlineCppSection>,
    #[serde(default)]
    dart: Option<InlineDartSection>,
    #[serde(default)]
    go: Option<InlineGoSection>,
    #[serde(default)]
    ruby: Option<InlineRubySection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineSwiftSection {
    #[serde(default)]
    module_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineAndroidSection {
    #[serde(default)]
    package: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineNodeSection {
    #[serde(default)]
    package_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineWasmSection {
    #[serde(default)]
    module_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineCSection {
    #[serde(default)]
    prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlinePythonSection {
    #[serde(default)]
    package_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineDotnetSection {
    #[serde(default)]
    namespace: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineCppSection {
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    header_name: Option<String>,
    #[serde(default)]
    standard: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineDartSection {
    #[serde(default)]
    package_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineGoSection {
    #[serde(default)]
    module_path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineRubySection {
    #[serde(default)]
    module_name: Option<String>,
    #[serde(default)]
    gem_name: Option<String>,
}

const KNOWN_GENERATOR_TARGETS: &[&str] = &[
    "swift", "android", "node", "wasm", "c", "python", "dotnet", "cpp", "dart", "go", "ruby",
];

/// Typed deserialization of the inline `[generators]` section.
///
/// Applies any populated fields onto `config`. Unknown targets or keys are
/// rejected via `serde`'s `deny_unknown_fields` and converted to
/// [`ValidationError::UnknownGeneratorConfigKey`].
fn merge_inline_generators(
    config: &mut GeneratorConfig,
    generators: &HashMap<String, toml::Value>,
) -> Result<(), ValidationError> {
    let table: toml::map::Map<String, toml::Value> = generators
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let section: InlineGeneratorsSection = toml::Value::Table(table)
        .try_into()
        .map_err(|e| unknown_generator_key_from_toml_error(&e, generators))?;

    if let Some(s) = section.swift {
        if let Some(v) = s.module_name {
            config.swift_module_name = Some(v);
        }
    }
    if let Some(s) = section.android {
        if let Some(v) = s.package {
            config.android_package = Some(v);
        }
    }
    if let Some(s) = section.node {
        if let Some(v) = s.package_name {
            config.node_package_name = Some(v);
        }
    }
    if let Some(s) = section.wasm {
        if let Some(v) = s.module_name {
            config.wasm_module_name = Some(v);
        }
    }
    if let Some(s) = section.c {
        if let Some(v) = s.prefix {
            config.c_prefix = Some(v);
        }
    }
    if let Some(s) = section.python {
        if let Some(v) = s.package_name {
            config.python_package_name = Some(v);
        }
    }
    if let Some(s) = section.dotnet {
        if let Some(v) = s.namespace {
            config.dotnet_namespace = Some(v);
        }
    }
    if let Some(s) = section.cpp {
        if let Some(v) = s.namespace {
            config.cpp_namespace = Some(v);
        }
        if let Some(v) = s.header_name {
            config.cpp_header_name = Some(v);
        }
        if let Some(v) = s.standard {
            config.cpp_standard = Some(v);
        }
    }
    if let Some(s) = section.dart {
        if let Some(v) = s.package_name {
            config.dart_package_name = Some(v);
        }
    }
    if let Some(s) = section.go {
        if let Some(v) = s.module_path {
            config.go_module_path = Some(v);
        }
    }
    if let Some(s) = section.ruby {
        if let Some(v) = s.module_name {
            config.ruby_module_name = Some(v);
        }
        if let Some(v) = s.gem_name {
            config.ruby_gem_name = Some(v);
        }
    }
    Ok(())
}

/// Extracts the offending key out of a serde/toml `deny_unknown_fields` error
/// and pairs it with the generator target it was nested under.
fn unknown_generator_key_from_toml_error(
    err: &toml::de::Error,
    generators: &HashMap<String, toml::Value>,
) -> ValidationError {
    let msg = err.to_string();
    let key = extract_unknown_field(&msg).unwrap_or_else(|| msg.trim().to_string());
    let target = if generators.contains_key(&key) {
        "generators".to_string()
    } else {
        generators
            .iter()
            .find(|(_, v)| v.as_table().is_some_and(|t| t.contains_key(&key)))
            .map(|(t, _)| t.clone())
            .unwrap_or_else(|| "generators".to_string())
    };
    ValidationError::UnknownGeneratorConfigKey { key, target }
}

/// Parses the key name out of serde's `unknown field \`X\`, ...` message.
fn extract_unknown_field(msg: &str) -> Option<String> {
    let marker = "unknown field `";
    let start = msg.find(marker)? + marker.len();
    let rest = &msg[start..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

/// Comma-separated list of valid keys for a generator target (or of valid
/// targets when `target == "generators"`), used in error suggestions.
fn valid_keys_for_generator_target(target: &str) -> String {
    match target {
        "swift" => "module_name".to_string(),
        "android" => "package".to_string(),
        "node" => "package_name".to_string(),
        "wasm" => "module_name".to_string(),
        "c" => "prefix".to_string(),
        "python" => "package_name".to_string(),
        "dotnet" => "namespace".to_string(),
        "cpp" => "namespace, header_name, standard".to_string(),
        "dart" => "package_name".to_string(),
        "go" => "module_path".to_string(),
        "ruby" => "module_name, gem_name".to_string(),
        "generators" => KNOWN_GENERATOR_TARGETS.join(", "),
        _ => String::new(),
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
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    let mut api = parse_api_str(&contents, format).map_err(|e| format_parse_error(input, e))?;
    validate_api(&mut api).map_err(format_validation_error)?;

    if let Some(ref generators) = api.generators {
        merge_inline_generators(&mut config, generators).map_err(format_validation_error)?;
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

    let selected_caps: Vec<(&str, &[Capability])> = selected
        .iter()
        .map(|g| (g.name(), g.capabilities()))
        .collect();
    validate_capabilities(&api, &selected_caps).map_err(format_validation_error)?;

    if dry_run {
        for gen in &selected {
            for path in gen.output_files_with_config(&api, out_dir, &config) {
                println!("{path}");
            }
        }
        return Ok(());
    }

    std::fs::create_dir_all(out_dir.as_std_path())
        .wrap_err_with(|| format!("failed to create output directory: {}", out))?;

    let engine = match templates_path {
        Some(dir) => {
            let mut te = TemplateEngine::new();
            te.load_dir(Utf8Path::new(dir))
                .map_err(|e| eyre!("failed to load templates from {}: {:#}", dir, e))?;
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
        .map_err(|e| eyre!("{:#}", e))?;

    if emit_scaffold {
        let scaffold_path = out_dir.join("scaffold.rs");
        let contents = scaffold::render_scaffold(&api);
        std::fs::write(scaffold_path.as_std_path(), contents)
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
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    let mut api = parse_api_str(&contents, format).map_err(|e| format_parse_error(input, e))?;

    match validate_api(&mut api) {
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
        Err(e) => Err(format_validation_error(e)),
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
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    let mut api = parse_api_str(&contents, format).map_err(|e| format_parse_error(input, e))?;
    validate_api(&mut api).map_err(format_validation_error)?;

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
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    let mut api = parse_api_str(&contents, format).map_err(|e| format_parse_error(input, e))?;
    validate_api(&mut api).map_err(format_validation_error)?;

    let tmp = tempfile::tempdir().wrap_err("failed to create temp directory")?;
    let tmp_path = Utf8Path::from_path(tmp.path())
        .ok_or_else(|| eyre!("temp directory path is not valid UTF-8"))?;

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
        .map_err(|e| eyre!("{:#}", e))?;

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
                let gen_content = std::fs::read_to_string(gen_file.as_std_path())?;
                let out_content = std::fs::read_to_string(out_file.as_std_path())?;
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
        .wrap_err_with(|| format!("failed to read directory: {}", dir))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let utf8 = Utf8Path::from_path(&path)
            .ok_or_else(|| eyre!("non-UTF-8 path: {:?}", path))?
            .to_owned();
        if utf8.is_dir() {
            walk_dir(base, &utf8, out)?;
        } else {
            let rel = utf8
                .strip_prefix(base)
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

fn format_parse_error(filename: &str, err: ParseError) -> Report {
    let suggestion = match &err {
        ParseError::Yaml { .. } => {
            "check YAML syntax: ensure correct indentation, quoting, and key-value formatting"
        }
        ParseError::Json { .. } => {
            "check JSON syntax: ensure all brackets, braces, and commas are correct"
        }
        ParseError::Toml { .. } => {
            "check TOML syntax: ensure correct table headers, key-value pairs, and quoting"
        }
        ParseError::UnsupportedFormat(_) => "use a supported format: yml, yaml, json, or toml",
    };

    let location = match &err {
        ParseError::Yaml { line, column, .. } | ParseError::Json { line, column, .. } => {
            format!("{}:{}:{}", filename, line, column)
        }
        _ => filename.to_string(),
    };

    eyre!(err).note(location).suggestion(suggestion)
}

fn format_validation_error(err: ValidationError) -> Report {
    if let ValidationError::UnknownGeneratorConfigKey { target, .. } = &err {
        let valid = valid_keys_for_generator_target(target);
        let suggestion = format!("valid keys for the `{target}` generator section are: {valid}");
        return eyre!(err).suggestion(suggestion);
    }
    let suggestion = validation_suggestion(&err);
    eyre!(err).suggestion(suggestion)
}

fn validation_suggestion(err: &ValidationError) -> &'static str {
    match err {
        ValidationError::NoModuleName => "every module must have a non-empty 'name' field",
        ValidationError::DuplicateModuleName(_) => {
            "module names must be unique within an API definition; rename or merge the duplicate"
        }
        ValidationError::InvalidModuleName(_, _) => {
            "choose a valid identifier (a-z, A-Z, 0-9, _) that is not a reserved word"
        }
        ValidationError::DuplicateFunctionName { .. } => {
            "function names must be unique within a module; rename the duplicate"
        }
        ValidationError::DuplicateParamName { .. } => {
            "parameter names must be unique within a function; rename the duplicate"
        }
        ValidationError::ReservedKeyword(_) => {
            "choose a different name that is not a language reserved word"
        }
        ValidationError::InvalidIdentifier(_, _) => {
            "identifiers must start with a letter or underscore and contain only alphanumeric or underscore characters"
        }
        ValidationError::ErrorDomainMissingName(_) => {
            "add a non-empty 'name' field to the error domain"
        }
        ValidationError::DuplicateErrorName { .. } => {
            "error code names must be unique within a module; rename the duplicate"
        }
        ValidationError::DuplicateErrorCode { .. } => {
            "numeric error codes must be unique within a module; assign a different value"
        }
        ValidationError::InvalidErrorCode { .. } => {
            "error codes must be non-zero; use a positive or negative integer"
        }
        ValidationError::NameCollisionWithErrorDomain { .. } => {
            "function and error domain names share a namespace; rename one to avoid the collision"
        }
        ValidationError::DuplicateStructName { .. } => {
            "struct names must be unique within a module; rename the duplicate"
        }
        ValidationError::DuplicateStructField { .. } => {
            "field names must be unique within a struct; rename the duplicate"
        }
        ValidationError::EmptyStruct { .. } => {
            "structs must have at least one field; add a field or remove the struct"
        }
        ValidationError::DuplicateEnumName { .. } => {
            "enum names must be unique within a module; rename the duplicate"
        }
        ValidationError::EmptyEnum { .. } => {
            "enums must have at least one variant; add a variant or remove the enum"
        }
        ValidationError::DuplicateEnumVariant { .. } => {
            "variant names must be unique within an enum; rename the duplicate"
        }
        ValidationError::DuplicateEnumValue { .. } => {
            "variant numeric values must be unique within an enum; assign a different value"
        }
        ValidationError::UnknownTypeRef { .. } => {
            "define a struct or enum with this name in the same module, or check for typos"
        }
        ValidationError::InvalidMapKey { .. } => {
            "map keys must be primitive types (i32, u32, i64, f64, bool, string); structs, lists, and maps cannot be keys"
        }
        ValidationError::BorrowedTypeInInvalidPosition { .. } => {
            "borrowed types (&str, &[u8]) can only be used as function parameters, not return types or struct fields"
        }
        ValidationError::DuplicateCallbackName { .. } => {
            "callback names must be unique within a module; rename the duplicate"
        }
        ValidationError::ListenerCallbackNotFound { .. } => {
            "listener event_callback must reference a callback defined in the same module"
        }
        ValidationError::DuplicateListenerName { .. } => {
            "listener names must be unique within a module; rename the duplicate"
        }
        ValidationError::IteratorInInvalidPosition { .. } => {
            "iterator types can only be used as function return types, not as parameters or struct fields"
        }
        ValidationError::BuilderStructEmpty { .. } => {
            "builder structs must have at least one field; add a field or set builder: false"
        }
        ValidationError::TargetMissingCapability { .. } => {
            "the selected target does not support this IR feature; remove the target with --targets, or remove the unsupported feature from the API"
        }
        ValidationError::UnknownGeneratorConfigKey { .. } => {
            "remove the unknown key or rename it to match a supported field in the inline [generators] section"
        }
    }
}

fn cmd_extract(input: &str, output: Option<&str>, format: &str, quiet: bool) -> Result<()> {
    let source = std::fs::read_to_string(input)
        .wrap_err_with(|| format!("failed to read source file: {}", input))?;

    let mut api = extract::extract_api_from_rust(&source)
        .wrap_err("failed to extract API from Rust source")?;

    if let Err(e) = validate_api(&mut api) {
        eprintln!("warning: {}", e);
    }

    let serialized = match format {
        "yaml" | "yml" => {
            serde_yaml::to_string(&api).wrap_err("failed to serialize API as YAML")?
        }
        "json" => serde_json::to_string_pretty(&api).wrap_err("failed to serialize API as JSON")?,
        "toml" => toml::to_string_pretty(&api).wrap_err("failed to serialize API as TOML")?,
        other => bail!(
            "unsupported output format: {} (expected yaml, json, or toml)",
            other
        ),
    };

    match output {
        Some(path) => {
            std::fs::write(path, &serialized)
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

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::validate::ValidationError;
    use weaveffi_ir::parse::ParseError;

    #[test]
    fn validation_suggestion_covers_all_variants() {
        let cases: Vec<ValidationError> = vec![
            ValidationError::NoModuleName,
            ValidationError::DuplicateModuleName("m".into()),
            ValidationError::InvalidModuleName("123".into(), "bad"),
            ValidationError::DuplicateFunctionName {
                module: "m".into(),
                function: "f".into(),
            },
            ValidationError::DuplicateParamName {
                module: "m".into(),
                function: "f".into(),
                param: "p".into(),
            },
            ValidationError::ReservedKeyword("type".into()),
            ValidationError::InvalidIdentifier("123".into(), "bad"),
            ValidationError::ErrorDomainMissingName("m".into()),
            ValidationError::DuplicateErrorName {
                module: "m".into(),
                name: "e".into(),
            },
            ValidationError::DuplicateErrorCode {
                module: "m".into(),
                code: 1,
            },
            ValidationError::InvalidErrorCode {
                module: "m".into(),
                name: "e".into(),
            },
            ValidationError::NameCollisionWithErrorDomain {
                module: "m".into(),
                name: "e".into(),
            },
            ValidationError::DuplicateStructName {
                module: "m".into(),
                name: "S".into(),
            },
            ValidationError::DuplicateStructField {
                struct_name: "S".into(),
                field: "f".into(),
            },
            ValidationError::EmptyStruct {
                module: "m".into(),
                name: "S".into(),
            },
            ValidationError::DuplicateEnumName {
                module: "m".into(),
                name: "E".into(),
            },
            ValidationError::EmptyEnum {
                module: "m".into(),
                name: "E".into(),
            },
            ValidationError::DuplicateEnumVariant {
                enum_name: "E".into(),
                variant: "V".into(),
            },
            ValidationError::DuplicateEnumValue {
                enum_name: "E".into(),
                value: 0,
            },
            ValidationError::UnknownTypeRef { name: "Foo".into() },
            ValidationError::InvalidMapKey {
                key_type: "struct Foo".into(),
            },
            ValidationError::BorrowedTypeInInvalidPosition {
                ty: "&str".into(),
                location: "return type".into(),
            },
            ValidationError::DuplicateCallbackName {
                module: "m".into(),
                name: "cb".into(),
            },
            ValidationError::ListenerCallbackNotFound {
                module: "m".into(),
                listener: "l".into(),
                callback: "cb".into(),
            },
            ValidationError::DuplicateListenerName {
                module: "m".into(),
                name: "l".into(),
            },
            ValidationError::TargetMissingCapability {
                target: "node".into(),
                capability: "callbacks".into(),
                location: "module 'events' callbacks".into(),
            },
            ValidationError::UnknownGeneratorConfigKey {
                key: "modul_name".into(),
                target: "swift".into(),
            },
        ];

        for err in &cases {
            let suggestion = validation_suggestion(err);
            assert!(!suggestion.is_empty(), "empty suggestion for {:?}", err);
        }
    }

    #[test]
    fn format_parse_error_preserves_yaml_error() {
        let _ = color_eyre::install();
        let err = ParseError::Yaml {
            line: 5,
            column: 3,
            message: "test error".into(),
        };
        let report = format_parse_error("input.yml", err);
        let msg = report.to_string();
        assert!(
            msg.contains("YAML parse error"),
            "missing error type in: {msg}"
        );
        assert!(msg.contains("line 5"), "missing line number in: {msg}");
        assert!(msg.contains("column 3"), "missing column number in: {msg}");
    }

    #[test]
    fn format_parse_error_preserves_json_error() {
        let _ = color_eyre::install();
        let err = ParseError::Json {
            line: 10,
            column: 1,
            message: "test error".into(),
        };
        let report = format_parse_error("data.json", err);
        let msg = report.to_string();
        assert!(
            msg.contains("JSON parse error"),
            "missing error type in: {msg}"
        );
        assert!(msg.contains("line 10"), "missing line in: {msg}");
    }

    #[test]
    fn format_parse_error_preserves_toml_error() {
        let _ = color_eyre::install();
        let err = ParseError::Toml {
            message: "test error".into(),
        };
        let report = format_parse_error("config.toml", err);
        let msg = report.to_string();
        assert!(
            msg.contains("TOML parse error"),
            "missing error type in: {msg}"
        );
    }

    #[test]
    fn format_validation_error_preserves_message() {
        let _ = color_eyre::install();
        let err = ValidationError::DuplicateModuleName("foo".into());
        let report = format_validation_error(err);
        let msg = report.to_string();
        assert!(
            msg.contains("duplicate module name"),
            "missing error message in: {msg}"
        );
        assert!(msg.contains("foo"), "missing module name in: {msg}");
    }

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
        let _ = color_eyre::install();
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
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap()).unwrap();
        assert_eq!(config.swift_module_name, Some("MySwiftModule".to_string()));
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
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap()).unwrap();
        assert_eq!(config.swift_module_name, Some("FromIDL".to_string()));
        assert_eq!(config.android_package, Some("com.idl.app".to_string()));
    }

    #[test]
    fn inline_generators_typed_deser_works() {
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
            "    module_name: MySwift\n",
            "  android:\n",
            "    package: com.example.app\n",
            "  node:\n",
            "    package_name: my-node-pkg\n",
            "  wasm:\n",
            "    module_name: my_wasm\n",
            "  c:\n",
            "    prefix: myffi\n",
            "  python:\n",
            "    package_name: my_python\n",
            "  dotnet:\n",
            "    namespace: My.Bindings\n",
            "  cpp:\n",
            "    namespace: mylib\n",
            "    header_name: mylib.hpp\n",
            "    standard: \"20\"\n",
            "  dart:\n",
            "    package_name: my_dart\n",
            "  go:\n",
            "    module_path: github.com/me/mylib\n",
            "  ruby:\n",
            "    module_name: MyRuby\n",
            "    gem_name: my_ruby_gem\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        let mut config = GeneratorConfig::default();
        merge_inline_generators(&mut config, api.generators.as_ref().unwrap()).unwrap();

        assert_eq!(config.swift_module_name.as_deref(), Some("MySwift"));
        assert_eq!(config.android_package.as_deref(), Some("com.example.app"));
        assert_eq!(config.node_package_name.as_deref(), Some("my-node-pkg"));
        assert_eq!(config.wasm_module_name.as_deref(), Some("my_wasm"));
        assert_eq!(config.c_prefix.as_deref(), Some("myffi"));
        assert_eq!(config.python_package_name.as_deref(), Some("my_python"));
        assert_eq!(config.dotnet_namespace.as_deref(), Some("My.Bindings"));
        assert_eq!(config.cpp_namespace.as_deref(), Some("mylib"));
        assert_eq!(config.cpp_header_name.as_deref(), Some("mylib.hpp"));
        assert_eq!(config.cpp_standard.as_deref(), Some("20"));
        assert_eq!(config.dart_package_name.as_deref(), Some("my_dart"));
        assert_eq!(
            config.go_module_path.as_deref(),
            Some("github.com/me/mylib")
        );
        assert_eq!(config.ruby_module_name.as_deref(), Some("MyRuby"));
        assert_eq!(config.ruby_gem_name.as_deref(), Some("my_ruby_gem"));
    }

    #[test]
    fn inline_generators_unknown_key_rejected() {
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
            "    modul_name: Typo\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(yaml).unwrap();
        let mut config = GeneratorConfig::default();
        let err = merge_inline_generators(&mut config, api.generators.as_ref().unwrap())
            .expect_err("typo'd key should be rejected");
        match err {
            ValidationError::UnknownGeneratorConfigKey { key, target } => {
                assert_eq!(key, "modul_name");
                assert_eq!(target, "swift");
            }
            other => panic!("expected UnknownGeneratorConfigKey, got {other:?}"),
        }

        let unknown_target_yaml = concat!(
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
            "  bogus:\n",
            "    module_name: X\n",
        );
        let api: weaveffi_ir::ir::Api = serde_yaml::from_str(unknown_target_yaml).unwrap();
        let mut config = GeneratorConfig::default();
        let err = merge_inline_generators(&mut config, api.generators.as_ref().unwrap())
            .expect_err("unknown target should be rejected");
        match err {
            ValidationError::UnknownGeneratorConfigKey { key, target } => {
                assert_eq!(key, "bogus");
                assert_eq!(target, "generators");
            }
            other => panic!("expected UnknownGeneratorConfigKey, got {other:?}"),
        }
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
        let _ = color_eyre::install();
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
            weaveffi_core::validate::validate_api(&mut api).unwrap();
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
        let _ = color_eyre::install();
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
        let _ = color_eyre::install();
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
}
