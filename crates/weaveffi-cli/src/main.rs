mod scaffold;

use camino::Utf8Path;
use clap::{Parser, Subcommand};
use color_eyre::eyre::{bail, eyre, Report, Result, WrapErr};
use color_eyre::Section;
use std::env;
use std::ffi::OsStr;
use std::process::Command;
use tracing_subscriber::EnvFilter;
use weaveffi_core::codegen::{Generator, Orchestrator};
use weaveffi_core::validate::{validate_api, ValidationError};
use weaveffi_gen_android::AndroidGenerator;
use weaveffi_gen_c::CGenerator;
use weaveffi_gen_node::NodeGenerator;
use weaveffi_gen_swift::SwiftGenerator;
use weaveffi_gen_wasm::WasmGenerator;
use weaveffi_ir::parse::{parse_api_str, ParseError};

#[derive(Parser, Debug)]
#[command(name = "weaveffi", version, about = "WeaveFFI CLI")]
struct Cli {
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
        /// Comma-separated list of targets to generate (c, swift, android, node, wasm)
        #[arg(short, long)]
        target: Option<String>,
        /// Also generate a scaffold.rs with Rust FFI function stubs
        #[arg(long)]
        scaffold: bool,
    },
    Validate {
        /// Input IDL/IR file (yaml|yml|json|toml)
        input: String,
    },
    Doctor,
}

fn main() -> Result<()> {
    let _ = color_eyre::install();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .without_time()
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::New { name } => cmd_new(&name)?,
        Commands::Generate {
            input,
            out,
            target,
            scaffold,
        } => cmd_generate(&input, &out, target.as_deref(), scaffold)?,
        Commands::Validate { input } => cmd_validate(&input)?,
        Commands::Doctor => cmd_doctor()?,
    }
    Ok(())
}

fn cmd_new(name: &str) -> Result<()> {
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
            "    functions:\n",
            "      - name: add\n",
            "        params:\n",
            "          - {{ name: a, type: i32 }}\n",
            "          - {{ name: b, type: i32 }}\n",
            "        return: i32\n",
            "      - name: mul\n",
            "        params:\n",
            "          - {{ name: a, type: i32 }}\n",
            "          - {{ name: b, type: i32 }}\n",
            "        return: i32\n",
            "      - name: echo\n",
            "        params:\n",
            "          - {{ name: s, type: string }}\n",
            "        return: string\n"
        ),
        module = module_name
    );
    std::fs::write(idl_path.as_std_path(), idl_contents)
        .wrap_err_with(|| format!("failed to write {}", idl_path))?;

    let readme_path = project_dir.join("README.md");
    let readme = format!(
        concat!(
            "# {name}\n\n",
            "This project was bootstrapped with WeaveFFI.\n\n",
            "- Edit `weaveffi.yml` to define your API.\n",
            "- Generate outputs: `weaveffi generate weaveffi.yml -o ../generated` (or choose any out dir).\n",
            "- See docs for memory/error model and platform specifics.\n"
        ),
        name = name
    );
    std::fs::write(readme_path.as_std_path(), readme)
        .wrap_err_with(|| format!("failed to write {}", readme_path))?;

    println!("Initialized WeaveFFI project at {}", project_dir);
    println!("- IDL: {}", idl_path);
    println!(
        "Next: run `weaveffi generate {}/weaveffi.yml -o generated`",
        name
    );
    Ok(())
}

fn cmd_generate(input: &str, out: &str, targets: Option<&str>, emit_scaffold: bool) -> Result<()> {
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
    let api = parse_api_str(&contents, format).map_err(|e| format_parse_error(input, e))?;
    validate_api(&api).map_err(format_validation_error)?;

    let out_dir = Utf8Path::new(out);
    std::fs::create_dir_all(out_dir.as_std_path())
        .wrap_err_with(|| format!("failed to create output directory: {}", out))?;

    let c = CGenerator;
    let swift = SwiftGenerator;
    let android = AndroidGenerator;
    let node = NodeGenerator;
    let wasm = WasmGenerator;
    let all: Vec<&dyn Generator> = vec![&c, &swift, &android, &node, &wasm];

    let filter: Option<Vec<&str>> = targets.map(|t| t.split(',').map(str::trim).collect());

    let mut orchestrator = Orchestrator::new();
    for &gen in &all {
        if filter.as_ref().is_none_or(|ts| ts.contains(&gen.name())) {
            orchestrator = orchestrator.with_generator(gen);
        }
    }

    orchestrator
        .run(&api, out_dir)
        .map_err(|e| eyre!("{:#}", e))?;

    if emit_scaffold {
        let scaffold_path = out_dir.join("scaffold.rs");
        let contents = scaffold::render_scaffold(&api);
        std::fs::write(scaffold_path.as_std_path(), contents)
            .wrap_err_with(|| format!("failed to write {}", scaffold_path))?;
        println!("Scaffold written to {}", scaffold_path);
    }

    println!("Generated artifacts in {}", out);
    Ok(())
}

fn cmd_validate(input: &str) -> Result<()> {
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
    let api = parse_api_str(&contents, format).map_err(|e| format_parse_error(input, e))?;

    match validate_api(&api) {
        Ok(()) => {
            let n_modules = api.modules.len();
            let n_functions: usize = api.modules.iter().map(|m| m.functions.len()).sum();
            let n_structs: usize = api.modules.iter().map(|m| m.structs.len()).sum();
            let n_enums: usize = api.modules.iter().map(|m| m.enums.len()).sum();
            println!("Validation passed");
            println!(
                "  {} modules, {} functions, {} structs, {} enums",
                n_modules, n_functions, n_structs, n_enums
            );
            Ok(())
        }
        Err(e) => Err(format_validation_error(e)),
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
        ValidationError::AsyncNotSupported { .. } => {
            "remove 'async: true' from the function definition; async is not supported in version 0.1.0"
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
    }
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

    println!("\nDoctor completed. Address any missing items above.");
    Ok(())
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
            ValidationError::AsyncNotSupported {
                module: "m".into(),
                function: "f".into(),
            },
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
}
