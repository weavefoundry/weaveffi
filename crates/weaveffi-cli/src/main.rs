use anyhow::{bail, Context, Result};
use camino::Utf8Path;
use clap::{Parser, Subcommand};
use std::env;
use std::ffi::OsStr;
use std::process::Command;
use tracing_subscriber::EnvFilter;
use weaveffi_core::codegen::{Generator, Orchestrator};
use weaveffi_core::validate::validate_api;
use weaveffi_gen_android::AndroidGenerator;
use weaveffi_gen_c::CGenerator;
use weaveffi_gen_node::NodeGenerator;
use weaveffi_gen_swift::SwiftGenerator;
use weaveffi_gen_wasm::WasmGenerator;
use weaveffi_ir::parse::parse_api_str;

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
        Commands::Generate { input, out, target } => cmd_generate(&input, &out, target.as_deref())?,
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
        .with_context(|| format!("failed to create project directory: {}", name))?;

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
        .with_context(|| format!("failed to write {}", idl_path))?;

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
        .with_context(|| format!("failed to write {}", readme_path))?;

    println!("Initialized WeaveFFI project at {}", project_dir);
    println!("- IDL: {}", idl_path);
    println!(
        "Next: run `weaveffi generate {}/weaveffi.yml -o generated`",
        name
    );
    Ok(())
}

fn cmd_generate(input: &str, out: &str, targets: Option<&str>) -> Result<()> {
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
        .with_context(|| format!("failed to read input file: {}", input))?;
    let api = parse_api_str(&contents, format)
        .with_context(|| format!("failed to parse {} as {}", input, format))?;
    validate_api(&api).context("IR validation failed")?;

    let out_dir = Utf8Path::new(out);
    std::fs::create_dir_all(out_dir.as_std_path())
        .with_context(|| format!("failed to create output directory: {}", out))?;

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

    orchestrator.run(&api, out_dir)?;
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
        .with_context(|| format!("failed to read input file: {}", input))?;
    let api = parse_api_str(&contents, format)
        .with_context(|| format!("failed to parse {} as {}", input, format))?;

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
        Err(e) => {
            bail!("Validation failed: {}", e);
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
