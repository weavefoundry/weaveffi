//! Unit tests of the Swift target's own policies: package and file naming,
//! configuration, identifier escaping, doc spelling, and how each value's
//! one C value is spelled. The `kitchen_sink` snapshot pins the rendering of
//! every ABI shape.

use camino::Utf8Path;
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

use super::{render_swift_wrapper, Layout, SwiftConfig};

/// Names that collide with what the generator declares: a parameter named
/// like a wrapper body's locals, an error code named like the catch-all
/// case and the error enum's members, a field named like the leading
/// message, domains whose names already end in `Error`/`Errors`, an
/// interface method named like a wrapper member, and doc text naming API
/// identifiers. Plus one of every crossing, so the call layer's spelling of
/// each C value is pinned.
const FIXTURE: &str = r#"
version: "0.13.0"
errors:
  - name: ShopErrors
    codes:
      - { name: Unknown, code: 1, message: "unknown" }
      - { name: Message, code: 2, message: "message" }
      - { name: Failed, code: 3, message: "failed", fields: [{ name: message, type: string }] }
  - name: Failure
    codes:
      - { name: Broken, code: 1, message: "broken" }
enums:
  - name: Mood
    variants: [{ name: Calm }, { name: Busy }]
records:
  - name: Stats
    fields:
      - { name: count, type: u64 }
interfaces:
  - name: Cart
    methods:
      - { name: ptr, params: [], return: i64 }
      - { name: totals, params: [{ name: weights, type: "[f32]" }], return: "[f64]" }
    statics:
      - { name: mood_of, params: [{ name: m, type: "Mood?" }], return: "Mood?" }
callbacks:
  - name: Watcher
    methods:
      - { name: seen, params: [{ name: stats, type: Stats }, { name: cart, type: Cart }], return: "u32?" }
      - { name: label, params: [], return: string, throws: ShopErrors }
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
  - { name: summarize, params: [], return: Stats }
  - { name: watch, params: [{ name: watcher, type: "Watcher?" }, { name: cart, type: "Cart?" }], return: bool }
  - { name: stream, params: [], return: "iter<i64?>" }
"#;

fn model() -> Model {
    let api = weaveffi_model::parse::parse_api_str(FIXTURE, "yaml").unwrap();
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
    assert_has(runtime, "public static let abiVersion: UInt32 = 6\n");
    assert_has(runtime, "typealias WvRun = shop_kit_run\n");
    assert_has(runtime, "extension shop_kit_opt_f64: WvOptional {}\n");
    assert_has(runtime, "shop_kit_run_free(run)");
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
fn the_contract_check_calls_every_declarations_function() {
    let model = model();
    let out = render();
    let rows = crate::codegen::contract::rows(&model);
    assert!(!rows.is_empty());
    for row in &rows {
        assert_has(
            &out,
            &format!("({}, 0x{:016x}, \"{}\"),", row.symbol, row.hash, row.path),
        );
    }
}

#[test]
fn free_functions_are_top_level_and_statics_are_members() {
    let out = render();
    assert_has(&out, "\npublic func summarize() -> Stats {\n");
    assert_has(&out, "    public static func moodOf(m: Mood?) -> Mood? {\n");
    assert!(!out.contains("public enum ShopKit"), "{out}");
}

#[test]
fn parameters_named_like_body_locals_keep_their_labels() {
    let out = render();
    assert_has(
        &out,
        "public func locals(err err_: Int32, rv rv_: String, in_: Int32?) throws -> Int32 {",
    );
    assert_has(&out, "let rv: Int32 = wvWithUTF8(rv_) { rv__run in");
    assert_has(
        &out,
        "shop_kit_locals(err_, rv__run, shop_kit_opt_i32(wv: in_), &err)",
    );
}

#[test]
fn every_value_is_one_c_value() {
    let out = render();
    // Typed arrays lend and take runs; optional enums are `opt_i32`s.
    assert_has(
        &out,
        "let rv: WvRun = wvWithSlice(weights) { weights_run in",
    );
    assert_has(&out, "shop_kit_Cart_totals(self.ptr, weights_run, &err)");
    assert_has(&out, "return wvTakeSlice(rv, as: Double.self)");
    assert_has(
        &out,
        "shop_kit_Cart_mood_of(shop_kit_opt_i32(wv: m?.rawValue), &err)",
    );
    assert_has(&out, "return rv.wvValue.map { wvEnumCase(Mood.self, $0) }");
    // An optional callback is its argument struct, zero for none; an
    // optional object its nullable pointer.
    assert_has(
        &out,
        "let watcher_cb = watcher.map { shop_kit_Watcher(ctx: wvRetain($0), vtable: WvWatcherVtable.shared.pointer) } ?? shop_kit_Watcher()",
    );
    assert_has(&out, "shop_kit_watch(watcher_cb, cart?.ptr, &err)");
    // Iterator items keep an optional element apart from the end.
    assert_has(&out, "var item: shop_kit_opt_i64 = shop_kit_opt_i64()");
    assert_has(&out, "return .some(item.wvValue)");
}

#[test]
fn vtables_are_open_and_trampolines_positional() {
    let out = render();
    assert_has(
        &out,
        "size: UInt32(MemoryLayout<shop_kit_Watcher_vtable>.size),",
    );
    assert_has(&out, "flags: 0,");
    assert_has(&out, "seen: { ctx, a0, a1, outErr in");
    assert_has(
        &out,
        "wvInvoke(ctx, outErr, as: (any Watcher).self, fallback: shop_kit_opt_u32()) { wvImpl in",
    );
    assert_has(
        &out,
        "shop_kit_opt_u32(wv: wvImpl.seen(stats: wvBorrowBuffer(a0, as: Stats.self), cart: Cart(ptr: wvNonNull(a1))))",
    );
    assert_has(
        &out,
        "wvInvoke(ctx, outErr, as: (any Watcher).self, throwing: ShopError.self, fallback: WvRun()) { wvImpl in",
    );
    assert_has(&out, "try wvHandOverString(wvImpl.label())");
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
fn interface_members_avoid_the_wrapper_members() {
    let out = render();
    assert_has(&out, "public func ptr_() -> Int64 {");
    assert_has(&out, "shop_kit_Cart_ptr(self.ptr, &err)");
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
