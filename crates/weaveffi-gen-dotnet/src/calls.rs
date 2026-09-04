//! Call-path renderers: the wrapper methods for every call shape (sync,
//! async, iterator), parameter marshalling driven by the shared
//! [`ArgPass`] plan, return conversions driven by [`Family`], and the
//! per-module static wrapper classes.

use heck::ToUpperCamelCase;
use weaveffi_core::abi::CType;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    CallShape, ErrorBinding, Family, FnBinding, IteratorBinding, ModuleBinding, ParamBinding, Ty,
};
use weaveffi_core::plan::{ArgPass, ErrorStrategy};
use weaveffi_core::utils::{local_type_name, wrapper_name};

use crate::codec::{emit_buffer_decode, emit_buffer_write};
use crate::docs::writer_fn_doc;
use crate::pinvoke::is_error_slot;
use crate::runtime::{check_method_name, dotnet_exception_name};
use crate::types::{camel_fn, cs_type, safe_cs_name, vtable_class_cs};

/// How a wrapper surfaces a non-zero error slot, rendering
/// [`ErrorStrategy`]: [`ErrorStrategy::Throws`] with a domain in scope raises
/// the typed domain exception; everything else (a producer trap, or a
/// throwing function without a declared domain) raises the plain
/// `WeaveFFIException`, which no domain exception check can catch by type.
#[derive(Clone, Copy)]
pub(crate) enum ErrCtx<'a> {
    /// Throw the generic `WeaveFFIException`.
    Generic,
    /// Throw the domain's typed exception via its `FromCode` factory.
    Domain(&'a ErrorBinding),
}

impl<'a> ErrCtx<'a> {
    /// The error context for one function: typed when the function's
    /// [`ErrorStrategy`] is `Throws` and its module has an error domain in
    /// scope, generic otherwise (including every `Trap` function, whose only
    /// failures are producer bugs and must not wear the domain type).
    pub(crate) fn for_fn(f: &FnBinding, error: Option<&'a ErrorBinding>) -> Self {
        match (f.error_strategy(), error) {
            (ErrorStrategy::Throws, Some(eb)) => ErrCtx::Domain(eb),
            _ => ErrCtx::Generic,
        }
    }

    /// The check statement placed after a native call writing into `err`.
    pub(crate) fn check_stmt(&self) -> String {
        self.check_stmt_for("err")
    }

    /// The check statement for a named `WeaveFFIError` local.
    pub(crate) fn check_stmt_for(&self, var: &str) -> String {
        match self {
            ErrCtx::Generic => format!("WeaveFFIError.Check({var});"),
            ErrCtx::Domain(eb) => format!("WeaveFFIError.{}({var});", check_method_name(eb)),
        }
    }

    /// The exception expression an async completion callback faults its
    /// `TaskCompletionSource` with. A domain exception decodes the error's
    /// structured payload, copied out of the heap-boxed error before the
    /// callback releases it with `weaveffi_error_free`.
    pub(crate) fn async_exception_expr(&self) -> String {
        match self {
            ErrCtx::Generic => "new WeaveFFIException(wErr.Code, msg)".into(),
            ErrCtx::Domain(eb) => {
                format!(
                    "{}.FromCode(wErr.Code, msg, payload)",
                    dotnet_exception_name(eb)
                )
            }
        }
    }

    /// Emit the `<exception>` XML doc line for a throwing wrapper; generic
    /// wrappers document nothing extra.
    pub(crate) fn write_exception_doc(&self, w: &mut CodeWriter) {
        if let ErrCtx::Domain(eb) = self {
            w.line(format!(
                "/// <exception cref=\"{}\">Thrown when the call reports a {} code.</exception>",
                dotnet_exception_name(eb),
                eb.type_name
            ));
        }
    }
}

/// The `public` wrapper signature parameter list for `params`.
pub(crate) fn params_sig(params: &[ParamBinding]) -> String {
    params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `[Obsolete]` attribute line for a deprecated callable.
pub(crate) fn write_obsolete(w: &mut CodeWriter, deprecated: &Option<String>) {
    if let Some(msg) = deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }
}

/// True when a parameter allocates something the wrapper must release after
/// the native call, per its [`ArgPass`] plan: strings (`CoTaskMem` UTF-8
/// copies), bytes (pinned arrays), and every buffered value (encoded into a
/// pinned value buffer). Direct and object arguments pass inline, and a
/// callback's `GCHandle` is released by the producer's `free(ctx)` instead.
pub(crate) fn param_needs_cleanup(p: &ParamBinding) -> bool {
    matches!(
        p.arg_pass(),
        ArgPass::String { .. } | ArgPass::Bytes { .. } | ArgPass::Buffer { .. }
    )
}

/// Emit the setup statements for one parameter before the native call,
/// dispatching on its [`ArgPass`] plan. Strings copy to `CoTaskMem` UTF-8;
/// bytes pin the managed array; buffered parameters encode into a `byte[]`
/// value buffer (`{name}Buf`) and pin it (`{name}Pin`), which the caller
/// owns for the duration of the call; a callback interface implementation is
/// registered in the `GCHandle` table and its handle (`{name}Ctx`) becomes
/// the `ctx` the producer hands back to every trampoline.
pub(crate) fn render_marshal_setup(w: &mut CodeWriter, p: &ParamBinding) {
    let name = safe_cs_name(&p.name);
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            w.line(format!("var {name}Writer = new WeaveFFIBufferWriter();"));
            emit_buffer_write(w, &p.ty, &name, &format!("{name}Writer"), 0);
            w.line(format!("var {name}Buf = {name}Writer.ToArray();"));
            w.line(format!(
                "var {name}Pin = GCHandle.Alloc({name}Buf, GCHandleType.Pinned);"
            ));
        }
        ArgPass::String { .. } => {
            w.line(format!(
                "var {name}Ptr = Marshal.StringToCoTaskMemUTF8({name});"
            ));
        }
        ArgPass::Bytes { .. } => {
            w.line(format!(
                "var {name}Pin = GCHandle.Alloc({name}, GCHandleType.Pinned);"
            ));
        }
        // The handle is owned by the producer from here on: its vtable
        // `free(ctx)` trampoline releases it when the last clone drops.
        ArgPass::Callback { .. } => {
            w.line(format!(
                "var {name}Ctx = GCHandle.ToIntPtr(GCHandle.Alloc({name}));"
            ));
        }
        ArgPass::Direct { .. } | ArgPass::Object { .. } => {}
    }
}

/// Emit the cleanup statements releasing what [`render_marshal_setup`]
/// allocated: the `CoTaskMem` string copy or the pinned array handle.
pub(crate) fn render_marshal_cleanup(w: &mut CodeWriter, p: &ParamBinding) {
    let name = safe_cs_name(&p.name);
    match p.arg_pass() {
        ArgPass::Buffer { .. } | ArgPass::Bytes { .. } => {
            w.line(format!("{name}Pin.Free();"));
        }
        ArgPass::String { .. } => {
            w.line(format!("Marshal.FreeCoTaskMem({name}Ptr);"));
        }
        ArgPass::Direct { .. } | ArgPass::Object { .. } | ArgPass::Callback { .. } => {}
    }
}

/// Emit every parameter's setup, then `body` inside a `try`/`finally` that
/// runs the cleanups when any parameter needs one, or `body` bare otherwise.
pub(crate) fn render_marshalled_call(
    w: &mut CodeWriter,
    params: &[ParamBinding],
    body: impl FnOnce(&mut CodeWriter),
) {
    for p in params {
        render_marshal_setup(w, p);
    }
    if params.iter().any(param_needs_cleanup) {
        w.line("try");
        w.line("{");
        w.scope(body);
        w.line("}");
        w.line("finally");
        w.line("{");
        w.scope(|w| {
            for p in params {
                render_marshal_cleanup(w, p);
            }
        });
        w.line("}");
    } else {
        body(w);
    }
}

/// The C# class hosting the static vtable a callback slot points at, named
/// from the slot's `{prefix}_{module}_{Name}_vtable` C type.
fn vtable_class_for_slot(vtable: &CType) -> String {
    let CType::Ptr { pointee, .. } = vtable else {
        unreachable!("a callback vtable slot is a pointer")
    };
    let CType::VtableTag { module, name } = pointee.as_ref() else {
        unreachable!("a callback vtable slot points at a vtable tag")
    };
    vtable_class_cs(module, name)
}

/// The joined native-call argument expressions for `params`, one expression
/// per ABI slot, dispatching on each parameter's [`ArgPass`] plan.
pub(crate) fn build_call_args(params: &[ParamBinding]) -> String {
    params
        .iter()
        .flat_map(|p| {
            let name = safe_cs_name(&p.name);
            match p.arg_pass() {
                // A buffered parameter passes its pinned value buffer as the
                // borrowed (ptr, len) pair; the caller owns and frees the pin.
                ArgPass::Buffer { .. } => vec![
                    format!("{name}Pin.AddrOfPinnedObject()"),
                    format!("(UIntPtr){name}Buf.Length"),
                ],
                ArgPass::String { .. } => vec![format!("{name}Ptr")],
                ArgPass::Bytes { .. } => vec![
                    format!("{name}Pin.AddrOfPinnedObject()"),
                    format!("(UIntPtr){name}.Length"),
                ],
                // Interface parameters borrow: pass the handle, the wrapper
                // keeps its own reference. A nullable object passes null as
                // IntPtr.Zero.
                ArgPass::Object { nullable: true, .. } => {
                    vec![format!("{name}?.Handle ?? IntPtr.Zero")]
                }
                ArgPass::Object {
                    nullable: false, ..
                } => {
                    vec![format!("{name}.Handle")]
                }
                // The GCHandle key plus the one process-wide vtable.
                ArgPass::Callback { vtable, .. } => vec![
                    format!("{name}Ctx"),
                    format!("{}.Pointer", vtable_class_for_slot(&vtable.ty)),
                ],
                // Direct slots pass by value; only the C# spelling of the
                // value expression depends on the surface type.
                ArgPass::Direct { .. } => vec![direct_to_slot(&p.ty, &name)],
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The C# expression converting a surface value `expr` of direct-family type
/// `ty` into its by-value ABI slot: `bool` as a byte, a typed enum as its
/// `int`, every other scalar as is.
pub(crate) fn direct_to_slot(ty: &Ty, expr: &str) -> String {
    match ty {
        Ty::Bool => format!("(byte)({expr} ? 1 : 0)"),
        Ty::Enum(_) => format!("(int){expr}"),
        _ => expr.to_string(),
    }
}

/// The joined native-call argument list: the implicit self handle (when
/// `self_expr` is given) followed by every lowered parameter slot.
pub(crate) fn full_call_args(f: &FnBinding, self_expr: Option<&str>) -> String {
    let args = build_call_args(&f.params);
    match self_expr {
        Some(s) if args.is_empty() => s.to_string(),
        Some(s) => format!("{s}, {args}"),
        None => args,
    }
}

/// `args` followed by a separating comma, or empty when there are none, for
/// splicing ahead of the trailing out-params and error slot.
fn args_part(args: &str) -> String {
    if args.is_empty() {
        String::new()
    } else {
        format!("{args}, ")
    }
}

/// True when a value crosses back as an owned `ptr` + `len` pair delivered
/// through a trailing `out_len` slot: bytes and every buffered type (records,
/// rich enums, lists, maps, and non-interface optionals).
pub(crate) fn is_ptr_len_result(ty: &Ty) -> bool {
    matches!(ty.family(), Family::Bytes | Family::Buffer)
}

/// Emit the native call, the error check, and the return conversion for one
/// synchronous callable.
pub(crate) fn render_pinvoke_call_and_return(
    w: &mut CodeWriter,
    f: &FnBinding,
    self_expr: Option<&str>,
    err: ErrCtx,
) {
    let c_sym = &f.c_base;
    let args = args_part(&full_call_args(f, self_expr));

    if let Some(ret) = &f.ret {
        let out_len_part = if is_ptr_len_result(ret) {
            "out var outLen, "
        } else {
            ""
        };
        w.line(format!(
            "var result = NativeMethods.{c_sym}({args}{out_len_part}ref err);"
        ));
    } else {
        w.line(format!("NativeMethods.{c_sym}({args}ref err);"));
    }
    w.line(err.check_stmt());

    if let Some(ret_ty) = &f.ret {
        render_return_conversion(w, ret_ty);
    }
}

/// Emit the statements converting the raw `result` slot (plus `outLen` for
/// bytes and buffered returns) into the returned C# value, releasing
/// producer-owned memory along the way. Dispatches on the return type's
/// [`Family`], which is exactly the classification behind the shared
/// `RetPass` receiving plan: copy and free a string or bytes, decode and free
/// a buffer, adopt an object.
pub(crate) fn render_return_conversion(w: &mut CodeWriter, ty: &Ty) {
    match ty.family() {
        // A buffered return is a producer-allocated value buffer: copy the
        // bytes, release them with `weaveffi_free_bytes`, then decode the
        // copy (adopting any object tokens it carries).
        Family::Buffer => {
            w.line("var resultBuf = new byte[(int)outLen];");
            w.line(
                "if (result != IntPtr.Zero && (int)outLen > 0) Marshal.Copy(result, resultBuf, 0, (int)outLen);",
            );
            w.line("NativeMethods.weaveffi_free_bytes(result, outLen);");
            // Named so it cannot shadow a user parameter (`value` is a common one).
            emit_buffer_decode(w, ty, "decoded", "resultBuf");
            w.line("return decoded;");
        }
        Family::String => {
            w.line("var str = Marshal.PtrToStringUTF8(result);");
            w.line("NativeMethods.weaveffi_free_string(result);");
            w.line("return str ?? \"\";");
        }
        Family::Bytes => {
            w.line("if (result == IntPtr.Zero) return Array.Empty<byte>();");
            w.line("var arr = new byte[(int)outLen];");
            w.line("Marshal.Copy(result, arr, 0, (int)outLen);");
            w.line("NativeMethods.weaveffi_free_bytes(result, outLen);");
            w.line("return arr;");
        }
        // One strong reference transfers: adopt it into a wrapper whose
        // Dispose (or finalizer) releases it exactly once.
        Family::Object { nullable } => {
            let adopt = adopt_object(object_class(ty), "result");
            if nullable {
                w.line(format!("return result == IntPtr.Zero ? null : {adopt};"));
            } else {
                w.line(format!("return {adopt};"));
            }
        }
        Family::Direct => {
            w.line(format!("return {};", direct_from_slot(ty, "result")));
        }
        Family::Callback => unreachable!("callback interfaces are never returned"),
        Family::Iterator => unreachable!("iterator functions render via CallShape::Iterator"),
    }
}

/// The C# expression adopting the strong reference in `ptr_expr` into a new
/// wrapper of class `cn`, through the `WeaveFFIHandle` marker the adopting
/// constructor takes.
pub(crate) fn adopt_object(cn: &str, ptr_expr: &str) -> String {
    format!("new {cn}(new WeaveFFIHandle({ptr_expr}))")
}

/// The wrapper class name for an object type (`Store` or `Store?`).
pub(crate) fn object_class(ty: &Ty) -> &str {
    local_type_name(
        ty.interface_name()
            .expect("object family types name an interface"),
    )
}

/// The C# expression converting a by-value slot named `slot` (holding a
/// direct-family value) into its surface type: `bool` from its byte, a typed
/// enum from its `int`, every other scalar as is.
pub(crate) fn direct_from_slot(ty: &Ty, slot: &str) -> String {
    match ty {
        Ty::Bool => format!("{slot} != 0"),
        Ty::Enum(name) => format!("({}){slot}", local_type_name(name)),
        _ => slot.to_string(),
    }
}

/// Render one wrapper method (any shape) named `method_name`. `self_expr` is
/// the receiver's handle expression for interface instance methods (`None`
/// for free functions, statics, and factories, which render as `static`);
/// `err` selects the typed or generic error surface.
pub(crate) fn render_wrapper_method(
    out: &mut String,
    f: &FnBinding,
    method_name: &str,
    self_expr: Option<&str>,
    err: ErrCtx,
) {
    if f.is_async {
        render_async_wrapper_method(out, f, method_name, self_expr, err);
        return;
    }
    if let CallShape::Iterator(it) = &f.shape {
        render_iterator_wrapper_method(out, f, it, method_name, self_expr, err);
        return;
    }
    let f = camel_fn(f);
    let ret_cs = f.ret.as_ref().map(cs_type).unwrap_or_else(|| "void".into());
    let staticness = if self_expr.is_none() { "static " } else { "" };

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    write_obsolete(&mut w, &f.deprecated);

    w.line(format!(
        "public {staticness}{ret_cs} {method_name}({})",
        params_sig(&f.params)
    ));
    w.line("{");
    w.scope(|w| {
        w.line("var err = new WeaveFFIError();");
        render_marshalled_call(w, &f.params, |w| {
            render_pinvoke_call_and_return(w, &f, self_expr, err);
        });
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The statements converting one `_next` out-item into the yielded C# value,
/// receiving it per the element's [`Family`] exactly as a return of the same
/// type (the `IteratorProtocol.elem` plan): copy and free a string
/// (`weaveffi_free_string`) or bytes (`weaveffi_free_bytes`), decode and free
/// a buffer, adopt an object into a wrapper the consumer owns. Returns the
/// expression to `yield return`.
pub(crate) fn iterator_item_conversion(w: &mut CodeWriter, elem: &Ty) -> String {
    match elem.family() {
        Family::Buffer => {
            w.line("var itemBuf = new byte[(int)out_len];");
            w.line("if (out_item != IntPtr.Zero && (int)out_len > 0) Marshal.Copy(out_item, itemBuf, 0, (int)out_len);");
            w.line("NativeMethods.weaveffi_free_bytes(out_item, out_len);");
            emit_buffer_decode(w, elem, "item", "itemBuf");
            "item".into()
        }
        Family::String => {
            w.line("var item = Marshal.PtrToStringUTF8(out_item) ?? \"\";");
            w.line("NativeMethods.weaveffi_free_string(out_item);");
            "item".into()
        }
        Family::Bytes => {
            w.line("var item = new byte[(int)out_len];");
            w.line("if (out_item != IntPtr.Zero && (int)out_len > 0) Marshal.Copy(out_item, item, 0, (int)out_len);");
            w.line("NativeMethods.weaveffi_free_bytes(out_item, out_len);");
            "item".into()
        }
        Family::Object { nullable } => {
            let adopt = adopt_object(object_class(elem), "out_item");
            if nullable {
                format!("out_item == IntPtr.Zero ? null : {adopt}")
            } else {
                adopt
            }
        }
        Family::Direct => direct_from_slot(elem, "out_item"),
        Family::Callback | Family::Iterator => {
            unreachable!("{elem} is not a valid iterator element")
        }
    }
}

/// An `iter<T>` function surfaces as `IEnumerable<T>`, rendering the
/// `IteratorProtocol` pull contract: an eager launcher call (so launch errors
/// throw immediately, per the function's `ErrorStrategy`), then a lazy
/// `yield return` enumerator issuing exactly one C `next` call per
/// `MoveNext`. Each yielded element is received per its element plan after
/// conversion, and the compiler-generated `finally` destroys the native
/// iterator exactly once, whether enumeration runs to exhaustion or is
/// abandoned early (C# `foreach` disposes the enumerator). Wrapping the
/// single enumerator in `WeaveFFIOnceEnumerable` makes a second enumeration
/// throw instead of double-destroying the consumed handle.
pub(crate) fn render_iterator_wrapper_method(
    out: &mut String,
    f: &FnBinding,
    it: &IteratorBinding,
    method_name: &str,
    self_expr: Option<&str>,
    err: ErrCtx,
) {
    let f = camel_fn(f);
    let elem_cs = cs_type(&it.elem);
    let staticness = if self_expr.is_none() { "static " } else { "" };

    let launch_call = format!(
        "var iter = NativeMethods.{}({}ref err);",
        it.launch.symbol,
        args_part(&full_call_args(&f, self_expr))
    );

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    w.line("/// <remarks>Streams lazily: each element is pulled from the native");
    w.line("/// iterator on demand, and the iterator is destroyed when enumeration");
    w.line("/// completes or the enumerator is disposed (a <c>foreach</c> disposes it");
    w.line("/// automatically, including on early exit). The returned sequence can be");
    w.line("/// enumerated only once.</remarks>");
    err.write_exception_doc(&mut w);
    write_obsolete(&mut w, &f.deprecated);

    let wrap_return =
        format!("return new WeaveFFIOnceEnumerable<{elem_cs}>(Enumerate{method_name}(iter));");
    w.line(format!(
        "public {staticness}IEnumerable<{elem_cs}> {method_name}({})",
        params_sig(&f.params)
    ));
    w.line("{");
    w.scope(|w| {
        w.line("var err = new WeaveFFIError();");
        render_marshalled_call(w, &f.params, |w| {
            w.line(launch_call);
            w.line(err.check_stmt());
            w.line(wrap_return);
        });
    });
    w.line("}");
    w.blank();

    // The `_next` out-slots after the iterator handle, excluding the error.
    let next_out_args: Vec<String> = it
        .next
        .params
        .iter()
        .skip(1)
        .filter(|slot| !is_error_slot(slot))
        .map(|slot| format!("out var {}", slot.name))
        .collect();

    // A `yield return` iterator method: the compiler emits the `finally`
    // into Dispose(), so the destroy below runs exactly once, on exhaustion
    // or when the consumer abandons enumeration early.
    w.line(format!(
        "private static IEnumerator<{elem_cs}> Enumerate{method_name}(IntPtr iter)"
    ));
    w.line("{");
    w.scope(|w| {
        w.line("try");
        w.line("{");
        w.scope(|w| {
            w.line("while (true)");
            w.line("{");
            w.scope(|w| {
                w.line("var iterErr = new WeaveFFIError();");
                w.line(format!(
                    "if (NativeMethods.{}(iter, {}, ref iterErr) == 0)",
                    it.next.symbol,
                    next_out_args.join(", ")
                ));
                w.line("{");
                w.scope(|w| {
                    w.line(err.check_stmt_for("iterErr"));
                    w.line("yield break;");
                });
                w.line("}");
                w.line(err.check_stmt_for("iterErr"));
                let item_expr = iterator_item_conversion(w, &it.elem);
                w.line(format!("yield return {item_expr};"));
            });
            w.line("}");
        });
        w.line("}");
        w.line("finally");
        w.line("{");
        w.scope(|w| {
            w.line(format!("NativeMethods.{}(iter);", it.destroy_symbol));
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// True when an async result crosses the completion callback as an owned
/// `ptr` + `len` pair: bytes and every buffered type (records, rich enums,
/// lists, maps, and non-interface optionals).
pub(crate) fn async_result_is_ptr_len(ty: &Ty) -> bool {
    is_ptr_len_result(ty)
}

/// The completion lambda's formal parameter list for one async return type,
/// matching the delegate declared by the P/Invoke layer.
pub(crate) fn async_cb_lambda_params(ret: &Option<Ty>) -> &'static str {
    match ret {
        None => "(context, err)",
        Some(ty) if async_result_is_ptr_len(ty) => "(context, err, result, resultLen)",
        Some(_) => "(context, err, result)",
    }
}

/// Render an async wrapper returning `Task`/`Task<T>` via a
/// `TaskCompletionSource` resolved from the native completion callback. A
/// non-zero error slot faults the task with the typed or generic exception
/// according to `err`.
pub(crate) fn render_async_wrapper_method(
    out: &mut String,
    f: &FnBinding,
    method_name: &str,
    self_expr: Option<&str>,
    err: ErrCtx,
) {
    let f = camel_fn(f);
    let c_sym = &f.c_base;
    let delegate_name = format!("NativeMethods.AsyncCb_{c_sym}");
    let staticness = if self_expr.is_none() { "static " } else { "" };

    let task_ret = f
        .ret
        .as_ref()
        .map(|ty| format!("Task<{}>", cs_type(ty)))
        .unwrap_or_else(|| "Task".into());

    let tcs_type = f.ret.as_ref().map(cs_type).unwrap_or_else(|| "bool".into());

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    write_obsolete(&mut w, &f.deprecated);

    w.line(format!(
        "public {staticness}async {task_ret} {method_name}({})",
        params_sig(&f.params)
    ));
    w.line("{");
    w.scope(|w| {
        w.line(format!(
            "var tcs = new TaskCompletionSource<{tcs_type}>(TaskCreationOptions.RunContinuationsAsynchronously);"
        ));

        let cb_lambda_params = async_cb_lambda_params(&f.ret);
        w.line(format!("{delegate_name} callback = {cb_lambda_params} =>"));
        w.line("{");
        w.scope(|w| {
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line("if (err != IntPtr.Zero)");
                w.line("{");
                w.scope(|w| {
                    w.line("var wErr = Marshal.PtrToStructure<WeaveFFIError>(err);");
                    w.line("if (wErr.Code != 0)");
                    w.line("{");
                    w.scope(|w| {
                        // The boxed error is owned by the consumer: copy the
                        // message and payload, then release it with
                        // `weaveffi_error_free`.
                        w.line("var msg = Marshal.PtrToStringUTF8(wErr.Message) ?? \"\";");
                        if matches!(err, ErrCtx::Domain(_)) {
                            w.line("var payload = WeaveFFIError.CopyPayload(wErr);");
                        }
                        w.line("NativeMethods.weaveffi_error_free(err);");
                        w.line(format!("tcs.SetException({});", err.async_exception_expr()));
                        w.line("return;");
                    });
                    w.line("}");
                });
                w.line("}");
                render_async_set_result(w, &f.ret);
            });
            w.line("}");
            w.line("finally");
            w.line("{");
            w.scope(|w| {
                w.line("if (context != IntPtr.Zero)");
                w.line("{");
                w.scope(|w| {
                    w.line("GCHandle.FromIntPtr(context).Free();");
                });
                w.line("}");
            });
            w.line("}");
        });
        w.line("};");
        w.line("var gcHandle = GCHandle.Alloc(callback, GCHandleType.Normal);");
        w.line("var ctx = GCHandle.ToIntPtr(gcHandle);");

        let args = args_part(&full_call_args(&f, self_expr));
        let cancel_arg = if f.cancellable { "IntPtr.Zero, " } else { "" };
        let launch = format!("NativeMethods.{c_sym}_async({args}{cancel_arg}callback, ctx);");

        render_marshalled_call(w, &f.params, |w| {
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line(launch);
            });
            w.line("}");
            w.line("catch");
            w.line("{");
            w.scope(|w| {
                w.line("if (gcHandle.IsAllocated) gcHandle.Free();");
                w.line("throw;");
            });
            w.line("}");
        });

        if f.ret.is_some() {
            w.line("return await tcs.Task;");
        } else {
            w.line("await tcs.Task;");
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the statements resolving the `TaskCompletionSource` from the
/// completion callback's result slots, honoring the `AsyncProtocol` owned
/// results clause: string, bytes, and buffered result buffers are owned by
/// the consumer, so they are deep-copied (and buffered results decoded)
/// here and then released through the runtime free symbols. An object
/// result transfers one strong reference instead: the wrapper adopts the
/// pointer.
pub(crate) fn render_async_set_result(w: &mut CodeWriter, ret: &Option<Ty>) {
    let Some(ty) = ret else {
        w.line("tcs.SetResult(true);");
        return;
    };
    match ty.family() {
        Family::Buffer => {
            w.line("var resultBuf = new byte[(int)resultLen];");
            w.line(
                "if (result != IntPtr.Zero && (int)resultLen > 0) Marshal.Copy(result, resultBuf, 0, (int)resultLen);",
            );
            w.line(
                "if (result != IntPtr.Zero) NativeMethods.weaveffi_free_bytes(result, resultLen);",
            );
            emit_buffer_decode(w, ty, "decoded", "resultBuf");
            w.line("tcs.SetResult(decoded);");
        }
        Family::String => {
            w.line("var str = Marshal.PtrToStringUTF8(result) ?? \"\";");
            w.line("if (result != IntPtr.Zero) NativeMethods.weaveffi_free_string(result);");
            w.line("tcs.SetResult(str);");
        }
        Family::Bytes => {
            w.line("var arr = new byte[(int)resultLen];");
            w.line(
                "if (result != IntPtr.Zero && (int)resultLen > 0) Marshal.Copy(result, arr, 0, (int)resultLen);",
            );
            w.line(
                "if (result != IntPtr.Zero) NativeMethods.weaveffi_free_bytes(result, resultLen);",
            );
            w.line("tcs.SetResult(arr);");
        }
        Family::Object { nullable } => {
            let adopt = adopt_object(object_class(ty), "result");
            if nullable {
                w.line(format!(
                    "tcs.SetResult(result == IntPtr.Zero ? null : {adopt});"
                ));
            } else {
                w.line(format!("tcs.SetResult({adopt});"));
            }
        }
        Family::Direct => {
            w.line(format!(
                "tcs.SetResult({});",
                direct_from_slot(ty, "result")
            ));
        }
        Family::Callback => unreachable!("callback interfaces are never returned"),
        Family::Iterator => unreachable!("iterator functions render via CallShape::Iterator"),
    }
}

/// Renders one module's static wrapper class. Submodules become sibling
/// classes named by their full path (`KvStats`, not a nested `Kv.Stats`):
/// flat classes keep generated type names (`Stats`) unambiguous, since a
/// nested module class with the same name as a struct wrapper would shadow it.
pub(crate) fn render_wrapper_class(
    out: &mut String,
    mb: &ModuleBinding,
    strip_module_prefix: bool,
) {
    let class_name: String = mb
        .segments
        .iter()
        .map(|s| s.to_upper_camel_case())
        .collect();
    out.push_str(&format!("    public static class {class_name}\n    {{\n"));

    for f in &mb.functions {
        let method_name =
            wrapper_name(&mb.path, &f.name, strip_module_prefix).to_upper_camel_case();
        let err = ErrCtx::for_fn(f, mb.error.as_ref());
        render_wrapper_method(out, f, &method_name, None, err);
    }

    out.push_str("    }\n\n");
}
