//! Dev helper: regenerate the checked-in `weaveffi.schema.json` from the IR
//! types, exactly as `weaveffi schema --format json-schema` renders it.
//!
//! Ignored by default; run with `cargo test -p weaveffi-ir --test
//! regen_schema -- --ignored` after changing the IR.

#[test]
#[ignore = "dev helper, rewrites weaveffi.schema.json at the repo root"]
fn regen_schema_json() {
    let schema = schemars::schema_for!(weaveffi_ir::ir::Api);
    let json = serde_json::to_string_pretty(&schema).unwrap();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../weaveffi.schema.json");
    std::fs::write(path, format!("{json}\n")).unwrap();
}
