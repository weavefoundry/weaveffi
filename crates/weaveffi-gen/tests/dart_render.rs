//! Dart rendering checks: identity-driven names and loading, ABI 3 string
//! and runtime symbols, the load-time contract check, object lifetimes,
//! nullable objects, object tokens in value buffers, iterators, async
//! cancellation, callback-interface vtables (isolate-local value methods and
//! forwarded void methods), and packaging.

use camino::Utf8Path;
use weaveffi_gen::backend::LanguageBackend;
use weaveffi_gen::package::{FileContent, PackageContext};
use weaveffi_gen::platform::{BinarySet, Platform};
use weaveffi_gen::targets::dart::{DartConfig, DartGenerator};
use weaveffi_model::ir::{
    Api, CallbackInterfaceDef, EnumDef, EnumVariant, Function, InterfaceDef, Module, Param,
    StructDef, StructField, TypeRef, CURRENT_SCHEMA_VERSION,
};
use weaveffi_model::model::BindingModel;
use weaveffi_model::pkg::Identity;
use weaveffi_model::resolved::ResolvedApi;
use weaveffi_model::validate::validate_api;

fn param(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.into(),
        ty,
        doc: None,
    }
}

fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
    Function {
        name: name.into(),
        params,
        returns,
        doc: None,
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
    }
}

fn field(name: &str, ty: TypeRef) -> StructField {
    StructField {
        name: name.into(),
        ty,
        doc: None,
    }
}

fn named(name: &str) -> TypeRef {
    TypeRef::Named(name.into())
}

/// One module exercising every shape: an interface, `Interface?` both ways,
/// a record holding objects, an iterator of objects, a string echo, a
/// cancellable async call, and a callback interface with value-returning
/// and void methods.
fn fixture() -> Api {
    Api {
        version: CURRENT_SCHEMA_VERSION.into(),
        modules: vec![Module {
            name: "bus".into(),
            doc: None,
            functions: vec![
                func(
                    "lookup",
                    vec![param(
                        "fallback",
                        TypeRef::Optional(Box::new(named("Ticker"))),
                    )],
                    Some(TypeRef::Optional(Box::new(named("Ticker")))),
                ),
                func(
                    "tickers",
                    vec![],
                    Some(TypeRef::Iterator(Box::new(named("Ticker")))),
                ),
                func(
                    "echo",
                    vec![param("text", TypeRef::StringUtf8)],
                    Some(TypeRef::StringUtf8),
                ),
                func(
                    "subscribe",
                    vec![param("listener", named("Subscriber"))],
                    None,
                ),
                func(
                    "describe",
                    vec![param("env", named("Envelope"))],
                    Some(named("Envelope")),
                ),
                Function {
                    r#async: true,
                    cancellable: true,
                    ..func(
                        "fetch",
                        vec![param("id", TypeRef::I64)],
                        Some(TypeRef::Optional(Box::new(named("Ticker")))),
                    )
                },
            ],
            interfaces: vec![InterfaceDef {
                name: "Ticker".into(),
                doc: Some("A counter.".into()),
                deprecated: None,
                constructors: vec![func("new", vec![param("start", TypeRef::I64)], None)],
                methods: vec![func("value", vec![], Some(TypeRef::I64))],
                statics: vec![],
            }],
            callback_interfaces: vec![CallbackInterfaceDef {
                name: "Subscriber".into(),
                doc: Some("Receives bus events.".into()),
                deprecated: None,
                methods: vec![
                    func(
                        "on_message",
                        vec![
                            param("text", TypeRef::StringUtf8),
                            param("weight", TypeRef::I32),
                            param("envelope", named("Envelope")),
                        ],
                        Some(TypeRef::Bool),
                    ),
                    func(
                        "on_ticker",
                        vec![
                            param("label", TypeRef::StringUtf8),
                            param("ticker", named("Ticker")),
                            param("alt", TypeRef::Optional(Box::new(named("Ticker")))),
                        ],
                        None,
                    ),
                    func(
                        "classify",
                        vec![param("weight", TypeRef::I32)],
                        Some(named("Priority")),
                    ),
                ],
            }],
            structs: vec![StructDef {
                name: "Envelope".into(),
                doc: None,
                deprecated: None,
                fields: vec![
                    field("topic", TypeRef::StringUtf8),
                    field("primary", named("Ticker")),
                    field("others", TypeRef::List(Box::new(named("Ticker")))),
                ],
            }],
            enums: vec![EnumDef {
                name: "Priority".into(),
                doc: None,
                deprecated: None,
                variants: vec![
                    EnumVariant {
                        name: "Low".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "High".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            errors: None,
            modules: vec![],
        }],
    }
}

fn api() -> ResolvedApi {
    validate_api(fixture(), None)
        .expect("fixture validates")
        .with_identity(Identity::named("bus-kit"))
}

fn render() -> String {
    let api = api();
    let model = BindingModel::build(&api);
    let files = DartGenerator.files(&api, &model, Utf8Path::new("out"), &DartConfig::default());
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "out/dart/lib/bus_kit.dart",
            "out/dart/pubspec.yaml",
            "out/dart/README.md"
        ]
    );
    files[0].contents.clone()
}

fn assert_has(src: &str, needle: &str) {
    assert!(src.contains(needle), "missing `{needle}` in:\n{src}");
}

#[test]
fn identity_names_the_package_and_library() {
    let api = api();
    let model = BindingModel::build(&api);
    let files = DartGenerator.files(&api, &model, Utf8Path::new("out"), &DartConfig::default());
    assert_has(&files[1].contents, "name: bus_kit\n");
    assert_has(&files[2].contents, "import 'package:bus_kit/bus_kit.dart';");

    let config = DartConfig {
        package_name: Some("custom".into()),
        ..DartConfig::default()
    };
    let files = DartGenerator.files(&api, &model, Utf8Path::new("out"), &config);
    assert_eq!(files[0].path.as_str(), "out/dart/lib/custom.dart");
    assert_has(&files[1].contents, "name: custom\n");

    let src = render();
    assert_has(&src, "Platform.environment['BUS_KIT_LIBRARY']");
    assert_has(&src, "const <String>['libbus_kit.dylib']");
    assert_has(&src, "const <String>['bus_kit.dll']");
    assert_has(&src, "if (Platform.isIOS) return DynamicLibrary.process();");
    assert!(
        !src.to_lowercase().contains("weaveffi_"),
        "branded symbol in output"
    );
}

#[test]
fn library_load_checks_abi_and_checksums() {
    let api = api();
    let model = BindingModel::build(&api);
    let checksum = model.modules[0].checksum.expect("root checksum");
    let src = render();
    assert_has(&src, "const int _abiVersion = 3;");
    assert_has(&src, "'bus_kit_abi_version'");
    assert_has(
        &src,
        &format!("  ('bus', 'bus_kit_bus_checksum', 0x{checksum:016x}),"),
    );
    assert_has(&src, "regenerate the bindings");
}

#[test]
fn strings_cross_as_pointer_and_length() {
    let src = render();
    assert_has(
        &src,
        "Pointer<Uint8> Function(Pointer<Uint8>, Size, Pointer<Size>, Pointer<_Error>)",
    );
    assert_has(&src, "final _textBytes = utf8.encode(text);");
    assert_has(
        &src,
        "_busKitBusEcho(_stage(_arena, _textBytes), _textBytes.length, _outLen, _err)",
    );
    assert_has(&src, "return _takeString(_result, _outLen.value);");
    assert_has(&src, "'bus_kit_free_bytes'");
    assert!(!src.contains("free_string"), "ABI 2 free_string in output");
}

#[test]
fn objects_are_guarded_reference_counted_wrappers() {
    let src = render();
    assert_has(&src, "final class Ticker extends _NativeObject {");
    assert_has(&src, "('bus_kit_bus_Ticker_clone')");
    assert_has(&src, "('bus_kit_bus_Ticker_destroy')");
    assert_has(
        &src,
        "NativeFinalizer get _finalizer => _busKitBusTickerDestroyFinalizer;",
    );
    // A dispose during an in-flight call defers the release.
    assert_has(&src, "if (_calls == 0) _release();");
    assert_has(&src, "if (--_calls == 0 && _disposed) _release();");
    assert_has(&src, "factory Ticker(int start) {");
    assert_has(&src, "return Ticker._(_result);");
    assert_has(&src, "final _self = _enter();");
    assert_has(&src, "_busKitBusTickerValue(_self, _err)");
}

#[test]
fn nullable_objects_are_borrowed_and_adopted() {
    let src = render();
    assert_has(&src, "Ticker? lookup(Ticker? fallback) {");
    assert_has(
        &src,
        "final _fallbackPtr = fallback == null ? nullptr : _borrow(_arena, fallback);",
    );
    assert_has(
        &src,
        "return _result == nullptr ? null : Ticker._(_result);",
    );
}

#[test]
fn objects_inside_records_are_cloned_tokens() {
    let src = render();
    assert_has(&src, "w.writeU64(v.primary._cloneRef().address);");
    assert_has(&src, "w.writeU64(_t1._cloneRef().address);");
    assert_has(
        &src,
        "primary: Ticker._(Pointer<Void>.fromAddress(r.readU64())),",
    );
    assert_has(
        &src,
        "others: <Ticker>[for (var i = r.readLength(); i > 0; i--) Ticker._(Pointer<Void>.fromAddress(r.readU64()))],",
    );
}

#[test]
fn iterators_pull_lazily_and_destroy_once() {
    let src = render();
    assert_has(&src, "Iterable<Ticker> tickers() sync* {");
    assert_has(
        &src,
        "_busKitBusTickersIteratorNext(_iter, _outItem.cast(), _err)",
    );
    assert_has(
        &src,
        "yield Ticker._(_outItem.cast<Pointer<Void>>().value);",
    );
    assert_has(
        &src,
        "_busKitBusTickersIteratorDestroyFinalizer.attach(_anchor, _iter, detach: _anchor);",
    );
    assert_has(&src, "_busKitBusTickersIteratorDestroy(_iter);");
}

#[test]
fn cancellable_async_binds_a_native_token() {
    let src = render();
    assert_has(
        &src,
        "Future<Ticker?> fetch(int id, {CancelToken? cancelToken}) {",
    );
    assert_has(&src, "final _cancel = _NativeCancel.bind(cancelToken);");
    assert_has(&src, "NativeCallable<_BusKitBusFetchCallback>.listener((");
    assert_has(
        &src,
        "_busKitBusFetch(id, _cancel?.pointer ?? nullptr, _callback.nativeFunction, nullptr);",
    );
    assert_has(&src, "_cancel?.release();");
    assert_has(&src, "'bus_kit_cancel_token_create'");
    assert_has(&src, "class CancelledException extends NativeException {");
    assert_has(&src, "static const int cancelledCode = -5;");
}

#[test]
fn callback_interfaces_split_value_and_void_methods() {
    let src = render();
    assert_has(&src, "abstract class Subscriber {");
    assert_has(
        &src,
        "bool onMessage(String text, int weight, Envelope envelope);",
    );
    assert_has(
        &src,
        "void onTicker(String label, Ticker ticker, Ticker? alt);",
    );
    // A value-returning method is an isolate-local trampoline that reports
    // a thrown exception as the foreign code.
    assert_has(
        &src,
        "..onMessage = _pin(NativeCallable<_BusKitBusSubscriberOnMessage>.isolateLocal(_subscriberOnMessage, exceptionalReturn: false))",
    );
    assert_has(
        &src,
        "return (_callbackTarget(ctx) as Subscriber).onMessage(_readString(textPtr, textLen), weight, _decode(_copyBytes(envelopePtr, envelopeLen), _unpackEnvelope));",
    );
    assert_has(&src, "_foreignError(outErr, e);");
    assert_has(
        &src,
        "return (_callbackTarget(ctx) as Subscriber).classify(weight).value;",
    );
    // A void method is forwarded from any thread and dispatched on the
    // event loop, adopting objects before decoding the rest.
    assert_has(
        &src,
        "..onTicker = _pin(NativeCallable<_BusKitBusSubscriberOnTicker>.isolateGroupBound(_subscriberOnTicker))",
    );
    assert_has(
        &src,
        "  _CallbackMessage(ctx, 1, 3)\n      ..bytes(labelPtr, labelLen)\n      ..pointer(ticker)\n      ..pointer(alt)\n      ..send();",
    );
    assert_has(
        &src,
        "final _a3Address = message[3]! as int;\n      final _a3 = Ticker._(Pointer<Void>.fromAddress(_a3Address));",
    );
    assert_has(&src, "final _a2 = utf8.decode(message[2]! as Uint8List);");
    assert_has(&src, "target.onTicker(_a2, _a3, _a4);");
    assert_has(&src, "..free = _callbackFree;");
    // Passing an implementation registers it last.
    assert_has(
        &src,
        "final _listenerCtx = _registerCallback(listener, _subscriberDispatch);",
    );
    assert_has(
        &src,
        "_busKitBusSubscribe(_listenerCtx, _subscriberVtable.cast(), _err);",
    );
    // No call may be a leaf call when a callback can re-enter Dart.
    assert!(!src.contains("isLeaf: true);\n\nfinal _busKitBus"));
    assert_has(&src, "'bus_kit_error_set'");
}

#[test]
fn package_bundles_only_desktop_binaries() {
    let api = api();
    let model = BindingModel::build(&api);
    let mut binaries = BinarySet::new("bus_kit");
    for p in Platform::ALL {
        binaries.insert(p, format!("/prebuilt/{}/lib", p.id()));
    }
    let ctx = PackageContext {
        binaries: &binaries,
        input_basename: Some("bus.yml"),
    };
    let files = DartGenerator
        .package(
            &api,
            &model,
            &ctx,
            Utf8Path::new("out"),
            &DartConfig::default(),
        )
        .expect("dart packages");
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
    let src = files
        .iter()
        .find(|f| f.path.as_str().ends_with("lib/bus_kit.dart"))
        .and_then(|f| match &f.content {
            FileContent::Text(s) => Some(s.as_str()),
            FileContent::Copy(_) => None,
        })
        .expect("packaged source");
    assert_has(
        src,
        "'native/darwin-arm64/libbus_kit.dylib', 'native/darwin-x64/libbus_kit.dylib', 'libbus_kit.dylib'",
    );
    assert_has(src, "'native/windows-x64/bus_kit.dll', 'bus_kit.dll'");
    assert_has(src, "Platform.environment['BUS_KIT_LIBRARY']");
}
