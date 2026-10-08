//! Unit tests: render a small API that exercises every C ABI revision 4
//! shape the Swift wrapper distinguishes and assert the key pieces of each
//! contract.

use camino::Utf8Path;
use weaveffi_model::contract::entries;
use weaveffi_model::ir::Api;
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

use super::{render_swift_wrapper, Layout, SwiftConfig};

/// One module with an error domain carrying a field; an interface with a
/// `new` constructor, a method, and a throwing static; nullable objects; an
/// iterator of objects; a cancellable async function; a record carrying
/// objects; and a callback interface whose methods take every argument
/// family and return a direct value, an enum, a string, a record, an object,
/// and an optional object, one of them `throws`, passed both required and
/// optional.
const FIXTURE: &str = r#"
version: "0.11.0"
modules:
  - name: shop
    errors:
      name: ShopError
      codes:
        - { name: OutOfStock, code: 1, message: "out of stock", fields: [{ name: sku, type: string }] }
        - { name: Closed, code: 2, message: "closed" }
    enums:
      - name: Mood
        variants: [{ name: Happy, value: 0 }, { name: Grumpy, value: 1 }]
    structs:
      - name: Order
        fields:
          - { name: cart, type: Cart }
          - { name: history, type: "[Cart]" }
          - { name: note, type: "string?" }
    interfaces:
      - name: Cart
        constructors:
          - { name: new, params: [{ name: owner, type: string }] }
        methods:
          - { name: add, params: [{ name: sku, type: string }], return: bool }
        statics:
          - { name: restore, params: [{ name: id, type: i64 }], return: Cart, throws: true }
    callback_interfaces:
      - name: Watcher
        methods:
          - name: on_event
            params:
              - { name: name, type: string }
              - { name: count, type: i32 }
              - { name: order, type: Order }
              - { name: cart, type: Cart }
            return: bool
          - { name: mood, return: Mood }
          - { name: label, return: string, throws: true }
          - { name: latest, return: "Order?" }
          - { name: favorite, return: Cart }
          - { name: maybe, return: "Cart?" }
          - { name: on_done }
    functions:
      - { name: find_cart, params: [{ name: current, type: "Cart?" }], return: "Cart?" }
      - { name: all_carts, return: "iter<Cart>" }
      - { name: watch, params: [{ name: watcher, type: Watcher }] }
      - { name: maybe_watch, params: [{ name: watcher, type: "Watcher?" }] }
      - { name: echo, params: [{ name: text, type: string }], return: string }
      - { name: tally, params: [{ name: counts, type: "{string:[i32?]}" }], return: "[Order]" }
      - { name: wait, params: [{ name: ms, type: i64 }], return: i64, async: true, cancellable: true }
"#;

fn model() -> Model {
    let api: Api = serde_yaml::from_str(FIXTURE).unwrap();
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
        ]
    );
    assert_has(&files[0].1, "name: \"ShopKit\"");
    assert_has(&files[0].1, ".systemLibrary(name: \"CShopKit\")");
    assert_has(
        &files[0].1,
        ".binaryTarget(name: \"CShopKit\", path: xcframework)",
    );
    assert_has(&files[1].1, "header \"shop_kit.h\"");
    assert_has(&files[1].1, "link \"shop_kit\"");
    let all = files.iter().map(|(_, c)| c.as_str()).collect::<String>();
    assert!(!all.contains("weaveffi_"), "{all}");
    assert!(!all.contains("WeaveFFIError"), "{all}");
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
}

#[test]
fn load_checks_embed_every_contract_entry() {
    let model = model();
    let out = render();
    assert_has(&out, "let wvAbiVersion: UInt32 = 4");
    assert_has(&out, "let abi = shop_kit_abi_version()");
    assert_has(
        &out,
        "    wvCheckAbiVersion()\n    wvCheckContract(shop_kit_shop_contract, [\n",
    );
    let root = model.roots().next().unwrap();
    let table = entries(&model, root);
    assert!(table.len() > 10);
    for e in &table {
        assert_has(
            &out,
            &format!("(0x{:016x}, 0x{:016x}, \"{}\"),", e.id, e.hash, e.path),
        );
    }
    assert_has(&out, "\\(path) is missing from the library 'shop_kit'");
    assert_has(&out, "\\(path) changed since these bindings were generated");
    assert!(!out.contains("checksum"), "{out}");
    // Static entry points run the checks before the first native call.
    assert_has(
        &out,
        "public static func echo(text: String) -> String {\n        wvLoad()",
    );
}

#[test]
fn strings_cross_as_pointer_and_length() {
    let out = render();
    assert_has(&out, "wvWithUTF8(text) { text_ptr, text_len in");
    assert_has(
        &out,
        "shop_kit_shop_echo(text_ptr, text_len, &outLen, &err)",
    );
    assert_has(&out, "return wvTakeString(rv, outLen)");
}

#[test]
fn composites_share_one_generic_codec() {
    let out = render();
    // Records conform once; every composite reuses the runtime's generic
    // Optional, Array, and Dictionary conformances instead of inlined loops.
    assert_has(&out, "extension Order: WvCodable {");
    assert_has(
        &out,
        "Order(cart: r.read(), history: r.read(), note: r.read())",
    );
    assert_has(&out, "        w.write(self.history)\n");
    assert_has(
        &out,
        "extension Array: WvCodable where Element: WvCodable {",
    );
    assert_has(
        &out,
        "extension Dictionary: WvCodable where Key: WvCodable, Value: WvCodable {",
    );
    assert_has(&out, "extension Mood: WvCEnum {}");
    assert_has(&out, "wvWithEncoded(counts) { counts_ptr, counts_len in");
    assert_has(&out, "return wvTakeBuffer(rv, outLen, as: [Order].self)");
    assert!(!out.contains("for v"), "{out}");
}

#[test]
fn interface_class_owns_one_reference() {
    let out = render();
    assert_has(&out, "public final class Cart: @unchecked Sendable {");
    assert_has(&out, "let ptr: OpaquePointer");
    assert_has(&out, "shop_kit_shop_Cart_destroy(ptr)");
    assert_has(&out, "wvNonNull(shop_kit_shop_Cart_clone(ptr))");
    assert_eq!(out.matches("shop_kit_shop_Cart_destroy(").count(), 1);
    assert_has(&out, "public init(owner: String) {");
    assert_has(&out, "self.ptr = wvNonNull(rv)");
    assert_has(&out, "public func add(sku: String) -> Bool {");
    assert_has(&out, "shop_kit_shop_Cart_add(ptr, sku_ptr, sku_len, &err)");
    assert_has(
        &out,
        "static func wvRead(_ r: inout WvReader) -> Cart { Cart(ptr: r.readObject()) }",
    );
    assert_has(
        &out,
        "func wvWrite(_ w: inout WvWriter) { w.writeObject(clonePtr()) }",
    );
}

#[test]
fn throwing_calls_map_the_domain_and_others_trap() {
    let out = render();
    assert_has(
        &out,
        "public enum ShopError: Error, LocalizedError, Sendable {",
    );
    assert_has(&out, "case outOfStock(message: String, sku: String)");
    assert_has(
        &out,
        "/// - Throws: ``ShopError`` for a declared failure, ``ShopKitRuntimeError`` otherwise.",
    );
    assert_has(
        &out,
        "public static func restore(id: Int64) throws -> Cart {",
    );
    assert_has(&out, "try wvCheckShop(&err)");
    assert_has(
        &out,
        "let error = ShopError.outOfStock(message: message.isEmpty ? \"out of stock\" : message, sku: r.read())",
    );
    // A call that can't throw stops the process, naming code and message.
    assert_has(&out, "wvTrap(&err)");
    assert_has(
        &out,
        "fatalError(\"ShopKit.\\(function) failed with code \\(code): \\(message)\")",
    );
}

#[test]
fn nullable_objects_map_to_optional_wrappers() {
    let out = render();
    assert_has(
        &out,
        "public static func findCart(current: Cart?) -> Cart? {",
    );
    assert_has(&out, "shop_kit_shop_find_cart(current?.ptr, &err)");
    assert_has(&out, "return rv.map { Cart(ptr: $0) }");
}

#[test]
fn object_iterator_adopts_each_element() {
    let out = render();
    assert_has(
        &out,
        "public final class ShopAllCartsIterator: Sequence, IteratorProtocol {",
    );
    assert_has(&out, "var item: OpaquePointer? = nil");
    assert_has(
        &out,
        "shop_kit_shop_AllCartsIterator_next(handle, &item, &err)",
    );
    assert_has(&out, "shop_kit_shop_AllCartsIterator_destroy(handle)");
    assert_has(&out, "return Cart(ptr: wvNonNull(item))");
}

#[test]
fn callback_vtable_starts_with_the_header() {
    let out = render();
    assert_has(&out, "public protocol Watcher: AnyObject, Sendable {");
    assert_has(
        &out,
        "static let shared = WvVtable(shop_kit_shop_Watcher_vtable(\n        size: UInt32(MemoryLayout<shop_kit_shop_Watcher_vtable>.stride),\n        flags: 0,\n        free: { ctx in\n            wvRelease(ctx, as: (any Watcher).self)\n        },\n        on_event: {",
    );
    // No trailing comma after the last entry (Swift before 6.1 rejects it).
    assert_has(&out, "        }\n    ))\n}");
}

#[test]
fn callback_arguments_are_copied_or_adopted() {
    let out = render();
    assert_has(
        &out,
        "func onEvent(name: String, count: Int32, order: Order, cart: Cart) throws -> Bool",
    );
    assert_has(
        &out,
        "on_event: { ctx, name_ptr, name_len, count, order_ptr, order_len, cart, out_err in",
    );
    assert_has(
        &out,
        "wvInvoke(ctx, out_err, as: (any Watcher).self, fallback: false) { wvImpl in",
    );
    assert_has(
        &out,
        "try wvImpl.onEvent(name: wvBorrowString(name_ptr, name_len), count: count, order: wvBorrowBuffer(order_ptr, order_len, as: Order.self), cart: Cart(ptr: wvNonNull(cart)))",
    );
}

#[test]
fn callback_returns_cover_every_family() {
    let out = render();
    // An enum by value.
    assert_has(&out, "func mood() throws -> Mood");
    assert_has(
        &out,
        "fallback: shop_kit_shop_Mood(rawValue: 0)) { wvImpl in\n                try shop_kit_shop_Mood(rawValue: numericCast(wvImpl.mood().rawValue))",
    );
    // A string and a buffer through the out slots, as `{p}_alloc` runs.
    assert_has(&out, "label: { ctx, out_ptr, out_len, out_err in");
    assert_has(&out, "try wvReturnString(wvImpl.label(), out_ptr, out_len)");
    assert_has(
        &out,
        "try wvReturnBuffer(wvImpl.latest(), out_ptr, out_len)",
    );
    assert_has(&out, "let run = shop_kit_alloc(bytes.count)");
    assert!(!out.contains("_dealloc"), "{out}");
    // Objects as a fresh reference the library adopts; `I?` may be nil.
    assert_has(&out, "func favorite() throws -> Cart");
    assert_has(
        &out,
        "fallback: nil) { wvImpl in\n                try wvImpl.favorite().clonePtr()",
    );
    assert_has(&out, "func maybe() throws -> Cart?");
    assert_has(&out, "try wvImpl.maybe()?.clonePtr()");
    assert_has(
        &out,
        "fallback: ()) { wvImpl in\n                try wvImpl.onDone()",
    );
}

#[test]
fn throwing_callback_methods_report_the_domain_error() {
    let out = render();
    assert_has(&out, "func label() throws -> String");
    assert_has(
        &out,
        "/// - Throws: ``ShopError`` to report a declared failure with its fields",
    );
    assert_has(
        &out,
        "wvInvoke(ctx, out_err, as: (any Watcher).self, fallback: (), domain: wvReportShop) { wvImpl in",
    );
    assert_eq!(out.matches("domain: wvReportShop").count(), 1, "{out}");
    assert_has(
        &out,
        "func wvReportShop(_ error: Error, _ outErr: UnsafeMutablePointer<WvError>?) -> Bool {",
    );
    assert_has(
        &out,
        "    case let .outOfStock(message, v0):\n        var payload = WvWriter()\n        payload.write(v0)\n        wvSetError(outErr, 1, message, payload)",
    );
    assert_has(
        &out,
        "    case let .closed(message):\n        wvSetError(outErr, 2, message)",
    );
    assert_has(
        &out,
        "shop_kit_error_set_payload(outErr, $0.baseAddress, $0.count)",
    );
    // Any other error is a callback failure.
    assert_has(
        &out,
        "shop_kit_error_set(outErr, ShopKitRuntimeError.foreignCode, $0)",
    );
}

#[test]
fn callback_parameters_retain_the_implementation() {
    let out = render();
    assert_has(&out, "public static func watch(watcher: any Watcher) {");
    assert_has(&out, "let watcher_ctx = wvRetain(watcher)");
    assert_has(
        &out,
        "shop_kit_shop_watch(watcher_ctx, WvWatcherVtable.shared.pointer, &err)",
    );
    // An optional callback passes a null vtable for nil.
    assert_has(
        &out,
        "public static func maybeWatch(watcher: (any Watcher)?) {",
    );
    assert_has(&out, "let watcher_ctx = watcher.map { wvRetain($0) }");
    assert_has(
        &out,
        "shop_kit_shop_maybe_watch(watcher_ctx, watcher == nil ? nil : WvWatcherVtable.shared.pointer, &err)",
    );
}

#[test]
fn cancellable_async_cancels_the_native_token() {
    let out = render();
    assert_has(
        &out,
        "public static func wait(ms: Int64) async throws -> Int64 {",
    );
    assert_has(&out, "let token = WvCancelToken()");
    assert_has(&out, "return try await withTaskCancellationHandler {");
    assert_has(
        &out,
        "shop_kit_shop_wait(ms, token.raw, { context, err, result in",
    );
    assert_has(
        &out,
        "cont.resume(throwing: wvTakeError(err, wvCancelledOrTrap))",
    );
    assert_has(&out, "} onCancel: {\n            token.cancel()");
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
