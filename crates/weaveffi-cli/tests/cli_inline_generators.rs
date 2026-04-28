use std::fs;

#[test]
fn inline_dart_package_name_used() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml = dir.path().join("api.yml");
    fs::write(
        &yml,
        concat!(
            "version: \"0.3.0\"\n",
            "modules:\n",
            "  - name: math\n",
            "    functions:\n",
            "      - name: add\n",
            "        params:\n",
            "          - { name: a, type: i32 }\n",
            "          - { name: b, type: i32 }\n",
            "        return: i32\n",
            "generators:\n",
            "  dart:\n",
            "    package_name: my_inline_dart_pkg\n",
        ),
    )
    .expect("failed to write api.yml");

    let out = dir.path().join("out");

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--target",
            "dart",
        ])
        .assert()
        .success();

    let pubspec =
        fs::read_to_string(out.join("dart/pubspec.yaml")).expect("missing dart/pubspec.yaml");
    assert!(
        pubspec.contains("name: my_inline_dart_pkg"),
        "pubspec should pick up inline dart.package_name override; got:\n{pubspec}"
    );
}
