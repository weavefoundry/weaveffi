//! The generated half of `NativeMethods`: the contract tables checked at
//! load and one `[LibraryImport]` per lowered C symbol.
//!
//! Every slot is blittable (`byte*`, `nuint`, `FfiError*`, enums, function
//! pointers; `bool` is marshalled as one byte) except object slots, which
//! take the interface's `SafeHandle` subclass: the source-generated stub
//! adds a reference to the handle for the duration of the call, so a
//! wrapper that's disposed or collected mid-call can't free the native
//! object under it.

use std::collections::HashMap;

use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{AbiFn, FnBinding, InterfaceBinding, Model};
use weaveffi_model::plan::ArgPass;

use crate::codegen::contract::{hex, tables};
use crate::codegen::CodeWriter;
use crate::targets::dotnet::types::{cs_ctype, cs_str, fn_pointer_type, safe_cs_name};

/// The C# type of an interface's `SafeHandle` subclass.
fn handle_type(interface: &str) -> String {
    format!("{interface}.NativeHandle")
}

/// Slot types that differ from the plain [`cs_ctype`] mapping for one
/// callable: the receiver and every object parameter take a `SafeHandle`,
/// and an async launcher's `callback` takes the completion's function
/// pointer type.
fn slot_overrides(ns: &str, f: &FnBinding, owner: Option<&str>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let (Some(slot), Some(owner)) = (&f.receiver, owner) {
        map.insert(slot.name.clone(), handle_type(owner));
    }
    for p in &f.params {
        if let ArgPass::Object {
            slot, interface, ..
        } = &p.pass
        {
            map.insert(slot.name.clone(), handle_type(interface));
        }
    }
    if let Some(a) = f.async_binding() {
        for slot in &f.abi.params {
            // The completion callback is the launcher's one by-value named
            // type (iterator and object handles are pointers).
            if matches!(slot.ty, CType::Named(_)) {
                map.insert(
                    slot.name.clone(),
                    fn_pointer_type(ns, &a.callback_params, &CType::Void),
                );
            }
        }
    }
    map
}

/// One parameter declaration for `slot`, honoring `overrides`. A `bool`
/// crosses as one byte.
fn slot_decl(ns: &str, slot: &AbiParam, overrides: &HashMap<String, String>) -> String {
    let name = safe_cs_name(&slot.name);
    match overrides.get(&slot.name) {
        Some(ty) => format!("{ty} {name}"),
        None if slot.ty == CType::Bool => format!("[MarshalAs(UnmanagedType.U1)] bool {name}"),
        None => format!("{} {name}", cs_ctype(ns, &slot.ty)),
    }
}

/// Emit one `[LibraryImport]` declaration.
fn import(w: &mut CodeWriter, symbol: &str, ret: &CType, ns: &str, params: &[String]) {
    w.line(format!(
        "[LibraryImport(LibName, EntryPoint = \"{symbol}\")]"
    ));
    w.line("[UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]");
    if *ret == CType::Bool {
        w.line("[return: MarshalAs(UnmanagedType.U1)]");
    }
    w.line(format!(
        "internal static partial {} {symbol}({});",
        cs_ctype(ns, ret),
        params.join(", ")
    ));
    w.blank();
}

/// Emit the import for one lowered function.
fn import_fn(w: &mut CodeWriter, ns: &str, abi: &AbiFn, overrides: &HashMap<String, String>) {
    let params: Vec<String> = abi
        .params
        .iter()
        .map(|s| slot_decl(ns, s, overrides))
        .collect();
    import(w, &abi.symbol, &abi.ret, ns, &params);
}

/// Emit every import behind one callable: its entry point (the sync
/// symbol, async launcher, or iterator launcher) and, for an iterator,
/// `_next` (taking the iterator's `SafeHandle`) and `_destroy`.
fn render_callable(w: &mut CodeWriter, ns: &str, f: &FnBinding, owner: Option<&str>) {
    import_fn(w, ns, &f.abi, &slot_overrides(ns, f, owner));
    if let Some(it) = f.iterator() {
        let iter = &it.next.params[0].name;
        let next_overrides = HashMap::from([(iter.clone(), "FfiIteratorHandle".to_string())]);
        import_fn(w, ns, &it.next, &next_overrides);
        import(
            w,
            &it.destroy_symbol,
            &CType::Void,
            ns,
            &["IntPtr iter".to_string()],
        );
    }
}

/// The imports behind one interface: `_clone`, `_destroy`, and every member.
fn render_interface(w: &mut CodeWriter, ns: &str, i: &InterfaceBinding) {
    w.line(format!(
        "[LibraryImport(LibName, EntryPoint = \"{}\")]",
        i.clone_symbol
    ));
    w.line("[UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]");
    w.line(format!(
        "internal static partial IntPtr {}({} self);",
        i.clone_symbol,
        handle_type(&i.name)
    ));
    w.blank();
    import(
        w,
        &i.destroy_symbol,
        &CType::Void,
        ns,
        &["IntPtr self".to_string()],
    );
    for f in i.members() {
        render_callable(w, ns, f, Some(&i.name));
    }
}

/// Render the generated half of `NativeMethods`: the contract check run at
/// load (every top-level module's table must carry each declaration these
/// bindings were generated with, unchanged) and every interface and
/// function import in declaration order.
pub(crate) fn render_native_methods(w: &mut CodeWriter, model: &Model, ns: &str) {
    w.line("internal static unsafe partial class NativeMethods");
    w.line("{");
    w.indent();
    w.line("static partial void VerifyContracts(IntPtr library)");
    w.block("{", "}", |w| {
        for table in tables(model) {
            w.line(format!(
                "VerifyContract(library, \"{}\", new (ulong, ulong, string)[]",
                table.symbol
            ));
            w.line("{");
            w.indent();
            for row in &table.rows {
                w.line(format!(
                    "({}UL, {}UL, \"{}\"), // {}",
                    hex(row.id),
                    hex(row.hash),
                    cs_str(&row.path),
                    row.signature
                ));
            }
            w.dedent();
            w.line("});");
        }
    });
    w.blank();
    for m in &model.modules {
        for i in &m.interfaces {
            render_interface(w, ns, i);
        }
        for f in &m.functions {
            render_callable(w, ns, f, None);
        }
    }
    w.dedent();
    w.line("}");
    w.blank();
}
