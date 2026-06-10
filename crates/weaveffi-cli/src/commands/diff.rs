//! `weaveffi diff` — regenerate into a temp directory and compare against
//! the on-disk output (`--check` for CI gating with distinct exit codes).

use crate::config::{merge_inline_generators, CliConfig};
use camino::Utf8Path;
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use similar::TextDiff;
use std::collections::BTreeSet;
use weaveffi_core::codegen::Orchestrator;

pub(crate) fn cmd_diff(input: &str, out: Option<&str>, check: bool, quiet: bool) -> Result<()> {
    let out = out.unwrap_or("./generated");

    let in_path = Utf8Path::new(input);
    let (api, _contents) = super::load_validated_api(input)?;

    let tmp = tempfile::tempdir()
        .into_diagnostic()
        .wrap_err("failed to create temp directory")?;
    let tmp_path = Utf8Path::from_path(tmp.path())
        .ok_or_else(|| miette!("temp directory path is not valid UTF-8"))?;

    let mut config = CliConfig::default();
    if let Some(ref generators) = api.generators {
        merge_inline_generators(&mut config, generators);
    }
    config.finalize(in_path.file_name().map(str::to_string));
    let hooks = config.hooks();
    let generators = config.build_generators();

    let mut orchestrator = Orchestrator::new();
    for gen in &generators {
        orchestrator = orchestrator.with_generator(gen.as_ref());
    }
    orchestrator
        .run(&api, tmp_path, &hooks, true)
        .map_err(|e| miette!("{:#}", e))?;

    let out_dir = Utf8Path::new(out);

    let generated = collect_relative_files(tmp_path)?;
    let existing = if out_dir.exists() {
        collect_relative_files(out_dir)?
    } else {
        BTreeSet::new()
    };

    let all_paths: BTreeSet<_> = generated.union(&existing).collect();
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut modified = 0usize;

    for rel in &all_paths {
        let gen_file = tmp_path.join(rel);
        let out_file = out_dir.join(rel);

        match (gen_file.exists(), out_file.exists()) {
            (true, false) => {
                added += 1;
                if !check {
                    println!("{rel}: [new file]");
                }
            }
            (false, true) => {
                removed += 1;
                if !check {
                    println!("{rel}: [would be removed]");
                }
            }
            (true, true) => {
                let gen_content =
                    std::fs::read_to_string(gen_file.as_std_path()).into_diagnostic()?;
                let out_content =
                    std::fs::read_to_string(out_file.as_std_path()).into_diagnostic()?;
                if gen_content != out_content {
                    modified += 1;
                    if !check {
                        print_unified_diff(rel, &out_content, &gen_content);
                    }
                }
            }
            _ => {}
        }
    }

    if check {
        println!("+ {added} added, - {removed} removed, ~ {modified} modified");
        if added > 0 || removed > 0 {
            std::process::exit(3);
        }
        if modified > 0 {
            std::process::exit(2);
        }
        return Ok(());
    }

    if added == 0 && removed == 0 && modified == 0 && !quiet {
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
        if utf8.file_name() == Some(".weaveffi-cache") {
            continue;
        }
        if utf8.is_dir() {
            walk_dir(base, &utf8, out)?;
        } else {
            let rel = utf8
                .strip_prefix(base)
                .into_diagnostic()
                .wrap_err("failed to strip prefix")?
                .to_string();
            out.insert(rel);
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

#[cfg(test)]
mod tests {
    #[test]
    fn diff_shows_new_files() {
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
