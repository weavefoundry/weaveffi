use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Recursively read every file under `dir` and concatenate the text contents.
fn read_tree(dir: &Path) -> String {
    let mut out = String::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.push_str(&read_tree(&path));
        } else if let Ok(contents) = fs::read_to_string(&path) {
            out.push_str(&contents);
            out.push('\n');
        }
    }
    out
}

/// The C ABI prefix must reach **every** language backend, not just C/C++.
/// Before the unified `c_prefix` plumbing, nine of the eleven generators
/// hard-coded `weaveffi_`, so a custom prefix produced consumer code that
/// linked against symbols the (re-prefixed) producer never exported. This
/// test is the toolchain-free regression oracle for that bug: it generates
/// all targets with `[global] c_prefix = "myffi"` and asserts each one emits
/// the prefixed user symbol `myffi_calculator_add` and never the
/// default-prefixed `weaveffi_calculator_add`.
#[test]
fn custom_c_prefix_propagates_to_all_targets() {
    let repo_root = repo_root();
    let input = repo_root.join("samples/calculator/calculator.yml");

    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let out = dir.path().join("out");
    let cfg = dir.path().join("cfg.toml");
    fs::write(&cfg, "[global]\nc_prefix = \"myffi\"\n").expect("failed to write cfg.toml");

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Every backend's output directory. The C symbol `calculator_add` is a
    // plain sync function present in the calculator sample, so every backend
    // that binds to it must reference the prefixed form.
    for target in [
        "c", "cpp", "swift", "android", "node", "wasm", "python", "dotnet", "dart", "go", "ruby",
    ] {
        let target_dir = out.join(target);
        assert!(
            target_dir.is_dir(),
            "target `{target}` produced no output directory at {}",
            target_dir.display()
        );
        let tree = read_tree(&target_dir);
        assert!(
            tree.contains("myffi_calculator_add"),
            "target `{target}` did not honor the custom c_prefix \
             (missing user symbol `myffi_calculator_add`)"
        );
        assert!(
            !tree.contains("weaveffi_calculator_add"),
            "target `{target}` leaked the default-prefixed user symbol \
             `weaveffi_calculator_add` despite a custom c_prefix"
        );
    }
}

#[test]
fn custom_c_prefix_propagates() {
    let repo_root = repo_root();
    let input = repo_root.join("samples/calculator/calculator.yml");

    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let out = dir.path().join("out");
    let cfg = dir.path().join("cfg.toml");
    fs::write(&cfg, "[c]\nprefix = \"myffi\"\n").expect("failed to write cfg.toml");

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--target",
            "c,cpp",
            "--config",
            cfg.to_str().unwrap(),
        ])
        .assert()
        .success();

    let header_path = out.join("c/myffi.h");
    assert!(
        header_path.exists(),
        "expected c/myffi.h to exist at {}",
        header_path.display()
    );
    let header = fs::read_to_string(&header_path).expect("missing c/myffi.h");

    assert!(
        header.contains("#define myffi_error weaveffi_error"),
        "header missing #define myffi_error alias:\n{header}"
    );
    assert!(
        header.contains("#define myffi_handle_t weaveffi_handle_t"),
        "header missing #define myffi_handle_t alias:\n{header}"
    );
    assert!(
        header.contains("#define myffi_free_string weaveffi_free_string"),
        "header missing #define myffi_free_string alias:\n{header}"
    );
    assert!(
        header.contains("#define myffi_cancel_token_create weaveffi_cancel_token_create"),
        "header missing #define myffi_cancel_token_create alias:\n{header}"
    );

    assert!(
        header.contains("myffi_calculator_add"),
        "header should declare prefixed user fn myffi_calculator_add:\n{header}"
    );
    assert!(
        !header.contains("weaveffi_calculator_add"),
        "header should not contain default-prefixed user fn:\n{header}"
    );

    let cpp_header_path = out.join("cpp/weaveffi.hpp");
    let cpp = fs::read_to_string(&cpp_header_path).expect("missing cpp/weaveffi.hpp");
    assert!(
        cpp.contains("extern \"C\" {"),
        "C++ header missing extern \"C\" block:\n{cpp}"
    );
    assert!(
        cpp.contains("myffi_calculator_add"),
        "C++ extern \"C\" block should reference prefixed C symbol myffi_calculator_add:\n{cpp}"
    );
    assert!(
        !cpp.contains("weaveffi_calculator_add"),
        "C++ extern \"C\" block should not retain default-prefixed user fn:\n{cpp}"
    );
    assert!(
        cpp.contains("#define myffi_error weaveffi_error"),
        "C++ header should also alias runtime symbols to the custom prefix:\n{cpp}"
    );
}
