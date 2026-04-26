//! End-to-end `--dry-run` parity test for `GeneratorConfig`.
//!
//! Pins the contract that `weaveffi generate --dry-run` prints the *exact*
//! set of file paths that a real (non-dry) run would write, even when the
//! user customises every string field of [`GeneratorConfig`]. A mismatch
//! means `output_files_with_config` lies for some generator and breaks
//! tooling that relies on the dry-run list (shell pipelines, CI checks,
//! IDE integrations).

use std::collections::BTreeSet;

const API_YML: &str = "version: \"0.1.0\"
modules:
  - name: calculator
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
";

const CONFIG_TOML: &str = "swift_module_name = \"MySwift\"
android_package = \"org.example.myapp\"
node_package_name = \"@myorg/cool-lib\"
wasm_module_name = \"my_wasm\"
c_prefix = \"mylib\"
python_package_name = \"my_python_pkg\"
dotnet_namespace = \"MyCompany.Bindings\"
cpp_namespace = \"mylib\"
cpp_header_name = \"mylib.hpp\"
cpp_standard = \"20\"
dart_package_name = \"my_dart_pkg\"
go_module_path = \"github.com/myorg/mylib\"
ruby_module_name = \"MyRubyMod\"
ruby_gem_name = \"my_ruby_gem\"
strip_module_prefix = true
template_dir = \"templates\"
pre_generate = \"echo pre-hook-ok\"
post_generate = \"echo post-hook-ok\"
";

fn walk_files(base: &std::path::Path, dir: &std::path::Path, out: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).expect("read_dir failed") {
        let entry = entry.expect("read entry");
        let path = entry.path();
        if path.is_dir() {
            walk_files(base, &path, out);
        } else {
            let rel = path.strip_prefix(base).expect("strip_prefix");
            let rel_str = rel
                .to_str()
                .expect("non-UTF-8 path")
                .replace(std::path::MAIN_SEPARATOR, "/");
            if rel_str != ".weaveffi-cache" && rel_str != "weaveffi.lock" {
                out.insert(rel_str);
            }
        }
    }
}

#[test]
fn dry_run_paths_match_real_outputs_with_full_custom_config() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let api_path = dir.path().join("api.yml");
    std::fs::write(&api_path, API_YML).expect("failed to write api.yml");
    let config_path = dir.path().join("weaveffi.toml");
    std::fs::write(&config_path, CONFIG_TOML).expect("failed to write weaveffi.toml");

    let out_path = dir.path().join("out");

    let dry = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            api_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .expect("failed to run weaveffi generate --dry-run");

    assert!(
        dry.status.success(),
        "--dry-run failed: {}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert!(
        !out_path.exists(),
        "--dry-run must not create the output directory"
    );

    let dry_stdout = String::from_utf8(dry.stdout).expect("dry-run stdout not UTF-8");
    let out_prefix = format!("{}/", out_path.to_str().unwrap());
    let dry_paths: BTreeSet<String> = dry_stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.strip_prefix(&out_prefix)
                .unwrap_or_else(|| panic!("dry-run path {l} outside of out dir {out_prefix}"))
                .to_string()
        })
        .collect();

    let gen = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            api_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run weaveffi generate");

    assert!(
        gen.status.success(),
        "generate failed: {}",
        String::from_utf8_lossy(&gen.stderr)
    );

    let mut real_paths: BTreeSet<String> = BTreeSet::new();
    walk_files(&out_path, &out_path, &mut real_paths);

    let missing_from_dry: Vec<_> = real_paths.difference(&dry_paths).collect();
    let extra_in_dry: Vec<_> = dry_paths.difference(&real_paths).collect();

    assert!(
        missing_from_dry.is_empty() && extra_in_dry.is_empty(),
        "--dry-run paths must match real outputs exactly\n\
         files written but not listed by --dry-run: {:#?}\n\
         files listed by --dry-run but not written: {:#?}",
        missing_from_dry,
        extra_in_dry,
    );
}
