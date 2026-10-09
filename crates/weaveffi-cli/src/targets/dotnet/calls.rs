//! Call-path renderers: the wrapper methods for every call shape (sync,
//! async, iterator), argument marshalling driven by [`ArgPass`], result
//! receiving driven by [`RetPass`], [`ResultPass`], and [`ItemPass`], and
//! the per-module static classes.
//!
//! Every wrapper follows one frame: encode arguments (pooled UTF-8, pooled
//! value buffers, callback registrations), pin them with `fixed`, call the
//! import with a stack `FfiError` and out slots, check the error, and
//! receive the result.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{
    AsyncBinding, FnBinding, IteratorBinding, Model, ModuleBinding, ParamBinding,
};
use weaveffi_model::plan::{ArgPass, ItemPass, ResultPass, RetPass};
use weaveffi_model::ty::Ty;

use crate::codegen::CodeWriter;
use crate::targets::dotnet::codec::{read_lambda, write_call};
use crate::targets::dotnet::docs::Docs;
use crate::targets::dotnet::errors::ErrCtx;
use crate::targets::dotnet::types::{
    camel, cs_ctype, item_cs, param_cs, prim_cs, result_cs, ret_cs, safe_cs_name, vtable_class_cs,
    Cx,
};

/// The `[UnmanagedCallersOnly]` attribute every native entry point carries.
pub(crate) const UNMANAGED_CALLERS_ONLY: &str =
    "[UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]";

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

/// The argument marshalling for one parameter list: statements run before
/// the call, the `fixed` pins it needs, one expression per ABI slot, and
/// the callback registrations to release if the call never reaches the
/// producer.
struct Marshal {
    setup: Vec<String>,
    pins: Vec<String>,
    args: Vec<String>,
    callbacks: Vec<String>,
}

impl Marshal {
    /// Plan the marshalling of `params`.
    fn new(cx: Cx<'_>, params: &[ParamBinding]) -> Self {
        let mut m = Marshal {
            setup: Vec::new(),
            pins: Vec::new(),
            args: Vec::new(),
            callbacks: Vec::new(),
        };
        let mut registrations = Vec::new();
        for p in params {
            let n = camel(&p.name);
            let local = p.name.to_lower_camel_case();
            match &p.pass {
                ArgPass::Direct { .. } => m.args.push(n),
                ArgPass::OptDirect { .. } => {
                    m.args.push(format!("{n}.HasValue"));
                    m.args.push(format!("{n}.GetValueOrDefault()"));
                }
                ArgPass::Slice { elem, .. } => {
                    m.pins.push(format!("{}* {local}Ptr = {n}", prim_cs(*elem)));
                    m.args.push(format!("{local}Ptr"));
                    m.args.push(format!("(nuint){n}.Length"));
                }
                ArgPass::String { .. } => {
                    m.setup
                        .push(format!("using var {local}Utf8 = new FfiUtf8({n});"));
                    m.pins.push(format!("byte* {local}Ptr = {local}Utf8"));
                    m.args.push(format!("{local}Ptr"));
                    m.args.push(format!("{local}Utf8.Length"));
                }
                ArgPass::Bytes { .. } => {
                    m.pins.push(format!("byte* {local}Ptr = {n}"));
                    m.args.push(format!("{local}Ptr"));
                    m.args.push(format!("(nuint){n}.Length"));
                }
                ArgPass::Buffer { .. } => {
                    let writer = format!("{local}Writer");
                    let ty = p.ty.value().expect("a buffered parameter has a value type");
                    m.setup
                        .push(format!("using var {writer} = new FfiBufferWriter();"));
                    m.setup.push(format!("{};", write_call(ty, &writer, &n, 0)));
                    m.pins.push(format!("byte* {local}Ptr = {writer}.Written"));
                    m.args.push(format!("{local}Ptr"));
                    m.args.push(format!("(nuint){writer}.Length"));
                }
                ArgPass::Object {
                    nullable,
                    interface,
                    ..
                } => {
                    if *nullable {
                        m.args.push(format!(
                            "{n}?.Handle ?? {}.NativeHandle.Null",
                            cx.ty(interface)
                        ));
                    } else {
                        m.args.push(format!("{n}.Handle"));
                    }
                }
                // The producer owns the registration once the call reaches
                // it: its vtable `free(ctx)` releases it. An absent optional
                // implementation passes a null vtable.
                ArgPass::Callback {
                    nullable,
                    interface,
                    ..
                } => {
                    let ctx = format!("{local}Ctx");
                    let pointer = format!("{}.Pointer", vtable_class_cs(interface));
                    m.args.push(ctx.clone());
                    if *nullable {
                        m.args
                            .push(format!("{n} == null ? IntPtr.Zero : {pointer}"));
                    } else {
                        m.args.push(pointer);
                    }
                    registrations.push(format!("var {ctx} = Ffi.Register({n});"));
                    m.callbacks.push(ctx);
                }
            }
        }
        // Register last, so a failing encode above can't strand a
        // registration.
        m.setup.extend(registrations);
        m
    }

    /// Emit the setup statements.
    fn write_setup(&self, w: &mut CodeWriter) {
        for line in &self.setup {
            w.line(line);
        }
    }

    /// Emit `body` inside one stacked `fixed` statement per pin, or bare
    /// when nothing needs pinning.
    fn pinned(&self, w: &mut CodeWriter, body: impl FnOnce(&mut CodeWriter)) {
        if self.pins.is_empty() {
            body(w);
            return;
        }
        for pin in &self.pins {
            w.line(format!("fixed ({pin})"));
        }
        w.line("{");
        w.scope(body);
        w.line("}");
    }

    /// Emit `call` (a statement), releasing every callback registration if
    /// it throws before reaching the producer. `declare` is the declaration
    /// of the variable the statement assigns, when it assigns one.
    fn guarded(&self, w: &mut CodeWriter, declare: Option<String>, call: &str) {
        if self.callbacks.is_empty() {
            match declare {
                Some(_) => w.line(format!("var {call}")),
                None => w.line(call),
            };
            return;
        }
        if let Some(decl) = declare {
            w.line(decl);
        }
        w.line("try");
        w.block("{", "}", |w| {
            w.line(call);
        });
        w.line("catch");
        w.block("{", "}", |w| {
            self.unregister(w);
            w.line("throw;");
        });
    }

    /// The statements releasing every callback registration.
    fn unregister(&self, w: &mut CodeWriter) {
        for ctx in &self.callbacks {
            w.line(format!("Ffi.Unregister({ctx});"));
        }
    }
}

/// The public signature's parameter list.
fn params_sig(params: &[ParamBinding]) -> Vec<String> {
    params
        .iter()
        .map(|p| format!("{} {}", param_cs(&p.pass, p.ty.value()), camel(&p.name)))
        .collect()
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

/// The modifiers, return type, and name opening a method (or the bare class
/// name of a constructor).
fn opener(receiver: Receiver<'_>, ret: &str, name: &str) -> String {
    match receiver {
        Receiver::Static => format!("public static {ret} {name}"),
        Receiver::Instance => format!("public {ret} {name}"),
        Receiver::Constructor(class) => format!("public {class}"),
    }
}

/// The element type of a pointer slot.
fn pointee(slot: &AbiParam) -> &CType {
    match &slot.ty {
        CType::Ptr { pointee, .. } => pointee,
        other => other,
    }
}

/// The expression adopting an object pointer into a wrapper (`null` for a
/// null pointer of a nullable object).
fn adopt(cx: Cx<'_>, ptr: &str, nullable: bool, interface: &str) -> String {
    let class = cx.ty(interface);
    if nullable {
        format!("{ptr} == IntPtr.Zero ? null : {class}.Adopt({ptr})")
    } else {
        format!("{class}.Adopt({ptr})")
    }
}

/// Render one wrapper (any shape) named `name`.
pub(crate) fn render_callable(
    w: &mut CodeWriter,
    docs: &Docs,
    f: &FnBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx,
    cx: Cx<'_>,
) {
    if let Some(a) = f.async_binding() {
        render_async(w, docs, f, a, name, receiver, err, cx);
    } else if let Some(it) = f.iterator() {
        render_iterator(w, docs, f, it, name, receiver, err, cx);
    } else {
        render_sync(w, docs, f, name, receiver, err, cx);
    }
}

/// The documentation, `<exception>`, and `[Obsolete]` lines of a wrapper.
fn write_header(w: &mut CodeWriter, docs: &Docs, f: &FnBinding, err: &ErrCtx) {
    docs.summary(w, &f.doc);
    for p in &f.params {
        docs.param(w, &camel(&p.name), &p.doc);
    }
    err.write_doc(w);
    docs.obsolete(w, &f.deprecated);
}

/// The out-slot locals of a sync call and the `&local` arguments passing
/// them, per its [`RetPass`].
fn ret_outs(cx: Cx<'_>, pass: &RetPass) -> (Vec<String>, Vec<String>) {
    match pass {
        RetPass::OptDirect { out_value } => (
            vec![format!(
                "{} ffiValue = default;",
                cs_ctype(cx.ns, pointee(out_value))
            )],
            vec!["&ffiValue".into()],
        ),
        RetPass::Slice { .. }
        | RetPass::String { .. }
        | RetPass::Bytes { .. }
        | RetPass::Buffer { .. } => (
            vec!["nuint ffiOutLen = 0;".into()],
            vec!["&ffiOutLen".into()],
        ),
        _ => (Vec::new(), Vec::new()),
    }
}

/// The expression receiving a sync call's result `ffiResult`.
fn receive_ret(cx: Cx<'_>, pass: &RetPass, ty: &Ty) -> String {
    match pass {
        RetPass::Void | RetPass::Iterator(_) => unreachable!("no value to receive"),
        RetPass::Direct => "ffiResult".into(),
        RetPass::OptDirect { .. } => "ffiResult ? ffiValue : null".into(),
        RetPass::Slice { .. } => "Ffi.TakeArray(ffiResult, ffiOutLen)".into(),
        RetPass::String { .. } => "Ffi.TakeString(ffiResult, ffiOutLen)".into(),
        RetPass::Bytes { .. } => "Ffi.TakeBytes(ffiResult, ffiOutLen)".into(),
        RetPass::Buffer { .. } => format!(
            "Ffi.TakeBuffer(ffiResult, ffiOutLen, {})",
            read_lambda(cx, ty, 0)
        ),
        RetPass::Object {
            nullable,
            interface,
            ..
        } => adopt(cx, "ffiResult", *nullable, interface),
    }
}

/// A synchronous wrapper: one call, one check, one received result. A
/// constructor stores the adopted pointer as its handle instead of
/// returning it.
fn render_sync(
    w: &mut CodeWriter,
    docs: &Docs,
    f: &FnBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx,
    cx: Cx<'_>,
) {
    let value = f.ret.as_ref().and_then(|r| r.value());
    let ret = match (receiver, value) {
        (Receiver::Constructor(_), _) => String::new(),
        (_, Some(ty)) => ret_cs(&f.ret_pass, ty),
        (_, None) => "void".into(),
    };
    let m = Marshal::new(cx, &f.params);
    let (locals, mut extra) = ret_outs(cx, &f.ret_pass);
    extra.push("&ffiErr".to_string());
    let call = format!(
        "NativeMethods.{}({})",
        f.abi.symbol,
        call_args(receiver, &m, &extra)
    );

    write_header(w, docs, f, err);
    w.line(format!(
        "{}({})",
        opener(receiver, &ret, name),
        params_sig(&f.params).join(", ")
    ));
    w.block("{", "}", |w| {
        m.write_setup(w);
        w.line("var ffiErr = default(FfiError);");
        for l in &locals {
            w.line(l);
        }
        m.pinned(w, |w| {
            if value.is_some() {
                let decl = format!("{} ffiResult;", cs_ctype(cx.ns, &f.abi.ret));
                m.guarded(w, Some(decl), &format!("ffiResult = {call};"));
            } else {
                m.guarded(w, None, &format!("{call};"));
            }
            w.line(err.check("ffiErr"));
            match (value, receiver) {
                (Some(_), Receiver::Constructor(_)) => {
                    w.line("Handle = new NativeHandle(ffiResult);");
                }
                (Some(ty), _) => {
                    w.line(format!("return {};", receive_ret(cx, &f.ret_pass, ty)));
                }
                (None, _) => {}
            }
        });
    });
    w.blank();
}

/// The expression receiving an async result from the completion's slots.
fn receive_result(cx: Cx<'_>, pass: &ResultPass, ty: Option<&Ty>) -> String {
    let n = |slot: &AbiParam| safe_cs_name(&slot.name);
    match pass {
        ResultPass::Void => "default".into(),
        ResultPass::Direct { result } => n(result),
        ResultPass::OptDirect { has, value } => format!("{} ? {} : null", n(has), n(value)),
        ResultPass::Slice { ptr, len, .. } => format!("Ffi.TakeArray({}, {})", n(ptr), n(len)),
        ResultPass::String { ptr, len } => format!("Ffi.TakeString({}, {})", n(ptr), n(len)),
        ResultPass::Bytes { ptr, len } => format!("Ffi.TakeBytes({}, {})", n(ptr), n(len)),
        ResultPass::Buffer { ptr, len } => format!(
            "Ffi.TakeBuffer({}, {}, {})",
            n(ptr),
            n(len),
            read_lambda(cx, ty.expect("a buffered result has a type"), 0)
        ),
        ResultPass::Object {
            result,
            nullable,
            interface,
            ..
        } => adopt(cx, &n(result), *nullable, interface),
    }
}

/// An async wrapper: a `Task`-returning method launching the call with a
/// pending `FfiCall` as `context`, plus the `[UnmanagedCallersOnly]`
/// completion that resolves it. Every async method takes a
/// `CancellationToken`: a cancellable function links it to the native
/// cancel token; any other stops waiting when it fires.
#[allow(clippy::too_many_arguments)]
fn render_async(
    w: &mut CodeWriter,
    docs: &Docs,
    f: &FnBinding,
    a: &AsyncBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx,
    cx: Cx<'_>,
) {
    let value = f.ret.as_ref().and_then(|r| r.value());
    let result = value.map(|ty| result_cs(&a.result, ty));
    let task = match &result {
        Some(t) => format!("Task<{t}>"),
        None => "Task".into(),
    };
    let call_ty = result.clone().unwrap_or_else(|| "FfiVoid".into());
    let complete = format!("Complete{name}");
    let m = Marshal::new(cx, &f.params);
    let mut extra = Vec::new();
    if a.cancellable() {
        extra.push("ffiCall.CancelToken()".to_string());
    }
    extra.push(format!("&{complete}"));
    extra.push("ffiCall.Context".to_string());
    let call = format!(
        "NativeMethods.{}({});",
        f.abi.symbol,
        call_args(receiver, &m, &extra)
    );

    write_header(w, docs, f, err);
    if a.cancellable() {
        w.line("/// <param name=\"cancellationToken\">Cancels the native call; the task then");
        w.line("/// completes as canceled.</param>");
    } else {
        w.line("/// <param name=\"cancellationToken\">Stops waiting: the task completes as");
        w.line("/// canceled at once, and the native call, which can't be cancelled, finishes");
        w.line("/// in the background and releases its result.</param>");
    }
    let mut sig = params_sig(&f.params);
    sig.push("CancellationToken cancellationToken = default".into());
    let opener = match receiver {
        Receiver::Instance => format!("public {task} {name}"),
        _ => format!("public static {task} {name}"),
    };
    w.line(format!("{opener}({})", sig.join(", ")));
    w.block("{", "}", |w| {
        w.line("if (cancellationToken.IsCancellationRequested)");
        w.block("{", "}", |w| {
            let from = match &result {
                Some(t) => format!("Task.FromCanceled<{t}>(cancellationToken)"),
                None => "Task.FromCanceled(cancellationToken)".into(),
            };
            w.line(format!("return {from};"));
        });
        m.write_setup(w);
        w.line(format!(
            "var ffiCall = new FfiCall<{call_ty}>(cancellationToken);"
        ));
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
        w.line("return ffiCall.Launched();");
    });
    w.blank();

    // The completion: fires exactly once, on a producer thread.
    let slots: Vec<String> = a
        .callback_params
        .iter()
        .map(|s| format!("{} {}", cs_ctype(cx.ns, &s.ty), safe_cs_name(&s.name)))
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
            w.line(format!(
                "ffiCall.SetResult({});",
                receive_result(cx, &a.result, value)
            ));
        });
        w.line("catch (Exception e)");
        w.block("{", "}", |w| {
            w.line("ffiCall.SetException(e);");
        });
    });
    w.blank();
}

/// The locals `_next` writes an item into, the `&local` arguments passing
/// them (in slot order), and the expression receiving the item.
fn item_outs(cx: Cx<'_>, pass: &ItemPass, elem: &Ty) -> (Vec<String>, Vec<String>, String) {
    let local = |slot: &AbiParam, name: &str| {
        format!("{} {name} = default;", cs_ctype(cx.ns, pointee(slot)))
    };
    match pass {
        ItemPass::Direct { out_item } => (
            vec![local(out_item, "ffiItem")],
            vec!["&ffiItem".into()],
            "ffiItem".into(),
        ),
        ItemPass::OptDirect { out_has, out_item } => (
            vec![local(out_has, "ffiHas"), local(out_item, "ffiItem")],
            vec!["&ffiHas".into(), "&ffiItem".into()],
            "ffiHas ? ffiItem : null".into(),
        ),
        ItemPass::Slice {
            out_item, out_len, ..
        } => (
            vec![local(out_item, "ffiItem"), local(out_len, "ffiLen")],
            vec!["&ffiItem".into(), "&ffiLen".into()],
            "Ffi.TakeArray(ffiItem, ffiLen)".into(),
        ),
        ItemPass::String { out_item, out_len } => (
            vec![local(out_item, "ffiItem"), local(out_len, "ffiLen")],
            vec!["&ffiItem".into(), "&ffiLen".into()],
            "Ffi.TakeString(ffiItem, ffiLen)".into(),
        ),
        ItemPass::Bytes { out_item, out_len } => (
            vec![local(out_item, "ffiItem"), local(out_len, "ffiLen")],
            vec!["&ffiItem".into(), "&ffiLen".into()],
            "Ffi.TakeBytes(ffiItem, ffiLen)".into(),
        ),
        ItemPass::Buffer { out_item, out_len } => (
            vec![local(out_item, "ffiItem"), local(out_len, "ffiLen")],
            vec!["&ffiItem".into(), "&ffiLen".into()],
            format!(
                "Ffi.TakeBuffer(ffiItem, ffiLen, {})",
                read_lambda(cx, elem, 0)
            ),
        ),
        ItemPass::Object {
            out_item,
            nullable,
            interface,
            ..
        } => (
            vec![local(out_item, "ffiItem")],
            vec!["&ffiItem".into()],
            adopt(cx, "ffiItem", *nullable, interface),
        ),
    }
}

/// An iterator wrapper: a re-enumerable `IEnumerable<T>` whose every
/// enumeration launches a new native iterator through a `Launch{Name}`
/// helper and pulls one item per `MoveNext` through a static `Next{Name}`.
/// Span parameters are copied once, since the sequence outlives the call.
#[allow(clippy::too_many_arguments)]
fn render_iterator(
    w: &mut CodeWriter,
    docs: &Docs,
    f: &FnBinding,
    it: &IteratorBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx,
    cx: Cx<'_>,
) {
    let elem = item_cs(&it.item, &it.elem);
    let launch = format!("Launch{name}");
    let next = format!("Next{name}");
    let m = Marshal::new(cx, &f.params);

    write_header(w, docs, f, err);
    w.line("/// <remarks>Streams lazily, one native call per item. Each enumeration launches a");
    w.line("/// new native iterator, so a failure to start throws from");
    w.line("/// <c>GetEnumerator</c> (the start of a <c>foreach</c>); disposing the enumerator");
    w.line("/// (as <c>foreach</c> does) releases it.</remarks>");
    let ret = format!("IEnumerable<{elem}>");
    w.line(format!(
        "{}({})",
        opener(receiver, &ret, name),
        params_sig(&f.params).join(", ")
    ));
    w.block("{", "}", |w| {
        let mut args = Vec::new();
        for p in &f.params {
            let n = camel(&p.name);
            if matches!(p.pass, ArgPass::Slice { .. } | ArgPass::Bytes { .. }) {
                let copy = format!("{}Copy", p.name.to_lower_camel_case());
                w.line(format!("var {copy} = {n}.ToArray();"));
                args.push(copy);
            } else {
                args.push(n);
            }
        }
        w.line(format!(
            "return new FfiSequence<{elem}>(() => {launch}({}), &{next});",
            args.join(", ")
        ));
    });
    w.blank();

    // The launcher: a sync call returning the native iterator.
    let call = format!(
        "NativeMethods.{}({})",
        f.abi.symbol,
        call_args(receiver, &m, &["&ffiErr".to_string()])
    );
    let modifier = match receiver {
        Receiver::Instance => "private",
        _ => "private static",
    };
    w.line(format!(
        "{modifier} FfiIteratorHandle {launch}({})",
        params_sig(&f.params).join(", ")
    ));
    w.block("{", "}", |w| {
        m.write_setup(w);
        w.line("var ffiErr = default(FfiError);");
        m.pinned(w, |w| {
            m.guarded(
                w,
                Some("IntPtr ffiResult;".into()),
                &format!("ffiResult = {call};"),
            );
            w.line(err.check("ffiErr"));
            w.line(format!(
                "return new FfiIteratorHandle(ffiResult, &NativeMethods.{});",
                it.destroy_symbol
            ));
        });
    });
    w.blank();

    // `_next`: one item per call; 0 means exhausted.
    let (locals, mut args, item) = item_outs(cx, &it.item, &it.elem);
    args.insert(0, "iter".into());
    args.push("&ffiErr".into());
    w.line(format!(
        "private static bool {next}(FfiIteratorHandle iter, out {elem} item)"
    ));
    w.block("{", "}", |w| {
        for l in &locals {
            w.line(l);
        }
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
        w.line(format!("item = {item};"));
        w.line("return true;");
    });
    w.blank();
}

/// The C# class of a module's free functions: its full path in PascalCase
/// (`KvStats` for `kv.stats`), so a module class never shadows a type.
pub(crate) fn module_class(m: &ModuleBinding) -> String {
    m.segments.iter().map(|s| s.to_upper_camel_case()).collect()
}

/// Render one module's static class.
pub(crate) fn render_module_class(
    w: &mut CodeWriter,
    model: &Model,
    docs: &Docs,
    m: &ModuleBinding,
    cx: Cx<'_>,
) {
    if m.functions.is_empty() {
        return;
    }
    docs.summary(w, &m.doc);
    w.line(format!("public static unsafe class {}", module_class(m)));
    w.block("{", "}", |w| {
        for f in &m.functions {
            let name = f.name.to_upper_camel_case();
            let err = ErrCtx::new(model, &f.error, cx);
            render_callable(w, docs, f, &name, Receiver::Static, &err, cx);
        }
    });
    w.blank();
}
