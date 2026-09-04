//! Callback-interface rendering: the Swift `protocol` a consumer implements,
//! the box class that retains one implementation across the C boundary, and
//! the process-wide vtable of `@convention(c)` trampolines the producer calls
//! through.
//!
//! Every clause of [`CallbackProtocol`] is rendered here: one static vtable
//! per interface, a pointer-keyed context (the retained box), arguments
//! received per [`RetPass`] (borrowed strings, bytes, and buffers are copied
//! or decoded; object arguments are adopted), and a `do`/`catch` around every
//! implementation call so a thrown Swift error is reported through
//! `weaveffi_error_set` with the foreign error code instead of unwinding
//! through the C frame.

use weaveffi_core::cabi::c_param_name;
use weaveffi_core::codegen::common::DocCommentStyle;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::lang;
use weaveffi_core::model::{
    CallbackInterfaceBinding, CallbackMethodBinding, ModuleBinding, ParamBinding, Ty,
};
use weaveffi_core::plan::{CallbackProtocol, RetPass};
use weaveffi_core::utils::local_type_name;

use crate::codec::{fresh, read_value_stmts};
use crate::docs::emit_fn_doc;
use crate::types::{
    c_enum_type, callback_box_name, callback_vtable_name, swift_ident, swift_type_ctx, SwiftCtx,
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

/// The Swift return clause of a protocol requirement: empty for `void`.
fn ret_clause(m: &CallbackMethodBinding, ctx: SwiftCtx) -> String {
    m.ret
        .as_ref()
        .map(|t| format!(" -> {}", swift_type_ctx(t, ctx)))
        .unwrap_or_default()
}

/// The Swift literal a trampoline returns after reporting a failure, in the
/// method's C return type: `false`, `0`, or a zero C enum.
fn c_default(m: &CallbackMethodBinding, c_prefix: &str, module: &str) -> Option<String> {
    m.ret.as_ref().map(|t| match t {
        Ty::Bool => "false".to_string(),
        Ty::Enum(name) => format!("{}(0)", c_enum_type(name, c_prefix, module)),
        _ => "0".to_string(),
    })
}

/// Emit the statements receiving one trampoline argument per its [`RetPass`]
/// and return the expression handed to the implementation. Borrowed strings,
/// bytes, and buffers are copied or decoded before the implementation runs
/// (the producer owns them only for the dispatch); object arguments carry
/// one strong reference the new wrapper adopts.
fn receive_arg(
    w: &mut CodeWriter,
    p: &ParamBinding,
    rp: &RetPass,
    ctx: SwiftCtx,
    counter: &mut usize,
) -> String {
    let n0 = slot_ident(&p.abi[0].name);
    match rp {
        RetPass::Direct => match &p.ty {
            Ty::Enum(_) => format!("{}(rawValue: {n0}.rawValue)!", swift_type_ctx(&p.ty, ctx)),
            _ => n0,
        },
        RetPass::String => format!("String(cString: {n0}!)"),
        RetPass::Bytes => {
            let n1 = slot_ident(&p.abi[1].name);
            format!("{n0}.map {{ Data(bytes: $0, count: {n1}) }} ?? Data()")
        }
        RetPass::Buffer => {
            let n1 = slot_ident(&p.abi[1].name);
            let buf = fresh(counter, "b");
            let reader = fresh(counter, "r");
            w.line(format!(
                "let {buf} = [UInt8](UnsafeBufferPointer(start: {n0}, count: {n1}))"
            ));
            w.line(format!("var {reader} = WvReader(bytes: {buf})"));
            let v = fresh(counter, "v");
            read_value_stmts(w, &p.ty, &v, &reader, ctx, counter);
            w.line(format!("{reader}.finish()"));
            v
        }
        RetPass::Object { nullable, .. } => {
            let wrapper = ctx.ty_name(local_type_name(
                p.ty.interface_name()
                    .expect("object argument names an interface"),
            ));
            if *nullable {
                format!("{n0}.map {{ {wrapper}(ptr: $0) }}")
            } else {
                format!("{wrapper}(ptr: {n0}!)")
            }
        }
        RetPass::Void => unreachable!("a parameter is never void"),
    }
}

/// Render the `public protocol` the consumer conforms to: one `throws`
/// requirement per method with idiomatic labels and types. A conforming
/// method may omit `throws`; one that throws is reported to the producer as a
/// foreign failure.
fn render_protocol(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, ctx: SwiftCtx) {
    let name = ctx.ty_name(local_type_name(&cb.name));
    if cb.doc.is_some() {
        w.doc(&cb.doc, DocCommentStyle::TripleSlash);
        w.line("///");
    }
    w.line("/// Implement this protocol and pass the value where the API expects it; the");
    w.line("/// producer keeps it alive for as long as it holds the callback and may call");
    w.line("/// any method from any thread. A thrown error aborts the producer's call and");
    w.line("/// surfaces to the original caller as a foreign error (code -4).");
    if let Some(msg) = &cb.deprecated {
        w.line(format!(
            "@available(*, deprecated, message: \"{}\")",
            msg.replace('"', "\\\"")
        ));
    }
    w.line(format!("public protocol {name} {{"));
    w.indent();
    for m in &cb.methods {
        let m = camel_method(m);
        {
            let mut tmp = String::new();
            emit_fn_doc(&mut tmp, &m.doc, &m.params, &w.indent_str());
            w.raw(tmp);
        }
        if let Some(msg) = &m.deprecated {
            w.line(format!(
                "@available(*, deprecated, message: \"{}\")",
                msg.replace('"', "\\\"")
            ));
        }
        let params = m
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, swift_type_ctx(&p.ty, ctx)))
            .collect::<Vec<_>>()
            .join(", ");
        w.line(format!(
            "func {}({params}) throws{}",
            swift_ident(&m.name),
            ret_clause(&m, ctx)
        ));
    }
    w.dedent();
    w.line("}");
    w.blank();
}

/// Render the internal box that pins one implementation behind the `void*
/// ctx` the producer holds: `Unmanaged.passRetained` when passing, one
/// `release` from the vtable's `free`.
fn render_box(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, ctx: SwiftCtx) {
    let proto = ctx.ty_name(local_type_name(&cb.name));
    let box_name = callback_box_name(&cb.name);
    w.line(format!(
        "/// Retains one `{proto}` implementation across the C boundary; the producer's"
    ));
    w.line("/// `free` entry releases it.");
    w.line(format!("final class {box_name} {{"));
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
    module: &str,
    ctx: SwiftCtx,
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
    let mut counter = 0usize;
    let mut call_args = Vec::new();
    for (p, rp) in camel.params.iter().zip(args) {
        let expr = receive_arg(w, p, rp, ctx, &mut counter);
        call_args.push(format!("{}: {expr}", p.name));
    }
    let call = format!(
        "try wvBox.impl.{}({})",
        swift_ident(&m.name),
        call_args.join(", ")
    );
    let default = c_default(m, ctx.c_prefix, module);
    w.line("do {");
    w.indent();
    match (&m.ret, &default) {
        (Some(Ty::Enum(name)), Some(_)) => {
            w.line(format!(
                "return {}({call}.rawValue)",
                c_enum_type(name, ctx.c_prefix, module)
            ));
        }
        (Some(_), Some(_)) => {
            w.line(format!("return {call}"));
        }
        _ => {
            w.line(call);
        }
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

/// Render the process-wide vtable namespace: `value` is the C struct filled
/// with capture-free trampolines (implicitly `@convention(c)`), and
/// `pointer` is a heap cell initialized once with that value so the
/// producer can hold its address for the process lifetime.
fn render_vtable(
    w: &mut CodeWriter,
    module: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
    protocol: &CallbackProtocol<'_>,
    ctx: SwiftCtx,
) {
    let proto = ctx.ty_name(local_type_name(&cb.name));
    let box_name = callback_box_name(&cb.name);
    let vtable_name = callback_vtable_name(&cb.name);
    let vtable_tag = &cb.vtable_tag;
    w.line(format!(
        "/// The process-wide `{vtable_tag}` the producer calls `{proto}`"
    ));
    w.line("/// implementations through. Every entry recovers the box from `ctx`, copies");
    w.line("/// or adopts its arguments, and reports a thrown error through `out_err`");
    w.line("/// instead of unwinding.");
    w.line(format!("enum {vtable_name} {{"));
    w.indent();
    w.line(format!("static let value = {vtable_tag}("));
    w.indent();
    for (m, args) in cb.methods.iter().zip(&protocol.method_args) {
        render_trampoline(w, m, args, &box_name, &module.path, ctx);
    }
    w.line("free: { ctx in");
    w.scope(|w| {
        w.line(format!("Unmanaged<{box_name}>.fromOpaque(ctx!).release()"));
    });
    w.line("}");
    w.dedent();
    w.line(")");
    w.line(format!(
        "static let pointer: UnsafePointer<{vtable_tag}> = {{"
    ));
    w.scope(|w| {
        w.line(format!(
            "let cell = UnsafeMutablePointer<{vtable_tag}>.allocate(capacity: 1)"
        ));
        w.line("cell.initialize(to: value)");
        w.line("return UnsafePointer(cell)");
    });
    w.line("}()");
    w.dedent();
    w.line("}");
    w.blank();
}

/// Render everything one callback interface contributes at file scope: the
/// public protocol, the retaining box, and the vtable namespace whose
/// `pointer` every call site passing this interface hands to the producer.
pub(crate) fn render_swift_callback_interface(
    out: &mut String,
    module: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
    ctx: SwiftCtx,
) {
    let protocol = cb.protocol(&module.path, ctx.c_prefix);
    let mut w = CodeWriter::four_space();
    render_protocol(&mut w, cb, ctx);
    render_box(&mut w, cb, ctx);
    render_vtable(&mut w, module, cb, &protocol, ctx);
    out.push_str(&w.finish());
}
