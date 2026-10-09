//! `weaveffi build` and `weaveffi package` end to end: artifacts written
//! from a `--binaries` tree, and the error messages for the setups that
//! can't build or package.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::weaveffi;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// A 64-bit little-endian ELF shared-object header with no sections: a
/// Linux library that needs no glibc version beyond the 2.17 floor, which
/// is what the wheel's manylinux tag is read from.
fn minimal_elf() -> Vec<u8> {
    let mut elf = vec![0u8; 64];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = 2; // ELFCLASS64
    elf[5] = 1; // ELFDATA2LSB
    elf[6] = 1; // EV_CURRENT
    elf[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
    elf[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    elf[20..24].copy_from_slice(&1u32.to_le_bytes()); // e_version
    elf[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
    elf
}

/// Lay out a fake `--binaries` tree (`<dir>/<platform>/<lib>`) covering the
/// desktop matrix, with placeholder library bytes so packaging has something
/// to copy (a bare ELF header on Linux).
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
        let bytes = if platform.starts_with("linux") {
            minimal_elf()
        } else {
            b"\x00fake-native\x01".to_vec()
        };
        std::fs::write(dir.join(&lib), bytes).expect("write fake lib");
    }
    std::fs::write(
        root.join("darwin-arm64")
            .join(format!("{lib_base}_node.node")),
        b"addon",
    )
    .unwrap();
}

/// An IDL project for the calculator API (`calculator` 1.0.0) in `dir`,
/// returning its IDL: what a producer packaged from `--binaries` declares.
fn calculator_idl(dir: &Path) -> PathBuf {
    let idl = dir.join("calculator.yml");
    std::fs::write(
        &idl,
        concat!(
            "version: \"0.12.0\"\n",
            "modules:\n",
            "  - name: calculator\n",
            "    functions:\n",
            "      - { name: add, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }\n",
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("weaveffi.toml"),
        "[package]\nname = \"calculator\"\nversion = \"1.0.0\"\nlicense = \"MIT\"\n",
    )
    .unwrap();
    idl
}

/// The `(path, bytes)` entries of a gzipped tarball.
fn untar_gz(path: &Path) -> Vec<(String, Vec<u8>)> {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    archive
        .entries()
        .unwrap()
        .map(|e| {
            let mut e = e.unwrap();
            let name = e.path().unwrap().to_string_lossy().into_owned();
            let mut data = Vec::new();
            e.read_to_end(&mut data).unwrap();
            (name, data)
        })
        .collect()
}

#[test]
fn package_writes_installable_artifacts_from_binaries() {
    let project = tempfile::tempdir().expect("temp project dir");
    let input = calculator_idl(project.path());
    let bins = tempfile::tempdir().expect("temp bins dir");
    write_prebuilt(bins.path(), "calculator");
    let out = tempfile::tempdir().expect("temp out dir");
    let dist = out.path();

    let targets = ["python", "node", "ruby", "c", "go"];
    weaveffi()
        .args(["package", input.to_str().unwrap(), "--binaries"])
        .arg(bins.path())
        .args(["--target", &targets.join(","), "-o"])
        .arg(dist)
        .assert()
        .success();

    python_wheels(dist);
    node_tarballs(dist);
    ruby_gems(dist);

    // C: headers and every platform's library in one tarball.
    let c = untar_gz(&dist.join("c/calculator-1.0.0-c.tar.gz"));
    let names: Vec<&str> = c.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        names.contains(&"calculator-1.0.0/include/calculator.h"),
        "{names:?}"
    );
    assert!(
        names.contains(&"calculator-1.0.0/lib/windows-x64/calculator.dll"),
        "{names:?}"
    );

    go_module(dist);
}

/// Python: one platform-tagged wheel per desktop platform.
fn python_wheels(dist: &Path) {
    for wheel in [
        "calculator-1.0.0-py3-none-macosx_11_0_arm64.whl",
        "calculator-1.0.0-py3-none-macosx_11_0_x86_64.whl",
        "calculator-1.0.0-py3-none-manylinux_2_17_x86_64.whl",
        "calculator-1.0.0-py3-none-manylinux_2_17_aarch64.whl",
        "calculator-1.0.0-py3-none-win_amd64.whl",
    ] {
        assert!(dist.join("python").join(wheel).is_file(), "missing {wheel}");
    }
}

/// Node: per-platform tarballs with the library (and the prebuilt addon
/// where one was built), plus the main package.
fn node_tarballs(dist: &Path) {
    let mac = untar_gz(&dist.join("node/calculator-darwin-arm64-1.0.0.tgz"));
    let names: Vec<&str> = mac.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "package/package.json",
            "package/libcalculator.dylib",
            "package/calculator_node.node"
        ]
    );
    let linux = untar_gz(&dist.join("node/calculator-linux-x64-1.0.0.tgz"));
    assert_eq!(linux.len(), 2, "no addon was prebuilt for linux-x64");
    let main = untar_gz(&dist.join("node/calculator-1.0.0.tgz"));
    let package_json = main
        .iter()
        .find(|(n, _)| n == "package/package.json")
        .map(|(_, d)| String::from_utf8_lossy(d).into_owned())
        .expect("main package.json");
    assert!(
        package_json.contains("\"calculator-darwin-arm64\": \"1.0.0\""),
        "{package_json}"
    );
}

/// Ruby: one precompiled gem per desktop platform.
fn ruby_gems(dist: &Path) {
    for gem in [
        "calculator-1.0.0-arm64-darwin.gem",
        "calculator-1.0.0-x86_64-linux.gem",
        "calculator-1.0.0-x64-mingw-ucrt.gem",
    ] {
        assert!(dist.join("ruby").join(gem).is_file(), "missing {gem}");
    }
}

/// Go: the module directory with per-platform libraries and cgo flags.
fn go_module(dist: &Path) {
    let bindings = std::fs::read_to_string(dist.join("go/calculator/bindings.go")).unwrap();
    assert!(
        bindings.contains("#cgo linux,amd64 LDFLAGS: -L${SRCDIR}/lib/linux-x64"),
        "{bindings}"
    );
    assert!(dist
        .join("go/calculator/lib/darwin-arm64/libcalculator.dylib")
        .is_file());
}

#[test]
fn package_reports_targets_with_no_platform() {
    let project = tempfile::tempdir().expect("temp project dir");
    let input = calculator_idl(project.path());
    let bins = tempfile::tempdir().expect("temp bins dir");
    write_prebuilt(bins.path(), "calculator");
    let out = tempfile::tempdir().expect("temp out dir");

    // The tree holds only desktop libraries, so a wasm-only package has no
    // `wasm32` module to carry and must fail rather than write nothing.
    let output = weaveffi()
        .args(["package", input.to_str().unwrap(), "--binaries"])
        .arg(bins.path())
        .args(["--target", "wasm", "-o"])
        .arg(out.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let err = crate::stderr(&output);
    assert!(err.contains("no artifacts for wasm"), "{err}");
    assert!(
        err.contains("none of the selected targets produced"),
        "{err}"
    );
}

#[test]
fn package_names_the_missing_platform_library() {
    let input = repo_root().join("samples/calculator");
    let bins = tempfile::tempdir().expect("temp bins dir");
    write_prebuilt(bins.path(), "calculator");
    let output = weaveffi()
        .args(["package", input.to_str().unwrap(), "--binaries"])
        .arg(bins.path())
        .args(["--platforms", "android-arm64", "--target", "c"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let err = crate::stderr(&output);
    assert!(err.contains("libcalculator.so"), "{err}");
    assert!(
        err.contains("weaveffi build --platforms android-arm64"),
        "{err}"
    );
}

#[test]
fn build_without_a_cargo_toml_explains_the_alternatives() {
    let dir = tempfile::tempdir().unwrap();
    let idl = dir.path().join("greeter.yml");
    std::fs::write(
        &idl,
        "version: \"0.12.0\"\nmodules:\n  - name: greeter\n    functions:\n      - { name: hi, params: [], return: i32 }\n",
    )
    .unwrap();
    for command in ["build", "package"] {
        let output = weaveffi()
            .args([command, idl.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{command} succeeded");
        let err = crate::stderr(&output);
        assert!(err.contains("no Cargo.toml at"), "{command}: {err}");
        assert!(err.contains("--binaries"), "{command}: {err}");
    }
}

#[test]
fn build_rejects_unknown_and_foreign_platforms_before_compiling() {
    let input = repo_root().join("samples/calculator");
    let output = weaveffi()
        .args([
            "build",
            input.to_str().unwrap(),
            "--platforms",
            "solaris-sparc",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let err = crate::stderr(&output);
    assert!(err.contains("unknown platform `solaris-sparc`"), "{err}");
    assert!(err.contains("darwin-arm64"), "{err}");

    let foreign = if cfg!(target_os = "windows") {
        "darwin-arm64"
    } else {
        "windows-x64"
    };
    let output = weaveffi()
        .args(["build", input.to_str().unwrap(), "--platforms", foreign])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let err = crate::stderr(&output);
    assert!(err.contains("can only be built on a"), "{err}");
    assert!(
        err.contains("none of the requested platforms can be built"),
        "{err}"
    );
    assert!(!err.contains("Compiling"), "checks run before cargo: {err}");
}
