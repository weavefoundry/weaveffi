//! `weaveffi generate`: parse, validate, plan the selected targets against
//! the output directory, then write the plan, or with `--dry-run`,
//! `--check`, or `--diff`, report it without writing.

use std::process::ExitCode;

use miette::Result;

use super::Locate;
use crate::codegen::Orchestrator;

/// Options for [`cmd_generate`].
pub struct GenerateArgs<'a> {
    /// Where the project is.
    pub locate: Locate<'a>,
    /// `--out`.
    pub out: Option<&'a str>,
    /// `--target`.
    pub targets: Option<&'a [String]>,
    /// `--warn`.
    pub warn: bool,
    /// `--dry-run`: list the files the targets render.
    pub dry_run: bool,
    /// `--check`: exit 1 when anything would change.
    pub check: bool,
    /// `--diff`: print the unified diff of what would change.
    pub diff: bool,
}

/// Run `weaveffi generate`.
pub fn cmd_generate(args: &GenerateArgs<'_>) -> Result<ExitCode> {
    let quiet = args.locate.quiet;
    let project = args.locate.project()?;
    let model = super::load_model(&project, args.warn)?;
    let out_dir = project.config.out_dir(args.out);
    let targets = project.config.targets(args.targets)?;
    let plan = Orchestrator::new()
        .with_targets(targets.iter().map(AsRef::as_ref))
        .plan(&model, &out_dir)?;

    if args.dry_run {
        for path in plan.rendered() {
            println!("{path}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    if args.diff {
        print!("{}", plan.unified_diff());
    }
    if args.check {
        let changes: Vec<_> = plan.changes().collect();
        if !args.diff {
            for change in &changes {
                println!("{} {}", change.kind.marker(), change.path);
            }
        }
        if changes.is_empty() {
            if !quiet {
                eprintln!("{out_dir} is up to date");
            }
            return Ok(ExitCode::SUCCESS);
        }
        eprintln!(
            "{} generated file{} in {out_dir} would change; run `weaveffi generate`",
            changes.len(),
            if changes.len() == 1 { "" } else { "s" }
        );
        return Ok(ExitCode::FAILURE);
    }
    if args.diff {
        return Ok(ExitCode::SUCCESS);
    }
    let report = plan.apply()?;
    if !quiet {
        println!("{}", report.summary(&out_dir));
    }
    Ok(ExitCode::SUCCESS)
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
            "version: \"0.12.0\"\nmodules:\n  - name: math\n    functions:\n      - { name: add, params: [{ name: a, type: i32 }], return: i32 }\n",
        )
        .unwrap();
        let out = dir.path().join("out");
        let targets = ["c".to_string()];
        cmd_generate(&GenerateArgs {
            locate: Locate {
                input: yml.to_str(),
                ..Locate::default()
            },
            out: out.to_str(),
            targets: Some(&targets),
            warn: false,
            dry_run: true,
            check: false,
            diff: false,
        })
        .unwrap();
        assert!(!out.exists(), "dry-run should not create output directory");
    }
}
