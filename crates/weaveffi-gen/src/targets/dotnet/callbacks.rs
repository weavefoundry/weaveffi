//! Callback-interface renderers: the consumer-facing C# `interface` plus the
//! process-wide vtable and its `[UnmanagedCallersOnly]` trampolines,
//! following the shared `CallbackProtocol`.
//!
//! * One static vtable per callback interface, allocated once in native
//!   memory and never freed, so its address is stable for the process.
//! * `ctx` is a `GCHandle` to the implementation, released by the vtable's
//!   `free` entry, so the implementation lives exactly as long as the
//!   producer holds it.
//! * Trampoline arguments are received like returns, except that strings,
//!   bytes, and buffers are borrowed (copied, never freed); objects are
//!   adopted into wrappers the implementation owns.
//! * A thrown exception never unwinds into native code: the trampoline
//!   reports it through `out_err` with the foreign error code and returns a
//!   default value. Trampolines run on whatever thread the producer calls
//!   from.

use crate::codegen::CodeWriter;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, ModuleBinding};

use crate::targets::dotnet::calls::{
    direct_to_slot, receive, write_obsolete, Own, UNMANAGED_CALLERS_ONLY,
};
use crate::targets::dotnet::docs::{write_doc, write_fn_doc};
use crate::targets::dotnet::types::{
    callback_interface_cs, cs_ctype, cs_type, fn_pointer_type, safe_cs_name, vtable_class_cs, Cx,
};

/// Render one callback interface: the `public interface I{Name}` the
/// consumer implements, then the internal class hosting its vtable.
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    module: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
    cx: Cx<'_>,
) {
    render_consumer_interface(w, cb, cx.base);
    render_vtable_class(w, cx, module, cb);
}

fn method_cs(m: &CallbackMethodBinding) -> String {
    m.name.to_upper_camel_case()
}

/// The consumer-facing interface: one method per callback method.
fn render_consumer_interface(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, base: &str) {
    let iface = callback_interface_cs(&cb.name);
    write_doc(w, &cb.doc);
    w.line("/// <remarks>The native library may call any method from any thread until");
    w.line("/// it releases its last reference to the implementation. Object arguments");
    w.line("/// belong to the implementation. An exception thrown by a method reaches");
    w.line(format!(
        "/// the native caller as <see cref=\"{base}.ForeignErrorCode\"/>.</remarks>"
    ));
    write_obsolete(w, &cb.deprecated);
    w.line(format!("public interface {iface}"));
    w.block("{", "}", |w| {
        for m in &cb.methods {
            let mut params = m.params.clone();
            for p in &mut params {
                p.name = p.name.to_lower_camel_case();
            }
            write_fn_doc(w, &m.doc, &params);
            write_obsolete(w, &m.deprecated);
            let ret = m.ret.as_ref().map(cs_type).unwrap_or_else(|| "void".into());
            let sig: Vec<String> = params
                .iter()
                .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
                .collect();
            w.line(format!("{ret} {}({});", method_cs(m), sig.join(", ")));
        }
    });
    w.blank();
}

/// The internal class owning the vtable (function pointers in declaration
/// order, then `free`) and the trampolines it points at.
fn render_vtable_class(
    w: &mut CodeWriter,
    cx: Cx<'_>,
    module: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
) {
    let iface = callback_interface_cs(&cb.name);
    let class = vtable_class_cs(&module.path, &cb.name);
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
            for m in &cb.methods {
                w.line(format!(
                    "public {} {};",
                    fn_pointer_type(&m.abi_params, &m.abi_ret),
                    safe_cs_name(&m.name)
                ));
            }
            w.line("public delegate* unmanaged[Cdecl]<IntPtr, void> free;");
        });
        w.blank();
        w.line("/// <summary>The vtable's address, valid for the process lifetime.</summary>");
        w.line("internal static readonly IntPtr Pointer = Allocate();");
        w.blank();
        w.line("private static IntPtr Allocate()");
        w.block("{", "}", |w| {
            w.line("var vtable = (Layout*)NativeMemory.Alloc((nuint)sizeof(Layout));");
            for m in &cb.methods {
                w.line(format!(
                    "vtable->{} = &{}Trampoline;",
                    safe_cs_name(&m.name),
                    method_cs(m)
                ));
            }
            w.line("vtable->free = &FreeTrampoline;");
            w.line("return (IntPtr)vtable;");
        });
        w.blank();
        for m in &cb.methods {
            render_trampoline(w, cx, &iface, m);
        }
        w.line(UNMANAGED_CALLERS_ONLY);
        w.line("private static void FreeTrampoline(IntPtr ctx)");
        w.block("{", "}", |w| {
            w.line("Ffi.Unregister(ctx);");
        });
    });
    w.blank();
}

/// One trampoline: receive the slots, call the implementation, and return
/// the direct result; any exception is reported through `out_err`.
fn render_trampoline(w: &mut CodeWriter, cx: Cx<'_>, iface: &str, m: &CallbackMethodBinding) {
    let sig: Vec<String> = m
        .abi_params
        .iter()
        .map(|s| format!("{} {}", cs_ctype(&s.ty), safe_cs_name(&s.name)))
        .collect();
    let ret = cs_ctype(&m.abi_ret);
    w.line(UNMANAGED_CALLERS_ONLY);
    w.line(format!(
        "private static {ret} {}Trampoline({})",
        method_cs(m),
        sig.join(", ")
    ));
    w.block("{", "}", |w| {
        w.line("try");
        w.block("{", "}", |w| {
            let mut args = Vec::new();
            for (idx, p) in m.params.iter().enumerate() {
                let ptr = safe_cs_name(&p.abi[0].name);
                let len = p
                    .abi
                    .get(1)
                    .map(|s| safe_cs_name(&s.name))
                    .unwrap_or_default();
                args.push(receive(
                    w,
                    cx,
                    &p.ty,
                    &ptr,
                    &len,
                    Own::Borrow,
                    &format!("arg{idx}"),
                ));
            }
            let call = format!(
                "Ffi.Target<{iface}>(ctx).{}({})",
                method_cs(m),
                args.join(", ")
            );
            match &m.ret {
                None => w.line(format!("{call};")),
                Some(ty) => w.line(format!("return {};", direct_to_slot(ty, &call))),
            };
        });
        w.line("catch (Exception e)");
        w.block("{", "}", |w| {
            w.line("Ffi.SetForeignError(out_err, e);");
            if m.ret.is_some() {
                w.line("return default;");
            }
        });
    });
    w.blank();
}
