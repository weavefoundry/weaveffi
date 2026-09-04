//! Callback-interface renderers: the consumer-facing C# `interface` plus the
//! process-wide vtable and its `[UnmanagedCallersOnly]` trampolines, honoring
//! the shared `CallbackProtocol` contract.
//!
//! * One static vtable per callback interface: a sequential struct of
//!   function pointers written once into unmanaged memory that's never
//!   freed, so its address is stable for the process lifetime.
//! * `ctx` is a `GCHandle` to the implementation, allocated when the wrapper
//!   passes it and released by the vtable's `free` trampoline, so the GC
//!   keeps the object alive exactly as long as the producer holds it.
//! * Trampoline arguments are received like returns: strings, bytes, and
//!   buffers are borrowed (copied or decoded, never freed) and objects are
//!   adopted into wrappers the implementation owns.
//! * A thrown exception never unwinds through the C frame: the trampoline
//!   reports it with `{prefix}_error_set(out_err, -4, message)` and returns a
//!   default value.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    CallbackInterfaceBinding, CallbackMethodBinding, Family, ModuleBinding, ParamBinding,
};

use crate::calls::{adopt_object, direct_from_slot, direct_to_slot, object_class, write_obsolete};
use crate::codec::emit_buffer_decode;
use crate::docs::{writer_doc, writer_fn_doc};
use crate::pinvoke::pinvoke_slot;
use crate::types::{
    callback_interface_cs, cs_pinvoke_ctype, cs_type, safe_cs_name, vtable_class_cs,
};

/// Render one callback interface: the `public interface I{Name}` the consumer
/// implements, followed by the internal static class hosting the vtable and
/// trampolines that adapt an implementation to the C ABI.
pub(crate) fn render_callback_interface(
    out: &mut String,
    module: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
) {
    render_consumer_interface(out, cb);
    render_vtable_class(out, module, cb);
}

/// The C# method name of one callback method.
fn method_cs(m: &CallbackMethodBinding) -> String {
    m.name.to_upper_camel_case()
}

/// The consumer-facing `interface`: one method per callback method with
/// idiomatic C# types (objects as wrapper classes the implementation owns,
/// `Interface?` as a nullable wrapper).
fn render_consumer_interface(out: &mut String, cb: &CallbackInterfaceBinding) {
    let iface = callback_interface_cs(&cb.name);
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &cb.doc);
    w.line("/// <remarks>Implement this interface and pass an instance wherever the");
    w.line("/// native library expects it. The library may call any method from any");
    w.line("/// thread until it releases its last reference to the instance. Object");
    w.line("/// arguments are owned by the implementation, which should dispose them");
    w.line("/// when done. An exception thrown by a method is reported to the native");
    w.line("/// caller as <see cref=\"WeaveFFIException.ForeignErrorCode\"/>.</remarks>");
    write_obsolete(&mut w, &cb.deprecated);
    w.line(format!("public interface {iface}"));
    w.block("{", "}", |w| {
        for m in &cb.methods {
            let params: Vec<ParamBinding> = m
                .params
                .iter()
                .map(|p| {
                    let mut p = p.clone();
                    p.name = p.name.to_lower_camel_case();
                    p
                })
                .collect();
            writer_fn_doc(w, &m.doc, &params);
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
    out.push_str(&w.finish());
}

/// The `delegate* unmanaged[Cdecl]<...>` function-pointer type matching one
/// trampoline's C signature: every slot's P/Invoke type, then the return.
fn function_pointer_type(m: &CallbackMethodBinding) -> String {
    let mut parts: Vec<String> = m
        .abi_params
        .iter()
        .map(|slot| cs_pinvoke_ctype(&slot.ty))
        .collect();
    parts.push(cs_pinvoke_ctype(&m.abi_ret));
    format!("delegate* unmanaged[Cdecl]<{}>", parts.join(", "))
}

/// The internal static class owning the vtable: a sequential struct of
/// function pointers (one per method in declaration order, then `free`),
/// allocated once in unmanaged memory, plus the trampolines it points at.
fn render_vtable_class(out: &mut String, module: &ModuleBinding, cb: &CallbackInterfaceBinding) {
    let iface = callback_interface_cs(&cb.name);
    let class = vtable_class_cs(&module.path, &cb.name);
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line(format!(
        "/// <summary>The process-wide <c>{}</c> vtable and the trampolines behind",
        cb.vtable_tag
    ));
    w.line(format!(
        "/// it, adapting an <see cref=\"{iface}\"/> to the C ABI.</summary>"
    ));
    w.line(format!("internal static unsafe class {class}"));
    w.line("{");
    w.indent();

    // The C struct: one function pointer per method in declaration order,
    // then the trailing `free`.
    w.line("[StructLayout(LayoutKind.Sequential)]");
    w.line("private struct Layout");
    w.block("{", "}", |w| {
        for m in &cb.methods {
            w.line(format!("public IntPtr {};", safe_cs_name(&m.name)));
        }
        w.line("public IntPtr free;");
    });
    w.blank();

    w.line("/// <summary>Address of the one static vtable for this interface. It is");
    w.line("/// allocated once and never freed, so the producer may keep the pointer");
    w.line("/// for the process lifetime.</summary>");
    w.line("internal static readonly IntPtr Pointer = Allocate();");
    w.blank();

    w.line("private static IntPtr Allocate()");
    w.block("{", "}", |w| {
        w.line("var layout = new Layout");
        w.line("{");
        w.scope(|w| {
            for m in &cb.methods {
                w.line(format!(
                    "{} = (IntPtr)({})&{}Trampoline,",
                    safe_cs_name(&m.name),
                    function_pointer_type(m),
                    method_cs(m)
                ));
            }
            w.line("free = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, void>)&FreeTrampoline,");
        });
        w.line("};");
        w.line("var mem = Marshal.AllocHGlobal(Marshal.SizeOf<Layout>());");
        w.line("Marshal.StructureToPtr(layout, mem, false);");
        w.line("return mem;");
    });
    w.blank();

    w.line(format!("private static {iface} Target(IntPtr ctx)"));
    w.block("{", "}", |w| {
        w.line(format!("return ({iface})GCHandle.FromIntPtr(ctx).Target!;"));
    });
    w.blank();

    for m in &cb.methods {
        render_trampoline(&mut w, m);
    }

    // Fires exactly once, when the producer drops its last reference; the
    // GCHandle it frees is the one the wrapper allocated when passing the
    // implementation.
    w.line("[UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]");
    w.line("private static void FreeTrampoline(IntPtr ctx)");
    w.block("{", "}", |w| {
        w.line("GCHandle.FromIntPtr(ctx).Free();");
    });

    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// One `[UnmanagedCallersOnly]` trampoline: decode the borrowed slots, adopt
/// object slots, call the implementation, and write the direct return; any
/// exception is reported through `out_err` with the foreign error code and a
/// default value is returned instead of unwinding.
fn render_trampoline(w: &mut CodeWriter, m: &CallbackMethodBinding) {
    let ret_cs = cs_pinvoke_ctype(&m.abi_ret);
    let sig: Vec<String> = m.abi_params.iter().map(pinvoke_slot).collect();
    let out_err = safe_cs_name(
        &m.abi_params
            .last()
            .expect("callback methods always end in out_err")
            .name,
    );
    let ctx = safe_cs_name(&m.abi_params[0].name);

    w.line("[UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]");
    w.line(format!(
        "private static {ret_cs} {}Trampoline({})",
        method_cs(m),
        sig.join(", ")
    ));
    w.block("{", "}", |w| {
        w.line("try");
        w.block("{", "}", |w| {
            w.line(format!("var impl = Target({ctx});"));
            let args: Vec<String> = m
                .params
                .iter()
                .enumerate()
                .map(|(idx, p)| render_trampoline_arg(w, p, idx))
                .collect();
            let call = format!("impl.{}({})", method_cs(m), args.join(", "));
            match &m.ret {
                None => w.line(format!("{call};")),
                Some(ty) => w.line(format!("return {};", direct_to_slot(ty, &call))),
            };
        });
        w.line("catch (Exception ex)");
        w.block("{", "}", |w| {
            w.line(format!(
                "NativeMethods.weaveffi_error_set({out_err}, WeaveFFIException.ForeignErrorCode, ex.Message);"
            ));
            if m.ret.is_some() {
                w.line("return default;");
            }
        });
    });
    w.blank();
}

/// Statements (written to `w`) plus the expression converting one callback
/// parameter's slots into the value handed to the implementation, receiving
/// it per its [`Family`] like a return of the same type: strings, bytes, and
/// buffers are borrowed for the dispatch and copied (never freed); objects
/// transfer one strong reference and are adopted into a wrapper.
fn render_trampoline_arg(w: &mut CodeWriter, p: &ParamBinding, idx: usize) -> String {
    let n0 = safe_cs_name(&p.abi[0].name);
    let arg = format!("arg{idx}");
    match p.ty.family() {
        Family::Buffer => {
            let len = safe_cs_name(&p.abi[1].name);
            w.line(format!("var {arg}Buf = new byte[(int){len}];"));
            w.line(format!(
                "if ({n0} != IntPtr.Zero && (int){len} > 0) Marshal.Copy({n0}, {arg}Buf, 0, (int){len});"
            ));
            emit_buffer_decode(w, &p.ty, &arg, &format!("{arg}Buf"));
            arg
        }
        Family::String => format!("Marshal.PtrToStringUTF8({n0}) ?? \"\""),
        Family::Bytes => {
            let len = safe_cs_name(&p.abi[1].name);
            w.line(format!("var {arg} = new byte[(int){len}];"));
            w.line(format!(
                "if ({n0} != IntPtr.Zero && (int){len} > 0) Marshal.Copy({n0}, {arg}, 0, (int){len});"
            ));
            arg
        }
        Family::Object { nullable } => {
            let adopt = adopt_object(object_class(&p.ty), &n0);
            if nullable {
                format!("{n0} == IntPtr.Zero ? null : {adopt}")
            } else {
                adopt
            }
        }
        Family::Direct => direct_from_slot(&p.ty, &n0),
        Family::Callback | Family::Iterator => {
            unreachable!("{} is not a valid callback method parameter", p.ty)
        }
    }
}
