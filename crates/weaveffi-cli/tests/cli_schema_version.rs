fn cargo_bin() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found")
}

#[test]
fn schema_version_command_prints_0_3_0() {
    let output = cargo_bin().arg("schema-version").output().unwrap();
    assert!(
        output.status.success(),
        "schema-version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "0.3.0");
}
