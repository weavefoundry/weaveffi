//! The generated half of `NativeMethods`: the contract checksum check and one
//! `[LibraryImport]` per lowered C symbol, matching each callable's shape.
//!
//! Every slot is blittable (`byte*`, `nuint`, `FfiError*`, function
//! pointers) except object slots, which take the interface's `SafeHandle`
//! subclass: the source-generated stub adds a reference to the handle for
//! the duration of the call, so a wrapper that's disposed or collected
//! mid-call can't free the native object under it.

use std::collections::HashMap;

use crate::codegen::CodeWriter;
use crate::utils::local_type_name;
use weaveffi_model::abi::AbiParam;
use weaveffi_model::model::{
    checksum_symbol, AbiFn, BindingModel, CallShape, FnBinding, InterfaceBinding,
};
use weaveffi_model::plan::ArgPass;

use crate::targets::dotnet::types::{cs_ctype, fn_pointer_type, safe_cs_name};

/// The C# type of an interface's `SafeHandle` subclass.
pub(crate) fn handle_type(interface: &str) -> String {
    format!("{}.NativeHandle", local_type_name(interface))
}

/// Slot types that differ from the plain [`cs_ctype`] mapping for one
/// callable: the receiver and every object parameter take a `SafeHandle`.
fn slot_overrides(f: &FnBinding, owner: Option<&str>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let (true, Some(owner)) = (f.has_self, owner) {
        map.insert("self".to_string(), handle_type(owner));
    }
    for p in &f.params {
        if let ArgPass::Object { slot, .. } = p.arg_pass() {
            let iface =
                p.ty.interface_name()
                    .expect("object parameters name an interface");
            map.insert(slot.name.clone(), handle_type(iface));
        }
    }
    map
}

/// One parameter declaration for `slot`, honoring `overrides`.
fn slot_decl(
    slot: &AbiParam,
    overrides: &HashMap<String, String>,
    callback: Option<&str>,
) -> String {
    let ty = match (overrides.get(&slot.name), callback) {
        (Some(ty), _) => ty.clone(),
        (None, Some(cb)) if slot.name == "callback" => cb.to_string(),
        _ => cs_ctype(&slot.ty),
    };
    format!("{ty} {}", safe_cs_name(&slot.name))
}

/// Emit one `[LibraryImport]` declaration.
fn import(w: &mut CodeWriter, symbol: &str, ret: &str, params: &[String]) {
    w.line(format!(
        "[LibraryImport(LibName, EntryPoint = \"{symbol}\")]"
    ));
    w.line("[UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]");
    w.line(format!(
        "internal static partial {ret} {symbol}({});",
        params.join(", ")
    ));
    w.blank();
}

/// Emit the import for one lowered function.
fn import_fn(
    w: &mut CodeWriter,
    abi: &AbiFn,
    overrides: &HashMap<String, String>,
    callback: Option<&str>,
) {
    let params: Vec<String> = abi
        .params
        .iter()
        .map(|s| slot_decl(s, overrides, callback))
        .collect();
    import(w, &abi.symbol, &cs_ctype(&abi.ret), &params);
}

/// Emit every import behind one callable: the sync symbol; the async
/// launcher (its `callback` slot typed as the completion function pointer);
/// or the iterator launcher, `_next` (taking the iterator's `SafeHandle`),
/// and `_destroy`.
fn render_callable(w: &mut CodeWriter, f: &FnBinding, owner: Option<&str>) {
    let overrides = slot_overrides(f, owner);
    match &f.shape {
        CallShape::Sync(abi) => import_fn(w, abi, &overrides, None),
        CallShape::Async(a) => {
            let cb = fn_pointer_type(&a.callback_params, &weaveffi_model::abi::CType::Void);
            import_fn(w, &a.launch, &overrides, Some(&cb));
        }
        CallShape::Iterator(it) => {
            import_fn(w, &it.launch, &overrides, None);
            let next_overrides =
                HashMap::from([("iter".to_string(), "FfiIteratorHandle".to_string())]);
            import_fn(w, &it.next, &next_overrides, None);
            import(w, &it.destroy_symbol, "void", &["IntPtr iter".to_string()]);
        }
    }
}

/// The imports behind one interface: `_clone`, `_destroy`, and every member.
fn render_interface(w: &mut CodeWriter, i: &InterfaceBinding) {
    import(
        w,
        &i.clone_symbol,
        "IntPtr",
        &[format!("{} self", handle_type(&i.name))],
    );
    import(w, &i.destroy_symbol, "void", &["IntPtr self".to_string()]);
    for f in i.constructors.iter().chain(&i.methods).chain(&i.statics) {
        render_callable(w, f, Some(&i.name));
    }
}

/// Render the generated half of `NativeMethods`: the checksum check run at
/// load, each root's checksum import, and every interface and function
/// import in declaration order.
pub(crate) fn render_native_methods(w: &mut CodeWriter, model: &BindingModel) {
    w.line("internal static unsafe partial class NativeMethods");
    w.line("{");
    w.indent();
    w.line("static partial void VerifyChecksums()");
    w.block("{", "}", |w| {
        for root in model.roots() {
            let checksum = root.checksum.expect("top-level modules carry a checksum");
            let symbol = checksum_symbol(&model.prefix, &root.name);
            w.line(format!(
                "VerifyChecksum(\"{}\", &{symbol}, 0x{checksum:016x}UL);",
                root.name
            ));
        }
    });
    w.blank();
    for root in model.roots() {
        import(w, &checksum_symbol(&model.prefix, &root.name), "ulong", &[]);
    }
    for m in &model.modules {
        for i in &m.interfaces {
            render_interface(w, i);
        }
        for f in &m.functions {
            render_callable(w, f, None);
        }
    }
    w.dedent();
    w.line("}");
    w.blank();
}
