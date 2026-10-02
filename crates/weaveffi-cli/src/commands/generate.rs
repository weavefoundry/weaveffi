//! `weaveffi generate`: parse, validate, and run the selected targets
//! through the orchestrator (plus `--dry-run`).

use camino::Utf8Path;
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use weaveffi_gen::codegen::{relative_path, GenerateReport, Hook, Orchestrator};

/// Options for [`cmd_generate`].
pub(crate) struct GenerateArgs<'a> {
    pub(crate) input: Option<&'a str>,
    pub(crate) out: Option<&'a str>,
    pub(crate) targets: Option<&'a str>,
    pub(crate) config: Option<&'a str>,
    pub(crate) warn: bool,
    pub(crate) force: bool,
    pub(crate) dry_run: bool,
    pub(crate) quiet: bool,
}

pub(crate) fn cmd_generate(args: &GenerateArgs<'_>) -> Result<()> {
    let project = super::load_project(args.input, args.config, args.warn)?;
    let out_dir = project.config.out_dir(args.out);
    let selected = project.config.select_targets(args.targets)?;

    if args.dry_run {
        for target in &selected {
            for file in target.render(&project.api, &out_dir) {
                println!("{}", relative_path(&out_dir, &file.path));
            }
        }
        return Ok(());
    }

    std::fs::create_dir_all(out_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create output directory: {out_dir}"))?;

    let mut orchestrator = Orchestrator::new();
    for target in &selected {
        orchestrator = orchestrator.with_target(target.as_ref());
    }
    let global = &project.config.global;
    let report = orchestrator
        .run(&project.api, &out_dir, args.force, &mut |hook| {
            let cmd = match hook {
                Hook::BeforeWrite => global.pre_generate.as_deref(),
                Hook::AfterWrite => global.post_generate.as_deref(),
            };
            match cmd {
                Some(cmd) => {
                    super::run_hook(&format!("{hook:?}"), cmd).map_err(|e| anyhow::anyhow!("{e}"))
                }
                None => Ok(()),
            }
        })
        .map_err(|e| miette!("{:#}", e))?;

    for w in &report.warnings {
        eprintln!("warning: {w}");
    }
    if !args.quiet {
        println!("{}", report_summary(&report, &out_dir));
    }
    Ok(())
}

/// One line describing what a generation run did.
fn report_summary(report: &GenerateReport, out_dir: &Utf8Path) -> String {
    if report.generated.is_empty() {
        return format!(
            "{out_dir} is up to date ({} targets)",
            report.up_to_date.len()
        );
    }
    let mut line = format!(
        "Generated {} in {out_dir}: {} written, {} unchanged",
        report.generated.join(", "),
        report.written,
        report.unchanged
    );
    if !report.removed.is_empty() {
        line.push_str(&format!(", {} stale removed", report.removed.len()));
    }
    if !report.up_to_date.is_empty() {
        line.push_str(&format!(" ({} up to date)", report.up_to_date.join(", ")));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_lists_files_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let yml = dir.path().join("api.yml");
        std::fs::write(
            &yml,
            "version: \"0.10.0\"\nmodules:\n  - name: math\n    functions:\n      - { name: add, params: [{ name: a, type: i32 }], return: i32 }\n",
        )
        .unwrap();
        let out = dir.path().join("out");
        cmd_generate(&GenerateArgs {
            input: yml.to_str(),
            out: out.to_str(),
            targets: Some("c"),
            config: None,
            warn: false,
            force: false,
            dry_run: true,
            quiet: false,
        })
        .unwrap();
        assert!(!out.exists(), "dry-run should not create output directory");
    }
}
