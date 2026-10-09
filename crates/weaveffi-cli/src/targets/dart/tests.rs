//! Unit tests: render a small API that exercises every C ABI revision 4
//! shape the Dart bindings distinguish and assert the key pieces of each
//! contract.

use camino::Utf8Path;
use weaveffi_model::contract::entries;
use weaveffi_model::ir::Api;
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

use crate::backend::LanguageBackend;
use crate::package::{FileContent, PackageContext};
use crate::platform::{BinarySet, NativeBinary, Platform};

use super::{DartConfig, DartGenerator, MIN_DART_SDK};

/// One module with an error domain carrying a field; a C-style enum; a
/// record carrying objects; an interface with a `new` constructor, a
/// method, and a throwing static; nullable objects; an iterator of objects;
/// a cancellable async function; composites; and a callback interface whose
/// methods take every argument family and return a direct value, an enum, a
/// string, a record, an object, and an optional object, one of them
/// `throws`, plus a void method, passed both required and optional.
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
          - { name: on_done, params: [{ name: summary, type: string }, { name: cart, type: "Cart?" }] }
    functions:
      - { name: find_cart, params: [{ name: current, type: "Cart?" }], return: "Cart?" }
      - { name: all_carts, return: "iter<Cart>" }
      - { name: watch, params: [{ name: watcher, type: Watcher }] }
      - { name: maybe_watch, params: [{ name: watcher, type: "Watcher?" }] }
      - { name: echo, params: [{ name: text, type: string }], return: string }
      - { name: tally, params: [{ name: counts, type: "{string:[i32?]}" }], return: "[Order]" }
      - { name: wait, params: [{ name: ms, type: i64 }], return: i64, async: true, cancellable: true }
      - { name: checkout, params: [{ name: order, type: Order }], return: Order, throws: true }
"#;

fn model() -> Model {
    let api: Api = serde_yaml::from_str(FIXTURE).unwrap();
    validate(&api, &Identity::named("shop_kit"), None).unwrap()
}

fn render() -> String {
    let files = DartGenerator.files(&model(), Utf8Path::new("out"), &DartConfig::default());
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "out/dart/lib/shop_kit.dart",
            "out/dart/pubspec.yaml",
            "out/dart/README.md"
        ]
    );
    files[0].contents.clone()
}

fn assert_has(out: &str, needle: &str) {
    assert!(out.contains(needle), "missing `{needle}` in:\n{out}");
}

/// The packaged library of the fixture, bundling every platform.
fn packaged() -> String {
    let mut binaries = BinarySet::new("shop_kit");
    for p in Platform::ALL {
        binaries.insert(NativeBinary::new(p, format!("/prebuilt/{}/lib", p.id())));
    }
    let ctx = PackageContext::new(&binaries);
    let artifacts = DartGenerator
        .package(&model(), &ctx, &DartConfig::default())
        .expect("dart packages");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].path, "dart/shop_kit");
    let files = &artifacts[0].files;
    let bundled: Vec<&str> = files
        .iter()
        .filter(|f| f.is_binary())
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(bundled.len(), Platform::DESKTOP.len());
    for p in [
        Platform::AndroidArm64,
        Platform::AndroidX64,
        Platform::Wasm32,
    ] {
        assert!(
            !bundled.iter().any(|b| b.contains(p.id())),
            "{} bundled",
            p.id()
        );
    }
    files
        .iter()
        .find(|f| f.path.as_str() == "lib/shop_kit.dart")
        .and_then(|f| match &f.content {
            FileContent::Text(s) => Some(s.clone()),
            _ => None,
        })
        .expect("packaged source")
}

#[test]
fn names_come_from_the_identity() {
    let model = model();
    let files = DartGenerator.files(&model, Utf8Path::new("out"), &DartConfig::default());
    assert_has(&files[1].contents, "name: shop_kit\n");
    assert_has(
        &files[2].contents,
        "import 'package:shop_kit/shop_kit.dart';",
    );

    let config = DartConfig {
        name: Some("custom".into()),
        ..DartConfig::default()
    };
    let files = DartGenerator.files(&model, Utf8Path::new("out"), &config);
    assert_eq!(files[0].path.as_str(), "out/dart/lib/custom.dart");
    assert_has(&files[1].contents, "name: custom\n");

    let out = render();
    assert_has(&out, "Platform.environment['SHOP_KIT_LIBRARY']");
    assert_has(&out, "? 'libshop_kit.dylib'");
    assert_has(&out, "? 'shop_kit.dll'");
    assert_has(&out, ": 'libshop_kit.so';");
    assert_has(&out, "if (Platform.isIOS) return DynamicLibrary.process();");
    assert!(
        !out.contains("_openBundled"),
        "unpackaged bindings look for bundles"
    );
    assert!(!out.to_lowercase().contains("weaveffi_"), "branded symbol");
}

#[test]
fn pubspec_states_the_minimum_sdk() {
    let files = DartGenerator.files(&model(), Utf8Path::new("out"), &DartConfig::default());
    assert_has(
        &files[1].contents,
        &format!("sdk: '>={MIN_DART_SDK} <4.0.0'"),
    );
    assert_has(
        &files[2].contents,
        &format!("Dart SDK `>={MIN_DART_SDK} <4.0.0`"),
    );
}

#[test]
fn load_checks_embed_every_contract_entry() {
    let model = model();
    let out = render();
    assert_has(&out, "const int _abiVersion = 4;");
    assert_has(&out, "'shop_kit_abi_version'");
    assert_has(&out, "  ('shop_kit_shop_contract', [\n");
    let root = model.roots().next().expect("one root");
    let expected = entries(&model, root);
    assert!(expected.len() > 10, "every declaration has an entry");
    for e in expected {
        assert_has(
            &out,
            &format!("    (0x{:016x}, 0x{:016x}, '{}'),\n", e.id, e.hash, e.path),
        );
    }
    assert_has(&out, "$path is missing from the library");
    assert_has(&out, "$path changed since these bindings were generated");
    assert_has(&out, "final class NativeLibraryError extends Error {");
    assert!(
        !out.contains("checksum"),
        "the revision 3 checksum survives"
    );
}

#[test]
fn strings_cross_as_pointer_and_length() {
    let out = render();
    assert_has(
        &out,
        "Pointer<Uint8> Function(Pointer<Uint8>, Size, Pointer<Size>, Pointer<_Error>)",
    );
    assert_has(&out, "final _textBytes = utf8.encode(text);");
    assert_has(
        &out,
        "_shopKitShopEcho(_stage(_arena, _textBytes), _textBytes.length, _outLen, _err)",
    );
    assert_has(&out, "return _takeString(_result, _outLen.value);");
    assert_has(&out, "'shop_kit_free_bytes'");
    assert!(!out.contains("dealloc"), "the revision 3 dealloc survives");
}

#[test]
fn composites_share_one_codec_per_type() {
    let out = render();
    let name = "_packMapOfStringToListOfOptionalOfI32";
    assert_eq!(out.matches(&format!("void {name}(")).count(), 1);
    assert_has(
        &out,
        "    w.writeMap(v, (k) => w.writeString(k), (e) => _packListOfOptionalOfI32(w, e));",
    );
    assert_has(
        &out,
        &format!("final _countsBytes = _encode(counts, {name});"),
    );
    assert_has(
        &out,
        "return _decode(_takeBytes(_result, _outLen.value), _unpackListOfOrder);",
    );
    assert_has(&out, "  _packListOfCart(w, v.history);");
    assert_has(&out, "      history: _unpackListOfCart(r),");
    assert!(
        !out.contains("for (var i = r."),
        "a call site inlines a loop"
    );
}

#[test]
fn records_compare_by_value() {
    let out = render();
    assert_has(&out, "final class Order {");
    assert_has(&out, "      _deepEquals(history, other.history) &&");
    assert_has(&out, "      note == other.note;");
    assert_has(
        &out,
        "int get hashCode => Object.hashAll([Order, cart, _deepHash(history), note]);",
    );
    assert_has(
        &out,
        "String toString() => 'Order(cart: $cart, history: $history, note: $note)';",
    );
}

#[test]
fn interface_class_owns_one_reference() {
    let out = render();
    assert_has(&out, "final class Cart extends _NativeObject {");
    assert_has(&out, "('shop_kit_shop_Cart_clone')");
    assert_has(&out, "('shop_kit_shop_Cart_destroy')");
    assert_has(
        &out,
        "NativeFinalizer get _finalizer => _shopKitShopCartDestroyFinalizer;",
    );
    // A dispose during an in-flight call defers the release.
    assert_has(&out, "if (_calls == 0) _release();");
    assert_has(&out, "if (--_calls == 0 && _disposed) _release();");
    assert_has(&out, "factory Cart(String owner) {");
    assert_has(&out, "return Cart._(_result);");
    assert_has(&out, "final _self = _enter();");
    assert_has(&out, "static Cart restore(int id) {");
}

#[test]
fn throwing_calls_map_the_domain_and_others_trap() {
    let out = render();
    assert_has(&out, "sealed class ShopException extends NativeException {");
    assert_has(
        &out,
        "final class OutOfStockException extends ShopException {",
    );
    assert_has(
        &out,
        "return _decode(payload, (r) => OutOfStockException(r.readString(), message));",
    );
    assert_has(&out, "return _runtimeException(code, message, payload);");
    // `restore` throws; `echo` can't, so a failure there is a trap.
    let restore = &out[out.find("static Cart restore").unwrap()..];
    assert_has(&restore[..300], "_check(_err, _mapShopException);");
    let echo = &out[out.find("String echo(String text)").unwrap()..];
    assert_has(&echo[..300], "_check(_err);");
    assert_has(&out, "final class NativeError extends Error {");
    assert_has(&out, ": NativeError(code, message);");
}

#[test]
fn nullable_objects_are_borrowed_and_adopted() {
    let out = render();
    assert_has(&out, "Cart? findCart(Cart? current) {");
    assert_has(
        &out,
        "final _currentPtr = current == null ? nullptr : _borrow(_arena, current);",
    );
    assert_has(&out, "return _result == nullptr ? null : Cart._(_result);");
}

#[test]
fn objects_inside_records_are_cloned_tokens() {
    let out = render();
    assert_has(&out, "w.writeU64(v.cart._cloneRef().address);");
    assert_has(
        &out,
        "cart: Cart._(Pointer<Void>.fromAddress(r.readU64())),",
    );
}

#[test]
fn object_iterator_adopts_each_element() {
    let out = render();
    assert_has(&out, "Iterable<Cart> allCarts() sync* {");
    assert_has(
        &out,
        "_shopKitShopAllCartsIteratorNext(_iter, _outItem.cast(), _err)",
    );
    assert_has(&out, "yield Cart._(_outItem.cast<Pointer<Void>>().value);");
    assert_has(
        &out,
        "_shopKitShopAllCartsIteratorDestroyFinalizer.attach(_anchor, _iter, detach: _anchor);",
    );
    assert_has(&out, "_shopKitShopAllCartsIteratorDestroy(_iter);");
}

#[test]
fn cancellable_async_binds_a_native_token() {
    let out = render();
    assert_has(
        &out,
        "Future<int> wait(int ms, {CancelToken? cancelToken}) {",
    );
    assert_has(&out, "final _cancel = _NativeCancel.bind(cancelToken);");
    assert_has(&out, "NativeCallable<_ShopKitShopWaitCallback>.listener((");
    assert_has(
        &out,
        "_shopKitShopWait(ms, _cancel?.pointer ?? nullptr, _callback.nativeFunction, nullptr);",
    );
    assert_has(
        &out,
        "if (error != nullptr) throw _takeAsyncError(error, _trap);",
    );
    assert_has(&out, "'shop_kit_cancel_token_create'");
    assert_has(&out, "class CancelledException extends NativeException {");
}

#[test]
fn callback_vtable_starts_with_the_header() {
    let out = render();
    assert_has(
        &out,
        "final class _WatcherVtable extends Struct {\n  @Uint32()\n  external int size;\n  \
         @Uint32()\n  external int flags;\n  \
         external Pointer<NativeFunction<Void Function(Pointer<Void>)>> free;\n  \
         external Pointer<NativeFunction<_ShopKitShopWatcherOnEvent>> onEvent;",
    );
    assert_has(
        &out,
        "      ..size = sizeOf<_WatcherVtable>()\n      ..flags = 0\n      ..free = _callbackFree\n",
    );
}

#[test]
fn callback_arguments_are_copied_or_adopted() {
    let out = render();
    // The object first, then the buffer (which may carry tokens).
    assert_has(
        &out,
        "    final _cart = Cart._(cart);\n    \
         final _order = _decode(_copyBytes(orderPtr, orderLen), _unpackOrder);\n    \
         return (_callbackTarget(ctx) as Watcher).onEvent(_readString(namePtr, nameLen), count, _order, _cart);",
    );
    assert_has(
        &out,
        "  } catch (e) {\n    _foreignError(outErr, e);\n  }\n  return false;",
    );
}

#[test]
fn callback_returns_cover_every_family() {
    let out = render();
    assert_has(
        &out,
        "return (_callbackTarget(ctx) as Watcher).mood().value;",
    );
    assert_has(&out, "_handOver(utf8.encode(result), outPtr, outLen);");
    assert_has(
        &out,
        "_handOver(_encode(result, _packOptionalOfOrder), outPtr, outLen);",
    );
    assert_has(&out, "    return result._cloneRef();\n");
    assert_has(&out, "    return result?._cloneRef() ?? nullptr;\n");
    assert_has(
        &out,
        "..mood = _pin(NativeCallable<_ShopKitShopWatcherMood>.isolateLocal(_watcherMood, exceptionalReturn: 0))",
    );
    assert_has(
        &out,
        "..favorite = _pin(NativeCallable<_ShopKitShopWatcherFavorite>.isolateLocal(_watcherFavorite))",
    );
    assert_has(
        &out,
        "typedef _ShopKitShopWatcherLabel = Void Function(Pointer<Void>, Pointer<Pointer<Uint8>>, Pointer<Size>, Pointer<_Error>);",
    );
    assert_has(&out, "'shop_kit_alloc'");
    assert_has(&out, "outPtr.value = run;");
}

#[test]
fn throwing_callback_methods_report_the_domain_error() {
    let out = render();
    assert_has(
        &out,
        "  } on ShopException catch (e) {\n    \
         _failCallback(outErr, e.code, e.message, _payloadOfShopException(e));\n  \
         } catch (e) {\n    _foreignError(outErr, e);\n  }",
    );
    assert_eq!(
        out.matches("} on ShopException catch (e) {").count(),
        1,
        "only `label` throws"
    );
    assert_has(
        &out,
        "Uint8List? _payloadOfShopException(ShopException e) {",
    );
    assert_has(
        &out,
        "    case OutOfStockException():\n      final w = _BufferWriter();\n      \
         w.writeString(e.sku);\n      return w.takeBytes();",
    );
    assert_has(&out, "    case ClosedException():\n      return null;");
    assert_has(&out, "'shop_kit_error_set_payload'");
}

#[test]
fn void_methods_are_forwarded_from_any_thread() {
    let out = render();
    assert_has(
        &out,
        "..onDone = _pin(NativeCallable<_ShopKitShopWatcherOnDone>.isolateGroupBound(_watcherOnDone));",
    );
    assert_has(
        &out,
        "  _CallbackMessage(ctx, 6, 2)\n      ..bytes(summaryPtr, summaryLen)\n      \
         ..pointer(cart)\n      ..send();",
    );
    assert_has(
        &out,
        "      final a3 = a3Address == 0 ? null : Cart._(Pointer<Void>.fromAddress(a3Address));\n      \
         final a2 = utf8.decode(message[2]! as Uint8List);\n      target.onDone(a2, a3);",
    );
}

#[test]
fn callback_parameters_register_the_implementation() {
    let out = render();
    assert_has(
        &out,
        "final _watcherCtx = _registerCallback(watcher, _watcherDispatch);\n  \
         _shopKitShopWatch(_watcherCtx, _watcherVtable.cast<Void>(), _err);",
    );
    // An optional callback passes a null vtable for none.
    assert_has(
        &out,
        "final _watcherCtx = watcher == null ? nullptr : _registerCallback(watcher, _watcherDispatch);",
    );
    assert_has(
        &out,
        "_shopKitShopMaybeWatch(_watcherCtx, watcher == null ? nullptr : _watcherVtable.cast<Void>(), _err);",
    );
    // No call may be a leaf call when a callback can re-enter Dart.
    assert!(!out.contains("isLeaf: true);\n\nfinal _shopKitShop"));
}

#[test]
fn calls_are_leaf_calls_without_callbacks() {
    let api: Api = serde_yaml::from_str(
        r#"
version: "0.11.0"
modules:
  - name: math
    functions:
      - { name: add, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }
"#,
    )
    .unwrap();
    let model = validate(&api, &Identity::named("calc"), None).unwrap();
    let files = DartGenerator.files(&model, Utf8Path::new("out"), &DartConfig::default());
    let out = &files[0].contents;
    assert_has(out, "Int32 Function(Int32, Int32, Pointer<_Error>),");
    assert_has(out, "('calc_math_add', isLeaf: true);");
    assert!(
        !out.contains("dart:isolate"),
        "isolate glue without callbacks"
    );
}

#[test]
fn packaged_loader_resolves_the_bundle_from_the_package() {
    let out = packaged();
    assert_has(&out, "import 'dart:io' show Directory, File, Platform;");
    assert_has(
        &out,
        "      Abi.macosArm64 => 'native/darwin-arm64/libshop_kit.dylib',",
    );
    assert_has(
        &out,
        "      Abi.windowsX64 => 'native/windows-x64/shop_kit.dll',",
    );
    assert_has(
        &out,
        "      Abi.linuxArm64 => 'native/linux-arm64/libshop_kit.so',",
    );
    assert_has(&out, "Uri.parse('package:shop_kit/shop_kit.dart')");
    assert_has(
        &out,
        "for (final root in [_packageRoot(), Directory.current.path]) {",
    );
    assert_has(
        &out,
        "  final bundled = _openBundled();\n  if (bundled != null) return bundled;\n",
    );
    assert_has(&out, "Platform.environment['SHOP_KIT_LIBRARY']");
}

#[test]
fn every_line_is_free_of_trailing_whitespace() {
    for out in [render(), packaged()] {
        for (i, line) in out.lines().enumerate() {
            assert_eq!(
                line.trim_end(),
                line,
                "trailing whitespace on line {}",
                i + 1
            );
        }
    }
}
