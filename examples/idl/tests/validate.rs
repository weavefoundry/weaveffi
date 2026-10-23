//! Every IDL document the book includes is checked here, the way
//! `weaveffi validate` checks it: loaded with its imports, then validated
//! against an identity named after the file.

use std::path::{Path, PathBuf};

use weaveffi_model::parse::load_api;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::{validate, ValidationDiagnostics};

/// The documents that must validate (each with the files it imports).
const ACCEPTED: &[&str] = &["contacts.yml", "drawing/drawing.yml", "greeter/greeter.yml"];

/// Files only ever read as another document's import.
const IMPORTED: &[&str] = &["drawing/shapes.yml", "drawing/units.yml"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Load and validate `path`, as `weaveffi validate <path>` does.
fn check(path: &Path) -> Result<(), ValidationDiagnostics> {
    let (api, sources) = load_api(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let name = path.file_stem().unwrap().to_string_lossy();
    validate(&api, &Identity::named(&name), Some(&sources)).map(|_| ())
}

/// Every `.yml` file under `dir`, relative to the crate root.
fn documents(dir: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            documents(&path, out);
        } else if path.extension().is_some_and(|e| e == "yml") {
            let relative = path.strip_prefix(root()).unwrap();
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn accepted_documents_validate() {
    for file in ACCEPTED {
        if let Err(e) = check(&root().join(file)) {
            let all: Vec<String> = e.diagnostics.iter().map(ToString::to_string).collect();
            panic!("{file} doesn't validate: {all:#?}");
        }
    }
}

#[test]
fn rejected_documents_fail_with_their_code() {
    let mut files = Vec::new();
    documents(&root().join("rejected"), &mut files);
    assert!(!files.is_empty());
    for file in files {
        let path = root().join(&file);
        let text = std::fs::read_to_string(&path).unwrap();
        let expected = path.file_stem().unwrap().to_string_lossy().to_string();
        assert_eq!(
            text.lines().next(),
            Some(format!("# rejected: {expected}").as_str()),
            "{file} names the code it's rejected with on its first line"
        );
        let Err(e) = check(&path) else {
            panic!("{file} validates, but should be rejected with {expected}");
        };
        let codes: Vec<String> = e
            .diagnostics
            .iter()
            .map(|d| {
                serde_json::to_value(&d.error).unwrap()["code"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert!(
            codes.contains(&expected),
            "{file}: expected {expected}, got {codes:?}"
        );
    }
}

#[test]
fn every_document_is_checked() {
    let mut files = Vec::new();
    documents(&root(), &mut files);
    for file in files {
        assert!(
            ACCEPTED.contains(&file.as_str())
                || IMPORTED.contains(&file.as_str())
                || file.starts_with("rejected/"),
            "{file} isn't checked by any test; add it to ACCEPTED or IMPORTED"
        );
    }
}
