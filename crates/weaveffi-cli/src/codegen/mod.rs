//! Orchestration: rendering targets and turning their output into a
//! [`Changeset`] against an output directory.
//!
//! Rendering is pure: a [`Target`] returns its files in memory, with paths
//! relative to its own directory (`{out_dir}/{target}/`). The
//! [`Orchestrator`] renders every target, compares the result with the
//! output directory ([`Orchestrator::plan`]), and only then touches disk
//! ([`Changeset::apply`]). That one plan is what `weaveffi generate` writes,
//! what `generate --check` reports, and what `generate --diff` prints. A
//! plan rewrites only files whose contents changed and removes files a
//! previous run wrote that the current run no longer produces (see the
//! generation records in `.weaveffi-cache/`).
//!
//! The submodules are the **shared emitters** every target renders through
//! instead of keeping its own copy:
//!
//! - `codecs`: the value-buffer composites an API uses and their one
//!   canonical stem (`list_i32`, `opt_Item`).
//! - `contract`: the contract rows a consumer checks at load.
//! - `errors`: every error domain with its codes and target type names.
//! - `docs`: doc and deprecation text with backticked identifiers in the
//!   target's spelling.
//! - `common`: doc-comment emission, prose wrapping, and `PascalCase`.
//! - [`CodeWriter`]: the indentation-aware writer.
//!
//! None of them matches on a value's family or re-derives lowering: the
//! passing contracts (`ArgPass`, `RetPass`, `ErrorStrategy`, ...) stored on
//! the [`Model`] say how every value crosses.

use std::fmt::Write as _;

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use rayon::prelude::*;
use similar::TextDiff;

use crate::record::{self, Record};
use crate::targets::Target;
use weaveffi_model::model::Model;

pub(crate) mod codecs;
pub(crate) mod common;
pub(crate) mod contract;
pub(crate) mod docs;
pub(crate) mod errors;
mod writer;

pub use writer::CodeWriter;

/// A single generated file: its path relative to the target's directory and
/// its rendered contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    /// Path relative to the target's directory (`{out_dir}/{target}/`),
    /// with `/` separators.
    pub path: Utf8PathBuf,
    /// The rendered file contents.
    pub contents: String,
}

impl OutputFile {
    /// Pair a path, relative to the target's directory, with its rendered
    /// contents.
    ///
    /// The path is normalized to `/` separators, which every platform's file
    /// APIs accept, so listings, generation records, and tests see the same
    /// path on Windows as elsewhere.
    pub fn new(path: impl Into<Utf8PathBuf>, contents: impl Into<String>) -> Self {
        let path: Utf8PathBuf = path.into();
        let path = if path.as_str().contains('\\') {
            Utf8PathBuf::from(path.as_str().replace('\\', "/"))
        } else {
            path
        };
        Self {
            path,
            contents: contents.into(),
        }
    }
}

/// The path of a target's `file` relative to the output directory
/// (`{target}/{file}`), with `/` separators on every platform.
fn output_path(target: &str, file: &Utf8Path) -> Result<String> {
    let mut parts = vec![target];
    for component in file.components() {
        match component {
            Utf8Component::Normal(part) => parts.push(part),
            Utf8Component::CurDir => {}
            _ => {
                return Err(miette!(
                    "the {target} target rendered {file}, which isn't a path inside its directory"
                ))
            }
        }
    }
    Ok(parts.join("/"))
}

/// How applying a [`Changeset`] changes one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// The file doesn't exist yet.
    Added,
    /// The file exists with different contents.
    Modified,
    /// A previous generation wrote the file and this one doesn't.
    Removed,
}

impl ChangeKind {
    /// The one-character marker `generate --check` prints before a path.
    #[must_use]
    pub fn marker(self) -> char {
        match self {
            Self::Added => '+',
            Self::Modified => '~',
            Self::Removed => '-',
        }
    }
}

/// One planned file of one target.
#[derive(Debug, Clone)]
struct PlannedFile {
    /// Path relative to the output directory.
    path: String,
    contents: String,
    /// What's on disk now, when the file exists.
    old: Option<Vec<u8>>,
}

impl PlannedFile {
    fn change(&self) -> Option<ChangeKind> {
        match &self.old {
            None => Some(ChangeKind::Added),
            Some(old) if old != self.contents.as_bytes() => Some(ChangeKind::Modified),
            Some(_) => None,
        }
    }
}

/// One target's part of a [`Changeset`].
#[derive(Debug, Clone)]
struct TargetPlan {
    name: &'static str,
    files: Vec<PlannedFile>,
    /// Files the previous generation wrote that this one doesn't, still on
    /// disk, with their current contents.
    removed: Vec<(String, Vec<u8>)>,
    record: Record,
    previous: Option<Record>,
}

impl TargetPlan {
    fn changes_files(&self) -> bool {
        !self.removed.is_empty() || self.files.iter().any(|f| f.change().is_some())
    }

    fn is_current(&self) -> bool {
        !self.changes_files() && self.previous.as_ref() == Some(&self.record)
    }
}

/// Everything regenerating would do to an output directory, computed
/// without writing: which files would be added, modified, or removed.
#[derive(Debug, Clone)]
pub struct Changeset {
    out_dir: Utf8PathBuf,
    targets: Vec<TargetPlan>,
}

/// One file a [`Changeset`] would add, modify, or remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change<'a> {
    /// What happens to the file.
    pub kind: ChangeKind,
    /// The file's path relative to the output directory.
    pub path: &'a str,
}

impl Changeset {
    /// The output directory the plan was made against.
    #[must_use]
    pub fn out_dir(&self) -> &Utf8Path {
        &self.out_dir
    }

    /// Every file the targets render, relative to the output directory, in
    /// target order (what `generate --dry-run` lists).
    pub fn rendered(&self) -> impl Iterator<Item = &str> {
        self.targets
            .iter()
            .flat_map(|t| t.files.iter().map(|f| f.path.as_str()))
    }

    /// Every file applying the plan would add, modify, or remove, in target
    /// order.
    pub fn changes(&self) -> impl Iterator<Item = Change<'_>> {
        self.targets.iter().flat_map(|t| {
            let written = t.files.iter().filter_map(|f| {
                f.change().map(|kind| Change {
                    kind,
                    path: &f.path,
                })
            });
            let removed = t.removed.iter().map(|(path, _)| Change {
                kind: ChangeKind::Removed,
                path,
            });
            written.chain(removed)
        })
    }

    /// Whether applying the plan would leave every generated file as it is.
    /// (A missing generation record alone doesn't count as a change.)
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes().next().is_none()
    }

    /// A unified diff (`git diff` style, with `a/` and `b/` prefixes) of
    /// every change, with `/dev/null` standing in for added and removed
    /// files.
    #[must_use]
    pub fn unified_diff(&self) -> String {
        let mut out = String::new();
        for t in &self.targets {
            for f in &t.files {
                let old = match (&f.old, f.change()) {
                    (_, None) => continue,
                    (None, _) => None,
                    (Some(old), _) => Some(String::from_utf8_lossy(old)),
                };
                push_diff(&mut out, &f.path, old.as_deref(), Some(&f.contents));
            }
            for (path, old) in &t.removed {
                push_diff(&mut out, path, Some(&String::from_utf8_lossy(old)), None);
            }
        }
        out
    }

    /// Apply the plan: write every added or modified file, remove stale
    /// files, and update the generation records.
    ///
    /// # Errors
    ///
    /// Returns an error when a directory can't be created or a file can't
    /// be written or removed.
    pub fn apply(&self) -> Result<GenerateReport> {
        let mut report = GenerateReport::default();
        if self.targets.iter().all(TargetPlan::is_current) {
            report.up_to_date = self.targets.iter().map(|t| t.name).collect();
            return Ok(report);
        }
        std::fs::create_dir_all(self.out_dir.as_std_path())
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to create the output directory {}", self.out_dir))?;
        for t in &self.targets {
            let record_changed = t.previous.as_ref() != Some(&t.record);
            let mut changed = record_changed;
            for file in &t.files {
                if file.change().is_none() {
                    report.unchanged += 1;
                    continue;
                }
                let path = self.out_dir.join(&file.path);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent.as_std_path())
                        .into_diagnostic()
                        .wrap_err_with(|| format!("failed to create {parent}"))?;
                }
                std::fs::write(path.as_std_path(), &file.contents)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("failed to write {path}"))?;
                report.written += 1;
                changed = true;
            }
            for (rel, _) in &t.removed {
                let path = self.out_dir.join(rel);
                std::fs::remove_file(path.as_std_path())
                    .into_diagnostic()
                    .wrap_err_with(|| format!("failed to remove the stale file {path}"))?;
                report.removed.push(rel.clone());
                changed = true;
            }
            if record_changed {
                record::write_record(&self.out_dir, t.name, &t.record)?;
            }
            if changed {
                report.generated.push(t.name);
            } else {
                report.up_to_date.push(t.name);
            }
        }
        Ok(report)
    }
}

/// Append one file's unified diff to `out`.
fn push_diff(out: &mut String, path: &str, old: Option<&str>, new: Option<&str>) {
    let old_name = if old.is_some() {
        format!("a/{path}")
    } else {
        "/dev/null".to_string()
    };
    let new_name = if new.is_some() {
        format!("b/{path}")
    } else {
        "/dev/null".to_string()
    };
    let diff = TextDiff::from_lines(old.unwrap_or(""), new.unwrap_or(""));
    let _ = write!(
        out,
        "{}",
        diff.unified_diff()
            .context_radius(3)
            .header(&old_name, &new_name)
    );
}

/// What a generation run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GenerateReport {
    /// Targets that wrote or removed a file.
    pub generated: Vec<&'static str>,
    /// Targets whose output already matched the output directory.
    pub up_to_date: Vec<&'static str>,
    /// Files written because they were new or their contents changed.
    pub written: usize,
    /// Files rendered with the contents already on disk.
    pub unchanged: usize,
    /// Files removed because the previous run wrote them and this run did
    /// not (paths relative to the output directory).
    pub removed: Vec<String>,
}

impl GenerateReport {
    /// One line describing the run, for `weaveffi generate`'s output.
    #[must_use]
    pub fn summary(&self, out_dir: &Utf8Path) -> String {
        if self.generated.is_empty() {
            return format!(
                "{out_dir} is up to date ({} targets)",
                self.up_to_date.len()
            );
        }
        let mut line = format!(
            "Generated {} in {out_dir}: {} written, {} unchanged",
            self.generated.join(", "),
            self.written,
            self.unchanged
        );
        if !self.removed.is_empty() {
            let _ = write!(line, ", {} stale removed", self.removed.len());
        }
        if !self.up_to_date.is_empty() {
            let _ = write!(line, " ({} up to date)", self.up_to_date.join(", "));
        }
        line
    }
}

/// Runs a set of targets against one model and output directory.
#[derive(Default)]
pub struct Orchestrator<'a> {
    targets: Vec<&'a dyn Target>,
}

impl<'a> Orchestrator<'a> {
    /// An orchestrator with no targets.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a target.
    #[must_use]
    pub fn with_target(mut self, target: &'a dyn Target) -> Self {
        self.targets.push(target);
        self
    }

    /// Add several targets.
    #[must_use]
    pub fn with_targets(mut self, targets: impl IntoIterator<Item = &'a dyn Target>) -> Self {
        self.targets.extend(targets);
        self
    }

    /// Render every target (in parallel) and compare the result with
    /// `out_dir`, without writing anything.
    ///
    /// # Errors
    ///
    /// Returns an error when a target renders a path outside its directory.
    pub fn plan(&self, model: &Model, out_dir: &Utf8Path) -> Result<Changeset> {
        let targets = self
            .targets
            .par_iter()
            .map(|&target| plan_target(target, model, out_dir))
            .collect::<Result<Vec<_>>>()?;
        Ok(Changeset {
            out_dir: out_dir.to_path_buf(),
            targets,
        })
    }

    /// Generate every target under `out_dir`: [`plan`](Self::plan), then
    /// [`apply`](Changeset::apply).
    ///
    /// # Errors
    ///
    /// Returns the errors of [`plan`](Self::plan) and
    /// [`apply`](Changeset::apply).
    pub fn run(&self, model: &Model, out_dir: &Utf8Path) -> Result<GenerateReport> {
        self.plan(model, out_dir)?.apply()
    }
}

/// Render `target` and compare its files with `out_dir`.
fn plan_target(target: &dyn Target, model: &Model, out_dir: &Utf8Path) -> Result<TargetPlan> {
    let name = target.name();
    let files = target
        .render(model)
        .into_iter()
        .map(|file| {
            let path = output_path(name, &file.path)?;
            let old = std::fs::read(out_dir.join(&path).as_std_path()).ok();
            Ok(PlannedFile {
                path,
                contents: file.contents,
                old,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let record = Record {
        files: files.iter().map(|f| f.path.clone()).collect(),
    };
    let previous = record::read_record(out_dir, name);
    let removed = previous
        .iter()
        .flat_map(|p| p.files.iter())
        .filter(|rel| !record.files.contains(*rel))
        .filter_map(|rel| {
            let bytes = std::fs::read(out_dir.join(rel).as_std_path()).ok()?;
            Some((rel.clone(), bytes))
        })
        .collect();
    Ok(TargetPlan {
        name,
        files,
        removed,
        record,
        previous,
    })
}

/// Parse and validate a YAML IDL for a unit test, with identity `kv`.
#[cfg(test)]
pub(crate) fn test_model(yaml: &str) -> Model {
    let api = weaveffi_model::parse::parse_api_str(yaml, "yaml").expect("valid YAML");
    let identity = weaveffi_model::pkg::Identity::named("kv");
    weaveffi_model::validate::validate(&api, &identity, None).expect("valid API")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use weaveffi_model::ir::{Api, Module};

    struct Counting {
        name: &'static str,
        calls: Arc<AtomicUsize>,
        files: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Target for Counting {
        fn name(&self) -> &'static str {
            self.name
        }

        fn render(&self, model: &Model) -> Vec<OutputFile> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.files
                .lock()
                .unwrap()
                .iter()
                .map(|f| {
                    OutputFile::new(*f, format!("{} {}", model.prefix(), model.modules[0].name))
                })
                .collect()
        }
    }

    fn model_named(name: &str) -> Model {
        let api = Api {
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![Module {
                name: name.into(),
                doc: None,
                functions: vec![],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callback_interfaces: vec![],
                errors: Vec::new(),
                modules: vec![],
            }],
        };
        let identity = weaveffi_model::pkg::Identity::named("api");
        weaveffi_model::validate::validate(&api, &identity, None).unwrap()
    }

    fn counting(name: &'static str, calls: &Arc<AtomicUsize>) -> Counting {
        Counting {
            name,
            calls: Arc::clone(calls),
            files: Arc::new(Mutex::new(vec!["out.txt"])),
        }
    }

    #[test]
    fn every_run_renders_but_only_changed_files_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = counting("c", &calls);
        let orch = Orchestrator::new().with_target(&c);
        let model = model_named("math");

        let first = orch.run(&model, out).unwrap();
        assert_eq!((first.generated.len(), first.written), (1, 1));
        let rendered = std::fs::read_to_string(out.join("c/out.txt")).unwrap();
        let second = orch.run(&model, out).unwrap();
        assert_eq!(second.up_to_date, ["c"]);
        assert_eq!(second.written, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        std::fs::write(out.join("c/out.txt"), "edited").unwrap();
        let plan = orch.plan(&model, out).unwrap();
        let changes: Vec<_> = plan.changes().collect();
        assert_eq!(
            changes,
            [Change {
                kind: ChangeKind::Modified,
                path: "c/out.txt"
            }]
        );
        assert!(
            plan.unified_diff().contains("-edited\n"),
            "{}",
            plan.unified_diff()
        );
        let third = plan.apply().unwrap();
        assert_eq!((third.generated.len(), third.written), (1, 1));
        assert_eq!(
            std::fs::read_to_string(out.join("c/out.txt")).unwrap(),
            rendered
        );
        assert!(orch.plan(&model, out).unwrap().is_empty());
    }

    #[test]
    fn stale_files_are_removed_but_user_files_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = counting("c", &calls);
        let files = Arc::clone(&c.files);
        *files.lock().unwrap() = vec!["a.txt", "b.txt"];
        let orch = Orchestrator::new().with_target(&c);
        orch.run(&model_named("math"), out).unwrap();
        std::fs::write(out.join("c/user.txt"), "mine").unwrap();

        *files.lock().unwrap() = vec!["a.txt"];
        let plan = orch.plan(&model_named("math"), out).unwrap();
        assert_eq!(
            plan.changes().collect::<Vec<_>>(),
            [Change {
                kind: ChangeKind::Removed,
                path: "c/b.txt"
            }]
        );
        let report = plan.apply().unwrap();
        assert_eq!(report.removed, ["c/b.txt"]);
        assert_eq!(report.generated, ["c"]);
        assert!(out.join("c/a.txt").exists());
        assert!(!out.join("c/b.txt").exists());
        assert!(out.join("c/user.txt").exists());
    }

    #[test]
    fn output_paths_stay_inside_the_target_directory() {
        assert_eq!(
            output_path("c", &Utf8Path::new("include").join("x.h")).unwrap(),
            "c/include/x.h"
        );
        assert!(output_path("c", Utf8Path::new("../x.h")).is_err());
        assert!(output_path("c", Utf8Path::new("/x.h")).is_err());
    }
}
