//! Callback-interface renderers: the consumer-facing C# `interface` plus the
//! process-wide vtable and its `[UnmanagedCallersOnly]` trampolines.
//!
//! * A parameter of a callback interface crosses as an `FfiCallback`, the
//!   `{p}_{Cb}` struct of a `ctx` and a vtable pointer, by value.
//! * One static vtable per callback interface, allocated once in native
//!   memory and never freed, so its address is stable for the process. It
//!   starts with the `{size, flags, free}` header, `size` being the struct's
//!   real size as C# lays it out and `flags` zero (methods may run on any
//!   thread).
//! * `ctx` is a `GCHandle` to the implementation, released by the vtable's
//!   `free` entry, so the implementation lives exactly as long as the
//!   producer holds it.
//! * Arguments are borrowed for the call: strings and buffers are decoded,
//!   typed arrays and bytes are spans over the producer's memory, and
//!   objects are adopted into wrappers the implementation owns.
//! * A return crosses as one C value: a scalar or optional scalar by
//!   value, an object as a fresh strong reference, and everything else as a
//!   `{p}_alloc` run the producer adopts.
//! * A thrown exception never unwinds into native code: an exception of the
//!   method's own domain reports its code and fields; any other is reported
//!   with code -1 (or -4 from a method that can't fail).

use weaveffi_model::model::{CallbackBinding, CallbackMethodBinding, ErrorStrategy, Model};

use crate::codegen::CodeWriter;
use crate::targets::dotnet::calls::{borrow, give, method_cs, UNMANAGED_CALLERS_ONLY};
use crate::targets::dotnet::docs::Docs;
use crate::targets::dotnet::errors::ErrCtx;
use crate::targets::dotnet::types::{
    callback_interface_cs, camel, cs_ctype, fn_pointer_type, lent_cs, owned_cs, safe_cs_name,
    vtable_class_cs, Cx,
};

/// Render one callback interface: the `public interface I{Name}` the
/// consumer implements, then the internal class hosting its vtable.
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    model: &Model,
    docs: &Docs,
    cb: &CallbackBinding,
    cx: Cx<'_>,
) {
    render_consumer_interface(w, model, docs, cb, cx);
    render_vtable_class(w, model, cb, cx);
}

/// The consumer-facing interface: one method per callback method.
fn render_consumer_interface(
    w: &mut CodeWriter,
    model: &Model,
    docs: &Docs,
    cb: &CallbackBinding,
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
            let ret = m.ret.as_ref().map_or_else(|| "void".into(), owned_cs);
            let sig: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{} {}", lent_cs(&p.ty), camel(&p.name)))
                .collect();
            w.line(format!("{ret} {}({});", method_cs(&m.name), sig.join(", ")));
        }
    });
    w.blank();
}

/// The internal class owning the vtable (the header, then function pointers
/// in declaration order) and the trampolines it points at.
fn render_vtable_class(w: &mut CodeWriter, model: &Model, cb: &CallbackBinding, cx: Cx<'_>) {
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
            w.line("vtable->Free = &FfiFree;");
            for m in &cb.methods {
                w.line(format!(
                    "vtable->{} = &{}Trampoline;",
                    safe_cs_name(&m.name),
                    method_cs(&m.name)
                ));
            }
            w.line("return (IntPtr)vtable;");
        });
        w.blank();
        w.line(UNMANAGED_CALLERS_ONLY);
        w.line("private static void FfiFree(IntPtr ctx)");
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

/// One trampoline: receive the arguments, call the implementation, and
/// hand its result back; any exception is reported through `out_err`.
fn render_trampoline(
    w: &mut CodeWriter,
    model: &Model,
    cx: Cx<'_>,
    iface: &str,
    m: &CallbackMethodBinding,
) {
    // The slots are `ctx`, one per parameter (named after it), then
    // `out_err`; the C# names are the escaped C names.
    let slot_name = |name: &str| format!("ffi_{name}");
    let sig: Vec<String> = m
        .abi
        .params
        .iter()
        .map(|s| format!("{} {}", cs_ctype(cx.ns, &s.ty), slot_name(&s.name)))
        .collect();
    let ret = cs_ctype(cx.ns, &m.abi.ret);
    let returns_value = m.ret.is_some();
    let ctx = slot_name("ctx");
    let out_err = slot_name("out_err");
    let err = ErrCtx::new(model, &m.error, cx);
    let fallback = match m.error {
        ErrorStrategy::Trap => "ForeignErrorCode",
        _ => "GenericErrorCode",
    };
    let method = method_cs(&m.name);
    w.line(UNMANAGED_CALLERS_ONLY);
    w.line(format!(
        "private static {ret} {method}Trampoline({})",
        sig.join(", ")
    ));
    w.block("{", "}", |w| {
        w.line("try");
        w.block("{", "}", |w| {
            let args: Vec<String> = m
                .params
                .iter()
                .map(|p| borrow(cx, &p.ty, &safe_cs_name(&slot_name(&p.name))))
                .collect();
            let call = format!("Ffi.Target<{iface}>({ctx}).{method}({})", args.join(", "));
            match &m.ret {
                None => w.line(format!("{call};")),
                Some(ty) => w.line(format!("return {};", give(ty, &call))),
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
