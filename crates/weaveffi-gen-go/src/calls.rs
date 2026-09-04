//! Call rendering: sync, async, and iterator wrappers, callback-interface
//! types and trampolines, and the argument/return marshalling they share.
//!
//! Marshalling dispatch follows the shared plan layer: each parameter's
//! passing contract comes from [`ParamBinding::arg_pass`], each result's
//! receiving contract from [`plan::ret_pass`], so this module only spells
//! those contracts in Go rather than re-deriving them from `Ty`.

use heck::ToUpperCamelCase;
use weaveffi_core::abi::{AbiParam, CType};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::lang;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    AsyncBinding, BindingModel, CallShape, CallbackInterfaceBinding, CallbackMethodBinding,
    FnBinding, IteratorBinding, ModuleBinding, ParamBinding,
};
use weaveffi_core::plan::{self, ArgPass, ErrorStrategy, RetPass};
use weaveffi_core::utils::c_abi_struct_name;

use crate::codec::{emit_buffer_read, emit_buffer_write};
use crate::docs::{emit_doc, emit_fn_doc};
use crate::types::{
    c_scalar_conv, cgo_slot_type, go_adopt_expr, go_param_ident, go_scalar_conv, go_type, go_zero,
    strip_const, vtable_accessor, vtable_var,
};

// ── Errors ──

/// How a wrapper body reports a non-zero `weaveffi_error` slot.
///
/// A callable with `throws == true` returns `(T, error)` and maps codes
/// through the declaring module's typed helper (`wvMapKv`), falling back to
/// the generic [`ERROR_BRAND`](weaveffi_core::errors::ERROR_BRAND) struct
/// when no domain is in scope. A callable with `throws == false` has a plain
/// signature and panics via `wvTrap` instead, since a reported error can only
/// be a producer panic, an argument-marshalling failure, or a callback
/// implementation that panicked.
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
    fn ret_sig(&self, ret: &Option<Ty>) -> String {
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

// ── Trampolines and the cgo preamble ──

/// The Go spelling of one C ABI slot name inside an exported trampoline's
/// formal list: a slot named after a Go keyword (an IDL parameter called
/// `type`, `func`, `range`) gains the trailing-underscore escape. The same
/// spelling is used in the preamble `extern` so the two prototypes agree.
fn go_slot_ident(name: &str) -> String {
    lang::escape_ident(name, lang::GO_KEYWORDS)
}

/// The C name of the exported Go trampoline for an async completion typedef.
pub(crate) fn trampoline_name(c_type_name: &str) -> String {
    format!("goWv_{c_type_name}")
}

/// The C name of the exported Go trampoline behind one callback-interface
/// vtable entry (`goWv_{c_tag}_{method}`); `free` names the trailing entry.
fn cb_trampoline_name(c_tag: &str, method: &str) -> String {
    format!("goWv_{c_tag}_{method}")
}

/// The preamble `extern` declaration for one exported trampoline. Pointer
/// types are rendered const-free to match the prototypes cgo writes into
/// `_cgo_export.h` from the Go signature.
fn extern_decl(name: &str, ret: &CType, params: &[AbiParam], prefix: &str) -> String {
    let args: Vec<String> = params
        .iter()
        .map(|p| {
            format!(
                "{} {}",
                strip_const(&p.ty).render_c(prefix),
                go_slot_ident(&p.name)
            )
        })
        .collect();
    format!(
        "extern {} {name}({});",
        ret.render_c(prefix),
        args.join(", ")
    )
}

/// The preamble definition of the one process-wide static vtable for `cb`:
/// one trampoline per method in declaration order, then `free`. An entry
/// whose C signature carries `const` pointers is cast back to the vtable's
/// exact field type, since the exported Go function is declared const-free.
fn vtable_def(cb: &CallbackInterfaceBinding, prefix: &str) -> String {
    let mut s = format!(
        "static const {} {} = {{\n",
        cb.vtable_tag,
        vtable_var(&cb.c_tag)
    );
    for m in &cb.methods {
        let tramp = cb_trampoline_name(&cb.c_tag, &m.name);
        let needs_cast = m.abi_params.iter().any(|p| strip_const(&p.ty) != p.ty);
        if needs_cast {
            let types: Vec<String> = m.abi_params.iter().map(|p| p.ty.render_c(prefix)).collect();
            s.push_str(&format!(
                "    ({} (*)({})){tramp},\n",
                m.abi_ret.render_c(prefix),
                types.join(", ")
            ));
        } else {
            s.push_str(&format!("    {tramp},\n"));
        }
    }
    s.push_str(&format!(
        "    {},\n}};\n",
        cb_trampoline_name(&cb.c_tag, "free")
    ));
    // cgo reaches a C variable through `//go:cgo_import_static`, which needs
    // external linkage, so a `static const` table can't be named as
    // `&C.wvVtable_...` from Go. A static accessor function is callable
    // through the ordinary cgo stub in the same translation unit.
    s.push_str(&format!(
        "static const {}* {}(void) {{ return &{}; }}",
        cb.vtable_tag,
        vtable_accessor(&cb.c_tag),
        vtable_var(&cb.c_tag)
    ));
    s
}

/// Every declaration the cgo preamble needs beyond the header include: for
/// each callback interface, the `extern` prototypes of its method and `free`
/// trampolines followed by its static vtable and accessor; then one `extern`
/// per async completion callback, including async interface members.
///
/// A file that uses `//export` may only put declarations in its preamble
/// (the preamble is compiled into two C translation units); the vtable is a
/// `static const` so each unit gets a private copy and no symbol is
/// duplicated. Go takes the address of the copy in its own unit through the
/// static accessor, so the producer always sees one vtable whose entries
/// live for the process.
pub(crate) fn collect_preamble_decls(model: &BindingModel, prefix: &str) -> Vec<String> {
    let mut decls = Vec::new();
    for m in &model.modules {
        for cb in &m.callback_interfaces {
            for meth in &cb.methods {
                decls.push(extern_decl(
                    &cb_trampoline_name(&cb.c_tag, &meth.name),
                    &meth.abi_ret,
                    &meth.abi_params,
                    prefix,
                ));
            }
            decls.push(extern_decl(
                &cb_trampoline_name(&cb.c_tag, "free"),
                &CType::Void,
                &[AbiParam::new("ctx", CType::ptr(CType::Void))],
                prefix,
            ));
            decls.push(vtable_def(cb, prefix));
        }
        for f in m.callables() {
            if let CallShape::Async(ab) = &f.shape {
                decls.push(extern_decl(
                    &trampoline_name(&ab.callback_type),
                    &CType::Void,
                    &ab.callback_params,
                    prefix,
                ));
            }
        }
    }
    decls
}

// ── Callback interfaces ──

/// The Go method signature of one callback-interface method, as it appears
/// in the consumer-implemented interface type: `OnMessage(text string,
/// weight int32) int64`.
fn cb_method_sig(m: &CallbackMethodBinding) -> String {
    let params: Vec<String> = m
        .params
        .iter()
        .map(|p| format!("{} {}", go_param_ident(&p.name), go_type(&p.ty)))
        .collect();
    let ret = match &m.ret {
        Some(ty) => format!(" {}", go_type(ty)),
        None => String::new(),
    };
    format!(
        "{}({}){ret}",
        m.name.to_upper_camel_case(),
        params.join(", ")
    )
}

/// Emit statements converting one callback-method parameter's C slots into
/// a Go value bound to `arg{idx}`, returning that local's name.
///
/// Strings, bytes, and buffers arriving in a trampoline are borrowed for the
/// dispatch: they are copied or decoded and nothing is freed. An object
/// argument transfers one strong reference, which is adopted into a wrapper
/// (a null `Interface?` adopts to nil).
fn emit_cb_param_arg(out: &mut String, idx: usize, p: &ParamBinding) -> String {
    let arg = format!("arg{idx}");
    let mut w = CodeWriter::tabs().with_depth(1);
    match p.arg_pass() {
        ArgPass::Buffer { ptr, len } => {
            w.line(format!(
                "rArg{idx} := &wvReader{{buf: wvBorrowBuffer({}, {})}}",
                go_slot_ident(&ptr.name),
                go_slot_ident(&len.name)
            ));
            w.line(format!("var {arg} {}", go_type(&p.ty)));
            emit_buffer_read(
                &mut w,
                &format!("rArg{idx}"),
                &arg,
                &p.ty,
                &format!("Arg{idx}"),
                0,
            );
            w.line(format!("rArg{idx}.expectEnd()"));
        }
        ArgPass::String { slot } => {
            let n = go_slot_ident(&slot.name);
            w.line(format!("{arg} := \"\""));
            w.block(format!("if {n} != nil {{"), "}", |w| {
                w.line(format!("{arg} = C.GoString({n})"));
            });
        }
        ArgPass::Bytes { ptr, len } => {
            let pn = go_slot_ident(&ptr.name);
            let ln = go_slot_ident(&len.name);
            w.line(format!("var {arg} []byte"));
            w.block(format!("if {pn} != nil {{"), "}", |w| {
                w.line(format!(
                    "{arg} = C.GoBytes(unsafe.Pointer({pn}), C.int({ln}))"
                ));
            });
        }
        ArgPass::Object { slot, .. } => {
            w.line(format!(
                "{arg} := {}",
                go_adopt_expr(&p.ty, &go_slot_ident(&slot.name))
            ));
        }
        ArgPass::Direct { slot } => {
            let n = go_slot_ident(&slot.name);
            w.line(format!("{arg} := {}", go_scalar_conv(&n, &p.ty)));
        }
        ArgPass::Callback { .. } => {
            unreachable!("validation rejects callback interfaces as callback-method parameters")
        }
    }
    out.push_str(&w.finish());
    arg
}

/// Emit the exported trampoline behind one vtable entry. It recovers the
/// implementation from the `cgo.Handle` passed as `ctx`, converts the
/// borrowed arguments, calls the Go method, and writes a direct-family
/// result into the C return. A panic in the implementation is recovered,
/// reported through `weaveffi_error_set(out_err, -4, message)`, and the
/// zero value is returned; nothing ever unwinds through the C frame.
fn render_cb_trampoline(
    w: &mut CodeWriter,
    prefix: &str,
    module: &str,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    iface_name: &str,
) {
    let tramp = cb_trampoline_name(&cb.c_tag, &m.name);
    let formals: Vec<String> = m
        .abi_params
        .iter()
        .map(|s| {
            format!(
                "{} {}",
                go_slot_ident(&s.name),
                cgo_slot_type(&s.ty, prefix)
            )
        })
        .collect();
    let ret_sig = match &m.ret {
        Some(_) => format!(" (ret {})", cgo_slot_type(&m.abi_ret, prefix)),
        None => String::new(),
    };
    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}){ret_sig} {{", formals.join(", ")),
        "}",
        |w| {
            w.block("defer func() {", "}()", |w| {
                w.block("if r := recover(); r != nil {", "}", |w| {
                    w.line("wvForeignError(out_err, r)");
                });
            });
            w.line(format!(
                "impl := cgo.Handle(uintptr(ctx)).Value().({iface_name})"
            ));
            let mut args = Vec::new();
            for (idx, p) in m.params.iter().enumerate() {
                let mut body = String::new();
                args.push(emit_cb_param_arg(&mut body, idx, p));
                w.raw(body);
            }
            let call = format!("impl.{}({})", m.name.to_upper_camel_case(), args.join(", "));
            match &m.ret {
                Some(ty) => {
                    w.line(format!(
                        "ret = {}",
                        c_scalar_conv(&call, ty, prefix, module)
                    ));
                    w.line("return");
                }
                None => {
                    w.line(call);
                }
            }
        },
    );
    w.blank();
}

/// Render one callback interface: the Go `interface` type the consumer
/// implements (one method per IDL method, PascalCase, direct-family or void
/// returns), the exported trampoline behind each vtable entry, and the
/// `free` trampoline that deletes the `cgo.Handle` once the producer drops
/// its last reference to the callback.
///
/// Passing an implementation to a producer function stores it in a
/// `cgo.Handle` (the `void* ctx` slot) and passes the address of the
/// interface's static vtable from the cgo preamble (see
/// [`collect_preamble_decls`]). The producer may invoke any trampoline from
/// any thread; cgo attaches the calling thread to the Go runtime.
pub(crate) fn render_callback_interface(
    out: &mut String,
    prefix: &str,
    module: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
) {
    let name = cb.name.to_upper_camel_case();
    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &cb.doc, "", Some(&name));
    if d.is_empty() {
        w.line(format!(
            "// {name} is a callback interface: implement it in Go and pass the value to"
        ));
        w.line("// native functions that accept it.");
    } else {
        w.raw(d);
        w.line("//");
        w.line("// Implement this interface in Go and pass the value to native functions");
        w.line("// that accept it.");
    }
    w.line("//");
    w.line("// The native library may call any method from any thread until it releases");
    w.line("// the implementation. A panic in a method is reported to the native caller");
    w.line("// as a foreign error (code -4) instead of crashing the process.");
    if let Some(msg) = &cb.deprecated {
        w.line("//");
        w.line(format!("// Deprecated: {msg}"));
    }
    w.block(format!("type {name} interface {{"), "}", |w| {
        for m in &cb.methods {
            let mut md = String::new();
            emit_fn_doc(
                &mut md,
                &m.doc,
                &m.params,
                "\t",
                &m.name.to_upper_camel_case(),
            );
            w.raw(md);
            if let Some(msg) = &m.deprecated {
                w.line(format!("// Deprecated: {msg}"));
            }
            w.line(cb_method_sig(m));
        }
    });
    w.blank();

    for m in &cb.methods {
        render_cb_trampoline(&mut w, prefix, &module.path, cb, m, &name);
    }

    let free = cb_trampoline_name(&cb.c_tag, "free");
    w.line(format!("//export {free}"));
    w.block(format!("func {free}(ctx unsafe.Pointer) {{"), "}", |w| {
        w.line("cgo.Handle(uintptr(ctx)).Delete()");
    });
    w.blank();
    out.push_str(&w.finish());
}

// ── Async ──

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
/// Result buffers (strings, bytes, value buffers) are owned by the consumer
/// per the shared async protocol: they are copied or decoded here and then
/// released through the runtime free symbols. An owned object result
/// transfers one strong reference, adopted into a wrapper.
fn emit_async_result_send(
    out: &mut String,
    ret: &Option<Ty>,
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
            // Owned by the consumer: wvCopyBuffer copies, then frees.
            w.line("rRes := &wvReader{buf: wvCopyBuffer(result_ptr, result_len)}");
            w.line(format!("var val {}", go_type(ty)));
            emit_buffer_read(&mut w, "rRes", "val", ty, "Res", 0);
            w.line("rRes.expectEnd()");
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        RetPass::String => {
            // Owned by the consumer: copy, then free.
            w.line("val := \"\"");
            w.block("if result != nil {", "}", |w| {
                w.line("val = C.GoString(result)");
                w.line("C.weaveffi_free_string(result)");
            });
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        RetPass::Bytes => {
            // Owned by the consumer: copy, then free.
            w.line("var val []byte");
            w.block("if result != nil {", "}", |w| {
                w.line("val = C.GoBytes(unsafe.Pointer(result), C.int(result_len))");
                w.line("C.weaveffi_free_bytes(result, result_len)");
            });
            w.line(format!("ch <- {outcome}{{val: val}}"));
        }
        RetPass::Object { .. } => {
            w.line(format!(
                "ch <- {outcome}{{val: {}}}",
                go_adopt_expr(ty, "result")
            ));
        }
        RetPass::Direct => {
            w.line(format!(
                "ch <- {outcome}{{val: {}}}",
                go_scalar_conv("result", ty)
            ));
        }
    }
    out.push_str(&w.finish());
}

/// An async callable: a blocking Go wrapper that launches the C call with a
/// completion trampoline and waits on a buffered channel, plus the outcome
/// type and the exported trampoline itself. The channel travels to the
/// producer as a `cgo.Handle` in the `context` slot; the trampoline resolves
/// and deletes it, so the completion is delivered exactly once.
///
/// The error split follows the shared plan's [`ErrorStrategy`]. A throwing
/// wrapper returns `(T, error)` and the trampoline maps a reported error
/// through the domain (`wvMap{Stem}`). A plain wrapper returns bare `T`; a
/// reported error can only be a producer bug, so the trampoline wraps it as
/// the generic brand error (never the typed domain) and the wrapper panics
/// with it on the calling goroutine (the trampoline itself must never panic:
/// it runs on a producer thread entered from C). With `receiver` set, the
/// wrapper is a method on that wrapper type passing `s.ptr` as the leading
/// launch argument and keeping the receiver alive until completion.
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
        .map(|s| {
            format!(
                "{} {}",
                go_slot_ident(&s.name),
                cgo_slot_type(&s.ty, prefix)
            )
        })
        .collect();
    let mut tramp_body = String::new();
    emit_async_result_send(&mut tramp_body, &f.ret, &outcome, prefix, module);
    // A non-throwing function's error slot can only carry a producer bug:
    // brand it generically rather than dressing it as a typed domain error.
    let map_err = if err.throws {
        err.map_call("wvTakeBoxedError(err)")
    } else {
        "wvBrandError(wvTakeBoxedError(err))".to_string()
    };
    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}) {{", formals.join(", ")),
        "}",
        |w| {
            w.line("h := cgo.Handle(uintptr(context))");
            w.line(format!("ch := h.Value().(chan {outcome})"));
            w.line("h.Delete()");
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
    c_args.push("C.wvHandlePtr(C.uintptr_t(h))".into());
    let launch_args = c_args.join(", ");

    let header = match receiver {
        Some(ty) => format!(
            "func (s *{ty}) {go_name}({}){ret_sig} {{",
            go_params.join(", ")
        ),
        None => format!("func {go_name}({}){ret_sig} {{", go_params.join(", ")),
    };
    w.block(header, "}", |w| {
        if let Some(ty) = receiver {
            w.raw(receiver_guard(ty, "\t"));
        }
        w.line(format!("ch := make(chan {outcome}, 1)"));
        w.line("h := cgo.NewHandle(ch)");
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
/// leading C argument and keeping the receiver alive across the call.
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
    if let Some(ty) = receiver {
        pre.push_str(&receiver_guard(ty, "\t"));
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
/// `next`: the pointee of the `T* out_item` slot, so one indirection less
/// than the C slot. Strings arrive as `*C.char`, bytes and buffered elements
/// as a `*C.uint8_t` buffer pointer, objects as `*C.{tag}`, and direct
/// values as their scalar C type.
fn iter_out_item_type(ib: &IteratorBinding, prefix: &str) -> String {
    let CType::Ptr { pointee, .. } = &ib.next.params[1].ty else {
        unreachable!("an iterator's out_item slot is always a pointer")
    };
    cgo_slot_type(pointee, prefix)
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
/// `item`, receiving it per the protocol's [`RetPass`] plan: strings are
/// copied then freed, bytes and buffered elements are copied/decoded and
/// released with `weaveffi_free_bytes` (via `wvCopyBuffer`), objects are
/// adopted into a wrapper, and by-value elements owe nothing.
fn emit_iter_elem_bind(w: &mut CodeWriter, inner: &Ty, elem: &RetPass) {
    match elem {
        RetPass::Void => unreachable!("an iterator element is never void"),
        RetPass::String => {
            w.line("item := C.GoString(outItem)");
            w.line("C.weaveffi_free_string(outItem)");
        }
        RetPass::Bytes => {
            w.line("item := wvCopyBuffer(outItem, outLen)");
        }
        RetPass::Buffer => {
            w.line("rItem := &wvReader{buf: wvCopyBuffer(outItem, outLen)}");
            w.line(format!("var item {}", go_type(inner)));
            emit_buffer_read(w, "rItem", "item", inner, "Item", 0);
            w.line("rItem.expectEnd()");
        }
        RetPass::Object { .. } => {
            w.line(format!("item := {}", go_adopt_expr(inner, "outItem")));
        }
        RetPass::Direct => {
            w.line(format!("item := {}", go_scalar_conv("outItem", inner)));
        }
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
/// call runs per consumer step, each yielded element is received per the
/// protocol's element [`RetPass`] after conversion, and the destroy runs
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
    let proto = ib.protocol(f, module, prefix);
    let throws = matches!(proto.error, ErrorStrategy::Throws);
    let elem = &ib.elem;
    let elem_go = go_type(elem);
    let item_ty = iter_out_item_type(ib, prefix);
    let has_len = matches!(proto.elem, RetPass::Bytes | RetPass::Buffer);
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
    if let Some(ty) = receiver {
        pre.push_str(&receiver_guard(ty, "\t"));
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
                emit_iter_elem_bind(w, elem, &proto.elem);
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

/// The statements every method opens with: reject a wrapper whose reference
/// was already released by `Close` (the ABI never accepts a null object
/// there, so a Go panic beats a producer-side abort), then pin the wrapper
/// so its finalizer cannot release the object while the call is in flight.
fn receiver_guard(ty: &str, indent: &str) -> String {
    format!(
        "{indent}if s.ptr == nil {{\n{indent}\tpanic(\"weaveffi: {ty} used after Close\")\n{indent}}}\n{indent}defer runtime.KeepAlive(s)\n"
    )
}

/// Emit the staging statements and C argument expressions for one Go
/// parameter, dispatching on the shared [`ArgPass`] contract.
///
/// A buffered parameter is packed into a `wvWriter` and passed as a borrowed
/// `(ptr, len)` pair; the encoding lives in Go memory kept alive for the
/// duration of the call by cgo's argument-pinning rules. An object parameter
/// passes the wrapper's own pointer (borrowed; the wrapper keeps its
/// reference) and pins the wrapper with `runtime.KeepAlive` so its finalizer
/// cannot release the object mid-call; a nil `Interface?` stages a NULL
/// slot. A callback interface stores the implementation in a `cgo.Handle`
/// passed as `ctx` alongside the address of the interface's static vtable.
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
        ArgPass::Object { slot, nullable } => {
            w.line(format!("defer runtime.KeepAlive({name})"));
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
        ArgPass::Callback { .. } => {
            let Ty::CallbackInterface(cb) = &p.ty else {
                unreachable!("callback family names a callback interface")
            };
            let hv = format!("h{}", name.to_upper_camel_case());
            w.line(format!("{hv} := cgo.NewHandle({name})"));
            args.push(format!("C.wvHandlePtr(C.uintptr_t({hv}))"));
            args.push(format!(
                "C.{}()",
                vtable_accessor(&c_abi_struct_name(cb, module, prefix))
            ));
        }
        ArgPass::Direct { .. } => args.push(c_scalar_conv(&name, &p.ty, prefix, module)),
    }
    pre.push_str(&w.finish());
}

/// Emit the out-parameter locals a return type needs. Bytes and buffered
/// returns carry one trailing `size_t* out_len` slot; everything else has
/// none.
fn emit_return_out_params(
    pre: &mut String,
    args: &mut Vec<String>,
    ty: &Ty,
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
/// for trailing bytes. An object return transfers one strong reference that
/// the wrapper adopts; a null `Interface?` adopts to nil.
fn emit_return(out: &mut String, ty: &Ty, prefix: &str, module: &str, tail: &str) {
    let mut w = CodeWriter::tabs().with_depth(1);
    match plan::ret_pass(Some(ty), module, prefix) {
        RetPass::Void => unreachable!("a present return type is never void"),
        RetPass::Buffer => {
            w.line("rRes := &wvReader{buf: wvCopyBuffer(result, cOutLen)}");
            w.line(format!("var goResult {}", go_type(ty)));
            emit_buffer_read(&mut w, "rRes", "goResult", ty, "Res", 0);
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
        RetPass::Object { .. } => {
            w.line(format!("return {}{tail}", go_adopt_expr(ty, "result")));
        }
        RetPass::Direct => {
            let conv = go_scalar_conv("result", ty);
            w.line(format!("return {conv}{tail}"));
        }
    }
    out.push_str(&w.finish());
}
