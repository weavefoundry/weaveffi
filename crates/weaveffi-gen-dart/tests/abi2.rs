//! ABI revision 2 rendering checks: reference-counted objects, nullable
//! objects, object tokens inside value buffers, iterators of objects, and
//! callback-interface vtables with their trampolines.

use camino::Utf8Path;
use weaveffi_core::backend::LanguageBackend;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::PackageContext;
use weaveffi_core::platform::{BinarySet, Platform};
use weaveffi_core::validate::validate_api;
use weaveffi_gen_dart::{DartConfig, DartGenerator};
use weaveffi_ir::ir::{
    Api, CallbackInterfaceDef, EnumDef, EnumVariant, Function, InterfaceDef, Module, Param,
    StructDef, StructField, TypeRef, CURRENT_SCHEMA_VERSION,
};

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

/// One module exercising every revision-2 shape: an interface with a
/// constructor and a method, `Interface?` in and out, a record with an
/// `Interface` field and a `[Interface]` field, an iterator over interface
/// elements, a callback interface with string, `i32`, record, and object
/// parameters (one `bool` return, one `void`), and a function taking it.
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

fn render() -> String {
    let api = validate_api(fixture(), None).expect("fixture validates");
    let config = DartConfig::default();
    let model = BindingModel::build(&api, DartGenerator.prefix(&config));
    let files = DartGenerator.files(&api, &model, Utf8Path::new("out"), &config);
    files
        .into_iter()
        .find(|f| f.path.as_str().ends_with("weaveffi.dart"))
        .expect("primary source")
        .contents
}

fn assert_has(src: &str, needle: &str) {
    assert!(src.contains(needle), "missing `{needle}` in:\n{src}");
}

#[test]
fn objects_are_reference_counted_wrappers_with_finalizers() {
    let src = render();
    assert_has(&src, "class Ticker implements Finalizable {");
    assert_has(&src, "_lib.lookupFunction<\n    _NativeWeaveffiBusTickerClone, _DartWeaveffiBusTickerClone>('weaveffi_bus_Ticker_clone')");
    assert_has(&src, "_lib.lookupFunction<\n    _NativeWeaveffiBusTickerDestroy, _DartWeaveffiBusTickerDestroy>('weaveffi_bus_Ticker_destroy')");
    assert_has(
        &src,
        "final _weaveffiBusTickerDestroyFinalizer = NativeFinalizer(\n    _lib.lookup<NativeFunction<Void Function(Pointer<Void>)>>('weaveffi_bus_Ticker_destroy'));",
    );
    // Adopting attaches the finalizer; dispose detaches and destroys once.
    assert_has(&src, "Ticker._(this._ptr) {\n    _weaveffiBusTickerDestroyFinalizer.attach(this, _ptr, detach: this);");
    assert_has(&src, "void dispose() {\n    if (_disposed) return;\n    _disposed = true;\n    _weaveffiBusTickerDestroyFinalizer.detach(this);\n    _weaveffiBusTickerDestroy(_ptr);");
    assert_has(
        &src,
        "if (_disposed) throw StateError('Ticker used after dispose()');",
    );
    assert_has(
        &src,
        "Pointer<Void> _cloneRef() => _weaveffiBusTickerClone(_handle);",
    );
    // Constructor and method render as factory and instance method.
    assert_has(&src, "factory Ticker(int start) {");
    assert_has(&src, "return Ticker._(result);");
    assert_has(&src, "int value() {");
    assert_has(&src, "_weaveffiBusTickerValue(_handle, err)");
}

#[test]
fn nullable_objects_are_nullable_pointers_both_ways() {
    let src = render();
    assert_has(&src, "Ticker? lookup(Ticker? fallback) {");
    assert_has(
        &src,
        "_weaveffiBusLookup(fallback?._handle ?? nullptr, err)",
    );
    assert_has(&src, "return result == nullptr ? null : Ticker._(result);");
    assert_has(&src, "Returns `null` when the producer reports no object.");
}

#[test]
fn objects_inside_records_are_cloned_tokens() {
    let src = render();
    assert_has(&src, "final Ticker primary;");
    assert_has(&src, "final List<Ticker> others;");
    // Encoding writes a fresh clone, never the wrapper's own pointer.
    assert_has(&src, "w.writeUint64(v.primary._cloneRef().address);");
    assert_has(&src, "w.writeUint64(t1._cloneRef().address);");
    // Decoding adopts each token into a new wrapper.
    assert_has(
        &src,
        "primary: Ticker._(Pointer<Void>.fromAddress(r.readUint64())),",
    );
    assert_has(
        &src,
        "others: List<Ticker>.generate(r.readLength(), (_) => Ticker._(Pointer<Void>.fromAddress(r.readUint64()))),",
    );
}

#[test]
fn iterators_adopt_object_elements() {
    let src = render();
    assert_has(&src, "Iterable<Ticker> tickers() sync* {");
    assert_has(&src, "final outItem = calloc<Pointer<Void>>();");
    assert_has(
        &src,
        "final itemPtr = outItem.value;\n      yield Ticker._(itemPtr);",
    );
    assert_has(&src, "_weaveffiBusTickersIteratorDestroy(iter);");
    assert_has(&src, "Each yielded element is owned by the caller");
}

#[test]
fn callback_interfaces_render_abstract_class_vtable_and_trampolines() {
    let src = render();
    // Consumer-facing abstract class.
    assert_has(&src, "abstract class Subscriber {");
    assert_has(
        &src,
        "bool onMessage(String text, int weight, Envelope envelope);",
    );
    assert_has(&src, "void onTicker(Ticker ticker, Ticker? alt);");
    assert_has(&src, "Priority classify(int weight);");
    assert_has(
        &src,
        "Dart limitation: methods are bound with `NativeCallable.isolateLocal`",
    );

    // The vtable layout: one entry per method in order, then free.
    assert_has(
        &src,
        "final class _SubscriberVtableStruct extends Struct {\n  external Pointer<NativeFunction<_NativeSubscriberVtOnMessage>> onMessage;\n  external Pointer<NativeFunction<_NativeSubscriberVtOnTicker>> onTicker;\n  external Pointer<NativeFunction<_NativeSubscriberVtClassify>> classify;\n  external Pointer<NativeFunction<_NativeSubscriberVtFree>> free;\n}",
    );
    // A C-style enum return crosses as its discriminant.
    assert_has(&src, "typedef _NativeSubscriberVtClassify = Int32 Function(Pointer<Void>, Int32, Pointer<_WeaveFFIError>);");
    assert_has(&src, "return impl.classify(weight).value;");
    assert_has(
        &src,
        "typedef _NativeSubscriberVtOnMessage = Bool Function(Pointer<Void>, Pointer<Utf8>, Int32, Pointer<Uint8>, Size, Pointer<_WeaveFFIError>);",
    );
    assert_has(
        &src,
        "typedef _NativeSubscriberVtOnTicker = Void Function(Pointer<Void>, Pointer<Void>, Pointer<Void>, Pointer<_WeaveFFIError>);",
    );

    // Trampolines decode borrowed args, adopt objects, and trap exceptions.
    assert_has(
        &src,
        "bool _subscriberVtOnMessage(Pointer<Void> ctx, Pointer<Utf8> text, int weight, Pointer<Uint8> envelopePtr, int envelopeLen, Pointer<_WeaveFFIError> outErr) {",
    );
    assert_has(&src, "final impl = _callbackFor(ctx) as Subscriber;");
    assert_has(
        &src,
        "final envelopeData = _copyNativeBytes(envelopePtr, envelopeLen);",
    );
    assert_has(
        &src,
        "final envelopeValue = _unpackEnvelope(envelopeReader);",
    );
    assert_has(
        &src,
        "return impl.onMessage(text == nullptr ? '' : text.toDartString(), weight, envelopeValue);",
    );
    assert_has(
        &src,
        "} catch (e) {\n    _foreignError(outErr, e);\n    return false;\n  }",
    );
    // Object arguments are adopted before the implementation lookup so a
    // failure never leaks the transferred reference.
    assert_has(
        &src,
        "final tickerValue = Ticker._(ticker);\n    final altValue = alt == nullptr ? null : Ticker._(alt);\n    final impl = _callbackFor(ctx) as Subscriber;\n    impl.onTicker(tickerValue, altValue);",
    );
    assert_has(&src, "} catch (e) {\n    _foreignError(outErr, e);\n  }");
    // free drops the handle-table entry.
    assert_has(
        &src,
        "void _subscriberVtFree(Pointer<Void> ctx) {\n  _callbackTable.remove(ctx.address);",
    );

    // Exactly one static vtable, calloc-allocated, pinned callables.
    assert_eq!(
        src.matches("final Pointer<_SubscriberVtableStruct> _SubscriberVtable = () {")
            .count(),
        1
    );
    assert_has(&src, "final vt = calloc<_SubscriberVtableStruct>();");
    assert_has(
        &src,
        "vt.ref.onMessage = _pinCallable(NativeCallable<_NativeSubscriberVtOnMessage>.isolateLocal(\n      _subscriberVtOnMessage, exceptionalReturn: false));",
    );
    assert_has(
        &src,
        "vt.ref.onTicker = _pinCallable(NativeCallable<_NativeSubscriberVtOnTicker>.isolateLocal(\n      _subscriberVtOnTicker));",
    );
    assert_has(&src, "vt.ref.free = _pinCallable(NativeCallable<_NativeSubscriberVtFree>.listener(\n      _subscriberVtFree));");
    assert_has(&src, "callable.keepIsolateAlive = false;");

    // Passing an implementation: handle-table key as ctx plus the vtable.
    assert_has(&src, "void subscribe(Subscriber listener) {");
    assert_has(
        &src,
        "_weaveffiBusSubscribe(_registerCallback(listener), _SubscriberVtable.cast<Void>(), err)",
    );
}

#[test]
fn runtime_binds_error_set_and_foreign_code() {
    let src = render();
    assert_has(&src, "typedef _NativeWeaveffiErrorSet = Void Function(Pointer<_WeaveFFIError>, Int32, Pointer<Utf8>);");
    assert_has(&src, "'weaveffi_error_set'");
    assert_has(&src, "static const int foreignCode = -4;");
    assert_has(
        &src,
        "_weaveffiErrorSet(outErr, WeaveFFIException.foreignCode, message);",
    );
    assert_has(&src, "const int _abiVersion = 2;");
    for gone in [
        "arena",
        "weaveffi_handle_t",
        "TypedHandle",
        "register_",
        "_listenerCallables",
    ] {
        assert!(!src.contains(gone), "stale `{gone}` in output");
    }
}

#[test]
fn package_bundles_only_desktop_binaries() {
    let api = validate_api(fixture(), None).expect("fixture validates");
    let config = DartConfig::default();
    let model = BindingModel::build(&api, DartGenerator.prefix(&config));
    let mut binaries = BinarySet::new("bus");
    for p in Platform::ALL {
        binaries.insert(p, format!("/prebuilt/{}/lib", p.id()));
    }
    let ctx = PackageContext {
        binaries: &binaries,
        input_basename: Some("bus.yml"),
    };
    let files = DartGenerator
        .package(&api, &model, &ctx, Utf8Path::new("out"), &config)
        .expect("dart packages");
    let bundled: Vec<&str> = files
        .iter()
        .filter(|f| f.is_binary())
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(bundled.len(), Platform::DESKTOP.len());
    for p in Platform::DESKTOP {
        assert!(
            bundled
                .iter()
                .any(|b| b.contains(&format!("native/{}/", p.id()))),
            "{} missing from {bundled:?}",
            p.id()
        );
    }
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
        .find(|f| f.path.as_str().ends_with("weaveffi.dart"))
        .and_then(|f| match &f.content {
            weaveffi_core::package::FileContent::Text(s) => Some(s.as_str()),
            weaveffi_core::package::FileContent::Copy(_) => None,
        })
        .expect("packaged source");
    assert_has(
        src,
        "'native/darwin-arm64/libbus.dylib', 'native/darwin-x64/libbus.dylib', 'libbus.dylib'",
    );
    assert_has(
        src,
        "'native/linux-x64/libbus.so', 'native/linux-arm64/libbus.so', 'libbus.so'",
    );
    assert_has(src, "'native/windows-x64/bus.dll', 'bus.dll'");
}
