//! ABI revision 2 rendering checks: reference-counted object wrappers,
//! nullable objects, object tokens inside value buffers, iterators of objects,
//! callback-interface vtables with their trampolines, the Emscripten
//! capability ceiling, and the npm package layout.

use camino::Utf8Path;
use weaveffi_core::backend::LanguageBackend;
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::{FileContent, PackageContext, PackagedFile};
use weaveffi_core::platform::{BinarySet, Platform};
use weaveffi_core::validate::validate_api;
use weaveffi_gen_wasm::{WasmConfig, WasmGenerator};
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
                Function {
                    r#async: true,
                    ..func(
                        "checksum",
                        vec![param("id", TypeRef::I64)],
                        Some(TypeRef::U64),
                    )
                },
            ],
            interfaces: vec![InterfaceDef {
                name: "Ticker".into(),
                doc: Some("A counter.".into()),
                deprecated: None,
                constructors: vec![func("new", vec![param("start", TypeRef::I64)], None)],
                methods: vec![
                    func("value", vec![], Some(TypeRef::I64)),
                    func("mask", vec![], Some(TypeRef::U64)),
                    func("bits", vec![], Some(TypeRef::U32)),
                ],
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
                    func(
                        "on_mask",
                        vec![param("mask", TypeRef::U64), param("bits", TypeRef::U32)],
                        None,
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

fn render_with(config: &WasmConfig) -> (String, String) {
    let api = validate_api(fixture(), None).expect("fixture validates");
    let model = BindingModel::build(&api, WasmGenerator.prefix(config));
    let files = WasmGenerator.files(&api, &model, Utf8Path::new("out"), config);
    let pick = |suffix: &str| {
        files
            .iter()
            .find(|f| f.path.as_str().ends_with(suffix))
            .unwrap_or_else(|| panic!("no {suffix} output"))
            .contents
            .clone()
    };
    (pick("weaveffi_wasm.js"), pick("weaveffi_wasm.d.ts"))
}

fn render() -> (String, String) {
    render_with(&WasmConfig::default())
}

fn assert_has(src: &str, needle: &str) {
    assert!(src.contains(needle), "missing `{needle}` in:\n{src}");
}

fn emscripten_config() -> WasmConfig {
    WasmConfig {
        emscripten: true,
        ..WasmConfig::default()
    }
}

#[test]
fn objects_are_reference_counted_wrappers_with_finalizers() {
    let (js, dts) = render();
    // The class lives inside the loader and is exposed on the module object.
    assert_has(&js, "  class Ticker {\n    constructor(start) {");
    assert_has(&js, "      Ticker: Ticker,");
    // The constructor adopts the returned reference with the destroy symbol.
    assert_has(
        &js,
        "_r = wasm.weaveffi_bus_Ticker_new(BigInt(start), _err);",
    );
    assert_has(&js, "_adopt(this, _r, wasm.weaveffi_bus_Ticker_destroy);");
    // Internal adoption path used by returns, tokens, iterators, and callbacks.
    assert_has(
        &js,
        "static _wrap(handle) {\n      return _adopt(Object.create(Ticker.prototype), handle, wasm.weaveffi_bus_Ticker_destroy);",
    );
    // Clone symbol produces the second reference for object tokens.
    assert_has(
        &js,
        "_clone() {\n      return wasm.weaveffi_bus_Ticker_clone(_borrow(this));",
    );
    // close() and Symbol.dispose both funnel into the once-only release.
    assert_has(&js, "close() {\n      _release(this);");
    assert_has(&js, "[_dispose]() {\n      _release(this);");
    assert_has(
        &js,
        "const _dispose = typeof Symbol.dispose === 'symbol' ? Symbol.dispose : Symbol.for('Symbol.dispose');",
    );
    assert_has(
        &js,
        "function _release(obj) {\n  if (!obj._handle) return;\n  if (_finalizer !== null) _finalizer.unregister(obj);\n  obj._destroy(obj._handle);\n  obj._handle = 0;\n}",
    );
    // The FinalizationRegistry backstop holds [destroy, handle], never the wrapper.
    assert_has(
        &js,
        "new FinalizationRegistry(([destroy, handle]) => destroy(handle))",
    );
    assert_has(
        &js,
        "if (_finalizer !== null) _finalizer.register(obj, [destroy, handle], obj);",
    );
    // Methods lend the pointer as the implicit self.
    assert_has(
        &js,
        "_r = wasm.weaveffi_bus_Ticker_value(_borrow(this), _err);",
    );
    assert_has(
        &js,
        "throw new WeaveFFIError(-3, 'expected a live object wrapper');",
    );

    assert_has(&dts, "export declare class Ticker {");
    assert_has(&dts, "  constructor(start: bigint);");
    assert_has(&dts, "  value(): bigint;");
    assert_has(&dts, "  close(): void;");
    assert_has(&dts, "Also reachable as `[Symbol.dispose]()` for `using`");
    assert_has(&dts, "    Ticker: typeof Ticker;");
}

#[test]
fn nullable_objects_are_nullable_pointers_both_ways() {
    let (js, dts) = render();
    assert_has(
        &js,
        "_r = wasm.weaveffi_bus_lookup((fallback === null || fallback === undefined ? 0 : _borrow(fallback)), _err);",
    );
    assert_has(&js, "return _r === 0 ? null : Ticker._wrap(_r);");
    // Async results adopt the same way.
    assert_has(&js, "unwrap: (w, h) => h === 0 ? null : Ticker._wrap(h)");
    assert_has(&dts, "lookup(fallback: Ticker | null): Ticker | null;");
    assert_has(&dts, "fetch(id: bigint): Promise<Ticker | null>;");
}

#[test]
fn objects_inside_records_are_cloned_tokens() {
    let (js, dts) = render();
    // Encoding writes a fresh clone as a u64 token, never the wrapper's own pointer.
    assert_has(&js, "w.obj(v.primary._clone());");
    assert_has(&js, "for (const _e1 of _a1) {\n      w.obj(_e1._clone());");
    assert_has(&js, "obj(ptr) { this.u64(BigInt(ptr >>> 0)); }");
    // Decoding adopts each token into a new wrapper and rejects zero tokens.
    assert_has(&js, "v.primary = Ticker._wrap(r.obj());");
    assert_has(&js, "_arr.push(Ticker._wrap(r.obj()));");
    assert_has(
        &js,
        "if (t === 0n || t > 0xffffffffn) this._bad('object token out of range');",
    );
    assert_has(
        &dts,
        "export interface Envelope {\n  topic: string;\n  primary: Ticker;\n  others: Ticker[];\n}",
    );
}

#[test]
fn iterators_adopt_object_elements() {
    let (js, dts) = render();
    assert_has(&js, "_it = wasm.weaveffi_bus_tickers(_err);");
    assert_has(&js, "return new _WeaveFFIIterator(wasm, _it, 4,");
    assert_has(
        &js,
        "(it, slot, ep) => wasm.weaveffi_bus_TickersIterator_next(it, slot, ep),",
    );
    assert_has(
        &js,
        "(it) => wasm.weaveffi_bus_TickersIterator_destroy(it),",
    );
    assert_has(
        &js,
        "(w, p) => Ticker._wrap(new DataView(w.memory.buffer).getUint32(p, true))",
    );
    assert_has(&dts, "tickers(): IterableIterator<Ticker>;");
}

#[test]
fn callback_interfaces_render_ts_interface_vtable_and_trampolines() {
    let (js, dts) = render();
    // Consumer-facing TS interface.
    assert_has(
        &dts,
        "export interface Subscriber {\n  onMessage(text: string, weight: number, envelope: Envelope): boolean;\n  onTicker(ticker: Ticker, alt: Ticker | null): void;\n  classify(weight: number): Priority;\n  onMask(mask: bigint, bits: number): void;\n}",
    );
    assert_has(&dts, "subscribe(listener: Subscriber): void;");

    // The handle map keyed by the integer ctx.
    assert_has(&js, "let _nextCbId = 1;\n  const _callbacks = new Map();");

    // One trampoline per method, whose wasm signature mirrors abi_params
    // (ctx, params..., out_err) and abi_ret.
    assert_has(
        &js,
        "const _cb_Subscriber_on_message = _registerTrampoline(_table, ['i32', 'i32', 'i32', 'i32', 'i32', 'i32'], ['i32'], (_ctx, a0, a1, a2, a3, _err) => {",
    );
    assert_has(
        &js,
        "const _cb_Subscriber_on_ticker = _registerTrampoline(_table, ['i32', 'i32', 'i32', 'i32'], [], (_ctx, a0, a1, _err) => {",
    );
    assert_has(
        &js,
        "const _cb_Subscriber_classify = _registerTrampoline(_table, ['i32', 'i32', 'i32'], ['i32'], (_ctx, a0, _err) => {",
    );
    // Borrowed args are decoded (string, i32, record), object args adopted.
    assert_has(&js, "const _impl = _callbacks.get(_ctx);");
    assert_has(&js, "const _p0 = _readCStr(wasm, a0);");
    assert_has(&js, "const _p1 = a1;");
    assert_has(
        &js,
        "const _p2 = _read_bus_Envelope(_p2_r);\n      _p2_r.end();",
    );
    assert_has(&js, "return _impl.onMessage(_p0, _p1, _p2) ? 1 : 0;");
    assert_has(
        &js,
        "const _p0 = Ticker._wrap(a0);\n      const _p1 = a1 === 0 ? null : Ticker._wrap(a1);\n      if (_pendingForeign !== null) {",
    );
    assert_has(&js, "return _impl.classify(_p0);");
    // Failure path: error_set with -4 and a default return.
    assert_has(
        &js,
        "} catch (e) {\n      _setForeignError(wasm, _err, e);\n      return 0;\n    }",
    );
    assert_has(
        &js,
        "} catch (e) {\n      _setForeignError(wasm, _err, e);\n    }",
    );
    assert_has(
        &js,
        "function _reportForeign(wasm, errPtr, msg) {\n  if (_pendingForeign === null) _pendingForeign = msg;\n  const [p, s] = _cstr(wasm, msg);\n  wasm.weaveffi_error_set(errPtr, -4, p);\n  wasm.weaveffi_dealloc(p, s);\n}",
    );
    assert_has(
        &js,
        "function _setForeignError(wasm, errPtr, e) {\n  _reportForeign(wasm, errPtr, e instanceof Error ? (e.message || e.name) : String(e));\n}",
    );
    // Without unwinding the producer keeps calling back after a failure;
    // those invocations are refused after their arguments are adopted.
    assert_has(
        &js,
        "if (_pendingForeign !== null) {\n        _reportForeign(wasm, _err, _pendingForeign);\n        return 0;\n      }\n      return _impl.classify(_p0);",
    );
    assert_has(
        &js,
        "if (_pendingForeign !== null) {\n        _reportForeign(wasm, _err, _pendingForeign);\n        return;\n      }\n      _impl.onTicker(_p0, _p1);",
    );
    // Every producer-call completion path clears the parked failure.
    assert_has(
        &js,
        "function _freeErr(wasm, errPtr) {\n  _pendingForeign = null;",
    );
    assert_has(
        &js,
        "function _checkErr(wasm, errPtr) {\n  _pendingForeign = null;",
    );
    assert_has(
        &js,
        "function _checkErrRef(wasm, errPtr, mkErr) {\n  _pendingForeign = null;",
    );
    // free deletes the map entry.
    assert_has(
        &js,
        "const _cb_Subscriber_free = _registerTrampoline(_table, ['i32'], [], (_ctx) => { _callbacks.delete(_ctx); });",
    );

    // Exactly one static vtable, allocated once with the module allocator and
    // filled with table indices in declaration order, then free.
    assert_eq!(
        js.matches("const _vtable_Subscriber = wasm.weaveffi_alloc(20);")
            .count(),
        1
    );
    assert_has(
        &js,
        "_dv.setUint32(_vtable_Subscriber + 0, _cb_Subscriber_on_message, true);\n    _dv.setUint32(_vtable_Subscriber + 4, _cb_Subscriber_on_ticker, true);\n    _dv.setUint32(_vtable_Subscriber + 8, _cb_Subscriber_classify, true);\n    _dv.setUint32(_vtable_Subscriber + 12, _cb_Subscriber_on_mask, true);\n    _dv.setUint32(_vtable_Subscriber + 16, _cb_Subscriber_free, true);",
    );

    // Passing an implementation: map key as ctx plus the vtable address.
    assert_has(
        &js,
        "subscribe(listener) {\n        const a0_ctx = _nextCbId++;\n        _callbacks.set(a0_ctx, listener);",
    );
    assert_has(
        &js,
        "wasm.weaveffi_bus_subscribe(a0_ctx, _vtable_Subscriber, _err);",
    );
}

/// `wasm32-unknown-unknown` can't unwind, so a producer panic reaches JS as a
/// `WebAssembly.RuntimeError` trap. Every producer call is guarded: the trap
/// is translated into the brand error (`-4` if a callback failure was parked
/// during the call, `-2` otherwise), the error slot is released, staged inputs
/// are freed in a `finally`, and async launchers reject their Promise and
/// forget the context.
#[test]
fn traps_are_translated_into_brand_errors() {
    let (js, _) = render();
    assert_has(&js, "let _pendingForeign = null;");
    assert_has(
        &js,
        "function _trapError(e) {\n  const foreign = _pendingForeign;\n  _pendingForeign = null;\n  if (!(e instanceof WebAssembly.RuntimeError)) return e;\n  if (foreign !== null) return new WeaveFFIError(-4, foreign);\n  return new WeaveFFIError(-2, 'producer panicked: ' + e.message);\n}",
    );
    assert_has(
        &js,
        "function _trap(wasm, errPtr, e) {\n  _freeErr(wasm, errPtr);\n  return _trapError(e);\n}",
    );
    // Sync wrapper: guarded call, then the checker on the untouched slot.
    assert_has(
        &js,
        "const _err = _allocErr(wasm);\n        let _r;\n        try {\n          _r = wasm.weaveffi_bus_lookup((fallback === null || fallback === undefined ? 0 : _borrow(fallback)), _err);\n        } catch (e) {\n          throw _trap(wasm, _err, e);\n        }\n        _checkErr(wasm, _err);",
    );
    // A buffered return releases its out-length slot on the trap path and
    // frees the staged argument in `finally`.
    assert_has(
        &js,
        "const _lp = wasm.weaveffi_alloc(4);\n        const _err = _allocErr(wasm);\n        let _r;\n        try {\n          _r = wasm.weaveffi_bus_describe(a0_p, a0_l, _lp, _err);\n        } catch (e) {\n          wasm.weaveffi_dealloc(_lp, 4);\n          throw _trap(wasm, _err, e);\n        } finally {\n          wasm.weaveffi_dealloc(a0_p, a0_l);\n        }",
    );
    // Constructor and iterator launch are guarded the same way.
    assert_has(
        &js,
        "let _r;\n      try {\n        _r = wasm.weaveffi_bus_Ticker_new(BigInt(start), _err);\n      } catch (e) {\n        throw _trap(wasm, _err, e);\n      }",
    );
    assert_has(
        &js,
        "let _it;\n        try {\n          _it = wasm.weaveffi_bus_tickers(_err);\n        } catch (e) {\n          throw _trap(wasm, _err, e);\n        }",
    );
    // Iterator steps release the slot on a trap before rethrowing.
    assert_has(
        &js,
        "_has = this._callNext(this._handle, this._slot, _err);\n    } catch (e) {\n      // A trap: the slot was never filled, so release it here.\n      this._close();\n      throw _trap(wasm, _err, e);\n    }",
    );
    // Async launch: forget the context and reject with the translated error.
    assert_has(
        &js,
        "try {\n            wasm.weaveffi_bus_fetch_async(BigInt(id), _cbPtr_i32_i32_i32, ctxId);\n          } catch (e) {\n            _asyncContexts.delete(ctxId);\n            reject(_trapError(e));\n          }",
    );
}

/// Wasm integers are signed on the JS side, so unsigned scalars arriving
/// through a direct slot (sync returns, async completions, callback
/// parameters) are reinterpreted: `u32` via `>>> 0`, `u64` via
/// `BigInt.asUintN(64, ...)`. Signed and 32-bit-or-narrower values need no
/// coercion, and inputs rely on the wasm calling convention's own wrapping.
#[test]
fn unsigned_scalars_are_reinterpreted_on_the_way_out() {
    let (js, _) = render();
    assert_has(
        &js,
        "_r = wasm.weaveffi_bus_Ticker_mask(_borrow(this), _err);",
    );
    assert_has(&js, "return BigInt.asUintN(64, _r);");
    assert_has(
        &js,
        "_r = wasm.weaveffi_bus_Ticker_bits(_borrow(this), _err);",
    );
    assert_has(&js, "return _r >>> 0;");
    // Signed 64-bit returns stay as the BigInt wasm hands back.
    assert_has(
        &js,
        "_r = wasm.weaveffi_bus_Ticker_value(_borrow(this), _err);\n      } catch (e) {\n        throw _trap(wasm, _err, e);\n      }\n      _checkErr(wasm, _err);\n      _freeErr(wasm, _err);\n      return _r;",
    );
    // Async completion of a u64 result.
    assert_has(
        &js,
        "_asyncContexts.set(ctxId, { resolve, reject, unwrap: (w, r) => BigInt.asUintN(64, r) });",
    );
    assert_has(
        &js,
        "wasm.weaveffi_bus_checksum_async(BigInt(id), _cbPtr_i32_i32_i64, ctxId);",
    );
    // Callback parameters.
    assert_has(
        &js,
        "const _p0 = BigInt.asUintN(64, a0);\n      const _p1 = a1 >>> 0;\n      if (_pendingForeign !== null) {",
    );
}

#[test]
fn runtime_checks_abi_revision_and_drops_legacy_surfaces() {
    let (js, dts) = render();
    assert_has(&js, "const _ABI_VERSION = 2;");
    assert_has(&js, "_checkAbiVersion(wasm);");
    assert_has(&js, "wasm.weaveffi_error_set(");
    for gone in [
        "arena",
        "weaveffi_handle_t",
        "TypedHandle",
        "register_",
        "_listeners",
        "free()",
        ".destroy()",
    ] {
        assert!(!js.contains(gone), "stale `{gone}` in loader");
        assert!(!dts.contains(gone), "stale `{gone}` in declarations");
    }
}

#[test]
fn emscripten_mode_is_partial_and_stubs_unsupported_entry_points() {
    let config = emscripten_config();
    assert_eq!(
        WasmGenerator.capabilities(&config),
        TargetCapabilities {
            async_functions: false,
            callback_interfaces: false,
            iterators: true,
        }
    );
    assert_eq!(
        WasmGenerator.capabilities(&WasmConfig::default()),
        TargetCapabilities::full()
    );
    assert!(!WasmGenerator.allows_unsupported(&config));
    assert!(WasmGenerator.allows_unsupported(&WasmConfig {
        allow_unsupported: true,
        ..emscripten_config()
    }));

    let (js, dts) = render_with(&config);
    assert_has(
        &js,
        "subscribe(listener) {\n        throw new Error(\"weaveffi: function 'subscribe' (it takes a callback interface) is not supported in Emscripten mode;",
    );
    assert_has(
        &js,
        "fetch(id) {\n        throw new Error(\"weaveffi: async function 'fetch' is not supported in Emscripten mode;",
    );
    // No trampoline machinery, but objects and iterators still work.
    assert!(
        !js.contains("_registerTrampoline"),
        "trampolines in Emscripten glue"
    );
    assert!(!js.contains("_vtable_"), "vtable in Emscripten glue");
    assert_has(
        &js,
        "get memory() { return { buffer: m['HEAPU8'].buffer }; },",
    );
    assert_has(
        &js,
        "weaveffi_bus_Ticker_clone: m['_weaveffi_bus_Ticker_clone'],",
    );
    assert_has(
        &js,
        "weaveffi_bus_Ticker_destroy: m['_weaveffi_bus_Ticker_destroy'],",
    );
    assert_has(
        &js,
        "weaveffi_bus_TickersIterator_next: m['_weaveffi_bus_TickersIterator_next'],",
    );
    assert!(
        !js.contains("m['_weaveffi_bus_subscribe']"),
        "stubbed symbol bound"
    );
    // The declarations omit the stubs and the callback interface entirely.
    assert!(!dts.contains("subscribe("), "stubbed function declared");
    assert!(!dts.contains("fetch("), "stubbed async function declared");
    assert!(
        !dts.contains("interface Subscriber"),
        "callback interface declared"
    );
    assert_has(&dts, "lookup(fallback: Ticker | null): Ticker | null;");
    assert_has(&dts, "tickers(): IterableIterator<Ticker>;");
}

fn packaged_text<'a>(files: &'a [PackagedFile], suffix: &str) -> &'a str {
    files
        .iter()
        .find(|f| f.path.as_str().ends_with(suffix))
        .and_then(|f| match &f.content {
            FileContent::Text(s) => Some(s.as_str()),
            FileContent::Copy(_) => None,
        })
        .unwrap_or_else(|| panic!("no packaged {suffix}"))
}

fn package_with(config: &WasmConfig) -> Vec<PackagedFile> {
    let api = validate_api(fixture(), None).expect("fixture validates");
    let model = BindingModel::build(&api, WasmGenerator.prefix(config));
    let mut binaries = BinarySet::new("bus");
    for p in Platform::ALL {
        binaries.insert(p, format!("/prebuilt/{}/lib", p.id()));
    }
    let ctx = PackageContext {
        binaries: &binaries,
        input_basename: Some("bus.yml"),
    };
    WasmGenerator
        .package(&api, &model, &ctx, Utf8Path::new("out"), config)
        .expect("wasm packages")
}

#[test]
fn package_bundles_only_the_wasm32_binary() {
    let files = package_with(&WasmConfig::default());
    let bundled: Vec<&PackagedFile> = files.iter().filter(|f| f.is_binary()).collect();
    assert_eq!(bundled.len(), 1, "{bundled:?}");
    assert_eq!(bundled[0].path.as_str(), "out/wasm/bus.wasm");
    assert_eq!(
        bundled[0].content,
        FileContent::Copy("/prebuilt/wasm32/lib".into())
    );
    let mut paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "out/wasm/README.md",
            "out/wasm/bus.wasm",
            "out/wasm/package.json",
            "out/wasm/weaveffi_wasm.d.ts",
            "out/wasm/weaveffi_wasm.js",
        ]
    );
    let manifest = packaged_text(&files, "package.json");
    assert_has(manifest, "\"main\": \"weaveffi_wasm.js\"");
    assert_has(manifest, "\"types\": \"weaveffi_wasm.d.ts\"");
    assert_has(
        manifest,
        "\"weaveffi_wasm.js\",\n    \"weaveffi_wasm.d.ts\",\n    \"bus.wasm\"",
    );
    let readme = packaged_text(&files, "README.md");
    assert_has(readme, "bundled as `bus.wasm`");
    assert_has(readme, "new URL('./bus.wasm', import.meta.url)");
    // The loader and declarations are the same as `generate` emits.
    let (js, dts) = render();
    assert_eq!(packaged_text(&files, "weaveffi_wasm.js"), js);
    assert_eq!(packaged_text(&files, "weaveffi_wasm.d.ts"), dts);
}

#[test]
fn emscripten_package_ships_without_a_binary() {
    let files = package_with(&emscripten_config());
    assert!(
        files.iter().all(|f| !f.is_binary()),
        "binary bundled in Emscripten package"
    );
    let manifest = packaged_text(&files, "package.json");
    assert!(
        !manifest.contains("\"files\""),
        "files list without a binary"
    );
    let readme = packaged_text(&files, "README.md");
    assert_has(readme, "The package carries no binary");
    assert_has(readme, "const api = await loadWeaveffiWasm(Module());");
}
