//! Call-path renderers: the wrapper methods for every call shape (sync,
//! async, iterator), argument marshalling driven by [`ArgPass`], result
//! receiving driven by [`Family`], and the per-module static classes.
//!
//! Every wrapper follows one frame: encode arguments (UTF-8 strings, value
//! buffers, callback registrations), pin them with one `fixed` statement,
//! call the import with a stack `FfiError`, check it, and receive the result.

use crate::codegen::CodeWriter;
use crate::utils::{local_type_name, wrapper_name};
use heck::ToUpperCamelCase;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{
    AsyncBinding, CallShape, ErrorBinding, Family, FnBinding, IteratorBinding, ModuleBinding,
    ParamBinding, Ty,
};
use weaveffi_model::plan::{ArgPass, ErrorStrategy};

use crate::targets::dotnet::codec::{emit_decode, emit_write};
use crate::targets::dotnet::docs::{write_doc, write_fn_doc};
use crate::targets::dotnet::runtime::dotnet_exception_name;
use crate::targets::dotnet::types::{
    camel_fn, cs_ctype, cs_type, safe_cs_name, vtable_class_cs, Cx,
};

/// The `[UnmanagedCallersOnly]` attribute every native entry point carries.
pub(crate) const UNMANAGED_CALLERS_ONLY: &str =
    "[UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]";

/// How a wrapper maps a non-zero error slot to an exception, rendering
/// [`ErrorStrategy`]: a throwing function with a domain in scope uses the
/// domain's typed `FromError`; everything else (producer traps) uses the
/// base exception's.
#[derive(Clone)]
pub(crate) struct ErrCtx<'a> {
    cx: Cx<'a>,
    map: String,
    domain: Option<(String, String)>,
}

impl<'a> ErrCtx<'a> {
    /// The error context for one function.
    pub(crate) fn for_fn(f: &FnBinding, error: Option<&ErrorBinding>, cx: Cx<'a>) -> Self {
        match (f.error_strategy(), error) {
            (ErrorStrategy::Throws, Some(eb)) => {
                let exc = dotnet_exception_name(eb);
                ErrCtx {
                    cx,
                    map: format!("{}.FromError", cx.ty(&exc)),
                    domain: Some((exc, eb.type_name.clone())),
                }
            }
            _ => ErrCtx {
                cx,
                map: format!("{}.FromError", cx.ty(cx.base)),
                domain: None,
            },
        }
    }

    /// The statement throwing when the local `FfiError` named `var` holds a
    /// failure.
    fn check(&self, var: &str) -> String {
        format!(
            "if ({var}.Code != 0) throw Ffi.TakeError(&{var}, {});",
            self.map
        )
    }

    /// Emit the `<exception>` doc line of a throwing wrapper.
    fn write_doc(&self, w: &mut CodeWriter) {
        if let Some((exc, ty)) = &self.domain {
            w.line(format!(
                "/// <exception cref=\"{exc}\">The call reported a {ty} code.</exception>"
            ));
        }
    }
}

/// The receiver of a wrapper: what the method is a member of.
#[derive(Clone, Copy)]
pub(crate) enum Receiver<'a> {
    /// A free function or interface static: a `static` method.
    Static,
    /// An interface instance method: passes `Handle` as `self`.
    Instance,
    /// The interface's `new` constructor: a real C# constructor of the
    /// named class.
    Constructor(&'a str),
}

/// The `[Obsolete]` attribute line for a deprecated item.
pub(crate) fn write_obsolete(w: &mut CodeWriter, deprecated: &Option<String>) {
    if let Some(msg) = deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }
}

/// The argument marshalling for one parameter list: statements run before
/// the call, the buffers pinned for it, one expression per ABI slot, and the
/// callback registrations to release if the call never reaches the producer.
struct Marshal {
    setup: String,
    pins: Vec<String>,
    args: Vec<String>,
    callbacks: Vec<String>,
}

impl Marshal {
    /// Plan the marshalling of `params` (already camelCased).
    fn new(cx: Cx<'_>, params: &[ParamBinding]) -> Self {
        let mut pins = Vec::new();
        let mut args = Vec::new();
        let mut callbacks = Vec::new();
        let mut setup = CodeWriter::four_space();
        for p in params {
            let n = safe_cs_name(&p.name);
            let local = p.name.as_str();
            match p.arg_pass() {
                ArgPass::Direct { .. } => args.push(direct_to_slot(&p.ty, &n)),
                ArgPass::String { .. } => {
                    setup.line(format!("var {local}Bytes = Ffi.Utf8({n});"));
                    pins.push(format!("{local}Ptr = {local}Bytes"));
                    args.push(format!("{local}Ptr"));
                    args.push(format!("(nuint){local}Bytes.Length"));
                }
                ArgPass::Bytes { .. } => {
                    pins.push(format!("{local}Ptr = {n}"));
                    args.push(format!("{local}Ptr"));
                    args.push(format!("(nuint){n}.Length"));
                }
                ArgPass::Buffer { .. } => {
                    let writer = format!("{local}Writer");
                    setup.line(format!("var {writer} = new FfiBufferWriter();"));
                    emit_write(&mut setup, &p.ty, &n, &writer, 0);
                    pins.push(format!("{local}Ptr = {writer}.Written"));
                    args.push(format!("{local}Ptr"));
                    args.push(format!("(nuint){writer}.Length"));
                }
                ArgPass::Object { nullable, .. } => {
                    if nullable {
                        let iface =
                            p.ty.interface_name()
                                .expect("object types name an interface");
                        args.push(format!("{n}?.Handle ?? {}.NativeHandle.Null", cx.ty(iface)));
                    } else {
                        args.push(format!("{n}.Handle"));
                    }
                }
                // The producer owns the registration once the call reaches
                // it: its vtable `free(ctx)` releases it.
                ArgPass::Callback { vtable, .. } => {
                    args.push(format!("{local}Ctx"));
                    args.push(format!("{}.Pointer", vtable_class_for_slot(&vtable.ty)));
                    callbacks.push((format!("{local}Ctx"), n));
                }
            }
        }
        // Register last, so a failing encode above can't strand a
        // registration.
        for (ctx, n) in &callbacks {
            setup.line(format!("var {ctx} = Ffi.Register({n});"));
        }
        let callbacks = callbacks.into_iter().map(|(ctx, _)| ctx).collect();
        Marshal {
            setup: setup.finish(),
            pins,
            args,
            callbacks,
        }
    }

    /// Emit the setup statements.
    fn write_setup(&self, w: &mut CodeWriter) {
        w.block_raw(&self.setup);
    }

    /// Emit `body` inside one `fixed` statement pinning every buffer, or bare
    /// when nothing needs pinning.
    fn pinned(&self, w: &mut CodeWriter, body: impl FnOnce(&mut CodeWriter)) {
        if self.pins.is_empty() {
            body(w);
            return;
        }
        w.line(format!("fixed (byte* {})", self.pins.join(", ")));
        w.line("{");
        w.scope(body);
        w.line("}");
    }

    /// The statements releasing every callback registration.
    fn unregister(&self, w: &mut CodeWriter) {
        for ctx in &self.callbacks {
            w.line(format!("Ffi.Unregister({ctx});"));
        }
    }
}

/// The C# class hosting the static vtable a callback slot points at, named
/// from the slot's vtable C type.
fn vtable_class_for_slot(vtable: &CType) -> String {
    let CType::Ptr { pointee, .. } = vtable else {
        unreachable!("a callback vtable slot is a pointer")
    };
    let CType::VtableTag { module, name } = pointee.as_ref() else {
        unreachable!("a callback vtable slot points at a vtable tag")
    };
    vtable_class_cs(module, name)
}

/// The expression converting a surface value into its by-value slot: `bool`
/// as a byte, a C-style enum as its `int`, every other scalar as is.
pub(crate) fn direct_to_slot(ty: &Ty, expr: &str) -> String {
    match ty {
        Ty::Bool => format!("(byte)({expr} ? 1 : 0)"),
        Ty::Enum(_) => format!("(int){expr}"),
        _ => expr.to_string(),
    }
}

/// The expression converting a by-value slot into its surface type.
pub(crate) fn direct_from_slot(ty: &Ty, slot: &str) -> String {
    match ty {
        Ty::Bool => format!("{slot} != 0"),
        Ty::Enum(name) => format!("({}){slot}", local_type_name(name)),
        _ => slot.to_string(),
    }
}

/// Who owns a received `(ptr, len)` run.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Own {
    /// The consumer: copy or decode it, then release it with `free_bytes`
    /// (returns, async results, iterator items).
    Take,
    /// The producer, for the duration of a callback: copy or decode only.
    Borrow,
}

/// Emit any statements receiving a value of `ty` from its slots (`ptr`, plus
/// `len` for the `(ptr, len)` families) and return the expression holding
/// the received value. `var` names the decoded local for buffers. Objects
/// always transfer one strong reference, which the new wrapper adopts.
pub(crate) fn receive(
    w: &mut CodeWriter,
    cx: Cx<'_>,
    ty: &Ty,
    ptr: &str,
    len: &str,
    own: Own,
    var: &str,
) -> String {
    let verb = match own {
        Own::Take => "Take",
        Own::Borrow => "Read",
    };
    match ty.family() {
        Family::Direct => direct_from_slot(ty, ptr),
        Family::String => format!("Ffi.{verb}String({ptr}, {len})"),
        Family::Bytes => format!("Ffi.{verb}Bytes({ptr}, {len})"),
        Family::Buffer => {
            let reader = match own {
                Own::Take => format!("Ffi.TakeBuffer({ptr}, {len})"),
                Own::Borrow => format!("new FfiBufferReader(Ffi.ReadBytes({ptr}, {len}))"),
            };
            emit_decode(w, cx, ty, var, &reader);
            var.to_string()
        }
        Family::Object { nullable } => {
            let class = cx.ty(ty.interface_name().expect("object types name an interface"));
            if nullable {
                format!("{ptr} == IntPtr.Zero ? null : {class}.Adopt({ptr})")
            } else {
                format!("{class}.Adopt({ptr})")
            }
        }
        Family::Callback | Family::Iterator => unreachable!("{ty} is never received"),
    }
}

/// The public signature's parameter list. With `span`, bytes parameters are
/// `ReadOnlySpan<byte>`.
fn params_sig(params: &[ParamBinding], span: bool) -> Vec<String> {
    params
        .iter()
        .map(|p| {
            let ty = if span && p.ty == Ty::Bytes {
                "ReadOnlySpan<byte>".to_string()
            } else {
                cs_type(&p.ty)
            };
            format!("{ty} {}", safe_cs_name(&p.name))
        })
        .collect()
}

/// The argument list forwarding a `byte[]` overload to its span overload.
fn forward_args(params: &[ParamBinding]) -> String {
    params
        .iter()
        .map(|p| {
            let n = safe_cs_name(&p.name);
            if p.ty == Ty::Bytes {
                format!("(ReadOnlySpan<byte>){n}")
            } else {
                n
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The slot arguments of a call: the receiver (when any), every marshalled
/// parameter slot, then `extra`.
fn call_args(receiver: Receiver<'_>, m: &Marshal, extra: &[String]) -> String {
    let mut args = Vec::new();
    if matches!(receiver, Receiver::Instance) {
        args.push("Handle".to_string());
    }
    args.extend(m.args.iter().cloned());
    args.extend(extra.iter().cloned());
    args.join(", ")
}

/// The signature line opener for a wrapper: modifiers, return type, and
/// name (or the bare class name for a constructor).
fn opener(receiver: Receiver<'_>, ret: &str, name: &str) -> String {
    match receiver {
        Receiver::Static => format!("public static {ret} {name}"),
        Receiver::Instance => format!("public {ret} {name}"),
        Receiver::Constructor(class) => format!("public {class}"),
    }
}

/// Render one wrapper (any shape) named `name`.
pub(crate) fn render_callable(
    w: &mut CodeWriter,
    f: &FnBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx<'_>,
) {
    let f = camel_fn(f);
    match &f.shape {
        CallShape::Sync(abi) => {
            let span = f.params.iter().any(|p| p.ty == Ty::Bytes);
            render_sync(w, &f, &abi.params, &abi.ret, name, receiver, err, span);
            if span {
                render_array_overload(w, &f, name, receiver, err);
            }
        }
        CallShape::Async(a) => render_async(w, &f, a, name, receiver, err),
        CallShape::Iterator(it) => render_iterator(w, &f, it, name, receiver, err),
    }
}

/// The documentation, `<exception>`, and `[Obsolete]` lines of a wrapper.
fn write_header(w: &mut CodeWriter, f: &FnBinding, err: &ErrCtx<'_>) {
    write_fn_doc(w, &f.doc, &f.params);
    err.write_doc(w);
    write_obsolete(w, &f.deprecated);
}

/// A synchronous wrapper: one call, one check, one received result. A
/// constructor stores the adopted pointer as its handle instead of
/// returning it.
#[allow(clippy::too_many_arguments)]
fn render_sync(
    w: &mut CodeWriter,
    f: &FnBinding,
    slots: &[AbiParam],
    ret: &CType,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx<'_>,
    span: bool,
) {
    let ret_cs = match receiver {
        Receiver::Constructor(_) => String::new(),
        _ => f.ret.as_ref().map(cs_type).unwrap_or_else(|| "void".into()),
    };
    let cx = err.cx;
    let m = Marshal::new(cx, &f.params);
    let has_out_len = slots.iter().any(|s| s.name == "out_len");
    let mut extra = Vec::new();
    if has_out_len {
        extra.push("&ffiOutLen".to_string());
    }
    extra.push("&ffiErr".to_string());
    let call = format!(
        "NativeMethods.{}({})",
        f.c_base,
        call_args(receiver, &m, &extra)
    );

    write_header(w, f, err);
    w.line(format!(
        "{}({})",
        opener(receiver, &ret_cs, name),
        params_sig(&f.params, span).join(", ")
    ));
    w.block("{", "}", |w| {
        m.write_setup(w);
        w.line("var ffiErr = default(FfiError);");
        if has_out_len {
            w.line("nuint ffiOutLen = 0;");
        }
        m.pinned(w, |w| {
            let returns = f.ret.is_some();
            if m.callbacks.is_empty() {
                if returns {
                    w.line(format!("var ffiResult = {call};"));
                } else {
                    w.line(format!("{call};"));
                }
            } else {
                if returns {
                    w.line(format!("{} ffiResult;", cs_ctype(ret)));
                }
                w.line("try");
                w.block("{", "}", |w| {
                    if returns {
                        w.line(format!("ffiResult = {call};"));
                    } else {
                        w.line(format!("{call};"));
                    }
                });
                w.line("catch");
                w.block("{", "}", |w| {
                    m.unregister(w);
                    w.line("throw;");
                });
            }
            w.line(err.check("ffiErr"));
            match (&f.ret, receiver) {
                (Some(_), Receiver::Constructor(_)) => {
                    w.line("Handle = new NativeHandle(ffiResult);");
                }
                (Some(ty), _) => {
                    let expr =
                        receive(w, cx, ty, "ffiResult", "ffiOutLen", Own::Take, "ffiDecoded");
                    w.line(format!("return {expr};"));
                }
                (None, _) => {}
            }
        });
    });
    w.blank();
}

/// The `byte[]` overload of a wrapper whose bytes parameters are
/// `ReadOnlySpan<byte>`, forwarding to it.
fn render_array_overload(
    w: &mut CodeWriter,
    f: &FnBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx<'_>,
) {
    let ret_cs = f.ret.as_ref().map(cs_type).unwrap_or_else(|| "void".into());
    let args = forward_args(&f.params);
    write_header(w, f, err);
    let sig = format!(
        "{}({})",
        opener(receiver, &ret_cs, name),
        params_sig(&f.params, false).join(", ")
    );
    match receiver {
        Receiver::Constructor(_) => {
            w.line(format!("{sig} : this({args})"));
            w.line("{");
            w.line("}");
        }
        _ => {
            w.line(format!("{sig} => {name}({args});"));
        }
    }
    w.blank();
}

/// An async wrapper: a `Task`-returning method launching the call with a
/// pending `FfiCall` as `context`, plus the `[UnmanagedCallersOnly]`
/// completion that resolves it. A cancellable function takes a
/// `CancellationToken` linked to the native cancel token.
fn render_async(
    w: &mut CodeWriter,
    f: &FnBinding,
    a: &AsyncBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx<'_>,
) {
    let result_cs = f.ret.as_ref().map(cs_type);
    let task = match &result_cs {
        Some(t) => format!("Task<{t}>"),
        None => "Task".into(),
    };
    let call_ty = result_cs.clone().unwrap_or_else(|| "bool".into());
    let complete = format!("Complete{name}");
    let cx = err.cx;
    let m = Marshal::new(cx, &f.params);
    let mut extra = Vec::new();
    if f.cancellable {
        extra.push("ffiCall.CancelToken()".to_string());
    }
    extra.push(format!("&{complete}"));
    extra.push("ffiCall.Context".to_string());
    let call = format!(
        "NativeMethods.{}({});",
        a.launch.symbol,
        call_args(receiver, &m, &extra)
    );

    write_header(w, f, err);
    if f.cancellable {
        w.line("/// <param name=\"cancellationToken\">Cancels the native call; the task");
        w.line("/// then completes as canceled.</param>");
    }
    let mut sig = params_sig(&f.params, false);
    if f.cancellable {
        sig.push("CancellationToken cancellationToken = default".into());
    }
    let opener = match receiver {
        Receiver::Instance => format!("public {task} {name}"),
        _ => format!("public static {task} {name}"),
    };
    w.line(format!("{opener}({})", sig.join(", ")));
    w.block("{", "}", |w| {
        if f.cancellable {
            w.line("if (cancellationToken.IsCancellationRequested)");
            w.block("{", "}", |w| {
                let from = match &result_cs {
                    Some(t) => format!("Task.FromCanceled<{t}>(cancellationToken)"),
                    None => "Task.FromCanceled(cancellationToken)".into(),
                };
                w.line(format!("return {from};"));
            });
        }
        m.write_setup(w);
        let token = if f.cancellable {
            "cancellationToken"
        } else {
            ""
        };
        w.line(format!("var ffiCall = new FfiCall<{call_ty}>({token});"));
        w.line("try");
        w.block("{", "}", |w| {
            m.pinned(w, |w| {
                w.line(&call);
            });
        });
        w.line("catch");
        w.block("{", "}", |w| {
            w.line("ffiCall.Abandon();");
            m.unregister(w);
            w.line("throw;");
        });
        w.line("return ffiCall.Task;");
    });
    w.blank();

    // The completion: fires exactly once, on a producer thread.
    let slots: Vec<String> = a
        .callback_params
        .iter()
        .map(|s| format!("{} {}", cs_ctype(&s.ty), safe_cs_name(&s.name)))
        .collect();
    w.line(UNMANAGED_CALLERS_ONLY);
    w.line(format!(
        "private static void {complete}({})",
        slots.join(", ")
    ));
    w.block("{", "}", |w| {
        w.line(format!(
            "var ffiCall = FfiCall<{call_ty}>.Complete(context);"
        ));
        w.line("try");
        w.block("{", "}", |w| {
            w.line("if (err != null)");
            w.block("{", "}", |w| {
                w.line(format!("ffiCall.SetError(err, {});", err.map));
                w.line("return;");
            });
            match &f.ret {
                None => {
                    w.line("ffiCall.SetResult(true);");
                }
                Some(ty) => {
                    let pair = a.callback_params.iter().any(|s| s.name == "result_len");
                    let (ptr, len) = if pair {
                        ("result_ptr", "result_len")
                    } else {
                        ("result", "")
                    };
                    let expr = receive(w, cx, ty, ptr, len, Own::Take, "ffiDecoded");
                    w.line(format!("ffiCall.SetResult({expr});"));
                }
            }
        });
        w.line("catch (Exception e)");
        w.block("{", "}", |w| {
            w.line("ffiCall.SetException(e);");
        });
    });
    w.blank();
}

/// An iterator wrapper: the launcher runs eagerly (so launch errors throw
/// at the call), and the returned single-use sequence pulls one item per
/// `MoveNext` through a static `Next{Name}` helper. The native iterator is a
/// `SafeHandle`, destroyed exactly once on exhaustion, disposal, or
/// finalization.
fn render_iterator(
    w: &mut CodeWriter,
    f: &FnBinding,
    it: &IteratorBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx<'_>,
) {
    let elem = cs_type(&it.elem);
    let next = format!("Next{name}");
    let cx = err.cx;
    let m = Marshal::new(cx, &f.params);
    let call = format!(
        "NativeMethods.{}({})",
        it.launch.symbol,
        call_args(receiver, &m, &["&ffiErr".to_string()])
    );

    write_header(w, f, err);
    w.line("/// <remarks>Streams lazily, one native call per item. The sequence can be");
    w.line("/// enumerated once; disposing its enumerator (as <c>foreach</c> does)");
    w.line("/// releases the native iterator.</remarks>");
    let ret = format!("IEnumerable<{elem}>");
    w.line(format!(
        "{}({})",
        opener(receiver, &ret, name),
        params_sig(&f.params, false).join(", ")
    ));
    w.block("{", "}", |w| {
        m.write_setup(w);
        w.line("var ffiErr = default(FfiError);");
        m.pinned(w, |w| {
            if m.callbacks.is_empty() {
                w.line(format!("var ffiResult = {call};"));
            } else {
                w.line("IntPtr ffiResult;");
                w.line("try");
                w.block("{", "}", |w| {
                    w.line(format!("ffiResult = {call};"));
                });
                w.line("catch");
                w.block("{", "}", |w| {
                    m.unregister(w);
                    w.line("throw;");
                });
            }
            w.line(err.check("ffiErr"));
            w.line(format!(
                "return new FfiSequence<{elem}>(new FfiIteratorHandle(ffiResult, &NativeMethods.{}), &{next});",
                it.destroy_symbol
            ));
        });
    });
    w.blank();

    // `_next` out-slots after the iterator handle, excluding `out_err`.
    let outs: Vec<&AbiParam> = it
        .next
        .params
        .iter()
        .skip(1)
        .filter(|s| s.name != "out_err")
        .collect();
    w.line(format!(
        "private static bool {next}(FfiIteratorHandle iter, out {elem} item)"
    ));
    w.block("{", "}", |w| {
        let mut args = vec!["iter".to_string()];
        for s in &outs {
            let CType::Ptr { pointee, .. } = &s.ty else {
                unreachable!("iterator out-slots are pointers")
            };
            let local = if s.name == "out_item" {
                "ffiItem"
            } else {
                "ffiLen"
            };
            w.line(format!("{} {local} = default;", cs_ctype(pointee)));
            args.push(format!("&{local}"));
        }
        args.push("&ffiErr".to_string());
        w.line("var ffiErr = default(FfiError);");
        w.line(format!(
            "var ffiMore = NativeMethods.{}({});",
            it.next.symbol,
            args.join(", ")
        ));
        w.line(err.check("ffiErr"));
        w.line("if (ffiMore == 0)");
        w.block("{", "}", |w| {
            w.line("item = default!;");
            w.line("return false;");
        });
        let expr = receive(
            w,
            cx,
            &it.elem,
            "ffiItem",
            "ffiLen",
            Own::Take,
            "ffiDecoded",
        );
        w.line(format!("item = {expr};"));
        w.line("return true;");
    });
    w.blank();
}

/// Render one module's static class. Submodules become sibling classes named
/// by their full path (`KvStats`), so a module class never shadows a type.
pub(crate) fn render_module_class(
    w: &mut CodeWriter,
    m: &ModuleBinding,
    strip_module_prefix: bool,
    cx: Cx<'_>,
) {
    if m.functions.is_empty() {
        return;
    }
    let class: String = m.segments.iter().map(|s| s.to_upper_camel_case()).collect();
    // The model falls back to the first function's doc when the module has
    // none; that would mislabel the class, so only a module's own doc is
    // used.
    if !m.functions.iter().any(|f| f.doc == m.doc) {
        write_doc(w, &m.doc);
    }
    w.line(format!("public static unsafe class {class}"));
    w.block("{", "}", |w| {
        for f in &m.functions {
            let name = wrapper_name(&m.path, &f.name, strip_module_prefix).to_upper_camel_case();
            let err = ErrCtx::for_fn(f, m.error.as_ref(), cx);
            render_callable(w, f, &name, Receiver::Static, &err);
        }
    });
    w.blank();
}
