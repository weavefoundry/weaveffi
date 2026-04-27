fn cargo_bin() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("weaveffi").expect("binary not found")
}

#[test]
fn schema_emits_valid_json_schema() {
    let output = cargo_bin()
        .args(["schema", "--format", "json-schema"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "schema failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("schema output is not valid JSON: {e}\n---\n{stdout}\n---"));
    assert!(
        value.get("$schema").is_some(),
        "schema output missing $schema: {stdout}"
    );
    assert!(
        value.get("properties").is_some(),
        "schema output missing properties: {stdout}"
    );
    let definitions = value
        .get("definitions")
        .and_then(|v| v.as_object())
        .expect("schema output should include 'definitions'");
    for ty in [
        "Module",
        "Function",
        "Param",
        "TypeRef",
        "StructDef",
        "StructField",
        "EnumDef",
        "EnumVariant",
        "CallbackDef",
        "ListenerDef",
        "ErrorDomain",
        "ErrorCode",
    ] {
        assert!(
            definitions.contains_key(ty),
            "schema definitions missing {ty}: keys = {:?}",
            definitions.keys().collect::<Vec<_>>()
        );
    }
}
