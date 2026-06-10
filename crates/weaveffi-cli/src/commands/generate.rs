//! `weaveffi generate` — parse, validate, and run the selected generators
//! through the orchestrator (plus `--scaffold` and `--dry-run`).

use crate::config::{merge_inline_generators, CliConfig};
use crate::scaffold;
use camino::Utf8Path;
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use weaveffi_core::codegen::{DynGenerator, Orchestrator};
use weaveffi_core::validate::collect_warnings;

#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_generate(
    input: &str,
    out: &str,
    targets: Option<&str>,
    emit_scaffold: bool,
    config_path: Option<&str>,
    warn: bool,
    force: bool,
    dry_run: bool,
    quiet: bool,
) -> Result<()> {
    let mut config = CliConfig::load(config_path)?;

    let in_path = Utf8Path::new(input);
    let (api, _contents) = super::load_validated_api(input)?;

    if let Some(ref generators) = api.generators {
        merge_inline_generators(&mut config, generators);
    }
    config.finalize(in_path.file_name().map(str::to_string));

    if warn {
        for w in collect_warnings(&api) {
            eprintln!("warning: {w}");
        }
    }

    let out_dir = Utf8Path::new(out);
    let scaffold_prefix = config.scaffold_prefix();
    let hooks = config.hooks();
    let generators = config.build_generators();

    let filter: Option<Vec<&str>> = targets.map(|t| t.split(',').map(str::trim).collect());
    let selected: Vec<&dyn DynGenerator> = generators
        .iter()
        .map(|g| g.as_ref())
        .filter(|g| filter.as_ref().is_none_or(|ts| ts.contains(&g.name())))
        .collect();

    if dry_run {
        for gen in &selected {
            for path in gen.output_files(&api, out_dir) {
                println!("{path}");
            }
        }
        return Ok(());
    }

    std::fs::create_dir_all(out_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create output directory: {}", out))?;

    let mut orchestrator = Orchestrator::new();
    for gen in &selected {
        orchestrator = orchestrator.with_generator(*gen);
    }

    orchestrator
        .run(&api, out_dir, &hooks, force)
        .map_err(|e| miette!("{:#}", e))?;

    if emit_scaffold {
        let scaffold_path = out_dir.join("scaffold.rs");
        let contents = scaffold::render_scaffold(&api, &scaffold_prefix);
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

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;

    #[test]
    fn dry_run_lists_files() {
        let dir = tempfile::tempdir().unwrap();
        let yml = dir.path().join("api.yml");
        std::fs::write(
            &yml,
            concat!(
                "version: \"0.3.0\"\n",
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

        cmd_generate(input, out_str, None, false, None, false, false, true, false).unwrap();

        assert!(!out.exists(), "dry-run should not create output directory");

        let api = {
            let contents = std::fs::read_to_string(&yml).unwrap();
            let mut api = weaveffi_ir::parse::parse_api_str(&contents, "yaml").unwrap();
            weaveffi_core::validate::validate_api(&mut api, None).unwrap();
            api
        };
        let out_dir = Utf8Path::new(out_str);

        let config = CliConfig::default();
        let generators = config.build_generators();

        let mut files: Vec<String> = Vec::new();
        for gen in &generators {
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
}
