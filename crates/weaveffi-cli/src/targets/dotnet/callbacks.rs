//! Callback-interface renderers: the consumer-facing C# `interface` plus the
//! process-wide vtable and its `[UnmanagedCallersOnly]` trampolines.
//!
//! * One static vtable per callback interface, allocated once in native
//!   memory and never freed, so its address is stable for the process. It
//!   starts with the `{size, flags, free}` header, `size` being the struct's
//!   real size as C# lays it out and `flags` zero (methods may run on any
//!   thread).
//! * `ctx` is a `GCHandle` to the implementation, released by the vtable's
//!   `free` entry, so the implementation lives exactly as long as the
//!   producer holds it.
//! * Arguments are borrowed for the call ([`ArgPass`]): strings and buffers
//!   are decoded, typed arrays and bytes are spans over the producer's
//!   memory, and objects are adopted into wrappers the implementation owns.
//! * Returns cross per [`CallbackRetPass`]: a scalar as the C return, an
//!   optional scalar as presence plus out slot, an object as a fresh strong
//!   reference, and everything else as a `{prefix}_alloc` run the producer
//!   adopts.
//! * A thrown exception never unwinds into native code: an exception of the
//!   method's own domain reports its code and fields; any other is reported
//!   with code -1 (or -4 from a method that can't fail).

use heck::ToUpperCamelCase;
use weaveffi_model::abi::AbiParam;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, Model};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ErrorStrategy};

use crate::codegen::CodeWriter;
use crate::targets::dotnet::calls::UNMANAGED_CALLERS_ONLY;
use crate::targets::dotnet::codec::{read_lambda, write_lambda};
use crate::targets::dotnet::docs::Docs;
use crate::targets::dotnet::errors::ErrCtx;
use crate::targets::dotnet::types::{
    callback_interface_cs, callback_param_cs, callback_ret_cs, camel, cs_ctype, fn_pointer_type,
    safe_cs_name, vtable_class_cs, Cx,
};

/// Render one callback interface: the `public interface I{Name}` the
/// consumer implements, then the internal class hosting its vtable.
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    model: &Model,
    docs: &Docs,
    cb: &CallbackInterfaceBinding,
    cx: Cx<'_>,
) {
    render_consumer_interface(w, model, docs, cb, cx);
    render_vtable_class(w, model, cb, cx);
}

fn method_cs(m: &CallbackMethodBinding) -> String {
    m.name.to_upper_camel_case()
}

/// The consumer-facing interface: one method per callback method.
fn render_consumer_interface(
    w: &mut CodeWriter,
    model: &Model,
    docs: &Docs,
    cb: &CallbackInterfaceBinding,
    cx: Cx<'_>,
) {
    let iface = callback_interface_cs(&cb.name);
    docs.summary(w, &cb.doc);
    w.line("/// <remarks>The native library may call any method from any thread until");
    w.line("/// it releases its last reference to the implementation. Span arguments are");
    w.line("/// valid only during the call; object arguments belong to the implementation.");
    w.line("/// An exception thrown by a method fails the native call in progress: an");
    w.line("/// exception of the domain a method documents is reported with its code and");
    w.line("/// fields, and any other reaches the library as a failure carrying its message.</remarks>");
    docs.obsolete(w, &cb.deprecated);
    w.line(format!("public interface {iface}"));
    w.block("{", "}", |w| {
        for (i, m) in cb.methods.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            docs.summary(w, &m.doc);
            for p in &m.params {
                docs.param(w, &camel(&p.name), &p.doc);
            }
            if let ErrorStrategy::Domain(_) = &m.error {
                let err = ErrCtx::new(model, &m.error, cx);
                if let Some(exc) = &err.domain {
                    w.line(format!(
                        "/// <exception cref=\"{exc}\">Reported to the library with its code and fields.</exception>"
                    ));
                }
            }
            docs.obsolete(w, &m.deprecated);
            let ret = m
                .ret
                .as_ref()
                .map(|ty| callback_ret_cs(&m.ret_pass, ty))
                .unwrap_or_else(|| "void".into());
            let sig: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{} {}", callback_param_cs(&p.pass, &p.ty), camel(&p.name)))
                .collect();
            w.line(format!("{ret} {}({});", method_cs(m), sig.join(", ")));
        }
    });
    w.blank();
}

/// The internal class owning the vtable (the header, then function pointers
/// in declaration order) and the trampolines it points at.
fn render_vtable_class(
    w: &mut CodeWriter,
    model: &Model,
    cb: &CallbackInterfaceBinding,
    cx: Cx<'_>,
) {
    let iface = callback_interface_cs(&cb.name);
    let class = vtable_class_cs(&cb.name);
    w.line(format!(
        "/// <summary>The process-wide <c>{}</c> and the trampolines behind it,",
        cb.vtable_tag
    ));
    w.line(format!(
        "/// adapting an <see cref=\"{iface}\"/> to the C ABI.</summary>"
    ));
    w.line(format!("internal static unsafe class {class}"));
    w.block("{", "}", |w| {
        w.line("[StructLayout(LayoutKind.Sequential)]");
        w.line("private struct Layout");
        w.block("{", "}", |w| {
            w.line("public uint Size;");
            w.line("public uint Flags;");
            w.line("public delegate* unmanaged[Cdecl]<IntPtr, void> Free;");
            for m in &cb.methods {
                w.line(format!(
                    "public {} {};",
                    fn_pointer_type(cx.ns, &m.abi.params, &m.abi.ret),
                    safe_cs_name(&m.name)
                ));
            }
        });
        w.blank();
        w.line("/// <summary>The vtable's address, valid for the process lifetime.</summary>");
        w.line("internal static readonly IntPtr Pointer = Allocate();");
        w.blank();
        w.line("private static IntPtr Allocate()");
        w.block("{", "}", |w| {
            w.line("var vtable = (Layout*)NativeMemory.AllocZeroed((nuint)sizeof(Layout));");
            w.line("vtable->Size = (uint)sizeof(Layout);");
            w.line("// Not thread-affine: every method may be called from any thread.");
            w.line("vtable->Flags = 0;");
            w.line("vtable->Free = &FreeTrampoline;");
            for m in &cb.methods {
                w.line(format!(
                    "vtable->{} = &{}Trampoline;",
                    safe_cs_name(&m.name),
                    method_cs(m)
                ));
            }
            w.line("return (IntPtr)vtable;");
        });
        w.blank();
        w.line(UNMANAGED_CALLERS_ONLY);
        w.line("private static void FreeTrampoline(IntPtr ctx)");
        w.block("{", "}", |w| {
            w.line("Ffi.Unregister(ctx);");
        });
        for m in &cb.methods {
            w.blank();
            render_trampoline(w, model, cx, &iface, m);
        }
    });
    w.blank();
}

/// The expression receiving one borrowed argument from its slots.
fn receive_arg(cx: Cx<'_>, pass: &ArgPass, ty: &weaveffi_model::ty::Ty) -> String {
    let n = |slot: &AbiParam| safe_cs_name(&slot.name);
    match pass {
        ArgPass::Direct { slot } => n(slot),
        ArgPass::OptDirect { has, value, .. } => format!("{} ? {} : null", n(has), n(value)),
        ArgPass::Slice { ptr, len, .. } | ArgPass::Bytes { ptr, len } => {
            format!("Ffi.Borrow({}, {})", n(ptr), n(len))
        }
        ArgPass::String { ptr, len } => format!("Ffi.ReadString({}, {})", n(ptr), n(len)),
        ArgPass::Buffer { ptr, len } => format!(
            "Ffi.ReadBuffer({}, {}, {})",
            n(ptr),
            n(len),
            read_lambda(cx, ty, 0)
        ),
        ArgPass::Object {
            slot,
            nullable,
            interface,
        } => {
            let class = cx.ty(interface);
            if *nullable {
                format!("{0} == IntPtr.Zero ? null : {class}.Adopt({0})", n(slot))
            } else {
                format!("{class}.Adopt({})", n(slot))
            }
        }
        ArgPass::Callback { .. } => unreachable!("a callback method never takes a callback"),
    }
}

/// One trampoline: receive the slots, call the implementation, and hand
/// its result back; any exception is reported through `out_err`.
fn render_trampoline(
    w: &mut CodeWriter,
    model: &Model,
    cx: Cx<'_>,
    iface: &str,
    m: &CallbackMethodBinding,
) {
    let sig: Vec<String> = m
        .abi
        .params
        .iter()
        .map(|s| format!("{} {}", cs_ctype(cx.ns, &s.ty), safe_cs_name(&s.name)))
        .collect();
    let ret = cs_ctype(cx.ns, &m.abi.ret);
    let returns_value = ret != "void";
    let ctx = safe_cs_name(&m.abi.params[0].name);
    let out_err = safe_cs_name(
        &m.abi
            .params
            .last()
            .expect("a callback method ends with out_err")
            .name,
    );
    let err = ErrCtx::new(model, &m.error, cx);
    let fallback = match m.error {
        ErrorStrategy::Trap => "ForeignErrorCode",
        _ => "GenericErrorCode",
    };
    let n = |slot: &AbiParam| safe_cs_name(&slot.name);
    w.line(UNMANAGED_CALLERS_ONLY);
    w.line(format!(
        "private static {ret} {}Trampoline({})",
        method_cs(m),
        sig.join(", ")
    ));
    w.block("{", "}", |w| {
        w.line("try");
        w.block("{", "}", |w| {
            let args: Vec<String> = m
                .params
                .iter()
                .map(|p| receive_arg(cx, &p.pass, &p.ty))
                .collect();
            let call = format!(
                "Ffi.Target<{iface}>({ctx}).{}({})",
                method_cs(m),
                args.join(", ")
            );
            match &m.ret_pass {
                CallbackRetPass::Void => w.line(format!("{call};")),
                CallbackRetPass::Direct => w.line(format!("return {call};")),
                CallbackRetPass::OptDirect { out_value } => w.line(format!(
                    "return Ffi.ReturnOptional({call}, {});",
                    n(out_value)
                )),
                CallbackRetPass::Slice {
                    out_ptr, out_len, ..
                } => w.line(format!(
                    "Ffi.ReturnArray({call}, {}, {});",
                    n(out_ptr),
                    n(out_len)
                )),
                CallbackRetPass::String { out_ptr, out_len } => w.line(format!(
                    "Ffi.ReturnString({call}, {}, {});",
                    n(out_ptr),
                    n(out_len)
                )),
                CallbackRetPass::Bytes { out_ptr, out_len } => w.line(format!(
                    "Ffi.ReturnBytes({call}, {}, {});",
                    n(out_ptr),
                    n(out_len)
                )),
                CallbackRetPass::Buffer { out_ptr, out_len } => {
                    let ty = m.ret.as_ref().expect("a buffered return has a type");
                    w.line(format!(
                        "Ffi.ReturnBuffer({call}, {}, {}, {});",
                        write_lambda(ty, 0),
                        n(out_ptr),
                        n(out_len)
                    ))
                }
                // One strong reference the producer adopts. A null for a
                // required object passes through as null, which the
                // producer rejects as a marshalling failure (-3).
                CallbackRetPass::Object { .. } => {
                    w.line(format!("return {call}?.CloneHandle() ?? IntPtr.Zero;"))
                }
            };
        });
        if let Some(exc) = &err.domain {
            w.line(format!("catch ({} e)", cx.ty(exc)));
            w.block("{", "}", |w| {
                w.line(format!("Ffi.ReportDomain({out_err}, e);"));
                if returns_value {
                    w.line("return default;");
                }
            });
        }
        w.line("catch (Exception e)");
        w.block("{", "}", |w| {
            w.line(format!("Ffi.Report({out_err}, e, {}.{fallback});", cx.base));
            if returns_value {
                w.line("return default;");
            }
        });
    });
}
