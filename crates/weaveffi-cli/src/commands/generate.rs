//! `weaveffi generate`: parse, validate, and run the selected targets
//! through the orchestrator (plus `--dry-run`).

use camino::Utf8Path;
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use weaveffi_cli::codegen::{relative_path, GenerateReport, Orchestrator};
use weaveffi_cli::project::Project;
use weaveffi_model::model::Model;

use super::Locate;

/// Options for [`cmd_generate`].
pub(crate) struct GenerateArgs<'a> {
    pub(crate) locate: Locate<'a>,
    pub(crate) out: Option<&'a str>,
    pub(crate) targets: Option<&'a str>,
    pub(crate) warn: bool,
    pub(crate) dry_run: bool,
}

pub(crate) fn cmd_generate(args: &GenerateArgs<'_>) -> Result<()> {
    let project = args.locate.project()?;
    let model = super::load_model(&project, args.warn)?;
    let out_dir = project.config.out_dir(args.out);
    let report = generate(&project, &model, &out_dir, args.targets, args.dry_run)?;
    if let Some(report) = report {
        if !args.locate.quiet {
            println!("{}", report_summary(&report, &out_dir));
        }
    }
    Ok(())
}

/// Generate the selected targets into `out_dir` (or, with `dry_run`, list
/// the files that would be written), returning the run's report.
pub(crate) fn generate(
    project: &Project,
    model: &Model,
    out_dir: &Utf8Path,
    targets: Option<&str>,
    dry_run: bool,
) -> Result<Option<GenerateReport>> {
    let selected = project.config.select_targets(targets)?;
    if dry_run {
        for target in &selected {
            for file in target.render(model, out_dir) {
                println!("{}", relative_path(out_dir, &file.path));
            }
        }
        return Ok(None);
    }

    std::fs::create_dir_all(out_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create output directory: {out_dir}"))?;

    let mut orchestrator = Orchestrator::new();
    for target in &selected {
        orchestrator = orchestrator.with_target(target.as_ref());
    }
    let report = orchestrator
        .run(model, out_dir)
        .map_err(|e| miette!("{:#}", e))?;
    Ok(Some(report))
}

/// One line describing what a generation run did.
pub(crate) fn report_summary(report: &GenerateReport, out_dir: &Utf8Path) -> String {
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
            "version: \"0.11.0\"\nmodules:\n  - name: math\n    functions:\n      - { name: add, params: [{ name: a, type: i32 }], return: i32 }\n",
        )
        .unwrap();
        let out = dir.path().join("out");
        cmd_generate(&GenerateArgs {
            locate: Locate {
                input: yml.to_str(),
                ..Locate::default()
            },
            out: out.to_str(),
            targets: Some("c"),
            warn: false,
            dry_run: true,
        })
        .unwrap();
        assert!(!out.exists(), "dry-run should not create output directory");
    }
}
