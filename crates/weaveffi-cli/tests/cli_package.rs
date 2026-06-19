use std::path::Path;

/// Lay out a fake `--binaries` tree (`<dir>/<platform>/<lib>`) covering the full
/// v1 matrix, with placeholder library bytes so packaging has something to copy.
fn write_prebuilt(root: &Path, lib_base: &str) {
    let entries = [
        ("darwin-arm64", format!("lib{lib_base}.dylib")),
        ("darwin-x64", format!("lib{lib_base}.dylib")),
        ("linux-x64", format!("lib{lib_base}.so")),
        ("linux-arm64", format!("lib{lib_base}.so")),
        ("windows-x64", format!("{lib_base}.dll")),
    ];
    for (platform, lib) in entries {
        let dir = root.join(platform);
        std::fs::create_dir_all(&dir).expect("create platform dir");
        std::fs::write(dir.join(&lib), b"\x00fake-native\x01").expect("write fake lib");
    }
}

#[test]
fn package_bundles_native_libraries_per_ecosystem() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/contacts/contacts.yml");

    let bins = tempfile::tempdir().expect("temp bins dir");
    write_prebuilt(bins.path(), "contacts");

    let out_dir = tempfile::tempdir().expect("temp out dir");
    let out_path = out_dir.path();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "package",
            input.to_str().unwrap(),
            "--binaries",
            bins.path().to_str().unwrap(),
            "--target",
            "dotnet,python,node",
            "-o",
            out_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // .NET: NuGet runtimes/<rid>/native/ layout.
    assert!(
        out_path
            .join("dotnet/runtimes/osx-arm64/native/libcontacts.dylib")
            .exists(),
        "missing macOS arm64 runtime library"
    );
    assert!(
        out_path
            .join("dotnet/runtimes/win-x64/native/contacts.dll")
            .exists(),
        "missing Windows runtime library"
    );

    // Python: a per-platform wheel tree with the library bundled in-package.
    assert!(
        out_path
            .join("python/linux-x64/contacts/libcontacts.so")
            .exists(),
        "missing bundled Linux library in the Python package"
    );

    // Node: optionalDependencies plus per-platform packages bundling the lib.
    let node_pkg = std::fs::read_to_string(out_path.join("node/package.json"))
        .expect("missing node/package.json");
    assert!(
        node_pkg.contains("\"optionalDependencies\""),
        "node package.json should declare optionalDependencies"
    );
    assert!(
        out_path
            .join("node/npm/contacts-darwin-arm64/libcontacts.dylib")
            .exists(),
        "missing per-platform npm package library"
    );
}

#[test]
fn package_skips_unsupported_targets() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/contacts/contacts.yml");

    let bins = tempfile::tempdir().expect("temp bins dir");
    write_prebuilt(bins.path(), "contacts");

    let out_dir = tempfile::tempdir().expect("temp out dir");

    // wasm has no native-matrix packaging, so packaging only wasm fails with a
    // clear message rather than producing an empty artifact.
    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "package",
            input.to_str().unwrap(),
            "--binaries",
            bins.path().to_str().unwrap(),
            "--target",
            "wasm",
            "-o",
            out_dir.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
}

#[test]
fn package_requires_a_binary_source() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/contacts/contacts.yml");
    let out_dir = tempfile::tempdir().expect("temp out dir");

    // Neither --binaries nor --build: nothing to bundle.
    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "package",
            input.to_str().unwrap(),
            "-o",
            out_dir.path().to_str().unwrap(),
        ])
        .assert()
        .failure();
}
