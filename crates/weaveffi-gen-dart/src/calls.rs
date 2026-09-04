//! Callable rendering: the FFI typedef/lookup pairs and idiomatic Dart
//! wrappers for sync, async, and iterator callables.
//!
//! Parameter marshalling dispatches on the shared [`ArgPass`] contract and
//! return handling on [`RetPass`], so this module never re-derives the
//! buffered-versus-direct split from raw [`Ty`]s.

use heck::ToLowerCamelCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{CallShape, FnBinding, IteratorBinding, ModuleBinding, ParamBinding};
use weaveffi_core::plan::{self, ArgPass, ErrorStrategy, RetPass};

use crate::codec::{read_expr, write_stmts};
use crate::docs::emit_wrapper_doc;
use crate::entities::dart_exception_name;
use crate::runtime::emit_typedef_and_lookup;
use crate::types::{
    dart_class, dart_ident, dart_type, dart_wrapper_fn_name, input_slots, object_class, return_ffi,
    return_out_slots, returns_buffer, scalar_ffi, vtable_var,
};

/// Error-reporting context for one wrapper: which check helper guards its
/// out-err slot and which exception its async completion path constructs.
///
/// The split follows [`ErrorStrategy`]: a throwing callable maps `out_err`
/// onto the module's typed domain exception, while a non-throwing callable
/// traps through the generic brand exception (a reported error there is only
/// ever a producer bug or a runtime trap, never a domain error).
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
    // values fan out to `(ptr, len)`, callback interfaces to `(ctx, vtable)`);
    // a bytes or buffered return adds its `out_len` slot; the trailing error
    // slot closes the signature. An instance method's `AbiFn` carries an
    // implicit leading `self` pointer.
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
        RetPass::Void | RetPass::Direct => {
            let (n, d) = scalar_ffi(ty);
            vec![pair(n, d, "result")]
        }
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
            w.line(format!(
                "completer.complete({});",
                adopt_expr("result", ty, nullable)
            ));
        }
        RetPass::Void | RetPass::Direct => match ty {
            Ty::Enum(name) => {
                w.line(format!(
                    "completer.complete({}.fromValue(result));",
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

/// The expression adopting the owned object pointer `expr` into its wrapper
/// class; when `nullable`, a null pointer becomes Dart `null`.
pub(crate) fn adopt_expr(expr: &str, ty: &Ty, nullable: bool) -> String {
    let class = object_class(ty);
    if nullable {
        format!("{expr} == nullptr ? null : {class}._({expr})")
    } else {
        format!("{class}._({expr})")
    }
}

/// Emit pre-call staging for one input parameter, returning the call-argument
/// expressions it contributes (in ABI order) and appending any cleanup
/// statements to `frees`. Dispatches on the parameter's [`ArgPass`] contract:
/// a buffered value is encoded into a `_BufferWriter`, staged into native
/// memory, and passed as a borrowed `(ptr, len)` pair the callee never frees;
/// an object passes the wrapper's borrowed handle; a callback interface is
/// registered in the handle table and passed as its key plus the interface's
/// static vtable.
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
        // A borrowed object pointer: the wrapper keeps its own reference and
        // the producer clones if it retains the object. When nullable, null
        // means none.
        ArgPass::Object {
            nullable: false, ..
        } => vec![format!("{name}._handle")],
        ArgPass::Object { nullable: true, .. } => {
            vec![format!("{name}?._handle ?? nullptr")]
        }
        // The implementation is parked in the handle table until the producer
        // calls the vtable's `free(ctx)`; nothing to release here.
        ArgPass::Callback { .. } => {
            let Ty::CallbackInterface(cb) = &p.ty else {
                unreachable!("callback family names a callback interface")
            };
            vec![
                format!("_registerCallback({name})"),
                format!("{}.cast<Void>()", vtable_var(cb)),
            ]
        }
        ArgPass::Direct { .. } => match &p.ty {
            Ty::Enum(_) => vec![format!("{name}.value")],
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
/// `weaveffi_free_bytes`, and decoded through the buffer reader; an object
/// return is adopted by its wrapper class.
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
        // One owned strong reference the wrapper class adopts; when nullable,
        // null means none.
        RetPass::Object { nullable, .. } => {
            w.line(format!("return {};", adopt_expr("result", ty, nullable)));
        }
        RetPass::Void | RetPass::Direct => match ty {
            Ty::Enum(name) => {
                w.line(format!("return {}.fromValue(result);", dart_class(name)));
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
/// buffered element do), driven by the element's [`RetPass`] plan.
fn iter_item_slot(elem: &Ty) -> (String, bool) {
    match plan::ret_pass(Some(elem), "", "") {
        RetPass::Bytes | RetPass::Buffer => ("Pointer<Uint8>".into(), true),
        RetPass::String => ("Pointer<Utf8>".into(), false),
        RetPass::Object { .. } => ("Pointer<Void>".into(), false),
        RetPass::Void | RetPass::Direct => (scalar_ffi(elem).0.to_string(), false),
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
/// yielded element, receiving each per the plan's [`RetPass`] (strings are
/// copied and freed through `weaveffi_free_string`; bytes and buffered
/// elements through `weaveffi_free_bytes`; interface elements are adopted by
/// their wrapper class, whose `dispose()` owns the destroy).
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
    let elem_pass = plan::ret_pass(Some(&ib.elem), "", "");
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
            match &elem_pass {
                RetPass::String => {
                    w.line("final itemPtr = outItem.value;");
                    w.line("final item = itemPtr.toDartString();");
                    w.line("_weaveffiFreeString(itemPtr);");
                    w.line("yield item;");
                }
                // A raw bytes element copies, then releases the producer's
                // buffer with weaveffi_free_bytes.
                RetPass::Bytes => {
                    w.line("final itemPtr = outItem.value;");
                    w.line("final itemLen = outLen.value;");
                    w.line("final item = _copyNativeBytes(itemPtr, itemLen);");
                    w.line("if (itemPtr != nullptr) _weaveffiFreeBytes(itemPtr, itemLen);");
                    w.line("yield item;");
                }
                // A buffered element decodes (adopting any object tokens it
                // carries), then releases the producer's buffer.
                RetPass::Buffer => {
                    w.line("final itemPtr = outItem.value;");
                    w.line("final itemLen = outLen.value;");
                    w.line("final itemData = _copyNativeBytes(itemPtr, itemLen);");
                    w.line("if (itemPtr != nullptr) _weaveffiFreeBytes(itemPtr, itemLen);");
                    w.line("final itemReader = _BufferReader(itemData);");
                    w.line(format!("final item = {};", read_expr("itemReader", elem)));
                    w.line("itemReader.expectEnd();");
                    w.line("yield item;");
                }
                // One owned strong reference per element, adopted by the
                // wrapper class.
                RetPass::Object { nullable, .. } => {
                    w.line("final itemPtr = outItem.value;");
                    w.line(format!("yield {};", adopt_expr("itemPtr", elem, *nullable)));
                }
                RetPass::Void | RetPass::Direct => {
                    let read = match elem {
                        Ty::Enum(n) => format!("{}.fromValue(outItem.value)", dart_class(n)),
                        _ => "outItem.value".to_string(),
                    };
                    w.line(format!("yield {read};"));
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
