//! Call-path renderers: the wrapper methods for every call shape (sync,
//! async, iterator), parameter marshalling driven by the shared
//! [`ArgPass`] plan, return conversions, listener registration, and the
//! per-module static wrapper classes.

use heck::ToUpperCamelCase;
use weaveffi_core::abi;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    CallShape, ErrorBinding, FnBinding, IteratorBinding, ListenerBinding, ModuleBinding,
    ParamBinding,
};
use weaveffi_core::plan::{self, ArgPass, ElemFree, ErrorStrategy};
use weaveffi_core::utils::{local_type_name, wrapper_name};

use crate::codec::{emit_buffer_decode, emit_buffer_write};
use crate::docs::{writer_doc, writer_fn_doc};
use crate::pinvoke::is_error_slot;
use crate::runtime::{check_method_name, dotnet_exception_name};
use crate::types::{camel_fn, cs_type, safe_cs_name, typed_handle_cs};

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

/// True when a parameter needs setup/cleanup statements around the native
/// call, per its [`ArgPass`] plan: strings (`CoTaskMem` UTF-8 copies), bytes
/// (pinned arrays), and every buffered value (encoded into a pinned value
/// buffer). Direct and object arguments pass inline.
pub(crate) fn param_needs_marshal(p: &ParamBinding) -> bool {
    matches!(
        p.arg_pass(),
        ArgPass::String { .. } | ArgPass::Bytes { .. } | ArgPass::Buffer { .. }
    )
}

/// Emit the setup statements for one parameter before the native call,
/// dispatching on its [`ArgPass`] plan. Strings copy to `CoTaskMem` UTF-8;
/// bytes pin the managed array; buffered parameters encode into a `byte[]`
/// value buffer (`{name}Buf`) and pin it (`{name}Pin`), which the caller
/// owns for the duration of the call.
pub(crate) fn render_marshal_setup(out: &mut String, p: &ParamBinding, indent: &str) {
    let name = safe_cs_name(&p.name);
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            w.line(format!("var {name}Writer = new WeaveFFIBufferWriter();"));
            emit_buffer_write(&mut w, &p.ty, &name, &format!("{name}Writer"), 0);
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
        ArgPass::Direct { .. } | ArgPass::Object { .. } => {}
    }
    out.push_str(&w.finish());
}

/// Emit the cleanup statements releasing what [`render_marshal_setup`]
/// allocated: the `CoTaskMem` string copy or the pinned array handle.
pub(crate) fn render_marshal_cleanup(out: &mut String, p: &ParamBinding, indent: &str) {
    let name = safe_cs_name(&p.name);
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    match p.arg_pass() {
        ArgPass::Buffer { .. } | ArgPass::Bytes { .. } => {
            w.line(format!("{name}Pin.Free();"));
        }
        ArgPass::String { .. } => {
            w.line(format!("Marshal.FreeCoTaskMem({name}Ptr);"));
        }
        ArgPass::Direct { .. } | ArgPass::Object { .. } => {}
    }
    out.push_str(&w.finish());
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
                // Interface parameters borrow: pass the handle, ownership
                // stays with the caller's wrapper. A nullable object passes
                // null as IntPtr.Zero.
                ArgPass::Object { nullable: true, .. } => {
                    vec![format!("{name}?.Handle ?? IntPtr.Zero")]
                }
                ArgPass::Object {
                    nullable: false, ..
                } => {
                    vec![format!("{name}.Handle")]
                }
                // Direct slots pass by value; only the C# spelling of the
                // value expression depends on the surface type.
                ArgPass::Direct { .. } => match &p.ty {
                    Ty::Bool => vec![format!("(byte)({name} ? 1 : 0)")],
                    Ty::Enum(_) => vec![format!("(int){name}")],
                    // A typed handle passes its raw pointer token by value.
                    Ty::TypedHandle(_) => vec![format!("{name}.Raw")],
                    _ => vec![name],
                },
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
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

/// Emit the native call, the error check, and the return conversion for one
/// synchronous callable at `indent`.
pub(crate) fn render_pinvoke_call_and_return(
    out: &mut String,
    f: &FnBinding,
    self_expr: Option<&str>,
    err: ErrCtx,
    indent: &str,
) {
    let c_sym = &f.c_base;
    let call_args = full_call_args(f, self_expr);

    // Bytes and buffered returns deliver their length through the trailing
    // `size_t* out_len` slot.
    let has_out_len = f
        .ret
        .as_ref()
        .is_some_and(|r| matches!(r, Ty::Bytes | Ty::BorrowedBytes) || r.is_buffered());

    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    if f.ret.is_some() {
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        let out_len_part = if has_out_len { "out var outLen, " } else { "" };
        w.line(format!(
            "var result = NativeMethods.{c_sym}({args_part}{out_len_part}ref err);"
        ));
    } else {
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        w.line(format!("NativeMethods.{c_sym}({args_part}ref err);"));
    }

    w.line(err.check_stmt());
    out.push_str(&w.finish());

    if let Some(ret_ty) = &f.ret {
        render_return_conversion(out, ret_ty, indent);
    }
}

/// Emit the statements converting the raw `result` slot (plus `outLen` for
/// bytes and buffered returns) into the returned C# value, releasing
/// producer-owned memory along the way.
pub(crate) fn render_return_conversion(out: &mut String, ty: &Ty, indent: &str) {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    // A buffered return is a producer-allocated value buffer: copy the bytes,
    // release them with `weaveffi_free_bytes`, then decode the copy.
    if ty.is_buffered() {
        w.line("var resultBuf = new byte[(int)outLen];");
        w.line(
            "if (result != IntPtr.Zero && (int)outLen > 0) Marshal.Copy(result, resultBuf, 0, (int)outLen);",
        );
        w.line("NativeMethods.weaveffi_free_bytes(result, outLen);");
        emit_buffer_decode(&mut w, ty, "value", "resultBuf");
        w.line("return value;");
        out.push_str(&w.finish());
        return;
    }
    match ty {
        Ty::Bool => {
            w.line("return result != 0;");
        }
        Ty::StringUtf8 | Ty::BorrowedStr => {
            w.line("var str = Marshal.PtrToStringUTF8(result);");
            w.line("NativeMethods.weaveffi_free_string(result);");
            w.line("return str ?? \"\";");
        }
        Ty::Enum(name) => {
            let cn = local_type_name(name);
            w.line(format!("return ({cn})result;"));
        }
        Ty::TypedHandle(name) => {
            let cn = typed_handle_cs(name);
            w.line(format!("return new {cn}(result);"));
        }
        // An interface return transfers ownership (`ReturnFree::OwnedObject`):
        // wrap the pointer in a new instance whose Dispose() releases it.
        Ty::Interface(name) => {
            let cn = local_type_name(name);
            w.line(format!("return new {cn}(result);"));
        }
        Ty::Bytes | Ty::BorrowedBytes => {
            w.line("if (result == IntPtr.Zero) return Array.Empty<byte>();");
            w.line("var arr = new byte[(int)outLen];");
            w.line("Marshal.Copy(result, arr, 0, (int)outLen);");
            w.line("NativeMethods.weaveffi_free_bytes(result, outLen);");
            w.line("return arr;");
        }
        // Only `Interface?` reaches here: a nullable owned object pointer.
        Ty::Optional(inner) => match inner.as_ref() {
            Ty::Interface(name) => {
                let cn = local_type_name(name);
                w.line(format!(
                    "return result == IntPtr.Zero ? null : new {cn}(result);"
                ));
            }
            _ => unreachable!("non-interface optionals are buffered"),
        },
        Ty::Iterator(_) => unreachable!("iterator functions render via CallShape::Iterator"),
        Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => {
            unreachable!("buffered return handled above")
        }
        _ => {
            w.line("return result;");
        }
    }
    out.push_str(&w.finish());
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

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }

    w.line(format!(
        "public {staticness}{ret_cs} {method_name}({})",
        params_sig.join(", ")
    ));
    w.line("{");
    w.scope(|w| {
        w.line("var err = new WeaveFFIError();");

        let needs_try = f.params.iter().any(param_needs_marshal);

        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            let mut tmp = String::new();
            render_pinvoke_call_and_return(&mut tmp, &f, self_expr, err, "                ");
            w.raw(tmp);
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            let mut tmp = String::new();
            render_pinvoke_call_and_return(&mut tmp, &f, self_expr, err, "            ");
            w.raw(tmp);
        }
    });

    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The statements converting one `_next` out-item into the yielded C# value,
/// freeing any producer-allocated memory per the shared [`plan::elem_free`]
/// classification (`ElemFree::String` via `weaveffi_free_string`,
/// `ElemFree::Bytes` via `weaveffi_free_bytes` for both bytes and buffered
/// elements). Returns the expression to `yield return`.
pub(crate) fn iterator_item_conversion(out: &mut String, elem: &Ty, indent: &str) -> String {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    if elem.is_buffered() {
        w.line("var itemBuf = new byte[(int)out_len];");
        w.line("if (out_item != IntPtr.Zero && (int)out_len > 0) Marshal.Copy(out_item, itemBuf, 0, (int)out_len);");
        w.line("NativeMethods.weaveffi_free_bytes(out_item, out_len);");
        emit_buffer_decode(&mut w, elem, "item", "itemBuf");
        out.push_str(&w.finish());
        return "item".into();
    }
    let expr = match plan::elem_free(elem) {
        ElemFree::String => {
            w.line("var item = Marshal.PtrToStringUTF8(out_item) ?? \"\";");
            w.line("NativeMethods.weaveffi_free_string(out_item);");
            "item".into()
        }
        // The buffered flavor of `ElemFree::Bytes` returned above; only raw
        // byte elements reach here.
        ElemFree::Bytes => {
            w.line("var item = new byte[(int)out_len];");
            w.line("if (out_item != IntPtr.Zero && (int)out_len > 0) Marshal.Copy(out_item, item, 0, (int)out_len);");
            w.line("NativeMethods.weaveffi_free_bytes(out_item, out_len);");
            "item".into()
        }
        ElemFree::None => match elem {
            Ty::I8
            | Ty::I16
            | Ty::I32
            | Ty::U8
            | Ty::U16
            | Ty::U32
            | Ty::I64
            | Ty::U64
            | Ty::F32
            | Ty::F64
            | Ty::Handle => "out_item".into(),
            Ty::Bool => "out_item != 0".into(),
            Ty::Enum(name) => format!("({})out_item", local_type_name(name)),
            Ty::TypedHandle(name) => {
                format!("new {}(out_item)", typed_handle_cs(name))
            }
            // The consumer owns each yielded wrapper; Dispose() destroys it
            // (owned-object elements are adopted rather than freed eagerly).
            Ty::Interface(name) => {
                format!("new {}(out_item)", local_type_name(name))
            }
            other => unreachable!("unsupported iterator element type {other:?}"),
        },
    };
    out.push_str(&w.finish());
    expr
}

/// An `iter<T>` function surfaces as `IEnumerable<T>`, rendering the
/// `IteratorProtocol` pull contract: an eager launcher call (so launch errors
/// throw immediately, per the function's `ErrorStrategy`), then a lazy
/// `yield return` enumerator issuing exactly one C `next` call per
/// `MoveNext`. Each yielded element is released per its `ElemFree` plan after
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

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    let call_args = full_call_args(&f, self_expr);
    let args_part = if call_args.is_empty() {
        String::new()
    } else {
        format!("{call_args}, ")
    };
    let launch_call = format!(
        "var iter = NativeMethods.{}({args_part}ref err);",
        it.launch.symbol
    );

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    w.line("/// <remarks>Streams lazily: each element is pulled from the native");
    w.line("/// iterator on demand, and the iterator is destroyed when enumeration");
    w.line("/// completes or the enumerator is disposed (a <c>foreach</c> disposes it");
    w.line("/// automatically, including on early exit). The returned sequence can be");
    w.line("/// enumerated only once.</remarks>");
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }

    let wrap_return =
        format!("return new WeaveFFIOnceEnumerable<{elem_cs}>(Enumerate{method_name}(iter));");
    w.line(format!(
        "public {staticness}IEnumerable<{elem_cs}> {method_name}({})",
        params_sig.join(", ")
    ));
    w.line("{");
    w.scope(|w| {
        w.line("var err = new WeaveFFIError();");

        let needs_try = f.params.iter().any(param_needs_marshal);
        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line(launch_call.clone());
                w.line(err.check_stmt());
                w.line(wrap_return.clone());
            });
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            w.line(launch_call.clone());
            w.line(err.check_stmt());
            w.line(wrap_return.clone());
        }
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
                let mut conv = String::new();
                let item_expr =
                    iterator_item_conversion(&mut conv, &it.elem, "                    ");
                w.raw(conv);
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
    matches!(ty, Ty::Bytes | Ty::BorrowedBytes) || ty.is_buffered()
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

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }

    w.line(format!(
        "public {staticness}async {task_ret} {method_name}({})",
        params_sig.join(", ")
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

                let mut tmp = String::new();
                render_async_set_result(&mut tmp, &f.ret, "                    ");
                w.raw(tmp);
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

        let needs_try = f.params.iter().any(param_needs_marshal);
        let call_args = full_call_args(&f, self_expr);
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        let cancel_arg = if f.cancellable { "IntPtr.Zero, " } else { "" };

        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line("try");
                w.line("{");
                w.scope(|w| {
                    w.line(format!(
                        "NativeMethods.{c_sym}_async({args_part}{cancel_arg}callback, ctx);"
                    ));
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
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line(format!(
                    "NativeMethods.{c_sym}_async({args_part}{cancel_arg}callback, ctx);"
                ));
            });
            w.line("}");
            w.line("catch");
            w.line("{");
            w.scope(|w| {
                w.line("if (gcHandle.IsAllocated) gcHandle.Free();");
                w.line("throw;");
            });
            w.line("}");
        }

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
/// here and then released through the runtime free symbols. Owned-object
/// results (interfaces, typed handles) transfer ownership instead: the
/// wrapper adopts the pointer.
pub(crate) fn render_async_set_result(out: &mut String, ret: &Option<Ty>, indent: &str) {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    if let Some(ty) = ret {
        if ty.is_buffered() {
            w.line("var resultBuf = new byte[(int)resultLen];");
            w.line(
                "if (result != IntPtr.Zero && (int)resultLen > 0) Marshal.Copy(result, resultBuf, 0, (int)resultLen);",
            );
            w.line(
                "if (result != IntPtr.Zero) NativeMethods.weaveffi_free_bytes(result, resultLen);",
            );
            emit_buffer_decode(&mut w, ty, "value", "resultBuf");
            w.line("tcs.SetResult(value);");
            out.push_str(&w.finish());
            return;
        }
    }
    match ret {
        None => {
            w.line("tcs.SetResult(true);");
        }
        Some(Ty::Bool) => {
            w.line("tcs.SetResult(result != 0);");
        }
        Some(Ty::StringUtf8 | Ty::BorrowedStr) => {
            w.line("var str = Marshal.PtrToStringUTF8(result) ?? \"\";");
            w.line("if (result != IntPtr.Zero) NativeMethods.weaveffi_free_string(result);");
            w.line("tcs.SetResult(str);");
        }
        Some(Ty::Enum(name)) => {
            let cn = local_type_name(name);
            w.line(format!("tcs.SetResult(({cn})result);"));
        }
        Some(Ty::TypedHandle(name)) => {
            let cn = typed_handle_cs(name);
            w.line(format!("tcs.SetResult(new {cn}(result));"));
        }
        Some(Ty::Interface(name)) => {
            let cn = local_type_name(name);
            w.line(format!("tcs.SetResult(new {cn}(result));"));
        }
        Some(Ty::Bytes | Ty::BorrowedBytes) => {
            w.line("var arr = new byte[(int)resultLen];");
            w.line(
                "if (result != IntPtr.Zero && (int)resultLen > 0) Marshal.Copy(result, arr, 0, (int)resultLen);",
            );
            w.line(
                "if (result != IntPtr.Zero) NativeMethods.weaveffi_free_bytes(result, resultLen);",
            );
            w.line("tcs.SetResult(arr);");
        }
        // Only `Interface?` reaches here: a nullable owned object pointer.
        Some(Ty::Optional(inner)) => match inner.as_ref() {
            Ty::Interface(name) => {
                let cn = local_type_name(name);
                w.line(format!(
                    "tcs.SetResult(result == IntPtr.Zero ? null : new {cn}(result));"
                ));
            }
            _ => unreachable!("non-interface optionals are buffered"),
        },
        // Remaining scalars pass by value in the result slot.
        Some(_) => {
            w.line("tcs.SetResult(result);");
        }
    }
    out.push_str(&w.finish());
}

/// Statements (appended to `out`) plus the expression converting one callback
/// parameter's delegate slots into the value handed to the user callback.
/// Buffered parameters arrive as a borrowed `ptr` + `len` pair valid only for
/// the dispatch, so the bytes are copied and decoded before the user's
/// delegate runs, and never freed here.
pub(crate) fn render_cb_arg(
    out: &mut String,
    p: &ParamBinding,
    idx: usize,
    indent: &str,
) -> String {
    let slots = abi::lower_param(&p.name, &p.ty, "", false);
    let n0 = safe_cs_name(&slots[0].name);
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    if p.ty.is_buffered() {
        let len = safe_cs_name(&slots[1].name);
        let arg = format!("arg{idx}");
        w.line(format!("var {arg}Buf = new byte[(int){len}];"));
        w.line(format!(
            "if ({n0} != IntPtr.Zero && (int){len} > 0) Marshal.Copy({n0}, {arg}Buf, 0, (int){len});"
        ));
        emit_buffer_decode(&mut w, &p.ty, &arg, &format!("{arg}Buf"));
        out.push_str(&w.finish());
        return arg;
    }
    let expr = match &p.ty {
        Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::I64
        | Ty::U64
        | Ty::F32
        | Ty::F64 => n0,
        Ty::Handle => n0,
        Ty::Bool => format!("{n0} != 0"),
        Ty::Enum(name) => format!("({}){n0}", local_type_name(name)),
        Ty::StringUtf8 | Ty::BorrowedStr => {
            format!("Marshal.PtrToStringUTF8({n0}) ?? \"\"")
        }
        Ty::Bytes | Ty::BorrowedBytes => {
            let len = safe_cs_name(&slots[1].name);
            let arg = format!("arg{idx}");
            w.line(format!("var {arg} = new byte[(int){len}];"));
            w.line(format!(
                "if ({n0} != IntPtr.Zero && (int){len} > 0) Marshal.Copy({n0}, {arg}, 0, (int){len});"
            ));
            arg
        }
        Ty::TypedHandle(name) => {
            format!("new {}({n0})", typed_handle_cs(name))
        }
        // Borrowed for the duration of the callback; the consumer must not
        // Dispose() the wrapper.
        Ty::Interface(name) => {
            format!("new {}({n0})", local_type_name(name))
        }
        // Only `Interface?` reaches here: every other optional is buffered.
        Ty::Optional(inner) => match inner.as_ref() {
            Ty::Interface(name) => {
                let cn = local_type_name(name);
                format!("{n0} == IntPtr.Zero ? null : new {cn}({n0})")
            }
            _ => unreachable!("non-interface optionals are buffered"),
        },
        Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => {
            unreachable!("buffered callback parameter handled above")
        }
        Ty::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
    };
    out.push_str(&w.finish());
    expr
}

/// The register/unregister method pair for one listener, emitted into the
/// module's wrapper class alongside `_listenerRefs`.
pub(crate) fn render_listener_methods(
    out: &mut String,
    mb: &ModuleBinding,
    l: &ListenerBinding,
    strip_module_prefix: bool,
) {
    let Some(cb) = mb.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };
    let register_name = wrapper_name(
        &mb.path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    )
    .to_upper_camel_case();
    let unregister_name = wrapper_name(
        &mb.path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    )
    .to_upper_camel_case();
    let delegate_name = format!("NativeMethods.Cb_{}", cb.c_fn_type);

    let action_type = if cb.params.is_empty() {
        "Action".to_string()
    } else {
        format!(
            "Action<{}>",
            cb.params
                .iter()
                .map(|p| cs_type(&p.ty))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let lambda_formals: Vec<String> = cb
        .abi_params
        .iter()
        .map(|slot| safe_cs_name(&slot.name))
        .collect();

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_doc(&mut w, &l.doc);
    w.line(format!(
        "/// <returns>A subscription id for {unregister_name}().</returns>"
    ));
    w.line(format!(
        "public static ulong {register_name}({action_type} callback)"
    ));
    w.line("{");
    w.scope(|w| {
        w.line(format!(
            "{delegate_name} trampoline = ({}) =>",
            lambda_formals.join(", ")
        ));
        w.line("{");
        w.scope(|w| {
            let mut stmts = String::new();
            let mut args = Vec::new();
            for (idx, p) in cb.params.iter().enumerate() {
                args.push(render_cb_arg(&mut stmts, p, idx, "                "));
            }
            w.raw(stmts);
            w.line(format!("callback({});", args.join(", ")));
        });
        w.line("};");
        w.line("ulong id;");
        w.line("lock (_listenerLock)");
        w.line("{");
        w.scope(|w| {
            w.line(format!(
                "id = NativeMethods.{}(trampoline, IntPtr.Zero);",
                l.register_symbol
            ));
            w.line("_listenerRefs[id] = trampoline;");
        });
        w.line("}");
        w.line("return id;");
    });
    w.line("}");
    w.blank();

    w.line(format!(
        "/// <summary>Unregisters a listener previously registered with {register_name}().</summary>"
    ));
    w.line(format!("public static void {unregister_name}(ulong id)"));
    w.line("{");
    w.scope(|w| {
        w.line(format!("NativeMethods.{}(id);", l.unregister_symbol));
        w.line("lock (_listenerLock)");
        w.line("{");
        w.scope(|w| {
            w.line("_listenerRefs.Remove(id);");
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
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

    if !mb.listeners.is_empty() {
        out.push_str("        private static readonly object _listenerLock = new object();\n");
        out.push_str(
            "        // Live listener delegates by subscription id. Holding the delegate\n",
        );
        out.push_str(
            "        // here keeps its native thunk alive until unregistered; without this\n",
        );
        out.push_str("        // the GC could collect a delegate the producer still calls.\n");
        out.push_str(
            "        private static readonly Dictionary<ulong, Delegate> _listenerRefs = new Dictionary<ulong, Delegate>();\n\n",
        );
        for l in &mb.listeners {
            render_listener_methods(out, mb, l, strip_module_prefix);
        }
    }
    for f in &mb.functions {
        let method_name =
            wrapper_name(&mb.path, &f.name, strip_module_prefix).to_upper_camel_case();
        let err = ErrCtx::for_fn(f, mb.error.as_ref());
        render_wrapper_method(out, f, &method_name, None, err);
    }

    out.push_str("    }\n\n");
}
