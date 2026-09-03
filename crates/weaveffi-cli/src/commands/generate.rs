//! `weaveffi generate`: parse, validate, and run the selected targets
//! through the orchestrator (plus `--dry-run`).

use camino::Utf8Path;
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use weaveffi_core::codegen::Orchestrator;

#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_generate(
    input: &str,
    out: &str,
    targets: Option<&str>,
    config_path: Option<&str>,
    warn: bool,
    force: bool,
    dry_run: bool,
    quiet: bool,
) -> Result<()> {
    let (config, api) = super::load_project(input, config_path, warn)?;
    let out_dir = Utf8Path::new(out);
    let hooks = config.hooks();
    let selected = config.select_targets(targets)?;

    if dry_run {
        for target in &selected {
            for path in target.output_files(&api, out_dir) {
                println!("{path}");
            }
        }
        return Ok(());
    }

    std::fs::create_dir_all(out_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create output directory: {}", out))?;

    let mut orchestrator = Orchestrator::new();
    for target in &selected {
        orchestrator = orchestrator.with_target(target.as_ref());
    }

    orchestrator
        .run(&api, out_dir, &hooks, force)
        .map_err(|e| miette!("{:#}", e))?;

    if !quiet {
        match &config.source {
            Some(src) => println!("Generated artifacts in {out} (config: {src})"),
            None => println!("Generated artifacts in {out}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProjectConfig;

    #[test]
    fn dry_run_lists_files_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let yml = dir.path().join("api.yml");
        std::fs::write(
            &yml,
            concat!(
                "version: \"0.9.0\"\n",
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

        cmd_generate(input, out_str, None, None, false, false, true, false).unwrap();
        assert!(!out.exists(), "dry-run should not create output directory");

        let (_, api) = super::super::load_project(input, None, false).unwrap();
        let out_dir = Utf8Path::new(out_str);
        let files: Vec<String> = ProjectConfig::default()
            .select_targets(None)
            .unwrap()
            .iter()
            .flat_map(|t| t.output_files(&api, out_dir))
            .collect();

        for expected in [
            "c/weaveffi.h",
            "swift/Package.swift",
            "kotlin/build.gradle.kts",
            "node/types.d.ts",
            "wasm/weaveffi_wasm.js",
            "python/weaveffi/__init__.py",
        ] {
            assert!(
                files.iter().any(|f| f.contains(expected)),
                "missing {expected}: {files:?}"
            );
        }
    }
}
