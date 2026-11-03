//! The target registry is the one list of targets: every place outside the
//! crate that enumerates them (the CI matrices and the `xtask` fixture and
//! conformance drivers) must list exactly the registered targets.

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
fn matrix_values(matrix: &serde_json::Value) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    for key in ["target", "lang"] {
        if let Some(list) = matrix.get(key).and_then(|v| v.as_array()) {
            values.extend(list.iter().filter_map(|v| v.as_str()).map(str::to_string));
        }
        if let Some(include) = matrix.get("include").and_then(|v| v.as_array()) {
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
        let doc: serde_json::Value = serde_saphyr::from_str(&text).unwrap();
        let Some(jobs) = doc.get("jobs").and_then(|j| j.as_object()) else {
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
                    name,
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

/// The string literals of the `const {name}: [&str; N] = [...]` array in
/// `xtask/src/main.rs`.
fn xtask_list(name: &str) -> BTreeSet<String> {
    let path = repo_root().join("xtask/src/main.rs");
    let text = std::fs::read_to_string(&path).unwrap();
    let start = text
        .find(&format!("const {name}: [&str;"))
        .unwrap_or_else(|| panic!("no `{name}` in xtask/src/main.rs"));
    let body = &text[start..];
    let body = &body[body.find("= [").expect("an array") + 3..];
    let body = &body[..body.find("];").expect("the array's end")];
    body.split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn xtask_lists_every_target() {
    assert_eq!(
        xtask_list("TARGETS"),
        registry(),
        "xtask's TARGETS (fixture checks)"
    );
    assert_eq!(
        xtask_list("LANGS"),
        registry(),
        "xtask's LANGS (conformance)"
    );
}
