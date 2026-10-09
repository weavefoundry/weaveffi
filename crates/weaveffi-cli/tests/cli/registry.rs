//! The target registry is the one list of targets: every place outside the
//! crate that enumerates them (the CI matrices, the fixture compile check,
//! and the conformance driver) must list exactly the registered targets.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use weaveffi_cli::targets;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn registry() -> BTreeSet<String> {
    targets::names().map(str::to_string).collect()
}

/// The values of the `target` and `lang` matrix keys of one job, from a
/// plain list or from `include` entries.
fn matrix_values(matrix: &serde_yaml_ng::Value) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    for key in ["target", "lang"] {
        if let Some(list) = matrix.get(key).and_then(|v| v.as_sequence()) {
            values.extend(list.iter().filter_map(|v| v.as_str()).map(str::to_string));
        }
        if let Some(include) = matrix.get("include").and_then(|v| v.as_sequence()) {
            values.extend(
                include
                    .iter()
                    .filter_map(|entry| entry.get(key)?.as_str())
                    .map(str::to_string),
            );
        }
    }
    values
}

/// Every job matrix that lists language targets (any value is a registered
/// target name; fuzz and release-triple matrices aren't) lists them all.
#[test]
fn workflow_matrices_list_every_target() {
    let registry = registry();
    let mut checked = 0;
    let dir = repo_root().join(".github/workflows");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("yml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let doc: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).unwrap();
        let Some(jobs) = doc.get("jobs").and_then(|j| j.as_mapping()) else {
            continue;
        };
        for (name, job) in jobs {
            let Some(matrix) = job.get("strategy").and_then(|s| s.get("matrix")) else {
                continue;
            };
            let values = matrix_values(matrix);
            if values.iter().any(|v| registry.contains(v)) {
                assert_eq!(
                    values,
                    registry,
                    "the {:?} matrix in {} must list exactly the registered targets",
                    name.as_str().unwrap_or_default(),
                    path.display()
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 2,
        "expected the fixtures and conformance matrices"
    );
}

/// The words of the shell assignment starting with `prefix` in `script`,
/// up to the closing `quote`.
fn shell_list(script: &str, prefix: &str, quote: char) -> BTreeSet<String> {
    let path = repo_root().join(script);
    let text = std::fs::read_to_string(&path).unwrap();
    let line = text
        .lines()
        .find_map(|l| l.trim().strip_prefix(prefix))
        .unwrap_or_else(|| panic!("no `{prefix}` line in {script}"));
    let end = line.find(quote).unwrap_or(line.len());
    line[..end].split_whitespace().map(str::to_string).collect()
}

#[test]
fn scripts_list_every_target() {
    assert_eq!(
        shell_list("scripts/check-fixtures.sh", "TARGETS=${*:-", '}'),
        registry(),
        "scripts/check-fixtures.sh's default TARGETS"
    );
    assert_eq!(
        shell_list("conformance/run.sh", "LANGS=\"", '"'),
        registry(),
        "conformance/run.sh's LANGS"
    );
}
