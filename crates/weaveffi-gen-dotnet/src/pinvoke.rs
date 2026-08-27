//! The `NativeMethods` class: one `[DllImport]` extern (or unmanaged
//! delegate type) per lowered C symbol, matching each callable's shape
//! exactly.

use weaveffi_core::abi::{self, AbiParam, CType};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, FnBinding, InterfaceBinding, IteratorBinding,
    ListenerBinding, ParamBinding,
};
use weaveffi_ir::ir::TypeRef;

use crate::calls::async_result_is_ptr_len;
use crate::types::{cs_out_param, cs_pinvoke_ctype, pinvoke_type, safe_cs_name};

/// The P/Invoke parameter list (one entry per ABI slot) for one IR-typed
/// parameter, keeping the IDL slot names with keyword escaping applied.
pub(crate) fn pinvoke_param_list(p: &ParamBinding) -> Vec<String> {
    abi::lower_param(&p.name, &p.ty, "", false)
        .iter()
        .map(|slot| {
            format!(
                "{} {}",
                cs_pinvoke_ctype(&slot.ty),
                safe_cs_name(&slot.name)
            )
        })
        .collect()
}

/// The extern return type plus any trailing out-params (for example the
/// `size_t* out_len` slot of a buffered return) for one IR return type.
pub(crate) fn pinvoke_return_info(ty: &TypeRef) -> (String, Vec<String>) {
    let r = abi::lower_return(ty, "");
    (
        cs_pinvoke_ctype(&r.ret),
        r.out_params.iter().map(cs_out_param).collect(),
    )
}

/// Whether an ABI slot is the trailing `{prefix}_error* out_err`.
pub(crate) fn is_error_slot(slot: &AbiParam) -> bool {
    matches!(&slot.ty, CType::Ptr { pointee, .. } if matches!(pointee.as_ref(), CType::Error))
}

/// Render the `NativeMethods` static class: the shared runtime imports
/// (`free_string`, `free_bytes`, `error_clear`) followed by every interface,
/// callback, listener, and function extern in declaration order.
pub(crate) fn render_native_methods(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("internal static class NativeMethods");
    w.line("{");
    w.indent();
    w.line("private const string LibName = \"weaveffi\";");
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
    w.dedent();

    // Records and rich enums are value types with no C symbols, so only
    // interfaces, callbacks, listeners, and functions declare P/Invokes.
    for m in &model.modules {
        for i in &m.interfaces {
            let mut tmp = String::new();
            render_interface_pinvoke(&mut tmp, i);
            w.raw(tmp);
        }
        for cb in &m.callbacks {
            let mut tmp = String::new();
            render_callback_pinvoke(&mut tmp, cb);
            w.raw(tmp);
        }
        for l in &m.listeners {
            let mut tmp = String::new();
            render_listener_pinvoke(&mut tmp, l);
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

/// The unmanaged delegate type for one module callback declaration, shared by
/// every listener that fires it.
pub(crate) fn render_callback_pinvoke(out: &mut String, cb: &CallbackBinding) {
    let delegate_name = format!("Cb_{}", cb.c_fn_type);
    let params: Vec<String> = cb
        .abi_params
        .iter()
        .map(|slot| {
            format!(
                "{} {}",
                cs_pinvoke_ctype(&slot.ty),
                safe_cs_name(&slot.name)
            )
        })
        .collect();
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line("[UnmanagedFunctionPointer(CallingConvention.Cdecl)]");
    w.line(format!(
        "internal delegate void {delegate_name}({});",
        params.join(", ")
    ));
    w.blank();
    out.push_str(&w.finish());
}

/// The register and unregister externs for one listener.
pub(crate) fn render_listener_pinvoke(out: &mut String, l: &ListenerBinding) {
    let delegate_name = format!("Cb_{}", l.callback_c_fn_type);
    let register_sym = &l.register_symbol;
    let unregister_sym = &l.unregister_symbol;

    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{register_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern ulong {register_sym}({delegate_name} callback, IntPtr context);"
    ));
    w.blank();

    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{unregister_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern void {unregister_sym}(ulong id);"
    ));
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

/// The `[DllImport]` set backing one interface: the destroy symbol plus one
/// shape-matched extern set per member. Instance members carry the implicit
/// leading `self` slot.
pub(crate) fn render_interface_pinvoke(out: &mut String, i: &InterfaceBinding) {
    let destroy_sym = &i.destroy_symbol;
    let mut w = CodeWriter::four_space().with_depth(2);
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

/// The single extern behind one synchronous callable: lowered parameter
/// slots, any return out-params, and the trailing error slot.
pub(crate) fn render_function_pinvoke(out: &mut String, f: &FnBinding) {
    if let CallShape::Iterator(it) = &f.shape {
        render_iterator_pinvoke(out, it);
        return;
    }
    let c_sym = &f.c_base;

    let mut params: Vec<String> = Vec::new();
    if f.has_self {
        params.push("IntPtr self".into());
    }
    params.extend(f.params.iter().flat_map(pinvoke_param_list));

    let ret_type = if let Some(ret) = &f.ret {
        let (ret_cs, extra) = pinvoke_return_info(ret);
        params.extend(extra);
        ret_cs
    } else {
        "void".into()
    };

    params.push("ref WeaveFFIError err".into());

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

/// One P/Invoke parameter for an iterator-shape ABI slot: the trailing error
/// slot becomes `ref WeaveFFIError`, `out_*` pointer slots become `out`
/// pointee values, everything else is passed by value.
pub(crate) fn iterator_slot_param(slot: &AbiParam) -> String {
    if is_error_slot(slot) {
        return format!("ref WeaveFFIError {}", slot.name);
    }
    match &slot.ty {
        CType::Ptr { .. } if slot.name.starts_with("out_") => cs_out_param(slot),
        ty => format!("{} {}", cs_pinvoke_ctype(ty), safe_cs_name(&slot.name)),
    }
}

/// The three entry points behind one `iter<T>` function: the constructor
/// returning the opaque iterator handle, `_next`, and `_destroy`.
pub(crate) fn render_iterator_pinvoke(out: &mut String, it: &IteratorBinding) {
    let launch_params: Vec<String> = it.launch.params.iter().map(iterator_slot_param).collect();
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

    let next_params: Vec<String> = it.next.params.iter().map(iterator_slot_param).collect();
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
/// nothing for void, a borrowed `(ptr, len)` pair for bytes and buffered
/// values, and a single by-value slot otherwise.
pub(crate) fn async_cb_delegate_result_params(ret: &Option<TypeRef>) -> String {
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
