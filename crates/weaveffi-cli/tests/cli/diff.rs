//! `weaveffi generate --diff`: the unified diff of what regenerating would
//! change, without writing anything.

use std::path::Path;

use crate::weaveffi;

fn sample(name: &str) -> std::path::PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    repo_root.join("samples").join(name)
}

#[test]
fn diff_against_an_empty_dir_adds_every_file_and_writes_nothing() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let empty_out = tmp.path().join("empty");
    std::fs::create_dir_all(&empty_out).unwrap();

    let output = weaveffi()
        .args(["generate", "--diff", "--target", "c"])
        .arg(sample("calculator"))
        .arg("--out")
        .arg(&empty_out)
        .output()
        .expect("failed to run weaveffi generate --diff");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "generate --diff failed: {stdout}");
    assert!(
        stdout.contains("--- /dev/null\n+++ b/c/calculator.h\n"),
        "{stdout}"
    );
    assert!(
        std::fs::read_dir(&empty_out).unwrap().next().is_none(),
        "--diff must not write"
    );
}

#[test]
fn diff_is_empty_when_nothing_changed() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = tmp.path().join("generated");

    weaveffi()
        .arg("generate")
        .arg(sample("calculator"))
        .arg("-o")
        .arg(&out_path)
        .assert()
        .success();

    let edited = out_path.join("c/calculator.h");
    let original = std::fs::read_to_string(&edited).unwrap();
    std::fs::write(&edited, format!("{original}// edited\n")).unwrap();
    let output = weaveffi()
        .args(["generate", "--diff"])
        .arg(sample("calculator"))
        .arg("--out")
        .arg(&out_path)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.starts_with("--- a/c/calculator.h\n+++ b/c/calculator.h\n")
            && stdout.contains("-// edited\n"),
        "{stdout}"
    );
    assert!(std::fs::read_to_string(&edited)
        .unwrap()
        .ends_with("// edited\n"));

    std::fs::write(&edited, original).unwrap();
    weaveffi()
        .args(["generate", "--diff"])
        .arg(sample("calculator"))
        .arg("--out")
        .arg(&out_path)
        .assert()
        .success()
        .stdout("");
}

/// `generate --check` applies the `[generators.*]` tables of the sample's
/// `weaveffi.toml` the same way `generate` does, so a freshly generated
/// tree with custom naming checks clean.
#[test]
fn check_honors_generator_tables() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = tmp.path().join("generated");

    weaveffi()
        .arg("generate")
        .arg(sample("kvstore"))
        .arg("-o")
        .arg(&out_path)
        .assert()
        .success();

    weaveffi()
        .args(["generate", "--check"])
        .arg(sample("kvstore"))
        .arg("--out")
        .arg(&out_path)
        .assert()
        .success()
        .stdout("");
}
