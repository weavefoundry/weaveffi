//! Integration tests for the global `--format json` flag. Each subcommand
//! must emit a stable JSON shape on stdout so IDE / CI tooling can parse the
//! output without regex-scraping human-readable text.

use std::io::Write;
use std::path::Path;

fn calculator_sample() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("samples/calculator/calculator.yml")
        .to_string_lossy()
        .into_owned()
}

fn cargo_bin() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found")
}

fn minimal_api_yml(dir: &Path) -> std::path::PathBuf {
    let yml = dir.join("api.yml");
    std::fs::write(
        &yml,
        concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: math\n",
            "    functions:\n",
            "      - name: add\n",
            "        doc: adds two numbers\n",
            "        params:\n",
            "          - { name: a, type: i32 }\n",
            "          - { name: b, type: i32 }\n",
            "        return: i32\n",
        ),
    )
    .unwrap();
    yml
}

#[test]
fn validate_json_success() {
    let sample = calculator_sample();
    let out = cargo_bin()
        .args(["--format", "json", "validate", &sample])
        .output()
        .expect("failed to run weaveffi validate");

    assert!(
        out.status.success(),
        "validate failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("validate JSON output is not valid JSON");

    assert_eq!(parsed["ok"], serde_json::Value::Bool(true));
    assert!(parsed["modules"].as_u64().unwrap() >= 1);
    assert!(parsed["functions"].as_u64().unwrap() >= 1);
    assert!(parsed["structs"].is_number());
    assert!(parsed["enums"].is_number());
    assert!(parsed["warnings"].is_array());
}

#[test]
fn validate_json_failure_returns_errors_and_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let yml_path = dir.path().join("bad.yml");
    std::fs::write(
        &yml_path,
        concat!(
            "version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: \"\"\n",
            "    functions: []\n",
        ),
    )
    .unwrap();

    let out = cargo_bin()
        .args(["--format", "json", "validate", yml_path.to_str().unwrap()])
        .output()
        .expect("failed to run weaveffi validate");

    assert!(
        !out.status.success(),
        "validate must exit non-zero on failure (stdout: {}, stderr: {})",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("validate JSON output is not valid JSON");

    assert_eq!(parsed["ok"], serde_json::Value::Bool(false));
    let errors = parsed["errors"].as_array().expect("errors must be array");
    assert!(!errors.is_empty(), "at least one error expected");
    let first = &errors[0];
    assert!(first["message"].is_string());
    assert!(first["message"].as_str().unwrap().contains("module"));
    assert!(first.get("location").is_some());
    assert!(first.get("suggestion").is_some());
}

#[test]
fn lint_json_clean_returns_empty_array() {
    let sample = calculator_sample();
    let out = cargo_bin()
        .args(["--format", "json", "lint", &sample])
        .output()
        .expect("failed to run weaveffi lint");

    assert!(
        out.status.success(),
        "lint clean sample must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("lint JSON output is not valid JSON");
    let arr = parsed.as_array().expect("lint JSON must be an array");
    assert!(arr.is_empty(), "expected empty warnings array, got {arr:?}");
}

#[test]
fn lint_json_with_warnings_lists_messages() {
    let dir = tempfile::tempdir().unwrap();
    let yml = dir.path().join("nodocs.yml");
    {
        let mut f = std::fs::File::create(&yml).unwrap();
        f.write_all(
            concat!(
                "version: \"0.1.0\"\n",
                "modules:\n",
                "  - name: nodocs\n",
                "    functions:\n",
                "      - name: do_stuff\n",
                "        params: []\n",
            )
            .as_bytes(),
        )
        .unwrap();
    }

    let out = cargo_bin()
        .args(["--format", "json", "lint", yml.to_str().unwrap()])
        .output()
        .expect("failed to run weaveffi lint");

    assert!(
        !out.status.success(),
        "lint must exit non-zero when warnings exist"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("lint JSON output is not valid JSON");
    let arr = parsed.as_array().expect("lint JSON must be an array");
    assert!(
        arr.iter()
            .any(|v| v.as_str().map(|s| s.contains("no doc")).unwrap_or(false)),
        "expected a 'no doc' warning in array: {arr:?}"
    );
}

#[test]
fn diff_json_shows_added_files_against_empty_out() {
    let sample = calculator_sample();
    let dir = tempfile::tempdir().unwrap();
    let empty_out = dir.path().join("empty");
    std::fs::create_dir_all(&empty_out).unwrap();

    let out = cargo_bin()
        .args([
            "--format",
            "json",
            "diff",
            &sample,
            "--out",
            empty_out.to_str().unwrap(),
            "--no-exit-code",
        ])
        .output()
        .expect("failed to run weaveffi diff");

    assert!(
        out.status.success(),
        "diff failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("diff JSON output is not valid JSON");
    let arr = parsed.as_array().expect("diff JSON must be an array");
    assert!(
        !arr.is_empty(),
        "expected non-empty diff against empty output directory"
    );
    for entry in arr {
        assert!(entry["path"].is_string());
        assert_eq!(entry["status"], serde_json::Value::String("added".into()));
        assert!(entry["patch"].is_string());
    }
}

#[test]
fn diff_json_exits_nonzero_when_changes_without_no_exit_code() {
    let sample = calculator_sample();
    let dir = tempfile::tempdir().unwrap();
    let empty_out = dir.path().join("empty");
    std::fs::create_dir_all(&empty_out).unwrap();

    let out = cargo_bin()
        .args([
            "--format",
            "json",
            "diff",
            &sample,
            "--out",
            empty_out.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run weaveffi diff");

    assert!(
        !out.status.success(),
        "diff must exit non-zero when diffs exist (stdout: {})",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("diff JSON output is not valid JSON");
    assert!(
        parsed.is_array(),
        "diff JSON must still be emitted on exit 1"
    );
}

#[test]
fn doctor_json_emits_per_check_array() {
    let out = cargo_bin()
        .args(["--format", "json", "doctor"])
        .output()
        .expect("failed to run weaveffi doctor");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("doctor JSON output is not valid JSON");
    let arr = parsed.as_array().expect("doctor JSON must be an array");
    assert!(!arr.is_empty(), "doctor should report at least one check");
    for check in arr {
        assert!(check["name"].is_string(), "missing name in {check}");
        assert!(check["ok"].is_boolean(), "missing ok bool in {check}");
        assert!(
            check.get("version").is_some(),
            "missing version key in {check}"
        );
        assert!(check.get("hint").is_some(), "missing hint key in {check}");
    }
    assert!(
        arr.iter().any(|c| c["name"] == "rustc"),
        "doctor JSON should include rustc check"
    );
    assert!(
        arr.iter().any(|c| c["name"] == "weaveffi-cli"),
        "doctor JSON should include weaveffi-cli check"
    );
}

#[test]
fn dry_run_json_emits_array_of_files() {
    let dir = tempfile::tempdir().unwrap();
    let yml = minimal_api_yml(dir.path());
    let out_dir = dir.path().join("out");

    let out = cargo_bin()
        .args([
            "--format",
            "json",
            "generate",
            yml.to_str().unwrap(),
            "-o",
            out_dir.to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .expect("failed to run weaveffi generate --dry-run");

    assert!(
        out.status.success(),
        "generate --dry-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out_dir.exists(),
        "--dry-run must not create the output directory"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("dry-run JSON output is not valid JSON");
    let arr = parsed.as_array().expect("dry-run JSON must be an array");
    assert!(
        arr.iter().all(|v| v.is_string()),
        "dry-run JSON entries must all be strings: {arr:?}"
    );
    assert!(
        arr.iter()
            .any(|v| v.as_str().unwrap().contains("c/weaveffi.h")),
        "dry-run JSON should list c/weaveffi.h: {arr:?}"
    );
}

#[test]
fn extract_json_via_global_format_flag() {
    let dir = tempfile::tempdir().unwrap();
    let src_path = dir.path().join("lib.rs");
    {
        let mut f = std::fs::File::create(&src_path).unwrap();
        write!(
            f,
            r#"
mod math {{
    #[weaveffi_export]
    fn add(a: i32, b: i32) -> i32 {{
        a + b
    }}
}}
"#
        )
        .unwrap();
    }

    let out = cargo_bin()
        .args(["--format", "json", "extract", src_path.to_str().unwrap()])
        .output()
        .expect("failed to run weaveffi extract");

    assert!(
        out.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("extract JSON output is not valid JSON");
    let modules = parsed["modules"].as_array().expect("missing modules array");
    assert_eq!(modules[0]["name"], serde_json::Value::String("math".into()));
}
