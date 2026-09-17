//! The `NativeMethods` class: one `[DllImport]` extern (or unmanaged
//! delegate type) per lowered C symbol, matching each callable's shape
//! exactly.

use weaveffi_core::abi::{AbiParam, CType};
use weaveffi_core::cabi::ABI_VERSION;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    BindingModel, CallShape, FnBinding, InterfaceBinding, IteratorBinding, ParamBinding,
};

use crate::calls::async_result_is_ptr_len;
use crate::types::{cs_out_param, cs_pinvoke_ctype, pinvoke_type, safe_cs_name};

/// The P/Invoke spelling of one ABI slot: its C type mapped onto the
/// P/Invoke vocabulary plus the IDL slot name with keyword escaping applied.
pub(crate) fn pinvoke_slot(slot: &AbiParam) -> String {
    format!(
        "{} {}",
        cs_pinvoke_ctype(&slot.ty),
        safe_cs_name(&slot.name)
    )
}

/// The P/Invoke parameter list (one entry per precomputed ABI slot) for one
/// IR-typed parameter.
pub(crate) fn pinvoke_param_list(p: &ParamBinding) -> Vec<String> {
    p.abi.iter().map(pinvoke_slot).collect()
}

/// Whether an ABI slot is the trailing `{prefix}_error* out_err`.
pub(crate) fn is_error_slot(slot: &AbiParam) -> bool {
    matches!(&slot.ty, CType::Ptr { pointee, .. } if matches!(pointee.as_ref(), CType::Error))
}

/// Render the `NativeMethods` static class: the ABI-revision check in its
/// static constructor, the shared runtime imports (`abi_version`,
/// `error_set`, `free_string`, `free_bytes`, `error_clear`, `error_free`),
/// and then every interface and function extern in declaration order.
/// Callback interfaces declare no exported symbols; their vtables live next
/// to the consumer-facing interface type.
pub(crate) fn render_native_methods(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("internal static class NativeMethods");
    w.line("{");
    w.indent();
    w.line("private const string LibName = \"weaveffi\";");
    w.blank();
    w.line("// The ABI revision these bindings were generated against.");
    w.line(format!("internal const uint AbiVersion = {ABI_VERSION};"));
    w.blank();
    w.line("// Runs before the first P/Invoke through this class, so a producer built");
    w.line("// for a different ABI revision fails loudly instead of misreading the");
    w.line("// error struct or a value buffer later.");
    w.line("static NativeMethods()");
    w.block("{", "}", |w| {
        w.line("uint found;");
        w.line("try");
        w.block("{", "}", |w| {
            w.line("found = weaveffi_abi_version();");
        });
        w.line("catch (EntryPointNotFoundException e)");
        w.block("{", "}", |w| {
            w.line("throw new InvalidOperationException(");
            w.line("    $\"the loaded WeaveFFI library predates ABI versioning (these bindings expect ABI revision {AbiVersion})\", e);");
        });
        w.line("if (found != AbiVersion)");
        w.block("{", "}", |w| {
            w.line("throw new InvalidOperationException(");
            w.line("    $\"WeaveFFI ABI mismatch: these bindings expect revision {AbiVersion} but the loaded library reports revision {found}\");");
        });
    });
    w.blank();
    w.line("[DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]");
    w.line("internal static extern uint weaveffi_abi_version();");
    w.blank();
    w.line("// Fills a producer-owned error slot with a copy of `message`; callback");
    w.line("// trampolines report a thrown exception through it so no managed");
    w.line("// allocation is ever handed to the producer's allocator.");
    w.line("[DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]");
    w.line("internal static extern void weaveffi_error_set(IntPtr err, int code, [MarshalAs(UnmanagedType.LPUTF8Str)] string message);");
    w.blank();
    w.line("[DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]");
    w.line("internal static extern void weaveffi_free_string(IntPtr ptr);");
    w.blank();
    w.line("[DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]");
    w.line("internal static extern void weaveffi_free_bytes(IntPtr ptr, UIntPtr len);");
    w.blank();
    w.line("[DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]");
    w.line("internal static extern void weaveffi_error_clear(ref WeaveFFIError err);");
    w.blank();
    w.line("[DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]");
    w.line("internal static extern void weaveffi_error_free(IntPtr err);");
    w.blank();
    w.dedent();

    // Records, rich enums, and callback interfaces declare no C symbols, so
    // only interfaces and functions declare P/Invokes.
    for m in &model.modules {
        for i in &m.interfaces {
            let mut tmp = String::new();
            render_interface_pinvoke(&mut tmp, i);
            w.raw(tmp);
        }
        for f in &m.functions {
            let mut tmp = String::new();
            render_shaped_pinvoke(&mut tmp, f);
            w.raw(tmp);
        }
    }

    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the extern declaration set matching one callable's shape exactly:
/// sync, async (delegate + launcher), or iterator (constructor, `next`,
/// `destroy`). Shared by free functions and interface members.
pub(crate) fn render_shaped_pinvoke(out: &mut String, f: &FnBinding) {
    match &f.shape {
        CallShape::Sync(_) => render_function_pinvoke(out, f),
        CallShape::Async(_) => render_async_function_pinvoke(out, f),
        CallShape::Iterator(it) => render_iterator_pinvoke(out, it),
    }
}

/// The `[DllImport]` set backing one interface: the `clone` and `destroy`
/// reference-count symbols plus one shape-matched extern set per member.
/// Instance members carry the implicit leading `self` slot.
pub(crate) fn render_interface_pinvoke(out: &mut String, i: &InterfaceBinding) {
    let clone_sym = &i.clone_symbol;
    let destroy_sym = &i.destroy_symbol;
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{clone_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern IntPtr {clone_sym}(IntPtr self);"
    ));
    w.blank();
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{destroy_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern void {destroy_sym}(IntPtr self);"
    ));
    w.blank();
    out.push_str(&w.finish());

    for f in i
        .constructors
        .iter()
        .chain(i.methods.iter())
        .chain(i.statics.iter())
    {
        render_shaped_pinvoke(out, f);
    }
}

/// The single extern behind one synchronous callable, rendered from its
/// precomputed [`CallShape::Sync`] slots: the optional `self`, every input
/// slot, any return out-params, and the trailing error slot.
pub(crate) fn render_function_pinvoke(out: &mut String, f: &FnBinding) {
    let abi = match &f.shape {
        CallShape::Sync(abi) => abi,
        CallShape::Iterator(it) => {
            render_iterator_pinvoke(out, it);
            return;
        }
        CallShape::Async(_) => {
            render_async_function_pinvoke(out, f);
            return;
        }
    };
    let c_sym = &abi.symbol;
    let params: Vec<String> = abi.params.iter().map(slot_param).collect();
    let ret_type = cs_pinvoke_ctype(&abi.ret);

    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{c_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern {ret_type} {c_sym}({});",
        params.join(", ")
    ));
    w.blank();
    out.push_str(&w.finish());
}

/// One P/Invoke parameter for a sync or iterator ABI slot: the trailing
/// error slot becomes `ref WeaveFFIError`, `out_*` pointer slots become
/// `out` pointee values, everything else is passed by value.
pub(crate) fn slot_param(slot: &AbiParam) -> String {
    if is_error_slot(slot) {
        return format!("ref WeaveFFIError {}", slot.name);
    }
    match &slot.ty {
        CType::Ptr { .. } if slot.name.starts_with("out_") => cs_out_param(slot),
        _ => pinvoke_slot(slot),
    }
}

/// The three entry points behind one `iter<T>` function: the constructor
/// returning the opaque iterator handle, `_next`, and `_destroy`.
pub(crate) fn render_iterator_pinvoke(out: &mut String, it: &IteratorBinding) {
    let launch_params: Vec<String> = it.launch.params.iter().map(slot_param).collect();
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]",
        it.launch.symbol
    ));
    w.line(format!(
        "internal static extern IntPtr {}({});",
        it.launch.symbol,
        launch_params.join(", ")
    ));
    w.blank();

    let next_params: Vec<String> = it.next.params.iter().map(slot_param).collect();
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]",
        it.next.symbol
    ));
    w.line(format!(
        "internal static extern int {}({});",
        it.next.symbol,
        next_params.join(", ")
    ));
    w.blank();

    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]",
        it.destroy_symbol
    ));
    w.line(format!(
        "internal static extern void {}(IntPtr iter);",
        it.destroy_symbol
    ));
    w.blank();
    out.push_str(&w.finish());
}

/// The completion delegate's result parameters for one async return type:
/// nothing for void, an owned `(ptr, len)` pair for bytes and buffered
/// values, and a single by-value slot otherwise (an object result is the
/// adopted `IntPtr`).
pub(crate) fn async_cb_delegate_result_params(ret: &Option<Ty>) -> String {
    match ret {
        None => String::new(),
        Some(ty) if async_result_is_ptr_len(ty) => ", IntPtr result, UIntPtr resultLen".into(),
        Some(ty) => format!(", {} result", pinvoke_type(ty)),
    }
}

/// The unmanaged completion delegate plus the `_async` launcher extern for
/// one async callable.
pub(crate) fn render_async_function_pinvoke(out: &mut String, f: &FnBinding) {
    let c_sym = &f.c_base;
    let delegate_name = format!("AsyncCb_{c_sym}");
    let cb_params = async_cb_delegate_result_params(&f.ret);

    let mut w = CodeWriter::four_space().with_depth(2);
    w.line("[UnmanagedFunctionPointer(CallingConvention.Cdecl)]");
    w.line(format!(
        "internal delegate void {delegate_name}(IntPtr context, IntPtr err{cb_params});"
    ));
    w.blank();

    let mut params: Vec<String> = Vec::new();
    if f.has_self {
        params.push("IntPtr self".into());
    }
    params.extend(f.params.iter().flat_map(pinvoke_param_list));
    if f.cancellable {
        params.push("IntPtr cancel_token".into());
    }
    params.push(format!("{delegate_name} callback"));
    params.push("IntPtr context".into());

    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{c_sym}_async\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern void {c_sym}_async({});",
        params.join(", ")
    ));
    w.blank();
    out.push_str(&w.finish());
}
