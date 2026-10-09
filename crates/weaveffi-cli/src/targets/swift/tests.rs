//! Unit tests of the Swift target's own policies: package and file naming,
//! configuration, identifier escaping, and doc spelling. The `kitchen_sink`
//! snapshot pins the rendering of every ABI shape.

use camino::Utf8Path;
use weaveffi_model::ir::Api;
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

use super::{render_swift_wrapper, Layout, SwiftConfig};

/// Names that collide with what the generator declares: a parameter named
/// like a wrapper body's locals, an error code named like the catch-all
/// case and the error enum's members, a field named like the leading
/// message, domains whose names already end in `Error`/`Errors`, a record
/// shadowed by a module namespace, and doc text naming API identifiers.
const FIXTURE: &str = r#"
version: "0.12.0"
modules:
  - name: shop
    errors:
      - name: ShopErrors
        codes:
          - { name: Unknown, code: 1, message: "unknown" }
          - { name: Message, code: 2, message: "message" }
          - { name: Failed, code: 3, message: "failed", fields: [{ name: message, type: string }] }
      - name: Failure
        codes:
          - { name: Broken, code: 1, message: "broken" }
    structs:
      - name: Stats
        fields:
          - { name: count, type: u64 }
    interfaces:
      - name: Cart
        methods:
          - { name: ptr, params: [], return: i64 }
    functions:
      - name: locals
        doc: "Calls `old_count`; fails with `ShopErrors`."
        params:
          - { name: err, type: i32 }
          - { name: rv, type: string }
          - { name: in, type: "i32?" }
        return: i32
        throws: ShopErrors
      - name: old_count
        deprecated: "Use `locals` instead"
        params: []
        return: u32
        throws: Failure
    modules:
      - name: stats
        functions:
          - { name: summarize, params: [], return: Stats }
"#;

fn model() -> Model {
    let api: Api = serde_yaml_ng::from_str(FIXTURE).unwrap();
    validate(&api, &Identity::named("shop_kit"), None).unwrap()
}

fn render() -> String {
    let model = model();
    let config = SwiftConfig::default();
    render_swift_wrapper(&Layout::new(&model, &config), &model, "ShopKit.swift")
}

#[track_caller]
fn assert_has(out: &str, needle: &str) {
    assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
}

#[test]
fn names_come_from_the_identity() {
    let model = model();
    let config = SwiftConfig::default();
    let files = Layout::new(&model, &config).sources(&model, Utf8Path::new("out"), &config);
    let paths: Vec<String> = files
        .iter()
        .map(|(p, _)| p.as_str().replace('\\', "/"))
        .collect();
    assert_eq!(
        paths,
        [
            "out/Package.swift",
            "out/Sources/CShopKit/module.modulemap",
            "out/Sources/CShopKit/shop_kit.h",
            "out/Sources/ShopKit/ShopKit.swift",
            "out/Sources/ShopKit/WeaveFFIRuntime.swift",
        ]
    );
    assert_has(&files[0].1, "name: \"ShopKit\"");
    assert_has(&files[0].1, ".systemLibrary(name: \"CShopKit\")");
    assert_has(&files[1].1, "header \"shop_kit.h\"");
    assert_has(&files[1].1, "link \"shop_kit\"");
    let runtime = &files[4].1;
    assert_has(runtime, "import CShopKit\n");
    assert_has(runtime, "public enum ShopKitLibrary {");
    assert_has(runtime, "public struct ShopKitRuntimeError: Error");
    assert_has(runtime, "let abi = shop_kit_abi_version()");
    assert_has(runtime, "public static let abiVersion: UInt32 = 5\n");
    assert!(!runtime.contains("{{"), "{runtime}");
    let all = files.iter().map(|(_, c)| c.as_str()).collect::<String>();
    assert!(!all.contains("weaveffi_"), "{all}");
}

#[test]
fn module_name_override_wins() {
    let model = model();
    let config = SwiftConfig {
        name: Some("Shop".into()),
        ..SwiftConfig::default()
    };
    let layout = Layout::new(&model, &config);
    assert_eq!(layout.module, "Shop");
    assert_eq!(layout.c_module, "CShop");
    assert_eq!(layout.library, "shop_kit");
    let out = render_swift_wrapper(&layout, &model, "Shop.swift");
    assert_has(&out, "func wvCheckContracts() -> ShopLibrary.LoadError? {");
    assert_has(&out, "``ShopRuntimeError``");
}

#[test]
fn parameters_named_like_body_locals_keep_their_labels() {
    let out = render();
    assert_has(
        &out,
        "public static func locals(err err_: Int32, rv rv_: String, in_: Int32?) throws -> Int32 {",
    );
    assert_has(&out, "wvWithUTF8(rv_) { rv__ptr, rv__len in");
    assert_has(
        &out,
        "shop_kit_shop_locals(err_, rv__ptr, rv__len, in_ != nil, in_ ?? 0, &err)",
    );
}

#[test]
fn domains_use_the_shared_type_name_and_avoid_their_members() {
    let out = render();
    // `ShopErrors` and `Failure` through `errors::type_name`.
    assert_has(
        &out,
        "public enum ShopError: Error, LocalizedError, Hashable, Sendable {",
    );
    assert_has(
        &out,
        "public enum FailureError: Error, LocalizedError, Hashable, Sendable {",
    );
    // A code named `Unknown` keeps its case; the catch-all moves aside. A
    // code named like a member and a field named like the message escape.
    assert_has(&out, "    case unknown(message: String)\n");
    assert_has(&out, "    case unknown_(code: Int32, message: String)\n");
    assert_has(&out, "    case message_(message: String)\n");
    assert_has(&out, "    case failed(message: String, message_: String)\n");
    assert_has(
        &out,
        "self = .failed(message: message.isEmpty ? \"failed\" : message, message_: r.read())",
    );
    assert_has(&out, "try wvCheck(&err, ShopError.self)");
}

#[test]
fn types_shadowed_by_a_namespace_are_qualified() {
    let out = render();
    assert_has(&out, "public struct Stats: Hashable, Sendable {");
    assert_has(&out, "public enum Stats {");
    assert_has(&out, "public static func summarize() -> ShopKit.Stats {");
}

#[test]
fn interface_members_avoid_the_wrapper_members() {
    let out = render();
    assert_has(&out, "public func ptr_() -> Int64 {");
    assert_has(&out, "shop_kit_shop_Cart_ptr(self.ptr, &err)");
}

#[test]
fn doc_text_uses_swift_spellings() {
    let out = render();
    assert_has(&out, "/// Calls `oldCount`; fails with `ShopError`.");
    assert_has(
        &out,
        "@available(*, deprecated, message: \"Use `locals` instead\")",
    );
}

#[test]
fn every_line_is_free_of_trailing_whitespace() {
    let out = render();
    for (i, line) in out.lines().enumerate() {
        assert_eq!(
            line,
            line.trim_end(),
            "line {} has trailing whitespace",
            i + 1
        );
    }
}
