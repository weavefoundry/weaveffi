//! The generated half of `NativeMethods`: the contract rows checked at load
//! and one `[LibraryImport]` per C symbol the API declares.
//!
//! Every slot is blittable (`FfiRun`, `FfiOpt*`, `FfiCallback`, scalars,
//! `byte` for a C `bool`, `FfiError*`, enums, function pointers) except
//! object slots, which take the interface's `SafeHandle` subclass: the
//! source-generated stub adds a reference to the handle for the duration of
//! the call, so a wrapper that's disposed or collected mid-call can't free
//! the native object under it.

use weaveffi_model::abi::{AbiFn, AbiParam, CType};
use weaveffi_model::model::{FnBinding, InterfaceBinding, Model};

use crate::codegen::contract::{hex, rows};
use crate::codegen::CodeWriter;
use crate::targets::dotnet::types::{cs_ctype, cs_str, fn_pointer_type, safe_cs_name};

/// The C# type of an interface's `SafeHandle` subclass.
fn handle_type(ns: &str, interface: &str) -> String {
    format!("global::{ns}.{interface}.NativeHandle")
}

/// One parameter declaration for `slot`. A borrowed object (the receiver,
/// or an object parameter) takes its `SafeHandle`, and an async launcher's
/// completion takes its function-pointer type.
fn slot_decl(ns: &str, slot: &AbiParam, f: Option<&FnBinding>) -> String {
    let name = safe_cs_name(&slot.name);
    let ty = match &slot.ty {
        CType::Ptr {
            konst: true,
            pointee,
        } => match pointee.as_ref() {
            CType::Object(interface) => handle_type(ns, interface),
            _ => cs_ctype(ns, &slot.ty),
        },
        CType::Named(_) => match f.and_then(FnBinding::async_binding) {
            Some(a) => fn_pointer_type(ns, &a.callback.params, &CType::Void),
            None => cs_ctype(ns, &slot.ty),
        },
        ty => cs_ctype(ns, ty),
    };
    format!("{ty} {name}")
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
fn import_fn(w: &mut CodeWriter, ns: &str, abi: &AbiFn, f: Option<&FnBinding>) {
    let params: Vec<String> = abi.params.iter().map(|s| slot_decl(ns, s, f)).collect();
    import(w, &abi.symbol, &cs_ctype(ns, &abi.ret), &params);
}

/// Emit every import behind one callable: its entry point (the sync
/// symbol, async launcher, or iterator launcher) and, for an iterator,
/// `_next` (taking the iterator's `SafeHandle`) and `_destroy`.
fn render_callable(w: &mut CodeWriter, ns: &str, f: &FnBinding) {
    import_fn(w, ns, &f.abi, Some(f));
    if let Some(it) = f.iterator() {
        let mut params = vec!["FfiIteratorHandle iter".to_string()];
        params.extend(it.next.params[1..].iter().map(|s| slot_decl(ns, s, None)));
        import(w, &it.next.symbol, &cs_ctype(ns, &it.next.ret), &params);
        import(w, &it.destroy_symbol, "void", &["IntPtr iter".to_string()]);
    }
}

/// The imports behind one interface: `_clone`, `_destroy`, and every member.
fn render_interface(w: &mut CodeWriter, ns: &str, i: &InterfaceBinding) {
    import(
        w,
        &i.clone_symbol,
        "IntPtr",
        &[format!("{} self", handle_type(ns, &i.name))],
    );
    import(w, &i.destroy_symbol, "void", &["IntPtr self".to_string()]);
    for f in i.members() {
        render_callable(w, ns, f);
    }
}

/// Render the generated half of `NativeMethods`: the contract check run at
/// load (the library must export each declaration's contract function
/// these bindings were generated with, returning the same hash) and every
/// interface and function import in declaration order.
pub(crate) fn render_native_methods(w: &mut CodeWriter, model: &Model, ns: &str) {
    w.line("internal static unsafe partial class NativeMethods");
    w.line("{");
    w.indent();
    w.line("static partial void VerifyContracts(IntPtr library)");
    w.block("{", "}", |w| {
        w.line("VerifyContract(library, new (string, ulong, string)[]");
        w.line("{");
        w.indent();
        for row in rows(model) {
            w.line(format!(
                "(\"{}\", {}UL, \"{}\"), // {}",
                row.symbol,
                hex(row.hash),
                cs_str(&row.path),
                row.signature
            ));
        }
        w.dedent();
        w.line("});");
    });
    w.blank();
    for i in &model.interfaces {
        render_interface(w, ns, i);
    }
    for f in &model.functions {
        render_callable(w, ns, f);
    }
    w.dedent();
    w.line("}");
    w.blank();
}
