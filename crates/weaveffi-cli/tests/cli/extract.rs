//! Library mode end to end: `weaveffi extract` and `weaveffi generate` read
//! a Rust producer's API from the library it builds. The producer is the
//! fixture crate at `tests/fixtures/producer`, whose expected API is
//! `tests/fixtures/producer/expected.yml`.

use std::path::{Path, PathBuf};

use crate::weaveffi;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/producer")
}

fn stdout_of(output: &std::process::Output) -> String {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

/// The extracted IDL is exactly the expected one: every declaration kind,
/// declaration order, docs, the type alias resolved, the sibling tree, and
/// only the declarations the default build compiles (`list_price`, not the
/// `#[cfg(feature = "extra")]` function, `impl` block, or module).
#[test]
fn extract_reads_the_api_from_the_built_library() {
    let output = weaveffi()
        .args(["-q", "extract"])
        .arg(fixture())
        .output()
        .unwrap();
    let expected = std::fs::read_to_string(fixture().join("expected.yml")).unwrap();
    assert_eq!(stdout_of(&output), expected);
    assert!(expected.contains("list_price") && !expected.contains("discount"));
}

/// Bindings generated from the library are byte-identical to bindings
/// generated from its extracted IDL under the same identity.
#[test]
fn generating_from_the_library_matches_generating_from_its_idl() {
    let dir = tempfile::tempdir().unwrap();
    let from_library = dir.path().join("library");
    weaveffi()
        .args(["-q", "generate"])
        .arg(fixture())
        .args(["--target", "c", "-o"])
        .arg(&from_library)
        .assert()
        .success();

    let idl_dir = dir.path().join("idl");
    std::fs::create_dir_all(&idl_dir).unwrap();
    std::fs::copy(fixture().join("expected.yml"), idl_dir.join("api.yml")).unwrap();
    std::fs::write(
        idl_dir.join("weaveffi.toml"),
        concat!(
            "[package]\n",
            "name = \"producer-fixture\"\n",
            "version = \"0.3.0\"\n",
            "c_prefix = \"producer_fixture\"\n",
            "library = \"producer_fixture\"\n",
        ),
    )
    .unwrap();
    let from_idl = dir.path().join("from-idl");
    weaveffi()
        .args(["-q", "generate"])
        .arg(idl_dir.join("api.yml"))
        .args(["--target", "c", "-o"])
        .arg(&from_idl)
        .assert()
        .success();

    let header = "c/producer_fixture.h";
    let a = std::fs::read_to_string(from_library.join(header)).unwrap();
    let b = std::fs::read_to_string(from_idl.join(header)).unwrap();
    assert!(a.contains("producer_fixture_shop_Cart_total"), "{a}");
    assert_eq!(a, b);
}

/// `--library` reads a library standing alone (no crate, no IDL), and the
/// output formats round-trip.
#[test]
fn extract_formats_and_a_library_alone() {
    let yaml = stdout_of(
        &weaveffi()
            .args(["-q", "extract"])
            .arg(fixture())
            .output()
            .unwrap(),
    );
    let api: weaveffi_model::ir::Api = serde_yaml_ng::from_str(&yaml).unwrap();
    let json = stdout_of(
        &weaveffi()
            .args(["-q", "extract", "-f", "json"])
            .arg(fixture())
            .output()
            .unwrap(),
    );
    assert_eq!(
        serde_json::from_str::<weaveffi_model::ir::Api>(&json).unwrap(),
        api
    );
    // An IDL is YAML or JSON; TOML is only for `weaveffi.toml`.
    let toml = weaveffi()
        .args(["-q", "extract", "-f", "toml"])
        .arg(fixture())
        .output()
        .unwrap();
    assert!(!toml.status.success());
}

/// What isn't a Rust producer's library is refused with a reason: an IDL,
/// a binary without WeaveFFI metadata, and Rust source.
#[test]
fn extract_explains_what_it_cannot_read() {
    let idl = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kitchen_sink.yml");
    let output = weaveffi().arg("extract").arg(&idl).output().unwrap();
    assert!(!output.status.success());
    let err = crate::stderr(&output);
    assert!(err.contains("already an IDL"), "{err}");

    // The CLI binary itself is a real executable with no metadata.
    let bin = assert_cmd::cargo::cargo_bin("weaveffi");
    let output = weaveffi()
        .args(["extract", "--library"])
        .arg(&bin)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let err = crate::stderr(&output);
    assert!(err.contains("no WeaveFFI metadata"), "{err}");
    assert!(err.contains("export_runtime!"), "{err}");

    let output = weaveffi()
        .arg("extract")
        .arg(fixture().join("src/lib.rs"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let err = crate::stderr(&output);
    assert!(err.contains("pass the crate"), "{err}");
}
