//! Call rendering: sync, async, and iterator wrappers, callback and listener
//! trampolines, and the argument/return marshalling they share.
//!
//! Marshalling dispatch follows the shared plan layer: each parameter's
//! passing contract comes from [`ParamBinding::arg_pass`], each result's
//! receiving contract from [`plan::ret_pass`], so this module only spells
//! those contracts in Go rather than re-deriving them from `TypeRef`.

use heck::ToUpperCamelCase;
use weaveffi_core::abi::AbiParam;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    AsyncBinding, BindingModel, CallShape, CallbackBinding, FnBinding, IteratorBinding,
    ListenerBinding, ModuleBinding, ParamBinding,
};
use weaveffi_core::plan::{self, elem_free, ArgPass, ElemFree, ErrorStrategy, RetPass};
use weaveffi_core::utils::{c_abi_struct_name, wrapper_name};
use weaveffi_ir::ir::TypeRef;

use crate::codec::{emit_buffer_read, emit_buffer_write};
use crate::docs::{emit_doc, emit_fn_doc};
use crate::types::{
    c_scalar_conv, c_scalar_type, cgo_slot_type, go_local, go_param_ident, go_scalar_conv, go_type,
    go_wrap_expr, go_zero, strip_const,
};

// ── Errors ──

/// How a wrapper body reports a non-zero `weaveffi_error` slot.
///
/// A callable with `throws == true` returns `(T, error)` and maps codes
/// through the declaring module's typed helper (`wvMapKv`), falling back to
/// the generic [`ERROR_BRAND`](weaveffi_core::errors::ERROR_BRAND) struct
/// when no domain is in scope. A callable with `throws == false` has a plain
/// signature and panics via `wvTrap` instead, since a reported error can only
/// be a producer panic or an argument-marshalling failure.
#[derive(Clone, Copy)]
pub(crate) struct ErrCtx<'a> {
    /// `true` when the wrapper returns `(T, error)` and surfaces typed errors.
    pub(crate) throws: bool,
    /// PascalCase stem of the domain in effect (`Kv` names `wvMapKv`); `None`
    /// falls back to the generic `wvBrandError` constructor.
    stem: Option<&'a str>,
}

impl<'a> ErrCtx<'a> {
    /// Build the wrapper error context for `f` from the shared plan's
    /// [`ErrorStrategy`]: a `Throws` callable returns `(T, error)` through
    /// `stem`'s typed domain, a `Trap` callable panics via `wvTrap`.
    pub(crate) fn of(f: &FnBinding, stem: Option<&'a str>) -> Self {
        Self {
            throws: matches!(f.error_strategy(), ErrorStrategy::Throws),
            stem,
        }
    }

    /// The Go expression converting a taken `(code, message, payload)` triple
    /// into an `error` value.
    pub(crate) fn map_call(&self, args: &str) -> String {
        match self.stem {
            Some(stem) => format!("wvMap{stem}({args})"),
            None => format!("wvBrandError({args})"),
        }
    }

    /// Emit the statement(s) checking the error slot named `slot` at `w`'s
    /// current depth. A throwing wrapper returns `zero` (when the function
    /// has a result) plus the mapped error; a plain wrapper traps.
    fn emit_check(&self, w: &mut CodeWriter, slot: &str, zero: Option<&str>) {
        if self.throws {
            let map = self.map_call(&format!("wvTakeError(&{slot})"));
            w.block(format!("if {slot}.code != 0 {{"), "}", |w| {
                match zero {
                    Some(z) => w.line(format!("return {z}, {map}")),
                    None => w.line(format!("return {map}")),
                };
            });
        } else {
            w.line(format!("wvTrap(&{slot})"));
        }
    }

    /// The Go return-type suffix (including the leading space) of a wrapper
    /// returning `ret`: `(T, error)`/`error` when throwing, `T`/nothing when
    /// plain.
    fn ret_sig(&self, ret: &Option<TypeRef>) -> String {
        match (ret, self.throws) {
            (Some(r), true) => format!(" ({}, error)", go_type(r)),
            (Some(r), false) => format!(" {}", go_type(r)),
            (None, true) => " error".into(),
            (None, false) => String::new(),
        }
    }

    /// The suffix appended to every successful `return` statement: `, nil`
    /// when the wrapper also returns an error, empty otherwise.
    fn ok_tail(&self) -> &'static str {
        if self.throws {
            ", nil"
        } else {
            ""
        }
    }
}

// ── Callbacks, listeners, and async support ──

/// The C name of the exported Go trampoline for a callback/async typedef.
pub(crate) fn trampoline_name(c_type_name: &str) -> String {
    format!("goWv_{c_type_name}")
}

/// The preamble `extern` declaration for one exported trampoline.
fn extern_decl(c_type_name: &str, params: &[AbiParam], prefix: &str) -> String {
    let args: Vec<String> = params
        .iter()
        .map(|p| format!("{} {}", strip_const(&p.ty).render_c(prefix), p.name))
        .collect();
    format!(
        "extern void {}({});",
        trampoline_name(c_type_name),
        args.join(", ")
    )
}

/// Every `extern` declaration the preamble needs: one per module callback
/// (shared by all listeners firing it) and one per async completion callback,
/// including async interface members.
pub(crate) fn collect_trampoline_externs(model: &BindingModel, prefix: &str) -> Vec<String> {
    let mut decls = Vec::new();
    for m in &model.modules {
        for cb in &m.callbacks {
            decls.push(extern_decl(&cb.c_fn_type, &cb.abi_params, prefix));
        }
        for f in m.callables() {
            if let CallShape::Async(ab) = &f.shape {
                decls.push(extern_decl(&ab.callback_type, &ab.callback_params, prefix));
            }
        }
    }
    decls
}

/// The Go signature of the user-facing callback for a module callback decl,
/// e.g. `func(key string)`.
fn go_callback_sig(cb: &CallbackBinding) -> String {
    let params: Vec<String> = cb
        .params
        .iter()
        .map(|p| format!("{} {}", go_param_ident(&p.name), go_type(&p.ty)))
        .collect();
    format!("func({})", params.join(", "))
}

/// Emit statements converting one callback parameter's C slots into a Go
/// value bound to `arg{idx}`, returning that local's name.
///
/// Every callback argument is borrowed for the dispatch: buffered values are
/// decoded from the borrowed `(ptr, len)` pair, strings and bytes are copied,
/// and object pointers are wrapped without adopting ownership.
fn emit_cb_param_arg(
    out: &mut String,
    idx: usize,
    p: &ParamBinding,
    prefix: &str,
    module: &str,
) -> String {
    let arg = format!("arg{idx}");
    let mut w = CodeWriter::tabs().with_depth(1);
    match p.arg_pass() {
        ArgPass::Buffer { ptr, len } => {
            w.line(format!(
                "rArg{idx} := &wvReader{{buf: wvBorrowBuffer({}, {})}}",
                ptr.name, len.name
            ));
            w.line(format!("var {arg} {}", go_type(&p.ty)));
            emit_buffer_read(
                &mut w,
                &format!("rArg{idx}"),
                &arg,
                &p.ty,
                &format!("Arg{idx}"),
                0,
                prefix,
                module,
            );
            w.line(format!("rArg{idx}.expectEnd()"));
        }
        ArgPass::String { slot } => {
            let n = &slot.name;
            w.line(format!("{arg} := \"\""));
            w.block(format!("if {n} != nil {{"), "}", |w| {
                w.line(format!("{arg} = C.GoString({n})"));
            });
        }
        ArgPass::Bytes { ptr, len } => {
            w.line(format!("var {arg} []byte"));
            w.block(format!("if {} != nil {{", ptr.name), "}", |w| {
                w.line(format!(
                    "{arg} = C.GoBytes(unsafe.Pointer({}), C.int({}))",
                    ptr.name, len.name
                ));
            });
        }
        // Object pointers are borrowed for the duration of the callback; the
        // wrapper must not be Closed by the consumer.
        ArgPass::Object { slot, nullable } => {
            let n = &slot.name;
            if nullable {
                let TypeRef::Optional(inner) = &p.ty else {
                    unreachable!("nullable object params are optional interfaces")
                };
                let TypeRef::Interface(name) = inner.as_ref() else {
                    unreachable!("every other optional is buffered")
                };
                let g = go_local(name);
                w.line(format!("var {arg} *{g}"));
                w.block(format!("if {n} != nil {{"), "}", |w| {
                    w.line(format!("{arg} = &{g}{{ptr: {n}}}"));
                });
            } else {
                w.line(format!("{arg} := {}", go_wrap_expr(&p.ty, n)));
            }
        }
        ArgPass::Direct { slot } => {
            let n = &slot.name;
            match &p.ty {
                TypeRef::Bool => {
                    w.line(format!("{arg} := cToBool({n})"));
                }
                // A typed handle is a borrowed opaque pointer wrapped
                // without ownership, like an interface.
                TypeRef::TypedHandle(_) => {
                    w.line(format!("{arg} := {}", go_wrap_expr(&p.ty, n)));
                }
                _ => {
                    w.line(format!("{arg} := {}", go_scalar_conv(n, &p.ty)));
                }
            }
        }
    }
    out.push_str(&w.finish());
    arg
}

/// One exported trampoline per module callback declaration; every listener
/// firing this callback shares it, with the registry id in `context` selecting
/// the Go callback.
pub(crate) fn render_callback_trampoline(
    out: &mut String,
    prefix: &str,
    module: &str,
    cb: &CallbackBinding,
) {
    let tramp = trampoline_name(&cb.c_fn_type);
    let formals: Vec<String> = cb
        .abi_params
        .iter()
        .map(|s| format!("{} {}", s.name, cgo_slot_type(&s.ty, prefix)))
        .collect();

    let mut w = CodeWriter::tabs();
    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}) {{", formals.join(", ")),
        "}",
        |w| {
            w.line("v := wvCallbackLoad(uint64(uintptr(context)))");
            w.block("if v == nil {", "}", |w| {
                w.line("return");
            });
            w.line(format!("cb := v.({})", go_callback_sig(cb)));
            let mut args = Vec::new();
            for (idx, p) in cb.params.iter().enumerate() {
                let mut body = String::new();
                args.push(emit_cb_param_arg(&mut body, idx, p, prefix, module));
                w.raw(body);
            }
            w.line(format!("cb({})", args.join(", ")));
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// The register/unregister wrapper pair for one listener. The wrapper names
/// follow the module-prefix-stripping default like free functions
/// (`RegisterEvictionListener` rather than `KvRegisterEvictionListener`).
pub(crate) fn render_listener_api(
    out: &mut String,
    m: &ModuleBinding,
    l: &ListenerBinding,
    strip_module_prefix: bool,
) {
    let Some(cb) = m.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };
    let register_go = wrapper_name(
        &m.path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    )
    .to_upper_camel_case();
    let unregister_go = wrapper_name(
        &m.path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    )
    .to_upper_camel_case();
    let tramp = trampoline_name(&cb.c_fn_type);

    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &l.doc, "", Some(&register_go));
    w.raw(d);
    w.line(format!("// Returns a subscription id for {unregister_go}."));
    w.block(
        format!("func {register_go}(callback {}) uint64 {{", go_callback_sig(cb)),
        "}",
        |w| {
            w.line("ctxID := wvCallbackStore(callback)");
            w.line(format!(
                "id := uint64(C.{}(C.{}(unsafe.Pointer(C.{tramp})), unsafe.Pointer(uintptr(ctxID))))",
                l.register_symbol, cb.c_fn_type
            ));
            w.line("wvCallbackMu.Lock()");
            w.line("wvListenerCtx[id] = ctxID");
            w.line("wvCallbackMu.Unlock()");
            w.line("return id");
        },
    );
    w.blank();

    w.line(format!(
        "// {unregister_go} unregisters a listener previously registered with {register_go}."
    ));
    w.block(format!("func {unregister_go}(id uint64) {{"), "}", |w| {
        w.line(format!("C.{}(C.uint64_t(id))", l.unregister_symbol));
        w.line("wvCallbackMu.Lock()");
        w.line("ctxID, ok := wvListenerCtx[id]");
        w.line("delete(wvListenerCtx, id)");
        w.line("wvCallbackMu.Unlock()");
        w.block("if ok {", "}", |w| {
            w.line("wvCallbackDelete(ctxID)");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The per-async-function outcome payload type name, derived from the
/// (unique) base C symbol with the ABI prefix dropped: free function
/// `weaveffi_io_read` names `wvOutcomeIoRead`, interface member
/// `weaveffi_kv_Store_compact` names `wvOutcomeKvStoreCompact`.
fn async_outcome_type(prefix: &str, f: &FnBinding) -> String {
    let base = f
        .c_base
        .strip_prefix(&format!("{prefix}_"))
        .unwrap_or(&f.c_base);
    format!("wvOutcome{}", base.to_upper_camel_case())
}

/// Send the converted async result over the outcome channel. Runs inside the
/// completion trampoline after the error path has been handled.
///
/// Result buffers (strings, bytes, value buffers) are borrowed for the
/// callback's duration per the shared async protocol: they are decoded or
/// deep copied here and never freed (the producer releases them after the
/// callback returns). Owned interface results are the exception: the callback
/// receives ownership and the wrapper adopts the pointer (its `Close` calls
/// the destroy symbol).
fn emit_async_result_send(
    out: &mut String,
    ret: &Option<TypeRef>,
    outcome: &str,
    prefix: &str,
    module: &str,
) {
    let mut w = CodeWriter::tabs().with_depth(1);
    let Some(ty) = ret else {
        w.line(format!("ch <- {outcome}{{}}"));
        out.push_str(&w.finish());
        return;
    };
    match plan::ret_pass(Some(ty), module, prefix) {
        RetPass::Void => unreachable!("a present return type is never void"),
        RetPass::Buffer => {
            // Borrowed for the callback's duration: decode, do not free.
            w.line("rRes := &wvReader{buf: wvBorrowBuffer(result_ptr, result_len)}");
            w.line(format!("var val {}", go_type(ty)));
            emit_buffer_read(&mut w, "rRes", "val", ty, "Res", 0, prefix, module);
            w.line("rRes.expectEnd()");
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        RetPass::String => {
            // Borrowed for the callback's duration: copy, do not free.
            w.line("val := \"\"");
            w.block("if result != nil {", "}", |w| {
                w.line("val = C.GoString(result)");
            });
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        RetPass::Bytes => {
            // Borrowed for the callback's duration: copy, do not free.
            w.line("var val []byte");
            w.block("if result != nil {", "}", |w| {
                w.line("val = C.GoBytes(unsafe.Pointer(result), C.int(result_len))");
            });
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        // An owned interface result is adopted by the wrapper (its Close
        // calls the destroy symbol).
        RetPass::Object { nullable, .. } => {
            if nullable {
                let TypeRef::Optional(inner) = ty else {
                    unreachable!("nullable object results are optional interfaces")
                };
                let TypeRef::Interface(name) = inner.as_ref() else {
                    unreachable!("every other optional is buffered")
                };
                let g = go_local(name);
                w.line(format!("var val *{g}"));
                w.block("if result != nil {", "}", |w| {
                    w.line(format!("val = &{g}{{ptr: result}}"));
                });
                w.line(format!("ch <- {outcome}{{val: val}}"));
            } else {
                w.line(format!(
                    "ch <- {outcome}{{val: {}}}",
                    go_wrap_expr(ty, "result")
                ));
            }
        }
        RetPass::Direct => match ty {
            TypeRef::Bool => {
                w.line(format!("ch <- {outcome}{{val: cToBool(result)}}"));
            }
            // A typed handle is a borrowed id wrapped without ownership.
            TypeRef::TypedHandle(_) => {
                w.line(format!(
                    "ch <- {outcome}{{val: {}}}",
                    go_wrap_expr(ty, "result")
                ));
            }
            _ => {
                w.line(format!(
                    "ch <- {outcome}{{val: {}}}",
                    go_scalar_conv("result", ty)
                ));
            }
        },
    }
    out.push_str(&w.finish());
}

/// An async callable: a blocking Go wrapper that launches the C call with a
/// completion trampoline and waits on a buffered channel, plus the outcome
/// type and the exported trampoline itself.
///
/// The error split follows the shared plan's [`ErrorStrategy`]. A throwing
/// wrapper returns `(T, error)` and the trampoline maps a reported error
/// through the domain (`wvMap{Stem}`). A plain wrapper returns bare `T`; a
/// reported error can only be a producer bug, so the trampoline wraps it as
/// the generic brand error (never the typed domain) and the wrapper panics
/// with it on the calling goroutine (the trampoline itself must never panic:
/// it runs on a producer thread entered from C). With `receiver` set, the
/// wrapper is a method on that wrapper type passing `s.ptr` as the leading
/// launch argument.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_async_function(
    out: &mut String,
    prefix: &str,
    module: &str,
    f: &FnBinding,
    ab: &AsyncBinding,
    go_name: &str,
    receiver: Option<&str>,
    err: ErrCtx,
) {
    let outcome = async_outcome_type(prefix, f);
    let tramp = trampoline_name(&ab.callback_type);

    let mut w = CodeWriter::tabs();

    // Outcome payload: the converted result (if any) or the producer error.
    w.block(format!("type {outcome} struct {{"), "}", |w| {
        if let Some(ret) = &f.ret {
            w.line(format!("val {}", go_type(ret)));
        }
        w.line("err error");
    });
    w.blank();

    // The exported completion trampoline. It always converts a reported error
    // into a Go error and sends it over the channel; the wrapper decides
    // whether to return or panic with it.
    let formals: Vec<String> = ab
        .callback_params
        .iter()
        .map(|s| format!("{} {}", s.name, cgo_slot_type(&s.ty, prefix)))
        .collect();
    let mut tramp_body = String::new();
    emit_async_result_send(&mut tramp_body, &f.ret, &outcome, prefix, module);
    // A non-throwing function's error slot can only carry a producer bug:
    // brand it generically rather than dressing it as a typed domain error.
    let map_err = if err.throws {
        err.map_call("wvTakeError(err)")
    } else {
        "wvBrandError(wvTakeError(err))".to_string()
    };
    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}) {{", formals.join(", ")),
        "}",
        |w| {
            w.line("v := wvCallbackTake(uint64(uintptr(context)))");
            w.block("if v == nil {", "}", |w| {
                w.line("return");
            });
            w.line(format!("ch := v.(chan {outcome})"));
            w.block("if err != nil && err.code != 0 {", "}", |w| {
                w.line(format!("ch <- {outcome}{{err: {map_err}}}"));
                w.line("return");
            });
            w.raw(tramp_body.as_str());
        },
    );
    w.blank();

    // The blocking wrapper. Cancellation tokens are not surfaced (NULL).
    let go_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", go_param_ident(&p.name), go_type(&p.ty)))
        .collect();
    let ret_sig = err.ret_sig(&f.ret);
    let mut doc = String::new();
    emit_fn_doc(&mut doc, &f.doc, &f.params, "", go_name);
    w.raw(doc);
    w.line("// Blocks the calling goroutine until the async producer completes.");
    if let Some(msg) = &f.deprecated {
        w.line(format!("// Deprecated: {msg}"));
    }

    let mut pre = String::new();
    let mut c_args: Vec<String> = Vec::new();
    if receiver.is_some() {
        c_args.push("s.ptr".into());
    }
    for p in &f.params {
        emit_param(&mut pre, &mut c_args, p, prefix, module);
    }
    if f.cancellable {
        c_args.push("nil".into());
    }
    c_args.push(format!("C.{}(unsafe.Pointer(C.{tramp}))", ab.callback_type));
    c_args.push("unsafe.Pointer(uintptr(ctxID))".into());
    let launch_args = c_args.join(", ");

    let header = match receiver {
        Some(ty) => format!(
            "func (s *{ty}) {go_name}({}){ret_sig} {{",
            go_params.join(", ")
        ),
        None => format!("func {go_name}({}){ret_sig} {{", go_params.join(", ")),
    };
    w.block(header, "}", |w| {
        w.line(format!("ch := make(chan {outcome}, 1)"));
        w.line("ctxID := wvCallbackStore(ch)");
        w.raw(pre.as_str());
        w.line(format!("C.{}({})", ab.launch.symbol, launch_args));
        w.line("outcome := <-ch");
        if err.throws {
            if let Some(ret) = &f.ret {
                w.block("if outcome.err != nil {", "}", |w| {
                    w.line(format!("return {}, outcome.err", go_zero(ret)));
                });
                w.line("return outcome.val, nil");
            } else {
                w.line("return outcome.err");
            }
        } else {
            w.block("if outcome.err != nil {", "}", |w| {
                w.line("panic(outcome.err)");
            });
            if f.ret.is_some() {
                w.line("return outcome.val");
            }
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

// ── Functions ──

/// A sync or iterator callable: the Go wrapper marshalling parameters in,
/// invoking the C symbol, checking the error slot per `err` (typed
/// `(T, error)` when throwing, `wvTrap` panic when plain), and converting the
/// result out. An iterator-returning callable renders through
/// [`render_iterator_fn`] as a lazy sequence instead. With `receiver` set,
/// the wrapper is a method on that wrapper type passing `s.ptr` as the
/// leading C argument.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_function(
    out: &mut String,
    prefix: &str,
    module: &str,
    f: &FnBinding,
    go_name: &str,
    receiver: Option<&str>,
    err: ErrCtx,
) {
    if let CallShape::Iterator(ib) = &f.shape {
        render_iterator_fn(out, prefix, module, f, ib, go_name, receiver, err);
        return;
    }

    let c_sym = &f.c_base;

    let go_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", go_param_ident(&p.name), go_type(&p.ty)))
        .collect();

    let ret_sig = err.ret_sig(&f.ret);
    let header = match receiver {
        Some(ty) => format!(
            "func (s *{ty}) {go_name}({}){ret_sig} {{",
            go_params.join(", ")
        ),
        None => format!("func {go_name}({}){ret_sig} {{", go_params.join(", ")),
    };

    let mut w = CodeWriter::tabs();
    let mut doc = String::new();
    emit_fn_doc(&mut doc, &f.doc, &f.params, "", go_name);
    w.raw(doc);
    if let Some(msg) = &f.deprecated {
        w.line(format!("// Deprecated: {msg}"));
    }

    let mut pre = String::new();
    let mut c_args: Vec<String> = Vec::new();
    if receiver.is_some() {
        c_args.push("s.ptr".into());
    }

    for p in &f.params {
        emit_param(&mut pre, &mut c_args, p, prefix, module);
    }

    if let Some(ref ret) = f.ret {
        emit_return_out_params(&mut pre, &mut c_args, ret, prefix, module);
    }

    pre.push_str("\tvar cErr C.weaveffi_error\n");
    c_args.push("&cErr".into());

    let args = c_args.join(", ");

    w.block(header, "}", |w| {
        w.raw(pre.as_str());

        if f.ret.is_some() {
            w.line(format!("result := C.{c_sym}({args})"));
        } else {
            w.line(format!("C.{c_sym}({args})"));
        }

        err.emit_check(w, "cErr", f.ret.as_ref().map(go_zero).as_deref());

        if let Some(ref ret) = f.ret {
            let mut tail = String::new();
            emit_return(&mut tail, ret, prefix, module, err.ok_tail());
            w.raw(tail);
        } else if err.throws {
            w.line("return nil");
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Go type of the `out_item` local whose address is passed to an iterator's
/// `next` (the C slot is `T*`, so the local is one indirection less).
/// Buffered and bytes elements arrive as a `const uint8_t*` buffer pointer.
fn iter_out_item_type(inner: &TypeRef, prefix: &str, module: &str) -> String {
    match elem_free(inner) {
        ElemFree::String => "*C.char".into(),
        ElemFree::Bytes => "*C.uint8_t".into(),
        ElemFree::None => match inner {
            TypeRef::TypedHandle(n) | TypeRef::Interface(n) => {
                format!("*C.{}", c_abi_struct_name(n, module, prefix))
            }
            _ => c_scalar_type(inner, prefix, module).unwrap_or_else(|| "C.int64_t".into()),
        },
    }
}

/// Re-indent `block` by one tab per non-empty line, used to move depth-1
/// staging code inside the sequence closure body.
fn indent_block(block: &str) -> String {
    let mut out = String::with_capacity(block.len());
    for line in block.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push('\t');
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Emit the statements converting one freshly-pulled `next` slot (`outItem`,
/// plus `outLen` for bytes/buffered elements) into a Go value bound to
/// `item`, releasing the slot per the protocol's [`ElemFree`] plan: strings
/// are freed after copying, bytes and buffered elements are copied/decoded
/// and released with `weaveffi_free_bytes` (via `wvCopyBuffer`), and by-value
/// elements owe nothing.
fn emit_iter_elem_bind(
    w: &mut CodeWriter,
    inner: &TypeRef,
    ef: &ElemFree,
    prefix: &str,
    module: &str,
) {
    match ef {
        ElemFree::String => {
            w.line("item := C.GoString(outItem)");
            w.line("C.weaveffi_free_string(outItem)");
        }
        ElemFree::Bytes => {
            if matches!(inner, TypeRef::Bytes | TypeRef::BorrowedBytes) {
                w.line("item := wvCopyBuffer(outItem, outLen)");
            } else {
                w.line("rItem := &wvReader{buf: wvCopyBuffer(outItem, outLen)}");
                w.line(format!("var item {}", go_type(inner)));
                emit_buffer_read(w, "rItem", "item", inner, "Item", 0, prefix, module);
                w.line("rItem.expectEnd()");
            }
        }
        ElemFree::None => match inner {
            TypeRef::Bool => {
                w.line("item := cToBool(outItem)");
            }
            // Typed handles and interfaces are opaque pointers the consumer
            // adopts, even though `elem_free` owes no runtime call for them.
            TypeRef::TypedHandle(_) | TypeRef::Interface(_) => {
                w.line(format!("item := {}", go_wrap_expr(inner, "outItem")));
            }
            _ => {
                let conv = go_scalar_conv("outItem", inner);
                w.line(format!("item := {conv}"));
            }
        },
    }
}

/// An `iter<T>`-returning callable, rendered per the shared
/// [`weaveffi_core::plan::IteratorProtocol`] pull contract as Go's standard
/// lazy iteration idiom (the `iter` package, Go 1.23+):
///
/// - A non-throwing function returns `iter.Seq[T]`. A launch or per-`next`
///   error can only be a producer bug, so it panics with the weaveffi
///   message via `wvTrap` ([`ErrorStrategy::Trap`]).
/// - A throwing function returns `iter.Seq2[T, error]`. A launch or
///   per-`next` domain error is yielded as the final `(zero, err)` pair and
///   iteration stops ([`ErrorStrategy::Throws`]).
///
/// The producer iterator is launched lazily inside the returned closure, so
/// an unused sequence allocates nothing on the producer side. One C `next`
/// call runs per consumer step, each yielded element is released per the
/// protocol's [`ElemFree`] plan after conversion, and the destroy runs
/// exactly once through a `defer` inside the closure, whether the sequence
/// is exhausted, stops on an error, or is abandoned by an early `break`.
#[allow(clippy::too_many_arguments)]
fn render_iterator_fn(
    out: &mut String,
    prefix: &str,
    module: &str,
    f: &FnBinding,
    ib: &IteratorBinding,
    go_name: &str,
    receiver: Option<&str>,
    err: ErrCtx,
) {
    let proto = ib.protocol(f);
    let throws = matches!(proto.error, ErrorStrategy::Throws);
    let elem = &ib.elem;
    let elem_go = go_type(elem);
    let item_ty = iter_out_item_type(elem, prefix, module);
    let has_len = matches!(proto.elem_free, ElemFree::Bytes);
    let zero = go_zero(elem);

    let go_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", go_param_ident(&p.name), go_type(&p.ty)))
        .collect();
    let (seq_ty, yield_ty) = if throws {
        (
            format!("iter.Seq2[{elem_go}, error]"),
            format!("func({elem_go}, error) bool"),
        )
    } else {
        (
            format!("iter.Seq[{elem_go}]"),
            format!("func({elem_go}) bool"),
        )
    };
    let header = match receiver {
        Some(ty) => format!(
            "func (s *{ty}) {go_name}({}) {seq_ty} {{",
            go_params.join(", ")
        ),
        None => format!("func {go_name}({}) {seq_ty} {{", go_params.join(", ")),
    };

    let mut w = CodeWriter::tabs();
    let mut doc = String::new();
    emit_fn_doc(&mut doc, &f.doc, &f.params, "", go_name);
    w.raw(doc);
    w.line("// Returns a lazy sequence: the producer iterator is launched on first");
    w.line("// iteration and one producer next call runs per element. The iterator is");
    w.line("// destroyed exactly once, whether the sequence is exhausted or abandoned");
    w.line("// early; each range over the sequence launches a fresh producer iterator.");
    if throws {
        w.line("// A launch or per-element error is yielded as the final (zero value,");
        w.line("// error) pair, and iteration stops.");
    } else {
        w.line("// A reported error can only be a producer bug and panics with the");
        w.line("// weaveffi message.");
    }
    if let Some(msg) = &f.deprecated {
        w.line(format!("// Deprecated: {msg}"));
    }

    // Parameter staging runs inside the closure so C strings and buffers are
    // live at launch time and each range restages them.
    let mut pre = String::new();
    let mut c_args: Vec<String> = Vec::new();
    if receiver.is_some() {
        c_args.push("s.ptr".into());
    }
    for p in &f.params {
        emit_param(&mut pre, &mut c_args, p, prefix, module);
    }
    c_args.push("&cErr".into());

    // Statements surfacing a non-zero error slot: yield the mapped domain
    // error and stop when throwing, trap when plain.
    let emit_err_check = |w: &mut CodeWriter, slot: &str| {
        if throws {
            let map = err.map_call(&format!("wvTakeError(&{slot})"));
            w.block(format!("if {slot}.code != 0 {{"), "}", |w| {
                w.line(format!("yield({zero}, {map})"));
                w.line("return");
            });
        } else {
            w.line(format!("wvTrap(&{slot})"));
        }
    };

    let next_args = if has_len {
        "it, &outItem, &outLen, &iterErr"
    } else {
        "it, &outItem, &iterErr"
    };

    w.block(header, "}", |w| {
        w.block(format!("return func(yield {yield_ty}) {{"), "}", |w| {
            w.raw(indent_block(&pre));
            w.line("var cErr C.weaveffi_error");
            w.line(format!(
                "it := C.{}({})",
                ib.launch.symbol,
                c_args.join(", ")
            ));
            emit_err_check(w, "cErr");
            w.line(format!("defer C.{}(it)", ib.destroy_symbol));
            w.block("for {", "}", |w| {
                w.line(format!("var outItem {item_ty}"));
                if has_len {
                    w.line("var outLen C.size_t");
                }
                w.line("var iterErr C.weaveffi_error");
                w.line(format!("ok := C.{}({next_args}) != 0", ib.next.symbol));
                emit_err_check(w, "iterErr");
                w.block("if !ok {", "}", |w| {
                    w.line("return");
                });
                emit_iter_elem_bind(w, elem, &proto.elem_free, prefix, module);
                let yield_call = if throws {
                    "if !yield(item, nil) {"
                } else {
                    "if !yield(item) {"
                };
                w.block(yield_call, "}", |w| {
                    w.line("return");
                });
            });
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

// ── Parameter and return marshalling ──

/// Emit the staging statements and C argument expressions for one Go
/// parameter, dispatching on the shared [`ArgPass`] contract. A buffered
/// parameter is packed into a `wvWriter` and passed as a borrowed
/// `(ptr, len)` pair; the C-owned encoding lives in Go memory kept alive for
/// the duration of the call by cgo's argument-pinning rules.
fn emit_param(
    pre: &mut String,
    args: &mut Vec<String>,
    p: &ParamBinding,
    prefix: &str,
    module: &str,
) {
    let name = go_param_ident(&p.name);
    let mut w = CodeWriter::tabs().with_depth(1);
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            let n = name.to_upper_camel_case();
            w.line(format!("w{n} := &wvWriter{{}}"));
            emit_buffer_write(&mut w, &format!("w{n}"), &name, &p.ty, &n, 0);
            w.line(format!("var c{n}Ptr *C.uint8_t"));
            w.block(format!("if len(w{n}.buf) > 0 {{"), "}", |w| {
                w.line(format!(
                    "c{n}Ptr = (*C.uint8_t)(unsafe.Pointer(&w{n}.buf[0]))"
                ));
            });
            args.push(format!("c{n}Ptr"));
            args.push(format!("C.size_t(len(w{n}.buf))"));
        }
        ArgPass::String { .. } => {
            let cv = format!("c{}", name.to_upper_camel_case());
            w.line(format!("{cv} := C.CString({name})"));
            w.line(format!("defer C.free(unsafe.Pointer({cv}))"));
            args.push(cv);
        }
        ArgPass::Bytes { .. } => {
            let pv = format!("c{}Ptr", name.to_upper_camel_case());
            let lv = format!("c{}Len", name.to_upper_camel_case());
            w.line(format!("var {pv} *C.uint8_t"));
            w.line(format!("{lv} := C.size_t(len({name}))"));
            w.block(format!("if len({name}) > 0 {{"), "}", |w| {
                w.line(format!("{pv} = (*C.uint8_t)(unsafe.Pointer(&{name}[0]))"));
            });
            args.push(pv);
            args.push(lv);
        }
        // A borrowed object pointer; when nullable, nil stages a NULL slot.
        ArgPass::Object { slot, nullable } => {
            if nullable {
                let cv = format!("c{}", name.to_upper_camel_case());
                w.line(format!("var {cv} {}", cgo_slot_type(&slot.ty, prefix)));
                w.block(format!("if {name} != nil {{"), "}", |w| {
                    w.line(format!("{cv} = {name}.ptr"));
                });
                args.push(cv);
            } else {
                args.push(format!("{name}.ptr"));
            }
        }
        ArgPass::Direct { .. } => match &p.ty {
            TypeRef::Handle => args.push(format!("C.weaveffi_handle_t({name})")),
            // A typed handle passes its wrapped opaque pointer by value.
            TypeRef::TypedHandle(_) => args.push(format!("{name}.ptr")),
            _ => args.push(c_scalar_conv(&name, &p.ty, prefix, module)),
        },
    }
    pre.push_str(&w.finish());
}

/// Emit the out-parameter locals a return type needs. Bytes and buffered
/// returns carry one trailing `size_t* out_len` slot; everything else has
/// none.
fn emit_return_out_params(
    pre: &mut String,
    args: &mut Vec<String>,
    ty: &TypeRef,
    prefix: &str,
    module: &str,
) {
    if matches!(
        plan::ret_pass(Some(ty), module, prefix),
        RetPass::Bytes | RetPass::Buffer
    ) {
        let mut w = CodeWriter::tabs().with_depth(1);
        w.line("var cOutLen C.size_t");
        args.push("&cOutLen".into());
        pre.push_str(&w.finish());
    }
}

/// Emit the success-path return conversion, dispatching on the shared
/// [`RetPass`] contract. `tail` is [`ErrCtx::ok_tail`]: `", nil"` when the
/// wrapper also returns an error, empty when plain.
///
/// A buffered return is copied out of the producer-allocated buffer (which
/// `wvCopyBuffer` releases with `weaveffi_free_bytes`), decoded, and checked
/// for trailing bytes.
fn emit_return(out: &mut String, ty: &TypeRef, prefix: &str, module: &str, tail: &str) {
    let mut w = CodeWriter::tabs().with_depth(1);
    match plan::ret_pass(Some(ty), module, prefix) {
        RetPass::Void => unreachable!("a present return type is never void"),
        RetPass::Buffer => {
            w.line("rRes := &wvReader{buf: wvCopyBuffer(result, cOutLen)}");
            w.line(format!("var goResult {}", go_type(ty)));
            emit_buffer_read(&mut w, "rRes", "goResult", ty, "Res", 0, prefix, module);
            w.line("rRes.expectEnd()");
            w.line(format!("return goResult{tail}"));
        }
        RetPass::String => {
            w.line("goResult := C.GoString(result)");
            w.line("C.weaveffi_free_string(result)");
            w.line(format!("return goResult{tail}"));
        }
        RetPass::Bytes => {
            w.block("if result == nil {", "}", |w| {
                w.line(format!("return nil{tail}"));
            });
            w.line("goResult := C.GoBytes(unsafe.Pointer(result), C.int(cOutLen))");
            w.line("C.weaveffi_free_bytes(result, cOutLen)");
            w.line(format!("return goResult{tail}"));
        }
        // An owned object pointer the wrapper adopts; when nullable, a NULL
        // return means none.
        RetPass::Object { nullable, .. } => {
            if nullable {
                let TypeRef::Optional(inner) = ty else {
                    unreachable!("nullable object returns are optional interfaces")
                };
                let TypeRef::Interface(n) = inner.as_ref() else {
                    unreachable!("every other optional is buffered")
                };
                let g = go_local(n);
                w.block("if result == nil {", "}", |w| {
                    w.line(format!("return nil{tail}"));
                });
                w.line(format!("return &{g}{{ptr: result}}{tail}"));
            } else {
                w.line(format!("return {}{tail}", go_wrap_expr(ty, "result")));
            }
        }
        RetPass::Direct => match ty {
            TypeRef::Bool => {
                w.line(format!("return cToBool(result){tail}"));
            }
            // A typed handle is a borrowed id wrapped without ownership.
            TypeRef::TypedHandle(_) => {
                w.line(format!("return {}{tail}", go_wrap_expr(ty, "result")));
            }
            _ => {
                let conv = go_scalar_conv("result", ty);
                w.line(format!("return {conv}{tail}"));
            }
        },
    }
    out.push_str(&w.finish());
}
