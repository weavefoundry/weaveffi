use std::fs;
use std::path::Path;

#[test]
fn custom_c_prefix_propagates() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/calculator/calculator.yml");

    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let out = dir.path().join("out");
    let cfg = dir.path().join("cfg.toml");
    fs::write(&cfg, "c_prefix = \"myffi\"\n").expect("failed to write cfg.toml");

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
