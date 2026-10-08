//! Unit tests: render a small API that exercises every C ABI revision 4
//! shape the Ruby bindings distinguish and assert the key pieces of each
//! contract.

use camino::Utf8Path;
use weaveffi_model::contract::entries;
use weaveffi_model::ir::Api;
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

use super::{RubyConfig, RubyGenerator};
use crate::backend::{LanguageBackend, OutputFile};
use crate::package::{ArtifactKind, PackageContext};
use crate::platform::{BinarySet, NativeBinary, Platform};

/// One module with an error domain carrying a field; an interface with a
/// `new` constructor, a method, and a throwing static; nullable objects; an
/// iterator of objects; a cancellable async function; a record carrying
/// objects; a record whose name collides with a composite's codec stem;
/// and a callback interface whose methods take every argument family and
/// return a direct value, an enum, a string, a record, an object, and an
/// optional object, one of them `throws`, passed both required and
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
      - name: ListI32
        fields:
          - { name: items, type: "[i32]" }
    interfaces:
      - name: Cart
        constructors:
          - { name: new, params: [{ name: owner, type: string }] }
        methods:
          - { name: add, params: [{ name: sku, type: string }], return: bool }
          - { name: total, return: i64, deprecated: "use sum" }
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
      - { name: firsts, params: [{ name: lists, type: "[i32]" }], return: ListI32 }
      - { name: wait, params: [{ name: ms, type: i64 }], return: i64, async: true, cancellable: true }
"#;

fn model() -> Model {
    let api: Api = serde_yaml::from_str(FIXTURE).unwrap();
    validate(&api, &Identity::named("shop_kit"), None).unwrap()
}

fn generate(config: &RubyConfig) -> Vec<OutputFile> {
    RubyGenerator.files(&model(), Utf8Path::new("out"), config)
}

fn file<'a>(files: &'a [OutputFile], path: &str) -> &'a str {
    &files
        .iter()
        .find(|f| f.path.as_str() == path)
        .unwrap_or_else(|| {
            panic!(
                "no {path} in {:?}",
                files.iter().map(|f| &f.path).collect::<Vec<_>>()
            )
        })
        .contents
}

fn bindings() -> String {
    file(
        &generate(&RubyConfig::default()),
        "out/ruby/lib/shop_kit.rb",
    )
    .to_string()
}

fn runtime() -> String {
    file(
        &generate(&RubyConfig::default()),
        "out/ruby/lib/shop_kit/runtime.rb",
    )
    .to_string()
}

#[track_caller]
fn assert_has(src: &str, needle: &str) {
    assert!(src.contains(needle), "missing {needle:?} in:\n{src}");
}

#[track_caller]
fn assert_lacks(src: &str, needle: &str) {
    assert!(!src.contains(needle), "unexpected {needle:?} in:\n{src}");
}

/// The text of the generated item that starts at the first line containing
/// `start`, up to its closing `end` at the same indentation.
fn item<'a>(src: &'a str, start: &str) -> &'a str {
    let at = src
        .find(start)
        .unwrap_or_else(|| panic!("no {start:?} in:\n{src}"));
    let line_start = src[..at].rfind('\n').map_or(0, |i| i + 1);
    let indent = &src[line_start..at];
    let indent = &indent[..indent.len() - indent.trim_start().len()];
    let close = format!("\n{indent}end\n");
    let end = src[at..].find(&close).expect("item has an end") + at + close.len();
    &src[line_start..end]
}

#[test]
fn layout_follows_the_identity() {
    let files = generate(&RubyConfig::default());
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "out/ruby/lib/shop_kit.rb",
            "out/ruby/lib/shop_kit/runtime.rb",
            "out/ruby/shop_kit.gemspec",
            "out/ruby/README.md",
        ]
    );
    assert_has(
        file(&files, "out/ruby/shop_kit.gemspec"),
        "s.name        = 'shop_kit'",
    );
    let src = file(&files, "out/ruby/lib/shop_kit.rb");
    assert_has(src, "require_relative 'shop_kit/runtime'");
    assert_has(src, "module ShopKit\n");
    // Nothing but the generated-file banner mentions WeaveFFI.
    for f in &files {
        for line in f.contents.lines().filter(|l| {
            !l.contains("Generated by WeaveFFI") && !l.contains("To regenerate: weaveffi generate")
        }) {
            assert!(
                !line.to_ascii_lowercase().contains("weaveffi"),
                "branded line in {}: {line}",
                f.path
            );
        }
    }
}

#[test]
fn config_overrides_gem_and_module_names() {
    let config = RubyConfig {
        name: Some("acme-shop".into()),
        module_name: Some("Acme".into()),
    };
    let files = generate(&config);
    assert_has(
        file(&files, "out/ruby/acme-shop.gemspec"),
        "s.name        = 'acme-shop'",
    );
    // The require path stays the identity prefix.
    let src = file(&files, "out/ruby/lib/shop_kit.rb");
    assert_has(src, "module Acme\n");
    assert_has(src, "Acme.shop_kit_shop_Cart_destroy(ptr)");
    assert_has(
        file(&files, "out/ruby/lib/shop_kit/runtime.rb"),
        "module Acme\n",
    );
}

#[test]
fn runtime_speaks_abi_4() {
    let rt = runtime();
    assert_has(&rt, "ENV['SHOP_KIT_LIBRARY']");
    assert_has(&rt, "'libshop_kit.dylib'");
    assert_has(&rt, "'libshop_kit.so'");
    assert_has(&rt, "'shop_kit.dll'");
    assert_has(&rt, &format!("ABI_VERSION = {}", crate::cabi::ABI_VERSION));
    assert_eq!(crate::cabi::ABI_VERSION, 4);
    assert_has(&rt, "attach_function :shop_kit_abi_version, [], :uint32");
    assert_has(&rt, "attach_function :shop_kit_alloc, [:size_t], :pointer");
    assert_has(
        &rt,
        "attach_function :shop_kit_error_set_payload, [:pointer, :pointer, :size_t], :void",
    );
    assert_has(
        &rt,
        "attach_function :shop_kit_free_bytes, [:pointer, :size_t], :void",
    );
    assert_lacks(&rt, "dealloc");
    assert_lacks(&rt, "checksum");
    assert_lacks(&rt, "{{");
    // The load errors name the declaration path.
    assert_has(&rt, "#{path} is missing from the library");
    assert_has(&rt, "#{path} changed since these bindings were generated");
    // The trap policy: a failure of a call that declares no errors is a
    // NativeBugError naming the code and message; -5 is Cancelled.
    assert_has(&rt, "class NativeBugError < Error");
    assert_has(&rt, "native call failed with code #{code}: #{message}");
    assert_has(&rt, "class Cancelled < Error");
    assert_has(&rt, "CANCELLED_ERROR_CODE = -5");
}

#[test]
fn load_checks_embed_every_contract_entry() {
    let model = model();
    let src = bindings();
    assert_has(
        &src,
        "  _wv_check_contract!(\n    shop_kit_shop_contract: [\n",
    );
    let root = model.roots().next().unwrap();
    let table = entries(&model, root);
    assert!(table.len() > 10);
    for e in &table {
        assert_has(
            &src,
            &format!("[0x{:016x}, 0x{:016x}, '{}'],", e.id, e.hash, e.path),
        );
    }
    assert_lacks(&src, "checksum");
}

#[test]
fn every_call_but_the_trivial_helpers_releases_the_gvl() {
    let src = bindings();
    for line in src.lines().filter(|l| l.contains("attach_function")) {
        let trivial = line.contains("_clone,") || line.contains("_destroy,");
        assert_eq!(!trivial, line.ends_with(", blocking: true"), "{line}");
    }
    assert_has(
        &src,
        "attach_function :shop_kit_shop_echo, [:pointer, :size_t, :pointer, :pointer], :pointer, blocking: true",
    );
    assert_has(
        &src,
        "attach_function :shop_kit_shop_AllCartsIterator_destroy, [:pointer], :void\n",
    );
}

#[test]
fn errors_follow_the_trap_policy() {
    let src = bindings();
    // A throwing call raises the domain hierarchy; any other call traps.
    assert_has(
        item(&src, "def self.restore(id)"),
        "ShopKit._wv_check_shop_error!(err)",
    );
    assert_has(item(&src, "def self.echo(text)"), "_wv_check!(err)");
    assert_has(&src, "queue << _wv_trap(*taken)");
    assert_has(&src, "class ShopError < Error");
    assert_has(&src, "class OutOfStock < ShopError");
    assert_has(&src, "def initialize(message = nil, sku: nil)");
    assert_has(&src, "super(CODE, message || 'out of stock')");
    assert_has(
        &src,
        "ShopError::OutOfStock.new(message, sku: r.read_string)",
    );
}

#[test]
fn vtables_carry_the_header_and_every_return_family() {
    let src = bindings();
    assert_has(
        &src,
        "layout :wv_size, :uint32,\n           :wv_flags, :uint32,\n           :wv_free, :pointer,\n           :on_event, :pointer,",
    );
    assert_has(&src, "WV_WATCHER_VTABLE[:wv_size] = WvWatcherVtable.size");
    assert_has(&src, "WV_WATCHER_VTABLE[:wv_flags] = 0");
    assert_has(&src, "WV_WATCHER_VTABLE[:wv_free] = WV_WATCHER_FREE");
    assert_eq!(src.matches("= WvWatcherVtable.new").count(), 1);

    // Direct: by value, coerced inside the rescue.
    let on_event = item(&src, "WV_WATCHER_ON_EVENT = ");
    assert_has(on_event, "FFI::Function.new(:bool, [:pointer, :pointer, :size_t, :int32, :pointer, :size_t, :pointer, :pointer])");
    assert_has(
        on_event,
        "do |wv_ctx, name_ptr, name_len, count, order_ptr, order_len, cart_ptr, wv_err|",
    );
    // Objects are adopted before anything that can raise.
    let adopt = on_event.find("cart = Cart._from_ptr(cart_ptr)").unwrap();
    assert!(adopt < on_event.find("name = _wv_borrow_string").unwrap());
    assert!(adopt < on_event.find("order = _wv_decode_borrowed").unwrap());
    assert_has(
        on_event,
        "_wv_cb_lookup(wv_ctx).on_event(name, count, order, cart) ? true : false",
    );
    assert_has(on_event, "    false\n");
    assert_has(
        item(&src, "WV_WATCHER_MOOD = "),
        "_wv_int(_wv_cb_lookup(wv_ctx).mood, :i32)",
    );
    // String: a {prefix}_alloc run in the out slots.
    let label = item(&src, "WV_WATCHER_LABEL = ");
    assert_has(label, "do |wv_ctx, wv_out_ptr, wv_out_len, wv_err|");
    assert_has(
        label,
        "_wv_cb_return_bytes(wv_out_ptr, wv_out_len, _wv_str(_wv_cb_lookup(wv_ctx).label))",
    );
    // A record carrying objects: its references are minted by the seal.
    let latest = item(&src, "WV_WATCHER_LATEST = ");
    assert_has(latest, "_wv_write_opt_order(writer, result)");
    assert_has(
        latest,
        "_wv_cb_return_bytes(wv_out_ptr, wv_out_len, _wv_seal(writer).first)",
    );
    // Objects: a fresh reference the producer adopts (NULL for nil).
    let favorite = item(&src, "WV_WATCHER_FAVORITE = ");
    assert_has(
        favorite,
        "FFI::Function.new(:pointer, [:pointer, :pointer])",
    );
    assert_has(
        favorite,
        "_wv_cb_return_object(_wv_cb_lookup(wv_ctx).favorite, Cart)",
    );
    assert_has(favorite, "FFI::Pointer::NULL\n");
    assert_has(
        item(&src, "WV_WATCHER_MAYBE = "),
        "_wv_cb_return_object(_wv_cb_lookup(wv_ctx).maybe, Cart)",
    );
    assert_has(&runtime(), "def self._wv_cb_return_object(value, cls)");
}

#[test]
fn throwing_callback_methods_report_the_domain_error() {
    let src = bindings();
    let label = item(&src, "WV_WATCHER_LABEL = ");
    assert_has(label, "rescue ShopError => e");
    assert_has(
        label,
        "_wv_cb_throw(wv_err, e) { _wv_shop_error_payload(e) }",
    );
    // Every other exception is -4, after the domain rescue.
    assert!(label.find("rescue ShopError").unwrap() < label.find("rescue Exception").unwrap());
    assert_has(label, "_wv_cb_fail(wv_err, e)");
    // A method without `throws` never reports a domain code.
    assert_lacks(item(&src, "WV_WATCHER_MOOD = "), "rescue ShopError");
    // The payload encoder writes each code's fields.
    let payload = item(&src, "def self._wv_shop_error_payload(error)");
    assert_has(payload, "when ShopError::OutOfStock\n");
    assert_has(payload, "w.write_string(error.sku)");
    assert_has(payload, "when ShopError::Closed then 2");
    assert_has(payload, "code && [code, w.bytes]");
    let rt = runtime();
    assert_has(
        &rt,
        "shop_kit_error_set_payload(out_err, payload, payload.bytesize)",
    );
    assert_has(
        &rt,
        "shop_kit_error_set(out_err, FOREIGN_ERROR_CODE, _wv_cb_message(exception))",
    );
}

#[test]
fn callback_parameters_register_their_implementation() {
    let src = bindings();
    let watch = item(&src, "def self.watch(watcher)");
    assert_has(watch, "_wv_present!(watcher, 'watcher')");
    assert_has(
        watch,
        "watcher_ctx, watcher_vtable = _wv_cb_register(watcher, WV_WATCHER_VTABLE)",
    );
    assert_has(
        watch,
        "shop_kit_shop_watch(watcher_ctx, watcher_vtable, err)",
    );
    // An optional callback defaults to nil, which passes two NULLs.
    let maybe = item(&src, "def self.maybe_watch(watcher = nil)");
    assert_lacks(maybe, "_wv_present!");
    assert_has(
        maybe,
        "watcher_ctx, watcher_vtable = _wv_cb_register(watcher, WV_WATCHER_VTABLE)",
    );
    assert_has(&runtime(), "return [nil, nil] if impl.nil?");
}

#[test]
fn objects_are_checked_pinned_and_adopted() {
    let src = bindings();
    assert_has(&src, "class CartPtr < FFI::AutoPointer");
    assert_has(&src, "class Cart < WvObject\n    WV_PTR = CartPtr\n");
    assert_has(&src, "ShopKit.shop_kit_shop_Cart_clone(ptr)");
    let find = item(&src, "def self.find_cart(current = nil)");
    assert_has(find, "_wv_object!(current, Cart, 'current', true)");
    assert_has(find, "_wv_pin(current) do |_wv_current|");
    assert_has(find, "result.null? ? nil : Cart._from_ptr(result)");
    let add = item(&src, "def add(sku)");
    assert_has(add, "ShopKit._wv_pin(self) do |_wv_self|");
    let init = item(&src, "def initialize(owner)");
    assert_has(init, "_wv_init(ShopKit._wv_nonnull(result))");
    assert_has(
        item(&src, "def self.restore(id)"),
        "_from_ptr(ShopKit._wv_nonnull(result))",
    );
    assert_has(
        item(&src, "def total"),
        "warn('ShopKit::Cart#total is deprecated: use sum', uplevel: 1)",
    );
}

#[test]
fn composite_types_get_one_codec_pair_each() {
    let src = bindings();
    for stem in [
        "map_string_list_opt_i32",
        "list_opt_i32",
        "opt_i32",
        "list_order",
        "list_cart",
        "opt_string",
        "opt_order",
        "list_i32_2",
    ] {
        assert_eq!(
            src.matches(&format!("def self._wv_write_{stem}(w, v)"))
                .count(),
            1,
            "{stem}"
        );
    }
    // The record `ListI32` owns the `list_i32` stem, so the `[i32]`
    // composite takes the next free one.
    assert_has(
        &src,
        "# Packs a ListI32 into the value-buffer wire format.\n  def self._wv_write_list_i32(w, v)",
    );
    assert_has(
        &src,
        "# Packs `[i32]` into the value-buffer wire format.\n  def self._wv_write_list_i32_2(w, v)",
    );
    assert_has(&src, "items: _wv_read_list_i32_2(r)");
    assert_has(
        &src,
        "counts_s = _wv_encode { |w| _wv_write_map_string_list_opt_i32(w, counts) }",
    );
    assert_has(
        &src,
        "_wv_decode(result, out_len.read(:size_t)) { |r| _wv_read_list_order(r) }",
    );
    let map = item(&src, "def self._wv_write_map_string_list_opt_i32(w, v)");
    assert_has(map, "w.write_len(v.length)");
    assert_has(map, "v.each do |k, e|");
    assert_has(map, "_wv_write_list_opt_i32(w, e)");
    assert_has(
        &src,
        "Array.new(r.read_len) { [r.read_string, _wv_read_list_opt_i32(r)] }.to_h",
    );
    assert_has(&src, "r.read_flag ? r.read_i32 : nil");
    // Records carrying objects reserve tokens and seal them before the call.
    assert_has(&src, "w.write_object(v.cart, Cart)");
    assert_has(&src, "cart: r.read_object(Cart)");
}

#[test]
fn iterator_is_a_lazy_enumerator_adopting_objects() {
    let src = bindings();
    let all = item(&src, "def self.all_carts");
    assert_has(all, "Enumerator.new do |y|");
    assert_has(
        all,
        "has_item = shop_kit_shop_AllCartsIterator_next(iter, out_item, err)",
    );
    assert_has(all, "value = Cart._from_ptr(_wv_nonnull(item))");
    assert_has(
        all,
        "shop_kit_shop_AllCartsIterator_destroy(iter) unless iter.null?",
    );
}

#[test]
fn cancellable_async_takes_a_cancel_token() {
    let src = bindings();
    let wait = item(&src, "def self.wait(ms, cancel: nil)");
    assert_has(wait, "own_token = CancelToken.new if cancel.nil?");
    assert_has(
        wait,
        "shop_kit_shop_wait(ms, token._wv_ptr, SHOP_KIT_SHOP_WAIT_CALLBACK, ctx)",
    );
    assert_has(wait, "_wv_async_wait(queue, token)");
    assert_has(wait, "own_token&.close");
    assert_has(
        &src,
        "SHOP_KIT_SHOP_WAIT_CALLBACK = FFI::Function.new(:void, [:pointer, :pointer, :int64]) do |ctx, err, result|",
    );
}

#[test]
fn rendering_is_deterministic() {
    assert_eq!(bindings(), bindings());
}

#[test]
fn package_skips_platforms_without_a_gem_string() {
    let model = model();
    let mut binaries = BinarySet::new("shop_kit");
    for (platform, path) in [
        (Platform::MacosArm64, "/tmp/darwin-arm64/libshop_kit.dylib"),
        (Platform::AndroidArm64, "/tmp/android-arm64/libshop_kit.so"),
        (Platform::Wasm32, "/tmp/wasm32/shop_kit.wasm"),
    ] {
        binaries.insert(NativeBinary::new(platform, path));
    }
    let ctx = PackageContext::new(&binaries);
    let artifacts = RubyGenerator
        .package(&model, &ctx, &RubyConfig::default())
        .expect("ruby supports packaging");
    assert_eq!(artifacts.len(), 1, "{artifacts:?}");
    let gem = &artifacts[0];
    assert_eq!(gem.path, "ruby/shop_kit-0.1.0-arm64-darwin.gem");
    let ArtifactKind::Gem(spec) = &gem.kind else {
        panic!("a gem");
    };
    assert_eq!(spec.platform, "arm64-darwin");
    assert_eq!(
        spec.dependencies,
        [("ffi".to_string(), "~> 1.15".to_string())]
    );
    let paths: Vec<&str> = gem.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "lib/shop_kit.rb",
            "lib/shop_kit/runtime.rb",
            "lib/native/libshop_kit.dylib",
            "README.md",
        ]
    );
}

#[test]
fn members_never_replace_the_wrapper_methods() {
    let yaml = r#"
version: "0.11.0"
modules:
  - name: kv
    interfaces:
      - name: Store
        constructors:
          - { name: allocate, params: [] }
        methods:
          - { name: close, params: [], return: bool }
          - { name: handle, params: [], return: i64 }
          - { name: initialize, params: [] }
          - { name: name, params: [], return: string }
        statics:
          - { name: hash, params: [], return: i64 }
          - { name: label, params: [], return: string }
"#;
    let api: Api = serde_yaml::from_str(yaml).unwrap();
    let model = validate(&api, &Identity::named("kv"), None).unwrap();
    let files = RubyGenerator.files(&model, Utf8Path::new("out"), &RubyConfig::default());
    let rb = file(&files, "out/ruby/lib/kv.rb");
    for def in [
        "def self.allocate_",
        "def close_",
        "def handle_",
        "def initialize_",
        "def name\n",
        "def self.hash\n",
        "def self.label\n",
    ] {
        assert!(rb.contains(def), "missing `{def}` in:\n{rb}");
    }
    assert!(!rb.contains("def close\n") && !rb.contains("def handle\n"));
}
