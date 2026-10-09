//! Callback-interface renderers: the consumer-facing C# `interface` plus the
//! process-wide vtable and its `[UnmanagedCallersOnly]` trampolines,
//! rendering every clause of [`CallbackProtocol`].
//!
//! * One static vtable per callback interface, allocated once in native
//!   memory and never freed, so its address is stable for the process. It
//!   starts with the `{size, flags, free}` header, `size` being the struct's
//!   real size as C# lays it out.
//! * `ctx` is a `GCHandle` to the implementation, released by the vtable's
//!   `free` entry, so the implementation lives exactly as long as the
//!   producer holds it.
//! * Trampoline arguments are received like returns, except that strings,
//!   bytes, and buffers are borrowed (copied, never freed); objects are
//!   adopted into wrappers the implementation owns.
//! * Returns cross per [`RetPass`]: a direct value by value, an object as a
//!   fresh strong reference (`CloneHandle()`), and a string, bytes, or
//!   buffer as a `{prefix}_alloc` run written to the out slots.
//! * A thrown exception never unwinds into native code: a method declared
//!   `throws` reports its module's domain exception with its code and
//!   payload, and any other exception is reported with the foreign error
//!   code (`-4`). Trampolines run on whatever thread the producer calls
//!   from.

use crate::codegen::CodeWriter;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, ErrorBinding};
use weaveffi_model::plan::{CallbackProtocol, ErrorStrategy, RetPass};

use crate::targets::dotnet::calls::{
    direct_to_slot, receive, write_obsolete, Own, UNMANAGED_CALLERS_ONLY,
};
use crate::targets::dotnet::codec::write_stmt;
use crate::targets::dotnet::docs::{write_doc, write_fn_doc};
use crate::targets::dotnet::runtime::dotnet_exception_name;
use crate::targets::dotnet::types::{
    callback_interface_cs, cs_ctype, cs_type, fn_pointer_type, safe_cs_name, vtable_class_cs, Cx,
};

/// Render one callback interface declared by the module at `module_path`:
/// the `public interface I{Name}` the consumer implements, then the
/// internal class hosting its vtable. `domain` is the error domain in scope
/// for the module, which methods declared `throws` report.
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    module_path: &str,
    cb: &CallbackInterfaceBinding,
    domain: Option<&ErrorBinding>,
    cx: Cx<'_>,
) {
    let domain = domain.map(dotnet_exception_name);
    render_consumer_interface(w, cb, domain.as_deref(), cx);
    render_vtable_class(w, cx, module_path, cb, domain.as_deref());
}

fn method_cs(m: &CallbackMethodBinding) -> String {
    m.name.to_upper_camel_case()
}

/// The consumer-facing interface: one method per callback method.
fn render_consumer_interface(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    domain: Option<&str>,
    cx: Cx<'_>,
) {
    let iface = callback_interface_cs(&cb.name);
    write_doc(w, &cb.doc);
    w.line("/// <remarks>The native library may call any method from any thread until");
    w.line("/// it releases its last reference to the implementation. Object arguments");
    w.line("/// belong to the implementation. An exception thrown by a method fails the");
    w.line("/// native library's call in progress: a method documented to throw a domain");
    w.line("/// exception reports it with its code and fields, and any other exception");
    w.line(format!(
        "/// reaches the library as <see cref=\"{}.ForeignErrorCode\"/>.</remarks>",
        cx.base
    ));
    write_obsolete(w, &cb.deprecated);
    w.line(format!("public interface {iface}"));
    w.block("{", "}", |w| {
        for (i, m) in cb.methods.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            let mut params = m.params.clone();
            for p in &mut params {
                p.name = p.name.to_lower_camel_case();
            }
            write_fn_doc(w, &m.doc, &params);
            if let (true, Some(exc)) = (m.throws, domain) {
                w.line(format!(
                    "/// <exception cref=\"{exc}\">Reported to the library with its code and fields.</exception>"
                ));
            }
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

/// The internal class owning the vtable (the header, then function pointers
/// in declaration order) and the trampolines it points at.
fn render_vtable_class(
    w: &mut CodeWriter,
    cx: Cx<'_>,
    module_path: &str,
    cb: &CallbackInterfaceBinding,
    domain: Option<&str>,
) {
    let protocol = cb.protocol();
    let iface = callback_interface_cs(&cb.name);
    let class = vtable_class_cs(module_path, &cb.name);
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
                    fn_pointer_type(&m.abi_params, &m.abi_ret),
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
        for (i, m) in cb.methods.iter().enumerate() {
            w.blank();
            let reports = match protocol.method_errors[i] {
                ErrorStrategy::Throws => domain,
                ErrorStrategy::Trap => None,
            };
            render_trampoline(w, cx, &iface, m, &protocol, i, reports);
        }
    });
    w.blank();
}

/// The name of the slot at `from_end` places before the end of `m`'s slot
/// list (`1` is `out_err`).
fn slot_from_end(m: &CallbackMethodBinding, from_end: usize) -> String {
    safe_cs_name(&m.abi_params[m.abi_params.len() - from_end].name)
}

/// One trampoline: receive the slots, call the implementation, and hand
/// its result back; any exception is reported through `out_err`. `domain`
/// is the exception class a `throws` method reports with its code and
/// payload.
fn render_trampoline(
    w: &mut CodeWriter,
    cx: Cx<'_>,
    iface: &str,
    m: &CallbackMethodBinding,
    protocol: &CallbackProtocol<'_>,
    index: usize,
    domain: Option<&str>,
) {
    let sig: Vec<String> = m
        .abi_params
        .iter()
        .map(|s| format!("{} {}", cs_ctype(&s.ty), safe_cs_name(&s.name)))
        .collect();
    let ret = cs_ctype(&m.abi_ret);
    let returns_value = ret != "void";
    let ctx = safe_cs_name(&m.abi_params[0].name);
    let out_err = slot_from_end(m, 1);
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
                .map(|p| {
                    let ptr = safe_cs_name(&p.abi[0].name);
                    let len = p
                        .abi
                        .get(1)
                        .map(|s| safe_cs_name(&s.name))
                        .unwrap_or_default();
                    receive(cx, &p.ty, &ptr, &len, Own::Borrow)
                })
                .collect();
            let call = format!(
                "Ffi.Target<{iface}>({ctx}).{}({})",
                method_cs(m),
                args.join(", ")
            );
            match (&protocol.method_returns[index], &m.ret) {
                (RetPass::Void, _) | (_, None) => w.line(format!("{call};")),
                (RetPass::Direct, Some(ty)) => {
                    w.line(format!("return {};", direct_to_slot(ty, &call)))
                }
                // One strong reference the producer adopts. A null for a
                // required object passes through as null, which the
                // producer rejects as a marshalling failure (-3).
                (RetPass::Object { .. }, _) => {
                    w.line(format!("return {call}?.CloneHandle() ?? IntPtr.Zero;"))
                }
                (rp, Some(ty)) => {
                    let (out_ptr, out_len) = (slot_from_end(m, 3), slot_from_end(m, 2));
                    match rp {
                        RetPass::String => {
                            w.line(format!("Ffi.ReturnString({call}, {out_ptr}, {out_len});"))
                        }
                        RetPass::Bytes => {
                            w.line(format!("Ffi.ReturnBytes({call}, {out_ptr}, {out_len});"))
                        }
                        _ => {
                            w.line(format!("var ffiResult = {call};"));
                            w.line("var ffiWriter = new FfiBufferWriter();");
                            w.line(write_stmt(ty, "ffiWriter", "ffiResult"));
                            w.line(format!(
                                "Ffi.ReturnBuffer(ffiWriter, {out_ptr}, {out_len});"
                            ))
                        }
                    }
                }
            };
        });
        if let Some(exc) = domain {
            w.line(format!("catch ({} e)", cx.ty(exc)));
            w.block("{", "}", |w| {
                w.line(format!(
                    "Ffi.SetDomainError({out_err}, e.Code, e, e.WritePayload);"
                ));
                if returns_value {
                    w.line("return default;");
                }
            });
        }
        w.line("catch (Exception e)");
        w.block("{", "}", |w| {
            w.line(format!("Ffi.SetForeignError({out_err}, e);"));
            if returns_value {
                w.line("return default;");
            }
        });
    });
}
