//! Callable rendering: the `lookupFunction` bindings and idiomatic Dart
//! wrappers for sync, async, and iterator callables.
//!
//! FFI signatures come straight from the model's lowered [`AbiFn`]s, so they
//! match the C header slot for slot. Parameter marshalling dispatches on the
//! shared [`ArgPass`] contract, result handling on [`RetPass`], and error
//! handling on [`ErrorStrategy`].

use crate::codegen::CodeWriter;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{AbiFn, AsyncBinding, CallShape, FnBinding, IteratorBinding};
use weaveffi_model::plan::{ArgPass, ErrorStrategy, RetPass};
use weaveffi_model::ty::Ty;

use crate::targets::dart::callbacks::{dispatch_fn, vtable_var};
use crate::targets::dart::codec::{pack_fn, unpack_fn};
use crate::targets::dart::docs::emit_wrapper_doc;
use crate::targets::dart::types::{
    dart_class, dart_ident, dart_type, ffi_type, ffi_typedef, ffi_var, object_class,
};

/// Error-reporting context for one wrapper: the mapper its error checks
/// route through.
///
/// The split follows [`ErrorStrategy`]: a throwing callable maps codes onto
/// the module's typed domain exceptions, while a non-throwing callable traps
/// (a failure there is a producer bug, raised as a `NativeError`).
#[derive(Clone, Copy)]
pub(crate) struct ErrCtx<'a> {
    /// The domain exception class a throwing callable maps codes onto, or
    /// `None` for a callable that traps.
    exception: Option<&'a str>,
}

impl<'a> ErrCtx<'a> {
    /// The context of `f`, whose module has the domain exception class
    /// `exception` in scope (its own or an ancestor's).
    pub(crate) fn of(f: &FnBinding, exception: Option<&'a str>) -> Self {
        Self {
            exception: exception.filter(|_| f.error_strategy() == ErrorStrategy::Throws),
        }
    }

    /// The domain exception this wrapper throws, if it throws one.
    pub(crate) fn thrown_exception(&self) -> Option<&'a str> {
        self.exception
    }

    /// The `_ErrorMapper` the wrapper's checks use.
    fn mapper(&self) -> String {
        self.exception
            .map_or_else(|| "_trap".to_string(), mapper_fn)
    }

    /// The statement checking the shared error slot after a call.
    fn check_stmt(&self) -> String {
        match self.exception {
            Some(exc) => format!("_check(_err, {});", mapper_fn(exc)),
            None => "_check(_err);".to_string(),
        }
    }
}

/// The `_map{Exception}` mapper of a domain exception class.
pub(crate) fn mapper_fn(exception: &str) -> String {
    format!("_map{exception}")
}

/// How one wrapper is declared: a top-level function, or a member of an
/// interface class.
pub(crate) enum DartDecl<'a> {
    /// A top-level function.
    TopLevel,
    /// An instance method: the call borrows the wrapper's own pointer.
    Method,
    /// A `static` method of an interface class.
    Static,
    /// A factory constructor; `named` is `false` for the canonical `new`.
    Factory {
        /// The interface class the factory constructs.
        class_name: &'a str,
        /// `false` for the canonical `new` constructor.
        named: bool,
    },
}

impl DartDecl<'_> {
    /// The declaration's opening line (through the `{`). `ret` is the public
    /// return type and `suffix` an optional body modifier (`sync*`).
    fn open_line(&self, ret: &str, name: &str, params: &str, suffix: &str) -> String {
        let suffix = if suffix.is_empty() {
            String::new()
        } else {
            format!(" {suffix}")
        };
        match self {
            DartDecl::TopLevel | DartDecl::Method => format!("{ret} {name}({params}){suffix} {{"),
            DartDecl::Static => format!("static {ret} {name}({params}){suffix} {{"),
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
}

/// Bind every C symbol `f` calls, at top level. `leaf` marks synchronous
/// calls as leaf calls (only sound when the API declares no callback
/// interfaces, so no call can re-enter Dart).
pub(crate) fn emit_bindings(w: &mut CodeWriter, f: &FnBinding, leaf: bool) {
    match &f.shape {
        CallShape::Sync(abi) => emit_lookup(w, abi, leaf, None),
        CallShape::Async(a) => {
            let cb = ffi_typedef(&a.callback_type);
            let (natives, _) = slot_types(&a.callback_params, None);
            w.blank();
            w.line(format!(
                "typedef {cb} = Void Function({});",
                natives.join(", ")
            ));
            emit_lookup(w, &a.launch, false, Some(&cb));
        }
        CallShape::Iterator(ib) => {
            emit_lookup(w, &ib.launch, false, None);
            emit_lookup(w, &ib.next, false, None);
            emit_destroy(w, &ib.destroy_symbol, false);
        }
    }
}

/// Render `f`'s wrapper, declared as `kind` and named `name`.
pub(crate) fn emit_wrapper(
    w: &mut CodeWriter,
    f: &FnBinding,
    kind: &DartDecl,
    name: &str,
    err: ErrCtx,
) {
    w.blank();
    emit_wrapper_doc(w, f, err);
    match &f.shape {
        CallShape::Sync(abi) => render_sync(w, f, abi, kind, name, err),
        CallShape::Async(a) => render_async(w, f, a, kind, name, err),
        CallShape::Iterator(ib) => render_iterator(w, f, ib, kind, name, err),
    }
}

/// The (native, Dart) FFI types of `params`, substituting `callback` for a
/// bare named slot (an async launcher's completion callback).
fn slot_types(params: &[AbiParam], callback: Option<&str>) -> (Vec<String>, Vec<String>) {
    params
        .iter()
        .map(|p| match (&p.ty, callback) {
            (CType::Named(_), Some(cb)) => {
                let t = format!("Pointer<NativeFunction<{cb}>>");
                (t.clone(), t)
            }
            (ty, _) => ffi_type(ty),
        })
        .unzip()
}

/// Bind one C symbol with `lookupFunction`.
fn emit_lookup(w: &mut CodeWriter, abi: &AbiFn, leaf: bool, callback: Option<&str>) {
    let (natives, darts) = slot_types(&abi.params, callback);
    let (ret_n, ret_d) = ffi_type(&abi.ret);
    lookup(
        w,
        &abi.symbol,
        &format!("{ret_n} Function({})", natives.join(", ")),
        &format!("{ret_d} Function({})", darts.join(", ")),
        leaf,
    );
}

/// Emit `final _sym = _lib.lookupFunction<...>('sym');`, binding C symbol
/// `sym` with the given native and Dart function types.
pub(crate) fn lookup(w: &mut CodeWriter, sym: &str, native: &str, dart: &str, leaf: bool) {
    let leaf = if leaf { ", isLeaf: true" } else { "" };
    w.blank();
    w.line(format!("final {} = _lib.lookupFunction<", ffi_var(sym)));
    w.line(format!("    {native},"));
    w.line(format!("    {dart}>('{sym}'{leaf});"));
}

/// Bind a destroy symbol `sym` (`void (*)(void*)`) and the `NativeFinalizer`
/// over it, named after the binding.
pub(crate) fn emit_destroy(w: &mut CodeWriter, sym: &str, leaf: bool) {
    lookup(
        w,
        sym,
        "Void Function(Pointer<Void>)",
        "void Function(Pointer<Void>)",
        leaf,
    );
    w.line(format!(
        "final {}Finalizer = NativeFinalizer(",
        ffi_var(sym)
    ));
    w.line(format!(
        "    _lib.lookup<NativeFunction<Void Function(Pointer<Void>)>>('{sym}'));"
    ));
}

/// The wrapper's parameter list: the IDL parameters, plus a named
/// `cancelToken` for a cancellable async call.
fn wrapper_params(f: &FnBinding) -> String {
    let mut params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", dart_type(&p.ty), dart_ident(&p.name)))
        .collect();
    if f.cancellable {
        params.push(format!("{{CancelToken? {}}}", cancel_param(f)));
    }
    params.join(", ")
}

/// The name of a cancellable call's token parameter: `cancelToken`, unless
/// an IDL parameter already took it.
fn cancel_param(f: &FnBinding) -> &'static str {
    if f.params
        .iter()
        .any(|p| dart_ident(&p.name) == "cancelToken")
    {
        "cancelToken_"
    } else {
        "cancelToken"
    }
}

/// The marshalling of a callable's inputs: staging statements, call
/// arguments, and whether an `Arena` scopes them.
struct Inputs {
    /// Statements run before the call (staging, borrowing, registering).
    stage: Vec<String>,
    /// The call's argument expressions, in ABI order.
    args: Vec<String>,
    /// Whether the statements allocate from (or borrow through) `_arena`.
    arena: bool,
}

/// Marshal `f`'s inputs. An instance method's call passes `_self` first.
/// Callback registrations run last, after everything that can throw, so a
/// failed staging step never strands a registered implementation.
fn marshal_inputs(f: &FnBinding) -> Inputs {
    let mut stage = Vec::new();
    let mut registrations = Vec::new();
    let mut args = Vec::new();
    let mut arena = false;
    if f.has_self {
        args.push("_self".into());
    }
    for p in &f.params {
        let name = dart_ident(&p.name);
        match p.arg_pass() {
            ArgPass::Direct { .. } => args.push(match &p.ty {
                Ty::Enum(_) => format!("{name}.value"),
                _ => name,
            }),
            ArgPass::String { .. } => {
                arena = true;
                stage.push(format!("final _{name}Bytes = utf8.encode({name});"));
                args.push(format!("_stage(_arena, _{name}Bytes)"));
                args.push(format!("_{name}Bytes.length"));
            }
            ArgPass::Bytes { .. } => {
                arena = true;
                args.push(format!("_stage(_arena, {name})"));
                args.push(format!("{name}.length"));
            }
            ArgPass::Buffer { .. } => {
                arena = true;
                stage.push(format!(
                    "final _{name}Bytes = _encode({name}, {});",
                    pack_fn(&p.ty)
                ));
                args.push(format!("_stage(_arena, _{name}Bytes)"));
                args.push(format!("_{name}Bytes.length"));
            }
            // The wrapper keeps its reference and is borrowed for the call.
            ArgPass::Object { nullable, .. } => {
                arena = true;
                let ptr = format!("_{name}Ptr");
                stage.push(if nullable {
                    format!("final {ptr} = {name} == null ? nullptr : _borrow(_arena, {name});")
                } else {
                    format!("final {ptr} = _borrow(_arena, {name});")
                });
                args.push(ptr);
            }
            // The producer owns the registration and releases it through the
            // vtable's `free`; a null vtable means no implementation.
            ArgPass::Callback { nullable, .. } => {
                let cb =
                    p.ty.callback_interface_name()
                        .expect("callback family names a callback interface");
                let ctx = format!("_{name}Ctx");
                let register = format!("_registerCallback({name}, {})", dispatch_fn(cb));
                let vtable = format!("{}.cast<Void>()", vtable_var(cb));
                if nullable {
                    registrations.push(format!(
                        "final {ctx} = {name} == null ? nullptr : {register};"
                    ));
                    args.push(ctx);
                    args.push(format!("{name} == null ? nullptr : {vtable}"));
                } else {
                    registrations.push(format!("final {ctx} = {register};"));
                    args.push(ctx);
                    args.push(vtable);
                }
            }
        }
    }
    stage.extend(registrations);
    Inputs { stage, args, arena }
}

/// The expression adopting the owned object pointer `expr` into its wrapper;
/// a null pointer becomes `null` when `nullable`.
pub(crate) fn adopt_expr(expr: &str, ty: &Ty, nullable: bool) -> String {
    let class = object_class(ty);
    if nullable {
        format!("{expr} == nullptr ? null : {class}._({expr})")
    } else {
        format!("{class}._({expr})")
    }
}

/// The expression turning a produced value into its Dart value per its
/// [`RetPass`]: `value` is the pointer or scalar and `len` its length slot.
/// Owned strings, bytes, and buffers are copied and released; objects are
/// adopted.
pub(crate) fn receive_expr(ty: &Ty, pass: &RetPass, value: &str, len: &str) -> String {
    match pass {
        RetPass::Void => String::new(),
        RetPass::Direct => match ty {
            Ty::Enum(n) => format!("{}.fromValue({value})", dart_class(n)),
            _ => value.to_string(),
        },
        RetPass::String => format!("_takeString({value}, {len})"),
        RetPass::Bytes => format!("_takeBytes({value}, {len})"),
        RetPass::Buffer => format!("_decode(_takeBytes({value}, {len}), {})", unpack_fn(ty)),
        RetPass::Object { nullable, .. } => adopt_expr(value, ty, *nullable),
    }
}

/// Write `body` inside the try/finally that scopes `_self` and `_arena`, or
/// bare when the call needs neither.
fn scoped(w: &mut CodeWriter, has_self: bool, arena: bool, body: impl FnOnce(&mut CodeWriter)) {
    if has_self {
        w.line("final _self = _enter();");
    }
    if arena {
        w.line("final _arena = Arena();");
    }
    if !has_self && !arena {
        body(w);
        return;
    }
    w.line("try {");
    w.scope(body);
    w.line("} finally {");
    w.scope(|w| {
        if arena {
            w.line("_arena.releaseAll();");
        }
        if has_self {
            w.line("_leave();");
        }
    });
    w.line("}");
}

/// A synchronous wrapper: stage, call, check, receive.
fn render_sync(
    w: &mut CodeWriter,
    f: &FnBinding,
    abi: &AbiFn,
    kind: &DartDecl,
    name: &str,
    err: ErrCtx,
) {
    let ret = f.ret.as_ref().map_or("void".to_string(), dart_type);
    let pass = RetPass::of(f.ret.as_ref());
    let mut inputs = marshal_inputs(f);
    if matches!(pass, RetPass::String | RetPass::Bytes | RetPass::Buffer) {
        inputs.args.push("_outLen".into());
    }
    inputs.args.push("_err".into());
    let call = format!("{}({})", ffi_var(&abi.symbol), inputs.args.join(", "));
    w.block(
        kind.open_line(&ret, name, &wrapper_params(f), ""),
        "}",
        |w| {
            scoped(w, f.has_self, inputs.arena, |w| {
                for s in &inputs.stage {
                    w.line(s);
                }
                match (&pass, f.ret.as_ref()) {
                    (RetPass::Void, _) | (_, None) => {
                        w.line(format!("{call};"));
                        w.line(err.check_stmt());
                    }
                    (_, Some(ty)) => {
                        w.line(format!("final _result = {call};"));
                        w.line(err.check_stmt());
                        w.line(format!(
                            "return {};",
                            receive_expr(ty, &pass, "_result", "_outLen.value")
                        ));
                    }
                }
            });
        },
    );
}

/// An async wrapper: a `Future` completed from a `NativeCallable.listener`
/// the producer invokes exactly once, from any thread. Results and errors
/// it receives are owned, so decoding them later on the event loop is safe.
/// A cancellable call binds a native token to its `CancelToken` for the
/// call's duration.
fn render_async(
    w: &mut CodeWriter,
    f: &FnBinding,
    a: &AsyncBinding,
    kind: &DartDecl,
    name: &str,
    err: ErrCtx,
) {
    let cb = ffi_typedef(&a.callback_type);
    let ret = f.ret.as_ref().map_or("void".to_string(), dart_type);
    let pass = RetPass::of(f.ret.as_ref());
    let mut inputs = marshal_inputs(f);
    if f.cancellable {
        inputs.args.push("_cancel?.pointer ?? nullptr".into());
    }
    inputs.args.push("_callback.nativeFunction".into());
    inputs.args.push("nullptr".into());
    let call = format!("{}({});", ffi_var(&a.launch.symbol), inputs.args.join(", "));

    // The listener's parameters, named after the callback's result slots.
    let (_, darts) = slot_types(&a.callback_params, None);
    let names = ["_", "error", "value", "valueLen"];
    let listener_params: Vec<String> = darts
        .iter()
        .zip(names)
        .map(|(d, n)| format!("{d} {n}"))
        .collect();
    let result = match (&pass, f.ret.as_ref()) {
        (RetPass::Void, _) | (_, None) => String::new(),
        (_, Some(ty)) => receive_expr(ty, &pass, "value", "valueLen"),
    };

    let open = kind.open_line(&format!("Future<{ret}>"), name, &wrapper_params(f), "");
    let has_self = f.has_self;
    w.block(open, "}", |w| {
        if has_self {
            w.line("final _self = _enter();");
        }
        if inputs.arena {
            w.line("final _arena = Arena();");
        }
        w.line(format!("final _completer = Completer<{ret}>();"));
        if f.cancellable {
            w.line(format!(
                "final _cancel = _NativeCancel.bind({});",
                cancel_param(f)
            ));
        }
        w.line(format!("late final NativeCallable<{cb}> _callback;"));
        w.line(format!("_callback = NativeCallable<{cb}>.listener(("));
        w.line(format!("    {}) {{", listener_params.join(", ")));
        w.scope(|w| {
            w.line("_callback.close();");
            if f.cancellable {
                w.line("_cancel?.release();");
            }
            w.line("try {");
            w.scope(|w| {
                w.line(format!(
                    "if (error != nullptr) throw _takeAsyncError(error, {});",
                    err.mapper()
                ));
                w.line(format!("_completer.complete({result});"));
            });
            w.line("} catch (e, s) {");
            w.scope(|w| {
                w.line("_completer.completeError(e, s);");
            });
            w.line("}");
        });
        w.line("});");
        w.line("try {");
        w.scope(|w| {
            for s in &inputs.stage {
                w.line(s);
            }
            w.line(&call);
        });
        w.line("} catch (_) {");
        w.scope(|w| {
            w.line("_callback.close();");
            if f.cancellable {
                w.line("_cancel?.release();");
            }
            w.line("rethrow;");
        });
        if has_self || inputs.arena {
            w.line("} finally {");
            w.scope(|w| {
                if inputs.arena {
                    w.line("_arena.releaseAll();");
                }
                if has_self {
                    w.line("_leave();");
                }
            });
        }
        w.line("}");
        w.line("return _completer.future;");
    });
}

/// An iterator wrapper: a lazy `sync*` body that launches the native
/// iterator on the first pull, issues one `next` per element, and destroys
/// the iterator exactly once (eagerly on completion or failure, or through
/// the finalizer when the iteration is abandoned).
fn render_iterator(
    w: &mut CodeWriter,
    f: &FnBinding,
    ib: &IteratorBinding,
    kind: &DartDecl,
    name: &str,
    err: ErrCtx,
) {
    let ret = f.ret.as_ref().map_or("void".to_string(), dart_type);
    let elem = RetPass::of(Some(&ib.elem));
    let mut inputs = marshal_inputs(f);
    inputs.args.push("_err".into());
    let launch = format!("{}({})", ffi_var(&ib.launch.symbol), inputs.args.join(", "));
    // `next(iter, out_item, [out_len,] out_err)`: `_outItem` is read as the
    // item slot's pointee.
    let item_type = ffi_type(ib.item_ctype()).0;
    let has_len = ib.next.params.len() == 4;
    let next_args = if has_len {
        "_iter, _outItem.cast(), _outLen, _err"
    } else {
        "_iter, _outItem.cast(), _err"
    };
    let destroy = ffi_var(&ib.destroy_symbol);
    let item = receive_expr(
        &ib.elem,
        &elem,
        &format!("_outItem.cast<{item_type}>().value"),
        "_outLen.value",
    );

    w.block(
        kind.open_line(&ret, name, &wrapper_params(f), "sync*"),
        "}",
        |w| {
            w.line("final Pointer<Void> _iter;");
            scoped(w, f.has_self, inputs.arena, |w| {
                for s in &inputs.stage {
                    w.line(s);
                }
                w.line(format!("_iter = {launch};"));
                w.line(err.check_stmt());
            });
            w.line("final _anchor = _IteratorAnchor();");
            w.line(format!(
                "{destroy}Finalizer.attach(_anchor, _iter, detach: _anchor);"
            ));
            w.line("try {");
            w.scope(|w| {
                w.block("while (true) {", "}", |w| {
                    w.line(format!(
                        "final _more = {}({next_args});",
                        ffi_var(&ib.next.symbol)
                    ));
                    w.line(err.check_stmt());
                    w.line("if (_more == 0) break;");
                    w.line(format!("yield {item};"));
                });
            });
            w.line("} finally {");
            w.scope(|w| {
                w.line(format!("{destroy}Finalizer.detach(_anchor);"));
                w.line(format!("{destroy}(_iter);"));
            });
            w.line("}");
        },
    );
}
