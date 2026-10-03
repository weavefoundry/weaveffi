//! Callback-interface rendering: the Swift `protocol` a consumer implements,
//! the box class that retains one implementation across the C boundary, and
//! the process-wide vtable of `@convention(c)` trampolines the library calls
//! through.
//!
//! Every clause of [`CallbackProtocol`] is rendered here: one static vtable
//! per interface, a pointer-keyed context (the retained box), arguments
//! received per [`RetPass`] (borrowed strings, bytes, and buffers are copied
//! or decoded; object arguments are adopted), and a `do`/`catch` around every
//! implementation call so a thrown Swift error is reported through the
//! runtime's `error_set` with the foreign error code instead of unwinding
//! through the C frame.

use crate::cabi::c_param_name;
use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use crate::lang;
use crate::utils::local_type_name;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, ParamBinding, Ty};
use weaveffi_model::plan::{CallbackProtocol, RetPass};

use crate::targets::swift::codec::decode_closure;
use crate::targets::swift::docs::emit_fn_doc;
use crate::targets::swift::types::{
    callback_box_name, callback_vtable_name, deprecated_attr, swift_ident, SwiftCtx,
};

/// The Swift spelling of one trampoline closure formal: the ABI slot name,
/// keyword-escaped (a param named `in` yields a slot named `in`, which can't
/// bind as a closure formal unescaped).
fn slot_ident(name: &str) -> String {
    lang::escape_ident(name, lang::SWIFT_KEYWORDS)
}

/// Clone a callback method with its parameter names camel-cased and
/// keyword-escaped, so the protocol requirement's argument labels and the
/// trampoline's call site agree. The ABI slot names are left untouched.
fn camel_method(m: &CallbackMethodBinding) -> CallbackMethodBinding {
    let mut m = m.clone();
    for p in &mut m.params {
        p.name = swift_ident(&p.name);
    }
    m
}

/// The expression handing one trampoline argument to the implementation,
/// received per its [`RetPass`]: borrowed strings, bytes, and buffers are
/// copied or decoded before the implementation runs (the library owns them
/// only for the dispatch); object arguments carry one strong reference the
/// new wrapper adopts.
fn receive_arg(w: &CodeWriter, p: &ParamBinding, rp: &RetPass, ctx: &SwiftCtx) -> String {
    let n0 = slot_ident(&p.abi[0].name);
    let n1 = || slot_ident(&p.abi[1].name);
    match rp {
        RetPass::Direct => match &p.ty {
            Ty::Enum(name) => format!(
                "wvEnumCase({}.self, numericCast({n0}.rawValue))",
                ctx.ty_name(local_type_name(name))
            ),
            _ => n0,
        },
        RetPass::String => format!("wvBorrowString({n0}, {})", n1()),
        RetPass::Bytes => format!("wvBorrowBytes({n0}, {})", n1()),
        RetPass::Buffer => {
            let decode = decode_closure(w, &p.ty, ctx);
            if decode.starts_with('{') {
                format!("wvBorrowBuffer({n0}, {}) {decode}", n1())
            } else {
                format!("wvBorrowBuffer({n0}, {}, {decode})", n1())
            }
        }
        RetPass::Object { nullable, .. } => {
            let wrapper = ctx.ty_name(local_type_name(
                p.ty.interface_name()
                    .expect("object argument names an interface"),
            ));
            if *nullable {
                format!("{n0}.map {{ {wrapper}(ptr: $0) }}")
            } else {
                format!("{wrapper}(ptr: wvNonNull({n0}))")
            }
        }
        RetPass::Void => unreachable!("a parameter is never void"),
    }
}

/// Render the `public protocol` the consumer conforms to: one `throws`
/// requirement per method with idiomatic labels and types. Implementations
/// are classes (`AnyObject`) the library may call from any thread
/// (`Sendable`).
fn render_protocol(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, ctx: &SwiftCtx) {
    let name = ctx.ty_name(local_type_name(&cb.name));
    if cb.doc.is_some() {
        w.doc(&cb.doc, DocCommentStyle::TripleSlash);
        w.line("///");
    }
    w.line("/// Implement this protocol with a class and pass an instance where the API");
    w.line("/// expects it; the library keeps it alive for as long as it holds the");
    w.line("/// callback and may call any method from any thread. A thrown error aborts");
    w.line("/// the library's call and surfaces to the original caller as a foreign error");
    w.line("/// (code -4).");
    if let Some(msg) = &cb.deprecated {
        w.line(deprecated_attr(msg));
    }
    w.line(format!("public protocol {name}: AnyObject, Sendable {{"));
    w.indent();
    for m in &cb.methods {
        let m = camel_method(m);
        {
            let mut tmp = String::new();
            emit_fn_doc(&mut tmp, &m.doc, &m.params, &w.indent_str());
            w.raw(tmp);
        }
        if let Some(msg) = &m.deprecated {
            w.line(deprecated_attr(msg));
        }
        let params = m
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, ctx.swift_type(&p.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let ret = m
            .ret
            .as_ref()
            .map(|t| format!(" -> {}", ctx.swift_type(t)))
            .unwrap_or_default();
        w.line(format!(
            "func {}({params}) throws{ret}",
            swift_ident(&m.name)
        ));
    }
    w.dedent();
    w.line("}");
    w.blank();
}

/// Render the internal box that pins one implementation behind the `void*
/// ctx` the library holds: `Unmanaged.passRetained` when passing, one
/// `release` from the vtable's `free`.
fn render_box(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, ctx: &SwiftCtx) {
    let proto = ctx.ty_name(local_type_name(&cb.name));
    let box_name = callback_box_name(&cb.name);
    w.line(format!(
        "/// Retains one `{proto}` implementation across the C boundary; the library's"
    ));
    w.line("/// `free` entry releases it.");
    w.line(format!("final class {box_name}: Sendable {{"));
    w.scope(|w| {
        w.line(format!("let impl: any {proto}"));
        w.line(format!("init(_ impl: any {proto}) {{ self.impl = impl }}"));
    });
    w.line("}");
    w.blank();
}

/// Render one trampoline closure literal (the vtable entry for `m`) as a
/// labeled argument of the vtable struct initializer.
fn render_trampoline(
    w: &mut CodeWriter,
    m: &CallbackMethodBinding,
    args: &[RetPass],
    box_name: &str,
    ctx: &SwiftCtx,
) {
    let camel = camel_method(m);
    let formals = m
        .abi_params
        .iter()
        .map(|s| slot_ident(&s.name))
        .collect::<Vec<_>>()
        .join(", ");
    // The vtable field is spelled exactly as the C header declares it, so a
    // method whose name is a C keyword carries the header's escape.
    w.line(format!("{}: {{ {formals} in", c_param_name(&m.name)));
    w.indent();
    w.line(format!(
        "let wvBox = Unmanaged<{box_name}>.fromOpaque(ctx!).takeUnretainedValue()"
    ));
    w.line("do {");
    w.indent();
    let call_args = camel
        .params
        .iter()
        .zip(args)
        .map(|(p, rp)| format!("{}: {}", p.name, receive_arg(w, p, rp, ctx)))
        .collect::<Vec<_>>()
        .join(", ");
    let call = format!("try wvBox.impl.{}({call_args})", swift_ident(&m.name));
    let default = match &m.ret {
        None => None,
        Some(Ty::Enum(name)) => {
            let c_type = ctx.c_enum_type(name);
            w.line(format!("let value = {call}"));
            w.line(format!(
                "return {c_type}(rawValue: numericCast(value.rawValue))"
            ));
            Some(format!("{c_type}(rawValue: 0)"))
        }
        Some(t) => {
            w.line(format!("return {call}"));
            Some(if matches!(t, Ty::Bool) { "false" } else { "0" }.to_string())
        }
    };
    if default.is_none() {
        w.line(call);
    }
    w.dedent();
    w.line("} catch {");
    w.indent();
    w.line("wvForeignError(out_err, error)");
    if let Some(default) = default {
        w.line(format!("return {default}"));
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("},");
}

/// Render the process-wide vtable namespace: `shared` holds the C struct
/// filled with capture-free trampolines (implicitly `@convention(c)`) at a
/// stable address the library can hold for the process lifetime.
fn render_vtable(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    protocol: &CallbackProtocol<'_>,
    ctx: &SwiftCtx,
) {
    let proto = ctx.ty_name(local_type_name(&cb.name));
    let box_name = callback_box_name(&cb.name);
    let vtable_name = callback_vtable_name(&cb.name);
    let vtable_tag = &cb.vtable_tag;
    w.line(format!(
        "/// The process-wide `{vtable_tag}` the library calls `{proto}`"
    ));
    w.line("/// implementations through. Every entry recovers the box from `ctx`, copies");
    w.line("/// or adopts its arguments, and reports a thrown error through `out_err`");
    w.line("/// instead of unwinding.");
    w.line(format!("enum {vtable_name} {{"));
    w.indent();
    w.line(format!(
        "static let shared = WvVtable<{vtable_tag}>({vtable_tag}("
    ));
    w.indent();
    for (m, args) in cb.methods.iter().zip(&protocol.method_args) {
        render_trampoline(w, m, args, &box_name, ctx);
    }
    w.line("free: { ctx in");
    w.scope(|w| {
        w.line(format!("Unmanaged<{box_name}>.fromOpaque(ctx!).release()"));
    });
    w.line("}");
    w.dedent();
    w.line("))");
    w.dedent();
    w.line("}");
    w.blank();
}

/// Render everything one callback interface contributes at file scope: the
/// public protocol, the retaining box, and the vtable namespace whose
/// `shared.pointer` every call site passing this interface hands to the
/// library.
pub(crate) fn render_swift_callback_interface(
    out: &mut String,
    cb: &CallbackInterfaceBinding,
    ctx: &SwiftCtx,
) {
    let protocol = cb.protocol(ctx.c_prefix);
    let mut w = CodeWriter::four_space();
    render_protocol(&mut w, cb, ctx);
    render_box(&mut w, cb, ctx);
    render_vtable(&mut w, cb, &protocol, ctx);
    out.push_str(&w.finish());
}
