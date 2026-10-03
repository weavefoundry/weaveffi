//! `weaveffi diff`: show how regenerating would change an existing output
//! directory, without writing anything or running hooks. `--check` turns it
//! into a CI gate.

use std::collections::{BTreeMap, BTreeSet};

use camino::Utf8Path;
use miette::{IntoDiagnostic, Result};
use similar::TextDiff;
use weaveffi_gen::cache;
use weaveffi_gen::codegen::relative_path;

/// Options for [`cmd_diff`].
pub(crate) struct DiffArgs<'a> {
    pub(crate) input: Option<&'a str>,
    pub(crate) out: Option<&'a str>,
    pub(crate) targets: Option<&'a str>,
    pub(crate) config: Option<&'a str>,
    pub(crate) check: bool,
    pub(crate) quiet: bool,
}

/// Exit status of `diff --check` when files differ.
const EXIT_MODIFIED: i32 = 2;
/// Exit status of `diff --check` when files would be added or removed.
const EXIT_ADDED_OR_REMOVED: i32 = 3;

pub(crate) fn cmd_diff(args: &DiffArgs<'_>) -> Result<()> {
    let project = super::load_project(args.input, args.config, false)?;
    let out_dir = project.config.out_dir(args.out);
    let targets = project.config.select_targets(args.targets)?;

    let mut generated: BTreeMap<String, String> = BTreeMap::new();
    let mut existing: BTreeSet<String> = BTreeSet::new();
    for target in &targets {
        for file in target.render(&project.api, &out_dir) {
            generated.insert(relative_path(&out_dir, &file.path), file.contents);
        }
        // Files a previous generation recorded are the generator's; without a
        // record, everything under the target's directory counts.
        match cache::read_record(&out_dir, target.name()) {
            Some(record) => existing.extend(
                record
                    .files
                    .into_keys()
                    .filter(|rel| out_dir.join(rel).exists()),
            ),
            None => collect_files(&out_dir, &out_dir.join(target.name()), &mut existing)?,
        }
    }

    let (mut added, mut removed, mut modified) = (0usize, 0usize, 0usize);
    let all: BTreeSet<&String> = generated.keys().chain(existing.iter()).collect();
    for rel in all {
        match (generated.get(rel), existing.contains(rel)) {
            (Some(_), false) => {
                added += 1;
                if !args.check {
                    println!("{rel}: [new file]");
                }
            }
            (None, true) => {
                removed += 1;
                if !args.check {
                    println!("{rel}: [would be removed]");
                }
            }
            (Some(new), true) => {
                let old =
                    std::fs::read_to_string(out_dir.join(rel).as_std_path()).into_diagnostic()?;
                if &old != new {
                    modified += 1;
                    if !args.check {
                        print_unified_diff(rel, &old, new);
                    }
                }
            }
            (None, false) => {}
        }
    }

    if args.check {
        println!("+ {added} added, - {removed} removed, ~ {modified} modified");
        if added > 0 || removed > 0 {
            std::process::exit(EXIT_ADDED_OR_REMOVED);
        }
        if modified > 0 {
            std::process::exit(EXIT_MODIFIED);
        }
    } else if added == 0 && removed == 0 && modified == 0 && !args.quiet {
        println!("No differences found.");
    }
    Ok(())
}

/// Every file under `dir` (recursively), relative to `base`.
fn collect_files(base: &Utf8Path, dir: &Utf8Path, out: &mut BTreeSet<String>) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir.as_std_path()) else {
        return Ok(());
    };
    for entry in entries {
        let path = entry.into_diagnostic()?.path();
        let Some(path) = Utf8Path::from_path(&path) else {
            continue;
        };
        if path.is_dir() {
            collect_files(base, path, out)?;
        } else {
            out.insert(relative_path(base, path));
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
