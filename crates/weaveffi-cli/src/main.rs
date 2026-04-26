mod extract;
mod scaffold;

use camino::Utf8Path;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use color_eyre::eyre::{bail, eyre, Report, Result, WrapErr};
use color_eyre::Section;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use similar::TextDiff;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::ffi::OsStr;
use std::process::Command;
use tracing_subscriber::EnvFilter;
use weaveffi_core::codegen::{Capability, Generator, Orchestrator, LOCKFILE};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::templates::TemplateEngine;
use weaveffi_core::validate::{
    collect_warnings, validate_api_with_spans, validate_capabilities, ValidationError,
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
use weaveffi_ir::ir::{Span, SpanTable, CURRENT_SCHEMA_VERSION};
use weaveffi_ir::parse::{parse_api_str_with_spans, ParseError};

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

impl OutputFormat {
    fn is_json(self) -> bool {
        matches!(self, OutputFormat::Json)
    }
}

/// Structured error entry used in JSON outputs of `validate`.
#[derive(Debug, Serialize)]
struct ErrorEntry {
    location: Option<String>,
    message: String,
    suggestion: Option<String>,
}

#[derive(Debug, Serialize)]
struct ValidateOkJson {
    ok: bool,
    modules: usize,
    functions: usize,
    structs: usize,
    enums: usize,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ValidateErrJson {
    ok: bool,
    errors: Vec<ErrorEntry>,
}

#[derive(Debug, Serialize)]
struct DiffEntry {
    path: String,
    status: &'static str,
    patch: String,
}

#[derive(Debug, Clone, Serialize)]
struct DoctorCheck {
    name: String,
    ok: bool,
    version: Option<String>,
    hint: Option<String>,
}

fn emit_json<T: Serialize>(value: &T) -> Result<()> {
    let rendered = serde_json::to_string_pretty(value).wrap_err("failed to serialize JSON")?;
    println!("{rendered}");
    Ok(())
}

fn parse_error_to_entry(filename: &str, err: &ParseError) -> ErrorEntry {
    let location = match err {
        ParseError::Yaml { line, column, .. } | ParseError::Json { line, column, .. } => {
            Some(format!("{filename}:{line}:{column}"))
        }
        _ => Some(filename.to_string()),
    };
    let suggestion = match err {
        ParseError::Yaml { .. } => Some(
            "check YAML syntax: ensure correct indentation, quoting, and key-value formatting"
                .to_string(),
        ),
        ParseError::Json { .. } => Some(
            "check JSON syntax: ensure all brackets, braces, and commas are correct".to_string(),
        ),
        ParseError::Toml { .. } => Some(
            "check TOML syntax: ensure correct table headers, key-value pairs, and quoting"
                .to_string(),
        ),
        ParseError::UnsupportedFormat(_) => {
            Some("use a supported format: yml, yaml, json, or toml".to_string())
        }
    };
    ErrorEntry {
        location,
        message: err.to_string(),
        suggestion,
    }
}

fn validation_error_to_entry(filename: &str, err: &ValidationError) -> ErrorEntry {
    let suggestion = if let ValidationError::UnknownGeneratorConfigKey { target, .. } = err {
        let valid = valid_keys_for_generator_target(target);
        Some(format!(
            "valid keys for the `{target}` generator section are: {valid}"
        ))
    } else {
        Some(validation_suggestion(err).to_string())
    };
    let location = validation_error_span(err).map(|s| format!("{filename}:{}:{}", s.line, s.col));
    ErrorEntry {
        location,
        message: err.to_string(),
        suggestion,
    }
}

#[derive(Parser, Debug)]
#[command(name = "weaveffi", version, about = "WeaveFFI CLI")]
struct Cli {
    #[arg(long, global = true)]
    quiet: bool,
    #[arg(long, short, global = true)]
    verbose: bool,
    /// Output format for CLI diagnostics and reports
    #[arg(long, value_enum, default_value_t = OutputFormat::Text, global = true)]
    format: OutputFormat,
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
        /// Write weaveffi.lock with SHA-256 hashes of generated files [default: on]
        #[arg(long, overrides_with = "no_lockfile")]
        lockfile: bool,
        /// Skip writing the weaveffi.lock file
        #[arg(long, overrides_with = "lockfile")]
        no_lockfile: bool,
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
        /// Serialization format for the extracted API: yaml (default), json, or toml.
        /// Overridden by the global --format json.
        #[arg(long = "output-format", default_value = "yaml")]
        output_format: Option<String>,
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
        /// Exit with non-zero status when differences are detected [default: on]
        #[arg(long, overrides_with = "no_exit_code")]
        exit_code: bool,
        /// Always exit 0, even when differences are detected
        #[arg(long, overrides_with = "exit_code")]
        no_exit_code: bool,
    },
    Doctor {
        /// Treat optional checks as required (exit non-zero if any fail)
        #[arg(long)]
        all: bool,
    },
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
    SchemaVersion,
    CheckStamp {
        /// Directory of generated files to scan
        dir: String,
        /// Expected IR schema version (defaults to the current schema version)
        #[arg(long)]
        expected_ir_version: Option<String>,
    },
    Verify {
        /// Directory of generated files to verify against a weaveffi.lock
        dir: String,
        /// Path to the lockfile (defaults to <dir>/weaveffi.lock)
        #[arg(long)]
        lockfile: Option<String>,
    },
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
    let format = cli.format;
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
            lockfile: _,
            no_lockfile,
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
            !no_lockfile,
            quiet,
            format,
        )?,
        Commands::Validate { input, warn } => {
            if !cmd_validate(&input, warn, quiet, format)? {
                std::process::exit(1);
            }
        }
        Commands::Extract {
            input,
            output,
            output_format,
        } => cmd_extract(
            &input,
            output.as_deref(),
            output_format.as_deref().unwrap_or("yaml"),
            quiet,
            format,
        )?,
        Commands::Lint { input } => {
            if !cmd_lint(&input, quiet, format)? {
                std::process::exit(1);
            }
        }
        Commands::Diff {
            input,
            out,
            exit_code: _,
            no_exit_code,
        } => {
            if !cmd_diff(&input, out.as_deref(), !no_exit_code, quiet, format)? {
                std::process::exit(1);
            }
        }
        Commands::Doctor { all } => {
            if !cmd_doctor(all, format)? {
                std::process::exit(1);
            }
        }
        Commands::Completions { shell } => cmd_completions(shell),
        Commands::SchemaVersion => println!("{CURRENT_SCHEMA_VERSION}"),
        Commands::CheckStamp {
            dir,
            expected_ir_version,
        } => {
            if !cmd_check_stamp(&dir, expected_ir_version.as_deref(), quiet)? {
                std::process::exit(1);
            }
        }
        Commands::Verify { dir, lockfile } => {
            if !cmd_verify(&dir, lockfile.as_deref(), quiet)? {
                std::process::exit(1);
            }
        }
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

    let (mut api, _) = parse_api_str_with_spans(&idl_contents, "yaml")
        .wrap_err("failed to parse generated IDL")?;
    validate_api_with_spans(&mut api, &SpanTable::default())
        .wrap_err("generated IDL failed validation")?;

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
    emit_lockfile: bool,
    quiet: bool,
    format: OutputFormat,
) -> Result<()> {
    let mut config = load_config(config_path)?;

    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    let fmt = match ext {
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
    let (mut api, spans) = parse_api_str_with_spans(&contents, fmt)
        .map_err(|e| pretty_parse_error(input, &contents, e))?;
    validate_api_with_spans(&mut api, &spans)
        .map_err(|e| format_validation_error(input, &contents, e))?;

    if let Some(ref generators) = api.generators {
        merge_inline_generators(&mut config, generators)
            .map_err(|e| format_validation_error(input, &contents, e))?;
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
    validate_capabilities(&api, &selected_caps)
        .map_err(|e| format_validation_error(input, &contents, e))?;

    if dry_run {
        let mut files: Vec<String> = Vec::new();
        for gen in &selected {
            for path in gen.output_files_with_config(&api, out_dir, &config) {
                files.push(path);
            }
        }
        if format.is_json() {
            emit_json(&files)?;
        } else {
            for path in &files {
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

    let mut orchestrator = Orchestrator::new().quiet(quiet).lockfile(emit_lockfile);
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

/// Returns `Ok(true)` when validation passed, `Ok(false)` when it failed in
/// JSON mode (JSON already printed to stdout; caller should exit 1). In text
/// mode failures are still reported via eyre.
fn cmd_validate(input: &str, warn: bool, quiet: bool, format: OutputFormat) -> Result<bool> {
    if format.is_json() {
        cmd_validate_json(input)
    } else {
        cmd_validate_text(input, warn, quiet)?;
        Ok(true)
    }
}

fn cmd_validate_text(input: &str, warn: bool, quiet: bool) -> Result<()> {
    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    let fmt = match ext {
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
    let (mut api, spans) = parse_api_str_with_spans(&contents, fmt)
        .map_err(|e| pretty_parse_error(input, &contents, e))?;

    match validate_api_with_spans(&mut api, &spans) {
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
        Err(e) => Err(format_validation_error(input, &contents, e)),
    }
}

fn cmd_validate_json(input: &str) -> Result<bool> {
    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        emit_json(&ValidateErrJson {
            ok: false,
            errors: vec![ErrorEntry {
                location: Some(input.to_string()),
                message: "input file has no extension (expected yml|yaml|json|toml)".to_string(),
                suggestion: Some("use a supported format: yml, yaml, json, or toml".to_string()),
            }],
        })?;
        return Ok(false);
    }
    let fmt = match ext {
        "yml" | "yaml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        other => {
            emit_json(&ValidateErrJson {
                ok: false,
                errors: vec![ErrorEntry {
                    location: Some(input.to_string()),
                    message: format!(
                        "unsupported input format: {other} (expected yml|yaml|json|toml)"
                    ),
                    suggestion: Some(
                        "use a supported format: yml, yaml, json, or toml".to_string(),
                    ),
                }],
            })?;
            return Ok(false);
        }
    };
    let contents = match std::fs::read_to_string(in_path.as_std_path()) {
        Ok(c) => c,
        Err(e) => {
            emit_json(&ValidateErrJson {
                ok: false,
                errors: vec![ErrorEntry {
                    location: Some(input.to_string()),
                    message: format!("failed to read input file: {e}"),
                    suggestion: None,
                }],
            })?;
            return Ok(false);
        }
    };
    let (mut api, spans) = match parse_api_str_with_spans(&contents, fmt) {
        Ok(v) => v,
        Err(e) => {
            emit_json(&ValidateErrJson {
                ok: false,
                errors: vec![parse_error_to_entry(input, &e)],
            })?;
            return Ok(false);
        }
    };
    if let Err(e) = validate_api_with_spans(&mut api, &spans) {
        emit_json(&ValidateErrJson {
            ok: false,
            errors: vec![validation_error_to_entry(input, &e)],
        })?;
        return Ok(false);
    }

    let n_modules = api.modules.len();
    let n_functions: usize = api.modules.iter().map(|m| m.functions.len()).sum();
    let n_structs: usize = api.modules.iter().map(|m| m.structs.len()).sum();
    let n_enums: usize = api.modules.iter().map(|m| m.enums.len()).sum();
    let warnings: Vec<String> = collect_warnings(&api)
        .iter()
        .map(|w| w.to_string())
        .collect();

    emit_json(&ValidateOkJson {
        ok: true,
        modules: n_modules,
        functions: n_functions,
        structs: n_structs,
        enums: n_enums,
        warnings,
    })?;
    Ok(true)
}

/// Returns `Ok(true)` when the file is clean, `Ok(false)` when warnings were found.
fn cmd_lint(input: &str, quiet: bool, format: OutputFormat) -> Result<bool> {
    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    let fmt = match ext {
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
    let (mut api, spans) = parse_api_str_with_spans(&contents, fmt)
        .map_err(|e| pretty_parse_error(input, &contents, e))?;
    validate_api_with_spans(&mut api, &spans)
        .map_err(|e| format_validation_error(input, &contents, e))?;

    let warnings = collect_warnings(&api);
    if format.is_json() {
        let messages: Vec<String> = warnings.iter().map(|w| w.to_string()).collect();
        emit_json(&messages)?;
        return Ok(warnings.is_empty());
    }

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

/// Returns `Ok(true)` when the command should exit 0, `Ok(false)` when the
/// command should exit 1 (differences detected in exit-code mode, or JSON
/// emitted with differences).
fn cmd_diff(
    input: &str,
    out: Option<&str>,
    exit_code: bool,
    quiet: bool,
    format: OutputFormat,
) -> Result<bool> {
    let out = out.unwrap_or("./generated");

    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    let fmt = match ext {
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
    let (mut api, spans) = parse_api_str_with_spans(&contents, fmt)
        .map_err(|e| pretty_parse_error(input, &contents, e))?;
    validate_api_with_spans(&mut api, &spans)
        .map_err(|e| format_validation_error(input, &contents, e))?;

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
    let mut orchestrator = Orchestrator::new().quiet(quiet).lockfile(false);
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
    let mut entries: Vec<DiffEntry> = Vec::new();

    for rel in &all_paths {
        let gen_file = tmp_path.join(rel);
        let out_file = out_dir.join(rel);

        match (gen_file.exists(), out_file.exists()) {
            (true, false) => {
                let gen_content = std::fs::read_to_string(gen_file.as_std_path())?;
                entries.push(DiffEntry {
                    path: (*rel).clone(),
                    status: "added",
                    patch: unified_patch(rel, "", &gen_content),
                });
            }
            (false, true) => {
                let out_content = std::fs::read_to_string(out_file.as_std_path())?;
                entries.push(DiffEntry {
                    path: (*rel).clone(),
                    status: "removed",
                    patch: unified_patch(rel, &out_content, ""),
                });
            }
            (true, true) => {
                let gen_content = std::fs::read_to_string(gen_file.as_std_path())?;
                let out_content = std::fs::read_to_string(out_file.as_std_path())?;
                if gen_content != out_content {
                    entries.push(DiffEntry {
                        path: (*rel).clone(),
                        status: "changed",
                        patch: unified_patch(rel, &out_content, &gen_content),
                    });
                }
            }
            _ => {}
        }
    }

    let has_diff = !entries.is_empty();

    if format.is_json() {
        emit_json(&entries)?;
        return Ok(!(has_diff && exit_code));
    }

    for entry in &entries {
        match entry.status {
            "added" => println!("{}: [new file]", entry.path),
            "removed" => println!("{}: [would be removed]", entry.path),
            "changed" => print!("{}", entry.patch),
            _ => {}
        }
    }

    if !has_diff {
        if !quiet {
            println!("No differences found.");
        }
        return Ok(true);
    }

    if exit_code {
        return Err(eyre!("generated output differs from '{out}'"))
            .suggestion("run 'weaveffi generate' to update the output, or pass --no-exit-code");
    }

    Ok(true)
}

fn unified_patch(path: &str, old: &str, new: &str) -> String {
    use std::fmt::Write;
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    let _ = writeln!(out, "--- {path}");
    let _ = writeln!(out, "+++ {path}");
    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        let _ = writeln!(out, "{hunk}");
    }
    out
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
            if rel != ".weaveffi-cache" && rel != "weaveffi.lock" {
                out.insert(rel);
            }
        }
    }
    Ok(())
}

/// Returns `Ok(true)` when every file in `dir` carries a matching stamp,
/// `Ok(false)` when one or more files are missing a stamp or carry a
/// mismatching IR version. Callers convert a `false` result into exit code 1.
fn cmd_check_stamp(dir: &str, expected_ir_version: Option<&str>, quiet: bool) -> Result<bool> {
    let dir_path = Utf8Path::new(dir);
    if !dir_path.exists() {
        bail!("directory does not exist: {}", dir);
    }
    if !dir_path.is_dir() {
        bail!("path is not a directory: {}", dir);
    }
    let expected = expected_ir_version.unwrap_or(CURRENT_SCHEMA_VERSION);

    let files = collect_relative_files(dir_path)?;
    let mut problems: Vec<String> = Vec::new();
    let mut checked: usize = 0;

    for rel in &files {
        if should_skip_stamp_check(rel) {
            continue;
        }

        let full_path = dir_path.join(rel);
        let contents = match std::fs::read_to_string(full_path.as_std_path()) {
            Ok(s) => s,
            Err(_) => continue,
        };

        match extract_stamp_ir_version(&contents) {
            Some(version) if version == expected => {
                checked += 1;
            }
            Some(version) => {
                checked += 1;
                problems.push(format!(
                    "{rel}: IR version mismatch (expected {expected}, got {version})"
                ));
            }
            None => {
                problems.push(format!("{rel}: missing stamp"));
            }
        }
    }

    if problems.is_empty() {
        if !quiet {
            println!("All {checked} files have matching stamps (IR version {expected})");
        }
        Ok(true)
    } else {
        for p in &problems {
            eprintln!("{p}");
        }
        Ok(false)
    }
}

/// Returns `Ok(true)` when every recorded file in `weaveffi.lock` still
/// hashes to the value it did at generation time (and no stray files have
/// appeared in `dir`), `Ok(false)` when at least one diff is reported.
/// Callers convert a `false` result into exit code 1.
fn cmd_verify(dir: &str, lockfile: Option<&str>, quiet: bool) -> Result<bool> {
    let dir_path = Utf8Path::new(dir);
    if !dir_path.exists() {
        bail!("directory does not exist: {}", dir);
    }
    if !dir_path.is_dir() {
        bail!("path is not a directory: {}", dir);
    }

    let lock_path = match lockfile {
        Some(p) => Utf8Path::new(p).to_owned(),
        None => dir_path.join(LOCKFILE),
    };
    if !lock_path.exists() {
        bail!("lockfile not found: {}", lock_path);
    }

    let contents = std::fs::read_to_string(lock_path.as_std_path())
        .wrap_err_with(|| format!("failed to read lockfile: {}", lock_path))?;
    let parsed: toml::Value = toml::from_str(&contents)
        .wrap_err_with(|| format!("failed to parse lockfile: {}", lock_path))?;

    let files_table = parsed
        .get("files")
        .and_then(|v| v.as_table())
        .ok_or_else(|| eyre!("lockfile missing [files] section: {}", lock_path))?;

    let mut recorded: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in files_table.iter() {
        let h = v
            .as_str()
            .ok_or_else(|| eyre!("[files].{} is not a string in {}", k, lock_path))?;
        recorded.insert(k.clone(), h.to_string());
    }

    let mut problems: Vec<String> = Vec::new();

    for (rel, expected_hash) in &recorded {
        let file_path = dir_path.join(rel);
        if !file_path.exists() {
            problems.push(format!("{rel}: missing"));
            continue;
        }
        let bytes = std::fs::read(file_path.as_std_path())
            .wrap_err_with(|| format!("failed to read {}", file_path))?;
        let actual_hash = format!("{:x}", Sha256::digest(&bytes));
        if actual_hash != *expected_hash {
            problems.push(format!(
                "{rel}: modified (expected {expected_hash}, got {actual_hash})"
            ));
        }
    }

    let existing = collect_relative_files(dir_path)?;
    for rel in &existing {
        if !recorded.contains_key(rel) {
            problems.push(format!("{rel}: untracked (not in lockfile)"));
        }
    }

    if problems.is_empty() {
        if !quiet {
            println!("All {} files match {}", recorded.len(), lock_path);
        }
        Ok(true)
    } else {
        for p in &problems {
            eprintln!("{p}");
        }
        Ok(false)
    }
}

/// Files that cannot carry a comment-based stamp (JSON, XML build manifests,
/// known binary artefacts) are excluded so the check doesn't false-positive.
fn should_skip_stamp_check(rel: &str) -> bool {
    let ext = Utf8Path::new(rel).extension().unwrap_or("");
    matches!(
        ext,
        "json" | "csproj" | "nuspec" | "node" | "wasm" | "so" | "dylib" | "dll" | "a" | "lib" | "o"
    )
}

/// Pulls the IR version token out of a stamp line like
/// `// WeaveFFI 0.4.0 c 0.1.0 - DO NOT EDIT - ...`.
/// Scans the first few lines so files that put a language directive before the
/// stamp (e.g. Swift's `swift-tools-version`) still parse.
fn extract_stamp_ir_version(contents: &str) -> Option<&str> {
    for line in contents.lines().take(20) {
        if let Some(pos) = line.find("WeaveFFI ") {
            let rest = &line[pos + "WeaveFFI ".len()..];
            if rest.contains("DO NOT EDIT") {
                return rest.split_whitespace().next();
            }
        }
    }
    None
}

/// Renders a parse error with source context: `filename:line:col` in bold,
/// the offending source line with a gutter, and a `^^^` underline in bold red
/// pointing at the column. Uses color-eyre for ANSI colour; callers pass the
/// original input content so the source line can be looked up by line number.
///
/// Falls back to a filename-only note when the error carries no location
/// (e.g. TOML, which embeds its own position inside the message).
fn pretty_parse_error(filename: &str, input: &str, err: ParseError) -> Report {
    let (line, column, suggestion) = match &err {
        ParseError::Yaml { line, column, .. } => (
            *line,
            *column,
            "check YAML syntax: ensure correct indentation, quoting, and key-value formatting",
        ),
        ParseError::Json { line, column, .. } => (
            *line,
            *column,
            "check JSON syntax: ensure all brackets, braces, and commas are correct",
        ),
        ParseError::Toml { .. } => (
            0,
            0,
            "check TOML syntax: ensure correct table headers, key-value pairs, and quoting",
        ),
        ParseError::UnsupportedFormat(_) => {
            (0, 0, "use a supported format: yml, yaml, json, or toml")
        }
    };
    let note = render_source_context(filename, input, line, column);
    eyre!(err).note(note).suggestion(suggestion)
}

/// Builds the coloured source-context note: `filename:line:col` header, the
/// offending source line prefixed with a gutter, and a `^^^` caret pointing
/// at `column`. When `line == 0` only `filename` is returned (for errors
/// without usable location info).
fn render_source_context(filename: &str, input: &str, line: usize, column: usize) -> String {
    use color_eyre::owo_colors::OwoColorize;
    if line == 0 {
        return filename.to_string();
    }
    let source_line = input.lines().nth(line - 1).unwrap_or("");
    let gutter = line.to_string();
    let pad = " ".repeat(gutter.len());
    let col_pad = " ".repeat(column.saturating_sub(1));
    let location = format!("{filename}:{line}:{column}");
    format!(
        "{loc}\n {gutter} | {src}\n {pad} | {col_pad}{caret}",
        loc = location.bold(),
        gutter = gutter,
        src = source_line,
        pad = pad,
        col_pad = col_pad,
        caret = "^^^".red().bold(),
    )
}

fn format_validation_error(filename: &str, input: &str, err: ValidationError) -> Report {
    if let ValidationError::UnknownGeneratorConfigKey { target, .. } = &err {
        let valid = valid_keys_for_generator_target(target);
        let suggestion = format!("valid keys for the `{target}` generator section are: {valid}");
        return eyre!(err).suggestion(suggestion);
    }
    let span = validation_error_span(&err);
    let suggestion = validation_suggestion(&err);
    let mut report = eyre!(err).suggestion(suggestion);
    if let Some(span) = span {
        let note = render_source_context(filename, input, span.line as usize, span.col as usize);
        report = report.note(note);
    }
    report
}

/// Returns the source span carried by a [`ValidationError`] variant, if any.
fn validation_error_span(err: &ValidationError) -> Option<Span> {
    match err {
        ValidationError::DuplicateFunctionName { span, .. }
        | ValidationError::DuplicateStructName { span, .. }
        | ValidationError::DuplicateStructField { span, .. }
        | ValidationError::DuplicateEnumVariant { span, .. } => *span,
        _ => None,
    }
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

fn cmd_extract(
    input: &str,
    output: Option<&str>,
    output_format: &str,
    quiet: bool,
    global_format: OutputFormat,
) -> Result<()> {
    let source = std::fs::read_to_string(input)
        .wrap_err_with(|| format!("failed to read source file: {}", input))?;

    let mut api = extract::extract_api_from_rust(&source).map_err(|e| {
        // `extract_api_from_rust` runs `syn::parse_file` first; if that failed,
        // render a pretty parse error with the same source-context style used
        // by the IDL commands. ParseError has no Rust variant, so we reuse
        // just the `render_source_context` primitive instead of routing
        // through `pretty_parse_error` (which would tag the message as YAML).
        if let Some(syn_err) = e.root_cause().downcast_ref::<syn::Error>() {
            let start = syn_err.span().start();
            let note =
                render_source_context(input, &source, start.line, start.column.saturating_add(1));
            eyre!("Rust parse error: {syn_err}")
                .note(note)
                .suggestion("check Rust syntax and fix the offending line")
        } else {
            e.wrap_err("failed to extract API from Rust source")
        }
    })?;

    if let Err(e) = validate_api_with_spans(&mut api, &SpanTable::default()) {
        eprintln!("warning: {}", e);
    }

    let effective_format = if global_format.is_json() {
        "json"
    } else {
        output_format
    };

    let serialized = match effective_format {
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
            if !quiet && !global_format.is_json() {
                println!("Extracted API written to {}", path);
            }
        }
        None => print!("{}", serialized),
    }

    Ok(())
}

/// Runs the doctor checks and returns `Ok(true)` when the process should exit
/// 0. Required checks (rustc, cargo, weaveffi-cli) always gate success;
/// optional checks only gate success when `all` is true.
fn cmd_doctor(all: bool, format: OutputFormat) -> Result<bool> {
    let json_mode = format.is_json();
    if !json_mode {
        println!("WeaveFFI Doctor: checking toolchain prerequisites\n");
        println!("Required checks:");
    }

    let mut required: Vec<DoctorCheck> = Vec::new();
    let mut optional: Vec<DoctorCheck> = Vec::new();

    required.push(run_tool_check(
        "rustc",
        &["--version"],
        "Rust compiler",
        Some("Install via https://rustup.rs"),
        json_mode,
    ));
    required.push(run_tool_check(
        "cargo",
        &["--version"],
        "Cargo (Rust package manager)",
        Some("Install via https://rustup.rs"),
        json_mode,
    ));
    required.push(run_weaveffi_cli_check(json_mode));

    if !json_mode {
        println!("\nOptional checks:");
    }
    if cfg!(target_os = "macos") {
        optional.push(run_tool_check(
            "xcodebuild",
            &["-version"],
            "Xcode command-line tools",
            Some("Install Xcode from the App Store, then run `xcode-select --install`"),
            json_mode,
        ));
    } else if !json_mode {
        println!("- Xcode: skipped (non-macOS)");
    }

    let ndk_hint = if cfg!(target_os = "macos") {
        Some("Install via Android Studio SDK Manager or `brew install android-ndk`. Set ANDROID_NDK_HOME.")
    } else {
        Some("Install via Android Studio SDK Manager. Set ANDROID_NDK_HOME.")
    };
    let ndk_check = run_tool_check(
        "ndk-build",
        &["-v"],
        "Android NDK (ndk-build)",
        ndk_hint,
        json_mode,
    );
    if !ndk_check.ok && !json_mode {
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
    optional.push(ndk_check);

    optional.push(run_tool_check(
        "node",
        &["-v"],
        "Node.js",
        Some("Install from https://nodejs.org or with your package manager"),
        json_mode,
    ));
    optional.push(run_tool_check(
        "npm",
        &["-v"],
        "npm",
        Some("Install Node.js which includes npm, or use pnpm/yarn"),
        json_mode,
    ));

    optional.extend(run_cross_target_checks(json_mode));

    if !json_mode {
        println!("\nWebAssembly tools:");
    }
    optional.push(run_tool_check(
        "wasm-pack",
        &["--version"],
        "wasm-pack",
        Some("install with `cargo install wasm-pack`"),
        json_mode,
    ));
    optional.push(run_tool_check(
        "wasm-bindgen",
        &["--version"],
        "wasm-bindgen-cli",
        Some("install with `cargo install wasm-bindgen-cli`"),
        json_mode,
    ));

    let required_failures: Vec<String> = required
        .iter()
        .filter(|c| !c.ok)
        .map(|c| c.name.clone())
        .collect();
    let optional_failures: Vec<String> = optional
        .iter()
        .filter(|c| !c.ok)
        .map(|c| c.name.clone())
        .collect();

    if json_mode {
        let mut all_checks = required;
        all_checks.extend(optional);
        emit_json(&all_checks)?;
        if !required_failures.is_empty() {
            return Ok(false);
        }
        if !optional_failures.is_empty() && all {
            return Ok(false);
        }
        return Ok(true);
    }

    println!();
    if !required_failures.is_empty() {
        eprintln!(
            "Doctor failed: required tool(s) missing: {}",
            required_failures.join(", ")
        );
        if !optional_failures.is_empty() {
            eprintln!(
                "Also missing optional tool(s): {}",
                optional_failures.join(", ")
            );
        }
        return Ok(false);
    }

    if !optional_failures.is_empty() {
        if all {
            eprintln!(
                "Doctor failed (--all): optional tool(s) missing: {}",
                optional_failures.join(", ")
            );
            return Ok(false);
        }
        println!(
            "Doctor completed with warnings: optional tool(s) missing: {}",
            optional_failures.join(", ")
        );
    } else {
        println!("Doctor completed: all checks passed.");
    }
    Ok(true)
}

/// Verifies the currently running `weaveffi` binary can introspect its own
/// install path. Serves as the "required" health check for the CLI itself.
fn run_weaveffi_cli_check(json_mode: bool) -> DoctorCheck {
    match std::env::current_exe() {
        Ok(path) if path.exists() => {
            if !json_mode {
                println!("- weaveffi-cli: OK ({})", path.display());
            }
            DoctorCheck {
                name: "weaveffi-cli".to_string(),
                ok: true,
                version: Some(path.display().to_string()),
                hint: None,
            }
        }
        Ok(path) => {
            if !json_mode {
                println!(
                    "- weaveffi-cli: MISSING (current exe {} does not exist)",
                    path.display()
                );
            }
            DoctorCheck {
                name: "weaveffi-cli".to_string(),
                ok: false,
                version: None,
                hint: Some(format!("current exe {} does not exist", path.display())),
            }
        }
        Err(e) => {
            if !json_mode {
                println!("- weaveffi-cli: MISSING (cannot resolve current executable: {e})");
            }
            DoctorCheck {
                name: "weaveffi-cli".to_string(),
                ok: false,
                version: None,
                hint: Some(format!("cannot resolve current executable: {e}")),
            }
        }
    }
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

/// Returns a DoctorCheck per required cross-compilation target (plus a
/// "rustup" entry when rustup itself is missing).
fn run_cross_target_checks(json_mode: bool) -> Vec<DoctorCheck> {
    if !json_mode {
        println!("\nCross-compilation targets:");
    }

    let installed = match Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        _ => {
            if !json_mode {
                println!("- rustup: MISSING (cannot check installed targets)");
                println!("  hint: install via https://rustup.rs");
            }
            return vec![DoctorCheck {
                name: "rustup".to_string(),
                ok: false,
                version: None,
                hint: Some("install via https://rustup.rs".to_string()),
            }];
        }
    };

    let required = [
        ("aarch64-apple-ios", "iOS"),
        ("aarch64-linux-android", "Android"),
        ("wasm32-unknown-unknown", "WebAssembly"),
    ];

    let mut checks: Vec<DoctorCheck> = Vec::new();
    for (target, label) in &required {
        let ok = installed.lines().any(|line| line.trim() == *target);
        if !json_mode {
            if ok {
                println!("- {} ({}): installed", label, target);
            } else {
                println!("- {} ({}): MISSING", label, target);
                println!("  hint: install with `rustup target add {}`", target);
            }
        }
        checks.push(DoctorCheck {
            name: format!("target:{target}"),
            ok,
            version: if ok {
                Some((*target).to_string())
            } else {
                None
            },
            hint: if ok {
                None
            } else {
                Some(format!("install with `rustup target add {target}`"))
            },
        });
    }
    checks
}

fn run_tool_check<S: AsRef<OsStr>>(
    cmd: &str,
    args: &[S],
    label: &str,
    hint: Option<&str>,
    json_mode: bool,
) -> DoctorCheck {
    match Command::new(cmd).args(args).output() {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !json_mode {
                if ver.is_empty() {
                    println!("- {}: OK ({})", label, cmd);
                } else {
                    println!("- {}: OK ({}: {})", label, cmd, ver);
                }
            }
            DoctorCheck {
                name: cmd.to_string(),
                ok: true,
                version: if ver.is_empty() { None } else { Some(ver) },
                hint: None,
            }
        }
        Ok(out) => {
            if !json_mode {
                println!(
                    "- {}: MISSING ({} exited with status {})",
                    label, cmd, out.status
                );
                if let Some(h) = hint {
                    println!("  hint: {}", h);
                }
            }
            DoctorCheck {
                name: cmd.to_string(),
                ok: false,
                version: None,
                hint: hint.map(|s| s.to_string()),
            }
        }
        Err(_) => {
            if !json_mode {
                println!("- {}: MISSING ({} not found in PATH)", label, cmd);
                if let Some(h) = hint {
                    println!("  hint: {}", h);
                }
            }
            DoctorCheck {
                name: cmd.to_string(),
                ok: false,
                version: None,
                hint: hint.map(|s| s.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::validate::ValidationError;
    use weaveffi_ir::parse::ParseError;

    /// Strip ANSI escape codes so assertions can match plain text. Keeps the test
    /// robust when `render_source_context` wraps tokens in `owo_colors` styles.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn validation_suggestion_covers_all_variants() {
        let cases: Vec<ValidationError> = vec![
            ValidationError::NoModuleName,
            ValidationError::DuplicateModuleName("m".into()),
            ValidationError::InvalidModuleName("123".into(), "bad"),
            ValidationError::DuplicateFunctionName {
                module: "m".into(),
                function: "f".into(),
                span: None,
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
                span: None,
            },
            ValidationError::DuplicateStructField {
                struct_name: "S".into(),
                field: "f".into(),
                span: None,
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
                span: None,
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
    fn parse_error_shows_source_with_caret() {
        let input = "version: \"0.1.0\"\nmodules:\n  - name: 123bad\n    functions: []\n";
        let note = render_source_context("schema.yml", input, 3, 11);
        let plain = strip_ansi(&note);

        assert!(
            plain.contains("schema.yml:3:11"),
            "filename:line:col header should appear: {plain}"
        );
        assert!(
            plain.contains("- name: 123bad"),
            "offending source line should be shown: {plain}"
        );
        assert!(
            plain.contains("^^^"),
            "caret underline should be shown: {plain}"
        );
        let caret_line = plain
            .lines()
            .find(|l| l.contains("^^^"))
            .expect("caret line missing");
        let caret_col = caret_line.find("^^^").expect("caret in caret line");
        assert!(
            caret_col >= 11,
            "caret should be indented to the error column (got col {caret_col}): {plain}"
        );

        assert!(
            note.contains("\x1b["),
            "note should contain ANSI colour escapes for terminal rendering: {note:?}"
        );

        let err = ParseError::Yaml {
            line: 3,
            column: 11,
            message: "invalid module name".into(),
        };
        let report = pretty_parse_error("schema.yml", input, err);
        assert!(
            report.to_string().contains("YAML parse error"),
            "main error message should be preserved: {report}"
        );
    }

    #[test]
    fn parse_error_without_location_uses_filename_only() {
        let note = render_source_context("config.toml", "", 0, 0);
        assert_eq!(note, "config.toml");
        assert!(
            !note.contains("^^^"),
            "caret should be omitted for errors without location info: {note}"
        );

        let err = ParseError::Toml {
            message: "bad table header".into(),
        };
        let report = pretty_parse_error("config.toml", "", err);
        assert!(
            report.to_string().contains("TOML parse error"),
            "main error message should be preserved: {report}"
        );
    }

    #[test]
    fn format_validation_error_preserves_message() {
        let _ = color_eyre::install();
        let err = ValidationError::DuplicateModuleName("foo".into());
        let report = format_validation_error("schema.yml", "", err);
        let msg = report.to_string();
        assert!(
            msg.contains("duplicate module name"),
            "missing error message in: {msg}"
        );
        assert!(msg.contains("foo"), "missing module name in: {msg}");
    }

    #[test]
    fn validation_error_shows_source_position() {
        let yaml = concat!(
            "version: 0.1.0\n",
            "modules:\n",
            "  - name: math\n",
            "    functions:\n",
            "      - name: add\n",
            "        params:\n",
            "          - { name: a, type: i32 }\n",
            "        return: i32\n",
            "      - name: add\n",
            "        params:\n",
            "          - { name: a, type: i32 }\n",
            "        return: i32\n",
        );
        let (mut api, spans) = parse_api_str_with_spans(yaml, "yaml").unwrap();
        let err = validate_api_with_spans(&mut api, &spans).unwrap_err();

        assert!(
            matches!(
                &err,
                ValidationError::DuplicateFunctionName { span: Some(_), .. }
            ),
            "duplicate function error should carry a span, got: {err:?}"
        );

        let span = validation_error_span(&err).expect("duplicate error should carry a span");
        assert_eq!(
            span.line, 5,
            "span should point at the first `add` declaration"
        );
        assert!(span.col >= 1, "span column should be 1-based");

        let entry = validation_error_to_entry("schema.yml", &err);
        let location = entry.location.as_deref().expect("location populated");
        assert!(
            location.starts_with("schema.yml:5:"),
            "JSON error entry should include precise source location, got: {location}"
        );

        let note = render_source_context("schema.yml", yaml, span.line as usize, span.col as usize);
        let plain = strip_ansi(&note);
        assert!(
            plain.contains("schema.yml:5"),
            "rendered context should include file:line: {plain}"
        );
        assert!(
            plain.contains("- name: add"),
            "rendered context should show offending line: {plain}"
        );
        assert!(
            plain.contains("^^^"),
            "rendered context should include caret underline: {plain}"
        );
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
            cmd_lint(&sample, false, OutputFormat::Text).unwrap(),
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
            input,
            out_str,
            None,
            false,
            None,
            false,
            false,
            true,
            None,
            true,
            false,
            OutputFormat::Text,
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
    fn doctor_exits_nonzero_when_required_missing() {
        let cmd = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .env("PATH", "/nonexistent-weaveffi-doctor-path")
            .arg("doctor")
            .output()
            .expect("failed to run weaveffi doctor");

        assert!(
            !cmd.status.success(),
            "doctor must exit non-zero when required tools are missing (stdout: {}, stderr: {})",
            String::from_utf8_lossy(&cmd.stdout),
            String::from_utf8_lossy(&cmd.stderr),
        );
        let stderr = String::from_utf8_lossy(&cmd.stderr);
        assert!(
            stderr.contains("required tool(s) missing"),
            "expected required-failure summary in stderr: {stderr}"
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
            .args(["diff", input, "--out", out_str, "--no-exit-code"])
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
    fn diff_exits_nonzero_when_changes() {
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

        assert!(
            !cmd.status.success(),
            "diff should exit non-zero when changes are detected"
        );
        let stdout = String::from_utf8_lossy(&cmd.stdout);
        assert!(
            stdout.contains("[new file]"),
            "diff stdout should still list new files: {stdout}"
        );
    }

    #[test]
    fn check_stamp_passes_for_freshly_generated_dir() {
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
            input,
            out_str,
            Some("c"),
            false,
            None,
            false,
            false,
            false,
            None,
            true,
            true,
            OutputFormat::Text,
        )
        .unwrap();

        assert!(
            cmd_check_stamp(out_str, None, true).unwrap(),
            "freshly generated dir should pass the default stamp check"
        );
        assert!(
            cmd_check_stamp(out_str, Some(CURRENT_SCHEMA_VERSION), true).unwrap(),
            "freshly generated dir should pass when expected IR version is supplied explicitly"
        );
        assert!(
            !cmd_check_stamp(out_str, Some("9.9.9-mismatch"), true).unwrap(),
            "stamp check should fail when the expected IR version does not match"
        );
    }

    #[test]
    fn extract_stamp_ir_version_parses_first_stamp() {
        let stamped = "// WeaveFFI 1.2.3 c 0.4.0 - DO NOT EDIT - regenerate with 'weaveffi generate'\n\npub fn foo() {}\n";
        assert_eq!(extract_stamp_ir_version(stamped), Some("1.2.3"));

        let no_stamp = "# WeaveFFI Python Bindings\n\nAuto-generated docs.\n";
        assert_eq!(extract_stamp_ir_version(no_stamp), None);
    }

    #[test]
    fn generate_writes_lockfile_by_default() {
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
                "c",
            ])
            .output()
            .expect("failed to run weaveffi generate");

        assert!(
            cmd.status.success(),
            "generate failed: {}",
            String::from_utf8_lossy(&cmd.stderr)
        );
        assert!(
            out.join("weaveffi.lock").exists(),
            "weaveffi.lock should be written by default"
        );
    }

    #[test]
    fn generate_no_lockfile_flag_skips_writing() {
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
                "c",
                "--no-lockfile",
            ])
            .output()
            .expect("failed to run weaveffi generate --no-lockfile");

        assert!(
            cmd.status.success(),
            "generate --no-lockfile failed: {}",
            String::from_utf8_lossy(&cmd.stderr)
        );
        assert!(
            !out.join("weaveffi.lock").exists(),
            "weaveffi.lock must not be written with --no-lockfile"
        );
    }

    fn write_verify_fixture(dir: &std::path::Path) -> (String, String) {
        let yml = dir.join("api.yml");
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

        let out = dir.join("out");
        let input = yml.to_str().unwrap().to_string();
        let out_str = out.to_str().unwrap().to_string();

        cmd_generate(
            &input,
            &out_str,
            Some("c"),
            false,
            None,
            false,
            false,
            false,
            None,
            true,
            true,
            OutputFormat::Text,
        )
        .unwrap();

        (input, out_str)
    }

    #[test]
    fn verify_succeeds_for_unchanged_dir() {
        let _ = color_eyre::install();
        let dir = tempfile::tempdir().unwrap();
        let (_input, out_str) = write_verify_fixture(dir.path());

        assert!(
            cmd_verify(&out_str, None, true).unwrap(),
            "freshly generated dir should verify against its own lockfile"
        );
    }

    #[test]
    fn verify_fails_when_file_modified() {
        let _ = color_eyre::install();
        let dir = tempfile::tempdir().unwrap();
        let (_input, out_str) = write_verify_fixture(dir.path());

        let header = std::path::Path::new(&out_str).join("c/weaveffi.h");
        assert!(header.exists(), "expected generated C header at {header:?}");
        std::fs::write(&header, "// tampered\n").unwrap();

        assert!(
            !cmd_verify(&out_str, None, true).unwrap(),
            "verify must fail when a tracked file is modified"
        );
    }
}
