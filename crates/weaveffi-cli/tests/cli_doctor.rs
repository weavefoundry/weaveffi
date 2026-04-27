//! Integration tests for the `weaveffi doctor` subcommand. These exercise
//! both the human-readable and `--format json` outputs as well as the
//! `--target` filter that lets users (and CI) ask about a single language
//! toolchain at a time.

fn cargo_bin() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found")
}

/// `weaveffi doctor --format json` should write a syntactically valid JSON
/// array to stdout, with each element shaped like the documented
/// `DoctorCheck` struct (`id`, `name`, `ok`, `version`, `hint`,
/// `applies_to`). The required `rustc` and `cargo` checks must always be
/// present, and every `applies_to` entry must be a string.
#[test]
fn doctor_json_outputs_valid_json() {
    let output = cargo_bin()
        .args(["doctor", "--format", "json"])
        .output()
        .expect("failed to run weaveffi doctor --format json");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --format json produced invalid JSON: {e}\n{stdout}"));

    let arr = value
        .as_array()
        .expect("doctor --format json should produce a top-level array");
    assert!(!arr.is_empty(), "doctor checks should not be empty");

    let mut ids = Vec::new();
    for check in arr {
        let obj = check
            .as_object()
            .expect("each check should be a JSON object");

        for field in ["id", "name", "ok", "version", "hint", "applies_to"] {
            assert!(
                obj.contains_key(field),
                "check is missing field '{field}': {check}"
            );
        }

        assert!(obj["id"].is_string(), "id should be a string: {check}");
        assert!(obj["name"].is_string(), "name should be a string: {check}");
        assert!(obj["ok"].is_boolean(), "ok should be a bool: {check}");

        let applies_to = obj["applies_to"]
            .as_array()
            .expect("applies_to should be an array");
        for entry in applies_to {
            assert!(
                entry.is_string(),
                "applies_to entries should be strings: {check}"
            );
        }

        ids.push(obj["id"].as_str().unwrap().to_string());
    }

    for required in ["rustc", "cargo"] {
        assert!(
            ids.iter().any(|id| id == required),
            "doctor JSON output should always include '{required}' check, got ids: {ids:?}"
        );
    }
}

/// The default human-readable output should mention every language
/// toolchain WeaveFFI generates code for so users can scan it and see
/// what's installed. We also re-assert the existing section markers so a
/// future refactor can't silently drop them.
#[test]
fn doctor_human_readable_lists_all_targets() {
    let output = cargo_bin()
        .arg("doctor")
        .output()
        .expect("failed to run weaveffi doctor");

    assert!(
        output.status.success(),
        "doctor (no --target) should always exit 0, got {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    let expected_sections = [
        "Required toolchain:",
        "Node.js:",
        "Cross-compilation targets:",
        "WebAssembly tools:",
        "C++:",
        "Dart:",
        "Go:",
        "Ruby:",
        ".NET:",
        "Python:",
    ];
    for section in expected_sections {
        assert!(
            stdout.contains(section),
            "doctor human output should contain section header '{section}', got:\n{stdout}"
        );
    }

    let expected_labels = [
        "Rust compiler",
        "CMake",
        "Dart SDK",
        "Flutter (optional)",
        "Go",
        "Ruby",
        "RubyGems",
        "Bundler",
        ".NET SDK",
        "Python 3",
        "Python ctypes module",
    ];
    for label in expected_labels {
        assert!(
            stdout.contains(label),
            "doctor human output should mention '{label}', got:\n{stdout}"
        );
    }
}

/// `--target dart` should restrict the check list to checks whose
/// `applies_to` contains either `dart` or the wildcard `*` (which marks
/// the always-run Rust toolchain). The result must be strictly smaller
/// than the unfiltered list and must never include unrelated checks like
/// the Node or Ruby ones.
#[test]
fn doctor_target_filter_runs_subset() {
    let full = cargo_bin()
        .args(["doctor", "--format", "json"])
        .output()
        .expect("failed to run unfiltered doctor");
    let full_stdout = String::from_utf8_lossy(&full.stdout);
    let full_value: serde_json::Value =
        serde_json::from_str(&full_stdout).expect("full doctor JSON should parse");
    let full_len = full_value.as_array().unwrap().len();

    let filtered = cargo_bin()
        .args(["doctor", "--target", "dart", "--format", "json"])
        .output()
        .expect("failed to run --target dart doctor");
    let filtered_stdout = String::from_utf8_lossy(&filtered.stdout);
    let filtered_value: serde_json::Value =
        serde_json::from_str(&filtered_stdout).expect("filtered doctor JSON should parse");
    let filtered_arr = filtered_value
        .as_array()
        .expect("filtered output should be an array");

    assert!(
        filtered_arr.len() < full_len,
        "filtered output ({}) should have fewer checks than full output ({})",
        filtered_arr.len(),
        full_len
    );

    for check in filtered_arr {
        let applies_to: Vec<&str> = check["applies_to"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            applies_to.contains(&"dart") || applies_to.contains(&"*"),
            "check {check} should apply to dart or be a wildcard"
        );
    }

    let ids: Vec<&str> = filtered_arr
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    for unrelated in ["node", "npm", "ruby", "gem", "dotnet", "go"] {
        assert!(
            !ids.contains(&unrelated),
            "--target dart should not include '{unrelated}', got ids: {ids:?}"
        );
    }
    assert!(
        ids.contains(&"dart"),
        "--target dart should include the dart check, got ids: {ids:?}"
    );
    assert!(
        ids.contains(&"rustc") && ids.contains(&"cargo"),
        "--target dart should include the always-run rustc/cargo checks, got ids: {ids:?}"
    );
}
