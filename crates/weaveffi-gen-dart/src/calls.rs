//! Callable rendering: the FFI typedef/lookup pairs and idiomatic Dart
//! wrappers for sync, async, and iterator callables, plus callback typedefs
//! and listener register/unregister pairs.
//!
//! Parameter marshalling dispatches on the shared [`ArgPass`] contract and
//! return handling on [`RetPass`], so this module never re-derives the
//! buffered-versus-direct split from raw `Ty`s.

use heck::ToLowerCamelCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    CallShape, CallbackBinding, FnBinding, IteratorBinding, ListenerBinding, ModuleBinding,
    ParamBinding,
};
use weaveffi_core::model::{Prim, WireType};
use weaveffi_core::plan::{self, ArgPass, ElemFree, ErrorStrategy, RetPass};

use crate::codec::{read_expr, write_stmts};
use crate::docs::{emit_doc, emit_wrapper_doc};
use crate::entities::dart_exception_name;
use crate::runtime::emit_typedef_and_lookup;
use crate::types::{
    dart_class, dart_ident, dart_type, dart_wrapper_fn_name, input_slots, return_ffi,
    return_out_slots, returns_buffer, scalar_ffi,
};

/// Error-reporting context for one wrapper: which check helper guards its
/// out-err slot and which exception its async completion path constructs.
///
/// The split follows [`ErrorStrategy`]: a throwing callable maps `out_err`
/// onto the module's typed domain exception, while a non-throwing callable
/// traps through the generic brand exception (a reported error there is only
/// ever a producer bug, never a domain error).
#[derive(Clone, Copy)]
pub(crate) struct ErrCtx<'a> {
    /// `true` when the wrapper surfaces typed domain errors (`throws: true`).
    throws: bool,
    /// The domain exception class in effect (`KvException` names `_checkKvException`
    /// and `_mapKvException`); `None` when no error domain is in scope.
    exception: Option<&'a str>,
}

impl<'a> ErrCtx<'a> {
    /// The domain exception this wrapper throws, or `None` for a non-throwing
    /// wrapper (which reports every failure as the generic brand exception).
    pub(crate) fn thrown_exception(&self) -> Option<&'a str> {
        self.exception.filter(|_| self.throws)
    }

    /// The statement checking the wrapper's `err` slot after a call.
    fn check_stmt(&self) -> String {
        match self.thrown_exception() {
            Some(exc) => format!("_check{exc}(err);"),
            None => "_checkError(err);".to_string(),
        }
    }

    /// The expression building the exception for an async completion's
    /// already-captured `code`/`msg` (and, for a domain error, `payload`)
    /// locals.
    fn map_expr(&self) -> String {
        match self.thrown_exception() {
            Some(exc) => format!("_map{exc}(code, msg, payload)"),
            None => "WeaveFFIException(code, msg)".to_string(),
        }
    }
}

/// The [`ErrCtx`] for one callable of a module: its [`ErrorStrategy`] paired
/// with the exception class of the domain in effect (own or inherited).
pub(crate) fn err_ctx<'a>(f: &FnBinding, exception: Option<&'a str>) -> ErrCtx<'a> {
    ErrCtx {
        throws: matches!(f.error_strategy(), ErrorStrategy::Throws),
        exception,
    }
}

/// How one rendered wrapper is declared in Dart source: a top-level function,
/// or a member (method, static, or factory constructor) of an interface class.
pub(crate) enum DartDecl<'a> {
    /// A top-level free function.
    TopLevel,
    /// An instance method of an interface class: the FFI call passes the
    /// wrapper's `_handle` as the implicit leading argument.
    Method,
    /// A `static` method of an interface class.
    Static,
    /// A `factory` constructor of the interface class. `named` is `false` for
    /// the canonical `new` constructor (`factory Store(...)`) and `true` for
    /// every other constructor (`factory Store.open(...)`).
    Factory {
        /// The interface class the factory constructs.
        class_name: &'a str,
        /// `false` for the canonical `new` constructor.
        named: bool,
    },
}

impl DartDecl<'_> {
    /// The declaration's opening line (through the `{`). `ret` is the public
    /// return type, already wrapped in `Future<...>` for an async member.
    fn open_line(&self, ret: &str, name: &str, params: &str) -> String {
        match self {
            DartDecl::TopLevel | DartDecl::Method => format!("{ret} {name}({params}) {{"),
            DartDecl::Static => format!("static {ret} {name}({params}) {{"),
            DartDecl::Factory {
                class_name,
                named: false,
            } => format!("factory {class_name}({params}) {{"),
            DartDecl::Factory {
                class_name,
                named: true,
            } => format!("factory {class_name}.{name}({params}) {{"),
        }
    }

    /// The opening line of a `sync*` generator wrapper (an `iter<T>` return).
    /// Constructors never return iterators, so no factory spelling exists.
    fn open_line_sync_star(&self, ret: &str, name: &str, params: &str) -> String {
        match self {
            DartDecl::TopLevel | DartDecl::Method => format!("{ret} {name}({params}) sync* {{"),
            DartDecl::Static => format!("static {ret} {name}({params}) sync* {{"),
            DartDecl::Factory { .. } => {
                unreachable!("constructors cannot return iterators")
            }
        }
    }
}

/// Render one free function of `module` at top level.
pub(crate) fn render_function(
    out: &mut String,
    module: &ModuleBinding,
    f: &FnBinding,
    strip: bool,
) {
    let name = dart_wrapper_fn_name(&module.path, &f.name, strip);
    let exc = module
        .error
        .as_ref()
        .map(|e| dart_exception_name(&e.type_name));
    let mut decl = String::new();
    render_callable(
        out,
        &mut decl,
        f,
        &DartDecl::TopLevel,
        &name,
        err_ctx(f, exc.as_deref()),
    );
    out.push_str(&decl);
}

/// Render one callable: its FFI typedefs and lookups into `lookups` (always
/// top-level) and its Dart wrapper declaration into `decl` (top-level for a
/// free function, spliced into the class body for an interface member).
pub(crate) fn render_callable(
    lookups: &mut String,
    decl: &mut String,
    f: &FnBinding,
    kind: &DartDecl,
    name: &str,
    err: ErrCtx,
) {
    // `c_base` is the prefixed `{prefix}_{module}_{name}` symbol the shared
    // BindingModel already computed; the async/iterator suffixing matches the C
    // ABI by construction.
    let c_sym = f.c_base.as_str();
    let pub_ret = f.ret.as_ref().map_or("void".into(), dart_type);
    let wrapper_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", dart_type(&p.ty), dart_ident(&p.name)))
        .collect();

    if f.is_async {
        render_async_function(
            lookups,
            decl,
            c_sym,
            f,
            kind,
            name,
            &pub_ret,
            &wrapper_params,
            err,
        );
        return;
    }

    // Each input parameter expands to its ABI slots (bytes and buffered
    // values fan out to `(ptr, len)`); a bytes or buffered return adds its
    // `out_len` slot; the trailing error slot closes the signature. An
    // instance method's `AbiFn` carries an implicit leading `self` pointer.
    let mut native_params: Vec<String> = Vec::new();
    let mut dart_params: Vec<String> = Vec::new();
    if f.has_self {
        native_params.push("Pointer<Void>".into());
        dart_params.push("Pointer<Void>".into());
    }
    for p in &f.params {
        for (n, d) in input_slots(p) {
            native_params.push(n);
            dart_params.push(d);
        }
    }
    if let Some(ret) = &f.ret {
        if !matches!(f.shape, CallShape::Iterator(_)) {
            for (n, d) in return_out_slots(ret) {
                native_params.push(n);
                dart_params.push(d);
            }
        }
    }
    native_params.push("Pointer<_WeaveFFIError>".into());
    dart_params.push("Pointer<_WeaveFFIError>".into());

    let (native_ret, dart_ret) = match &f.shape {
        // The iterator launcher returns the opaque iterator handle.
        CallShape::Iterator(_) => ("Pointer<Void>".to_string(), "Pointer<Void>".to_string()),
        _ => match &f.ret {
            Some(ret) => return_ffi(ret),
            None => ("Void".into(), "void".into()),
        },
    };

    emit_typedef_and_lookup(
        lookups,
        c_sym,
        &native_params.join(", "),
        &dart_params.join(", "),
        &native_ret,
        &dart_ret,
    );

    // Iterator-returning functions also bind the element `next`/`destroy`
    // symbols plus the GC-finalizer backstop for abandoned iterations.
    if let CallShape::Iterator(ib) = &f.shape {
        emit_iter_lookups(lookups, ib);
    }

    let mut w = CodeWriter::two_space();
    w.blank();
    emit_wrapper_doc(&mut w, f, err);
    let params = wrapper_params.join(", ");
    if let CallShape::Iterator(ib) = &f.shape {
        // The wrapper is a lazy `sync*` generator; everything (staging,
        // launch, per-element pulls, cleanup) lives in the generator body.
        w.line(kind.open_line_sync_star(&pub_ret, name, &params));
        let mut body = String::new();
        emit_iterator_body(&mut body, f, c_sym, ib, err);
        w.raw(body);
    } else {
        w.line(kind.open_line(&pub_ret, name, &params));
        let mut body = String::new();
        emit_function_body(&mut body, f, c_sym, err);
        w.raw(body);
    }
    w.line("}");
    decl.push_str(&w.finish());
}

/// The native FFI typedef for a module-level callback declaration, shared by
/// every listener that fires it.
pub(crate) fn render_callback_typedef(out: &mut String, cb: &CallbackBinding) {
    let mut slots: Vec<String> = Vec::new();
    for p in &cb.params {
        for (n, _) in input_slots(p) {
            slots.push(n);
        }
    }
    slots.push("Pointer<Void>".into());
    out.push_str(&format!(
        "\ntypedef _NativeCb_{} = Void Function({});\n",
        cb.c_fn_type,
        slots.join(", ")
    ));
}

/// Emit the statements converting one callback's trampoline slots into the
/// values handed to the user callback, returning the argument expressions.
/// Buffered arguments arrive as borrowed `(ptr, len)` pairs valid only for
/// the dispatch, so they are decoded here, inside the borrow window. Slot
/// names follow the lowered ABI (`{n}` or `{n}_ptr`/`{n}_len`).
fn emit_cb_args(w: &mut CodeWriter, cb: &CallbackBinding) -> Vec<String> {
    let mut args = Vec::new();
    for p in &cb.params {
        let base = dart_ident(&p.name);
        let n0 = dart_ident(&p.abi[0].name);
        args.push(match p.arg_pass() {
            ArgPass::Buffer { len, .. } => {
                let n1 = dart_ident(&len.name);
                w.line(format!("final {base}Data = _copyNativeBytes({n0}, {n1});"));
                w.line(format!("final {base}Reader = _BufferReader({base}Data);"));
                w.line(format!(
                    "final {base}Value = {};",
                    read_expr(&format!("{base}Reader"), &p.ty)
                ));
                w.line(format!("{base}Reader.expectEnd();"));
                format!("{base}Value")
            }
            ArgPass::String { .. } => {
                format!("{n0} == nullptr ? '' : {n0}.toDartString()")
            }
            ArgPass::Bytes { len, .. } => {
                let n1 = dart_ident(&len.name);
                format!("{n0} == nullptr ? <int>[] : {n0}.asTypedList({n1}).toList()")
            }
            // Borrowed for the duration of the callback: do not dispose().
            ArgPass::Object {
                nullable: false, ..
            } => {
                let Ty::Interface(name) = &p.ty else {
                    unreachable!("non-nullable object params are interfaces")
                };
                format!("{}._({n0})", dart_class(name))
            }
            // A nullable borrowed object pointer: null means none.
            ArgPass::Object { nullable: true, .. } => {
                let Ty::Optional(inner) = &p.ty else {
                    unreachable!("nullable object params are optional interfaces")
                };
                let Ty::Interface(name) = inner.as_ref() else {
                    unreachable!("only optional interfaces stay unbuffered")
                };
                format!("{n0} == nullptr ? null : {}._({n0})", dart_class(name))
            }
            ArgPass::Direct { .. } => match &p.ty {
                Ty::Enum(name) => format!("{}.fromValue({n0})", dart_class(name)),
                // Borrowed for the duration of the callback: do not dispose().
                Ty::TypedHandle(name) => format!("{}._({n0})", dart_class(name)),
                _ => n0,
            },
        });
    }
    args
}

/// The register/unregister wrapper pair for one listener. The trampoline is an
/// `isolateLocal` NativeCallable: WeaveFFI listeners fire synchronously on the
/// thread calling the producer API, so arguments are converted inside the
/// borrow window (a `.listener` callable would read freed pointers later).
pub(crate) fn render_listener(
    out: &mut String,
    m: &ModuleBinding,
    l: &ListenerBinding,
    strip: bool,
) {
    let Some(cb) = m.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };
    let cb_typedef = format!("_NativeCb_{}", cb.c_fn_type);
    let register_name = dart_wrapper_fn_name(&m.path, &format!("register_{}", l.name), strip);
    let unregister_name = dart_wrapper_fn_name(&m.path, &format!("unregister_{}", l.name), strip);

    emit_typedef_and_lookup(
        out,
        &l.register_symbol,
        &format!("Pointer<NativeFunction<{cb_typedef}>>, Pointer<Void>"),
        &format!("Pointer<NativeFunction<{cb_typedef}>>, Pointer<Void>"),
        "Uint64",
        "int",
    );
    emit_typedef_and_lookup(out, &l.unregister_symbol, "Uint64", "int", "Void", "void");

    let user_fn_params: Vec<String> = cb
        .params
        .iter()
        .map(|p| format!("{} {}", dart_type(&p.ty), dart_ident(&p.name)))
        .collect();
    let mut tramp_decls: Vec<String> = Vec::new();
    for p in &cb.params {
        for ((_, d), slot) in input_slots(p).iter().zip(p.abi.iter()) {
            tramp_decls.push(format!("{d} {}", dart_ident(&slot.name)));
        }
    }
    tramp_decls.push("Pointer<Void> context".into());

    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &l.doc, "");
        w.raw(d);
    }
    w.line(format!(
        "/// Registers a {} listener. Returns a subscription id for {unregister_name}().",
        cb.name
    ));
    w.block(
        format!(
            "int {register_name}(void Function({}) callback) {{",
            user_fn_params.join(", ")
        ),
        "}",
        |w| {
            w.line(format!(
                "final callable = NativeCallable<{cb_typedef}>.isolateLocal(({}) {{",
                tramp_decls.join(", ")
            ));
            w.scope(|w| {
                let call_args = emit_cb_args(w, cb);
                w.line(format!("callback({});", call_args.join(", ")));
            });
            w.line("});");
            w.line(format!(
                "final id = _{}(callable.nativeFunction, nullptr);",
                l.register_symbol.to_lower_camel_case()
            ));
            w.line("_listenerCallables[id] = callable;");
            w.line("return id;");
        },
    );

    w.blank();
    w.line(format!(
        "/// Unregisters a listener previously registered with {register_name}()."
    ));
    w.block(format!("void {unregister_name}(int id) {{"), "}", |w| {
        w.line(format!(
            "_{}(id);",
            l.unregister_symbol.to_lower_camel_case()
        ));
        w.line("_listenerCallables.remove(id)?.close();");
    });
    out.push_str(&w.finish());
}

/// The (native, dart, name) slot triples an async completion callback carries
/// after its `(context, err)` prefix. Bytes and buffered results arrive as
/// owned `(result, resultLen)` pairs the consumer frees; interfaces as
/// adopted pointers; everything else by value.
fn async_cb_result_slots(ret: Option<&Ty>) -> Vec<(String, String, String)> {
    let Some(ty) = ret else {
        return vec![];
    };
    let pair = |n: &str, d: &str, name: &str| (n.to_string(), d.to_string(), name.to_string());
    match plan::ret_pass(Some(ty), "", "") {
        RetPass::Buffer | RetPass::Bytes => vec![
            pair("Pointer<Uint8>", "Pointer<Uint8>", "result"),
            pair("Size", "int", "resultLen"),
        ],
        RetPass::String => vec![pair("Pointer<Utf8>", "Pointer<Utf8>", "result")],
        // A (possibly nullable) adopted object pointer.
        RetPass::Object { .. } => vec![pair("Pointer<Void>", "Pointer<Void>", "result")],
        RetPass::Void | RetPass::Direct => match ty {
            // A typed handle's slot is an opaque adopted pointer.
            Ty::TypedHandle(_) => vec![pair("Pointer<Void>", "Pointer<Void>", "result")],
            ty => {
                let (n, d) = scalar_ffi(ty);
                vec![pair(n, d, "result")]
            }
        },
    }
}

/// Render one async callable: its callback typedef and launcher lookup into
/// `lookups`, and its `Future`-returning wrapper into `decl`. A method's
/// launcher carries the implicit leading `self` pointer.
#[allow(clippy::too_many_arguments)]
fn render_async_function(
    lookups: &mut String,
    decl: &mut String,
    c_sym: &str,
    f: &FnBinding,
    kind: &DartDecl,
    name: &str,
    pub_ret: &str,
    wrapper_params: &[String],
    err: ErrCtx,
) {
    let cb_extras = async_cb_result_slots(f.ret.as_ref());
    let cb_native_params: Vec<String> = std::iter::once("Pointer<Void>".to_string())
        .chain(std::iter::once("Pointer<_WeaveFFIError>".to_string()))
        .chain(cb_extras.iter().map(|(n, _, _)| n.clone()))
        .collect();

    let cb_typedef = format!("_NativeAsyncCb_{c_sym}");
    lookups.push_str(&format!(
        "\ntypedef {cb_typedef} = Void Function({});\n",
        cb_native_params.join(", ")
    ));

    let async_sym = format!("{c_sym}_async");
    let self_slot = if f.has_self {
        vec![("Pointer<Void>".to_string(), "Pointer<Void>".to_string())]
    } else {
        vec![]
    };
    let mut input_ffi: Vec<(String, String)> = self_slot;
    for p in &f.params {
        input_ffi.extend(input_slots(p));
    }
    if f.cancellable {
        input_ffi.push(("Pointer<Void>".into(), "Pointer<Void>".into()));
    }
    input_ffi.push((
        format!("Pointer<NativeFunction<{cb_typedef}>>"),
        format!("Pointer<NativeFunction<{cb_typedef}>>"),
    ));
    input_ffi.push(("Pointer<Void>".into(), "Pointer<Void>".into()));
    let native_params: Vec<String> = input_ffi.iter().map(|(n, _)| n.clone()).collect();
    let dart_params: Vec<String> = input_ffi.iter().map(|(_, d)| d.clone()).collect();

    emit_typedef_and_lookup(
        lookups,
        &async_sym,
        &native_params.join(", "),
        &dart_params.join(", "),
        "Void",
        "void",
    );

    let completer_type = if f.ret.is_some() {
        pub_ret.to_string()
    } else {
        "void".to_string()
    };

    // Stage every input up front, exactly like the sync path; staged native
    // memory is pinned until the future completes and released in
    // whenComplete (or in the catch when the launch itself throws).
    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    if f.has_self {
        call_args.push("_handle".into());
    }
    let mut stage = CodeWriter::two_space().with_depth(1);
    let mut tmp = 0usize;
    for p in &f.params {
        let args = emit_input(&mut stage, p, &mut frees, &mut tmp);
        call_args.extend(args);
    }
    let staging = stage.finish();
    if f.cancellable {
        call_args.push("nullptr".into());
    }
    call_args.push("callable.nativeFunction".into());
    call_args.push("nullptr".into());

    let cb_param_decls: Vec<String> = std::iter::once("Pointer<Void> context".to_string())
        .chain(std::iter::once("Pointer<_WeaveFFIError> err".to_string()))
        .chain(cb_extras.iter().map(|(_, d, n)| format!("{d} {n}")))
        .collect();

    let var = async_sym.to_lower_camel_case();

    let mut ac = String::new();
    emit_async_complete(&mut ac, f.ret.as_ref(), "      ");

    let mut w = CodeWriter::two_space();
    w.blank();
    emit_wrapper_doc(&mut w, f, err);
    w.block(
        kind.open_line(
            &format!("Future<{pub_ret}>"),
            name,
            &wrapper_params.join(", "),
        ),
        "}",
        |w| {
            w.line(format!("final completer = Completer<{completer_type}>();"));
            w.raw(&staging);
            w.line(format!("late NativeCallable<{cb_typedef}> callable;"));
            w.line(format!(
                "callable = NativeCallable<{cb_typedef}>.listener(({}) {{",
                cb_param_decls.join(", ")
            ));
            w.scope(|w| {
                w.line("try {");
                w.scope(|w| {
                    w.line("if (err.address != 0 && err.ref.code != 0) {");
                    w.scope(|w| {
                        w.line("final code = err.ref.code;");
                        w.line("final msg = err.ref.message.toDartString();");
                        if err.thrown_exception().is_some() {
                            w.line(
                                "final payload = _copyNativeBytes(err.ref.payloadPtr, err.ref.payloadLen);",
                            );
                        }
                        w.line("_weaveffiErrorFree(err);");
                        w.line(format!("completer.completeError({});", err.map_expr()));
                        w.line("return;");
                    });
                    w.line("}");
                    w.raw(&ac);
                });
                w.line("} catch (e) {");
                w.scope(|w| {
                    w.line("completer.completeError(e);");
                });
                w.line("} finally {");
                w.scope(|w| {
                    w.line("callable.close();");
                });
                w.line("}");
            });
            w.line("});");
            w.line("try {");
            w.scope(|w| {
                w.line(format!("_{var}({});", call_args.join(", ")));
            });
            w.line("} catch (e) {");
            w.scope(|w| {
                w.line("callable.close();");
                for fr in &frees {
                    w.line(fr);
                }
                w.line("rethrow;");
            });
            w.line("}");
            if frees.is_empty() {
                w.line("return completer.future;");
            } else {
                w.line("return completer.future.whenComplete(() {");
                w.scope(|w| {
                    for fr in &frees {
                        w.line(fr);
                    }
                });
                w.line("});");
            }
        },
    );
    decl.push_str(&w.finish());
}

/// Emit the callback statements that resolve the completer from the result
/// slots. Async results transfer ownership to the consumer (which is what
/// lets `NativeCallable.listener` defer this code past the native callback's
/// return): strings are freed with `weaveffi_free_string`, byte and value
/// buffers with `weaveffi_free_bytes`, and an owned interface result is
/// adopted by its wrapper class.
fn emit_async_complete(out: &mut String, ty: Option<&Ty>, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let Some(ty) = ty else {
        w.line("completer.complete();");
        out.push_str(&w.finish());
        return;
    };
    match plan::ret_pass(Some(ty), "", "") {
        // Copy the owned encoding, release it, then decode.
        RetPass::Buffer => {
            w.line("final resultData = _copyNativeBytes(result, resultLen);");
            w.line("_weaveffiFreeBytes(result, resultLen);");
            w.line("final resultReader = _BufferReader(resultData);");
            w.line(format!(
                "final decoded = {};",
                read_expr("resultReader", ty)
            ));
            w.line("resultReader.expectEnd();");
            w.line("completer.complete(decoded);");
        }
        RetPass::Bytes => {
            w.line("final resultData = _copyNativeBytes(result, resultLen);");
            w.line("_weaveffiFreeBytes(result, resultLen);");
            w.line("completer.complete(resultData);");
        }
        // Copy the owned C string, then release the producer allocation.
        RetPass::String => {
            w.line("final decoded = result.toDartString();");
            w.line("_weaveffiFreeString(result);");
            w.line("completer.complete(decoded);");
        }
        // The callback receives ownership of an object result; the wrapper
        // adopts the pointer and its `dispose()` owns the eventual destroy.
        RetPass::Object { nullable, .. } => {
            let name = object_class(ty);
            if nullable {
                w.line(format!(
                    "completer.complete(result == nullptr ? null : {name}._(result));"
                ));
            } else {
                w.line(format!("completer.complete({name}._(result));"));
            }
        }
        RetPass::Void | RetPass::Direct => match ty {
            Ty::Enum(name) => {
                w.line(format!(
                    "completer.complete({}.fromValue(result));",
                    dart_class(name)
                ));
            }
            // An adopted typed-handle pointer, wrapped like an interface.
            Ty::TypedHandle(name) => {
                w.line(format!(
                    "completer.complete({}._(result));",
                    dart_class(name)
                ));
            }
            _ => {
                w.line("completer.complete(result);");
            }
        },
    }
    out.push_str(&w.finish());
}

/// The Dart wrapper class of a direct or nullable interface reference.
fn object_class(ty: &Ty) -> String {
    match ty {
        Ty::Interface(name) => dart_class(name),
        Ty::Optional(inner) => object_class(inner),
        _ => unreachable!("object returns are (optional) interfaces"),
    }
}

/// Emit pre-call staging for one input parameter, returning the call-argument
/// expressions it contributes (in ABI order) and appending any cleanup
/// statements to `frees`. Dispatches on the parameter's [`ArgPass`] contract:
/// a buffered value is encoded into a `_BufferWriter`, staged into native
/// memory, and passed as a borrowed `(ptr, len)` pair the callee never frees.
fn emit_input(
    w: &mut CodeWriter,
    p: &ParamBinding,
    frees: &mut Vec<String>,
    tmp: &mut usize,
) -> Vec<String> {
    let name = dart_ident(&p.name);
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            let writer = format!("{name}Writer");
            let buf = format!("{name}Buf");
            let ptr = format!("{name}Ptr");
            w.line(format!("final {writer} = _BufferWriter();"));
            write_stmts(w, &writer, &name, &p.ty, tmp);
            w.line(format!("final {buf} = {writer}.takeBytes();"));
            w.line(format!("final {ptr} = _stageBytes({buf});"));
            frees.push(format!("calloc.free({ptr});"));
            vec![ptr, format!("{buf}.length")]
        }
        ArgPass::String { .. } => {
            let ptr = format!("{name}Ptr");
            w.line(format!("final {ptr} = {name}.toNativeUtf8();"));
            frees.push(format!("calloc.free({ptr});"));
            vec![ptr]
        }
        ArgPass::Bytes { .. } => {
            let ptr = format!("{name}Ptr");
            w.line(format!(
                "final {ptr} = {name}.isEmpty ? nullptr : calloc<Uint8>({name}.length);"
            ));
            w.line(format!(
                "for (var i = 0; i < {name}.length; i++) {{ {ptr}[i] = {name}[i]; }}"
            ));
            frees.push(format!("if ({ptr} != nullptr) calloc.free({ptr});"));
            vec![ptr, format!("{name}.length")]
        }
        // A borrowed object pointer; when nullable, null means none.
        ArgPass::Object {
            nullable: false, ..
        } => vec![format!("{name}._handle")],
        ArgPass::Object { nullable: true, .. } => {
            vec![format!("{name}?._handle ?? nullptr")]
        }
        ArgPass::Direct { .. } => match &p.ty {
            Ty::Enum(_) => vec![format!("{name}.value")],
            // A typed handle passes its wrapped pointer, like an interface.
            Ty::TypedHandle(_) => vec![format!("{name}._handle")],
            _ => vec![name],
        },
    }
}

/// Allocate the out-parameter locals a bytes or buffered return needs before
/// the call, returning the extra call-argument expressions and recording
/// cleanup.
fn emit_return_alloc(w: &mut CodeWriter, ty: &Ty, frees: &mut Vec<String>) -> Vec<String> {
    if returns_buffer(ty) {
        w.line("final outLen = calloc<Size>();");
        frees.push("calloc.free(outLen);".into());
        vec!["outLen".into()]
    } else {
        vec![]
    }
}

/// Emit the post-call decode of a return into the wrapper's Dart return
/// value, dispatching on the return's [`RetPass`] contract. A buffered return
/// is copied out of the producer's buffer, released with
/// `weaveffi_free_bytes`, and decoded through the buffer reader.
fn emit_return_decode(out: &mut String, ty: &Ty, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    match plan::ret_pass(Some(ty), "", "") {
        RetPass::Buffer => {
            w.line("final n = outLen.value;");
            w.line("final data = _copyNativeBytes(result, n);");
            w.line("if (result != nullptr) _weaveffiFreeBytes(result, n);");
            w.line("final reader = _BufferReader(data);");
            // Named so it cannot shadow a user parameter (`value` is common).
            w.line(format!("final decoded = {};", read_expr("reader", ty)));
            w.line("reader.expectEnd();");
            w.line("return decoded;");
        }
        RetPass::Bytes => {
            w.line("final n = outLen.value;");
            w.line("if (result == nullptr) return <int>[];");
            w.line("final bytes = List<int>.generate(n, (i) => result[i]);");
            // Copy first, then release the producer's buffer.
            w.line("_weaveffiFreeBytes(result, n);");
            w.line("return bytes;");
        }
        RetPass::String => {
            w.line("final decoded = result.toDartString();");
            w.line("_weaveffiFreeString(result);");
            w.line("return decoded;");
        }
        // An owned object pointer the wrapper class adopts; when nullable,
        // null means none.
        RetPass::Object { nullable, .. } => {
            let name = object_class(ty);
            if nullable {
                w.line("if (result == nullptr) return null;");
            }
            w.line(format!("return {name}._(result);"));
        }
        RetPass::Void | RetPass::Direct => match ty {
            Ty::Enum(name) => {
                w.line(format!("return {}.fromValue(result);", dart_class(name)));
            }
            // An adopted typed-handle pointer, wrapped like an interface.
            Ty::TypedHandle(name) => {
                w.line(format!("return {}._(result);", dart_class(name)));
            }
            _ => {
                w.line("return result;");
            }
        },
    }
    out.push_str(&w.finish());
}

/// Emit the staging, call, error check, and return decode of one synchronous
/// wrapper body.
fn emit_function_body(out: &mut String, f: &FnBinding, c_sym: &str, err: ErrCtx) {
    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    if f.has_self {
        call_args.push("_handle".into());
    }
    let mut stage = CodeWriter::two_space().with_depth(1);
    let mut tmp = 0usize;
    for p in &f.params {
        let args = emit_input(&mut stage, p, &mut frees, &mut tmp);
        call_args.extend(args);
    }
    if let Some(ret) = &f.ret {
        call_args.extend(emit_return_alloc(&mut stage, ret, &mut frees));
    }
    let staging = stage.finish();
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());

    let var = c_sym.to_lower_camel_case();
    let args = call_args.join(", ");
    let void_call = f.ret.is_none();
    let mut dec = String::new();
    if let Some(ret) = &f.ret {
        emit_return_decode(&mut dec, ret, "    ");
    }

    let mut w = CodeWriter::two_space().with_depth(1);
    w.raw(staging);
    w.line("final err = calloc<_WeaveFFIError>();");
    w.line("try {");
    w.scope(|w| {
        if void_call {
            w.line(format!("_{var}({args});"));
        } else {
            w.line(format!("final result = _{var}({args});"));
        }
        w.line(err.check_stmt());
        w.raw(&dec);
    });
    w.line("} finally {");
    w.scope(|w| {
        for fr in &frees {
            w.line(fr);
        }
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// The dart:ffi pointee type of an iterator's `out_item` slot, plus whether
/// the element also carries a `size_t* out_len` slot (bytes and every
/// buffered element do), driven by the element's [`ElemFree`] plan.
fn iter_item_slot(elem: &Ty) -> (String, bool) {
    match plan::elem_free(elem) {
        ElemFree::Bytes => ("Pointer<Uint8>".into(), true),
        ElemFree::String => ("Pointer<Utf8>".into(), false),
        ElemFree::None => match elem {
            Ty::Interface(_) | Ty::TypedHandle(_) => ("Pointer<Void>".into(), false),
            _ => (scalar_ffi(elem).0.to_string(), false),
        },
    }
}

/// Convert a single native by-value element (`expr`) into its Dart
/// representation: enums map through `fromValue`, interface elements are
/// adopted by their wrapper class, scalars pass through.
fn direct_elem_read(expr: &str, ty: &Ty) -> String {
    match ty {
        Ty::Enum(n) => format!("{}.fromValue({expr})", dart_class(n)),
        Ty::Interface(n) | Ty::TypedHandle(n) => {
            format!("{}._({expr})", dart_class(n))
        }
        _ => expr.to_string(),
    }
}

/// Bind the element `next`/`destroy` symbols of an iterator-returning
/// function, plus a `NativeFinalizer` over the destroy symbol. The finalizer
/// is the disposal backstop for abandoned iterations: Dart runs a `sync*`
/// body only inside `moveNext`, so a consumer that stops pulling (a broken
/// `for` loop, `first`, `take`) never resumes the generator and its `finally`
/// block never runs; the finalizer reclaims the native handle when the
/// suspended frame is collected instead.
fn emit_iter_lookups(out: &mut String, ib: &IteratorBinding) {
    let (pointee, has_len) = iter_item_slot(&ib.elem);
    let mut params = vec!["Pointer<Void>".to_string(), format!("Pointer<{pointee}>")];
    if has_len {
        params.push("Pointer<Size>".into());
    }
    params.push("Pointer<_WeaveFFIError>".into());
    let joined = params.join(", ");
    emit_typedef_and_lookup(out, &ib.next.symbol, &joined, &joined, "Int32", "int");
    emit_typedef_and_lookup(
        out,
        &ib.destroy_symbol,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );
    out.push_str(&format!(
        "final _{}Finalizer = NativeFinalizer(\n    \
         _lib.lookup<NativeFunction<Void Function(Pointer<Void>)>>('{}'));\n",
        ib.destroy_symbol.to_lower_camel_case(),
        ib.destroy_symbol
    ));
}

/// Emit the `sync*` generator body of an `iter<T>` wrapper.
///
/// The body runs lazily, on the first pull: it stages the inputs, launches
/// the C iterator, and then issues exactly one producer `next` call per
/// yielded element, releasing each element per the plan's [`ElemFree`] after
/// copying or decoding (strings through `weaveffi_free_string`; bytes and
/// buffered elements through `weaveffi_free_bytes`; interface elements are
/// adopted by their wrapper class, whose `dispose()` owns the destroy).
///
/// The handle is destroyed exactly once. The `try`/`finally` destroys it when
/// iteration exhausts, a launch or `next` error throws, or the generator is
/// otherwise torn down, then nulls the local handle so the finalizer detach
/// path cannot double-destroy. For iterations abandoned mid-stream (where the
/// `finally` never runs, see [`emit_iter_lookups`]) the `NativeFinalizer`
/// attached to the generator-local anchor destroys the handle when the frame
/// is collected; the eager path detaches before destroying.
fn emit_iterator_body(
    out: &mut String,
    f: &FnBinding,
    c_sym: &str,
    ib: &IteratorBinding,
    err: ErrCtx,
) {
    let free_plan = plan::elem_free(&ib.elem);
    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    if f.has_self {
        call_args.push("_handle".into());
    }
    let mut stage = CodeWriter::two_space().with_depth(1);
    let mut tmp = 0usize;
    for p in &f.params {
        let args = emit_input(&mut stage, p, &mut frees, &mut tmp);
        call_args.extend(args);
    }
    let staging = stage.finish();
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());

    let var = c_sym.to_lower_camel_case();
    let elem = &ib.elem;
    let (pointee, has_len) = iter_item_slot(elem);
    let next_var = ib.next.symbol.to_lower_camel_case();
    let destroy_var = ib.destroy_symbol.to_lower_camel_case();
    let next_args = if has_len {
        "iter, outItem, outLen, err"
    } else {
        "iter, outItem, err"
    };

    let mut w = CodeWriter::two_space().with_depth(1);
    w.raw(staging);
    w.line("final err = calloc<_WeaveFFIError>();");
    w.line(format!("final outItem = calloc<{pointee}>();"));
    if has_len {
        w.line("final outLen = calloc<Size>();");
    }
    w.line("Pointer<Void> iter = nullptr;");
    w.line("final anchor = _IteratorLifetime();");
    w.line("try {");
    w.scope(|w| {
        w.line(format!("iter = _{var}({});", call_args.join(", ")));
        w.line(err.check_stmt());
        w.line(format!(
            "_{destroy_var}Finalizer.attach(anchor, iter, detach: anchor);"
        ));
        w.line(format!("while (_{next_var}({next_args}) != 0) {{"));
        w.scope(|w| {
            w.line(err.check_stmt());
            match free_plan {
                ElemFree::String => {
                    w.line("final itemPtr = outItem.value;");
                    w.line("final item = itemPtr.toDartString();");
                    w.line("_weaveffiFreeString(itemPtr);");
                    w.line("yield item;");
                }
                // Bytes and buffered elements: copy or decode, then release
                // the producer's buffer with weaveffi_free_bytes. A raw bytes
                // element (wire shape Bytes) copies; every other buffered
                // element decodes.
                ElemFree::Bytes => {
                    w.line("final itemPtr = outItem.value;");
                    w.line("final itemLen = outLen.value;");
                    if matches!(elem.wire(), WireType::Prim(Prim::Bytes)) {
                        w.line("final item = _copyNativeBytes(itemPtr, itemLen);");
                        w.line("if (itemPtr != nullptr) _weaveffiFreeBytes(itemPtr, itemLen);");
                        w.line("yield item;");
                    } else {
                        w.line("final itemData = _copyNativeBytes(itemPtr, itemLen);");
                        w.line("if (itemPtr != nullptr) _weaveffiFreeBytes(itemPtr, itemLen);");
                        w.line("final itemReader = _BufferReader(itemData);");
                        w.line(format!("final item = {};", read_expr("itemReader", elem)));
                        w.line("itemReader.expectEnd();");
                        w.line("yield item;");
                    }
                }
                // By-value element (or an adopted interface handle).
                ElemFree::None => {
                    w.line(format!(
                        "yield {};",
                        direct_elem_read("outItem.value", elem)
                    ));
                }
            }
        });
        w.line("}");
        w.line(err.check_stmt());
    });
    w.line("} finally {");
    w.scope(|w| {
        w.line("if (iter != nullptr) {");
        w.scope(|w| {
            w.line(format!("_{destroy_var}Finalizer.detach(anchor);"));
            w.line(format!("_{destroy_var}(iter);"));
            w.line("iter = nullptr;");
        });
        w.line("}");
        if has_len {
            w.line("calloc.free(outLen);");
        }
        w.line("calloc.free(outItem);");
        for fr in &frees {
            w.line(fr);
        }
    });
    w.line("}");
    out.push_str(&w.finish());
}
