//! Callback-interface rendering: the Swift `protocol` a consumer implements
//! and the process-wide vtable of `@convention(c)` trampolines the library
//! calls through.
//!
//! Every clause of [`CallbackProtocol`] is rendered here: one static vtable
//! per interface with the `{size, flags, free}` header, a context that
//! retains the implementation (released by `free`), arguments received per
//! [`RetPass`] (borrowed strings, bytes, and buffers are copied or decoded;
//! object arguments are adopted), returns handed back per family (a direct
//! value by value, an object as a fresh reference, a string, bytes, or
//! buffer as a `{prefix}_alloc` run in the out slots), and failures reported
//! through `out_err` instead of unwinding through the C frame: a method
//! declared `throws` reports its module's domain error with its code and
//! payload, and any other error is a callback failure (`-4`).

use crate::cabi::c_param_name;
use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use crate::lang;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, ParamBinding};
use weaveffi_model::plan::{CallbackProtocol, ErrorStrategy, RetPass};
use weaveffi_model::ty::{Prim, Ty};

use crate::targets::swift::docs::emit_fn_doc;
use crate::targets::swift::types::{callback_vtable_name, deprecated_attr, swift_ident, SwiftCtx};

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
/// only for the call); object arguments carry one strong reference the new
/// wrapper adopts.
fn receive_arg(p: &ParamBinding, rp: &RetPass, ctx: &SwiftCtx) -> String {
    let n0 = slot_ident(&p.abi[0].name);
    let n1 = || slot_ident(&p.abi[1].name);
    match rp {
        RetPass::Direct => match &p.ty {
            Ty::Enum(name) => format!(
                "wvEnumCase({}.self, numericCast({n0}.rawValue))",
                ctx.ty_name(name)
            ),
            _ => n0,
        },
        RetPass::String => format!("wvBorrowString({n0}, {})", n1()),
        RetPass::Bytes => format!("wvBorrowBytes({n0}, {})", n1()),
        RetPass::Buffer => format!(
            "wvBorrowBuffer({n0}, {}, as: {}.self)",
            n1(),
            ctx.swift_type(&p.ty)
        ),
        RetPass::Object { nullable } => {
            let wrapper = ctx.ty_name(
                p.ty.interface_name()
                    .expect("object argument names an interface"),
            );
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
fn render_protocol(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    domain: Option<&str>,
    ctx: &SwiftCtx,
) {
    let name = ctx.ty_name(&cb.name);
    if cb.doc.is_some() {
        w.doc(&cb.doc, DocCommentStyle::TripleSlash);
        w.line("///");
    }
    w.line("/// Implement this protocol with a class and pass an instance where the API");
    w.line("/// expects it. The library keeps the instance until it no longer needs it and");
    w.line("/// may call any method from any thread. A method that throws fails the");
    w.line("/// library's call in progress: a method documented to throw a module error");
    w.line("/// reports that error, with its code and fields, and any other error reaches");
    w.line("/// the library as a callback failure (code -4).");
    if let Some(msg) = &cb.deprecated {
        w.line(deprecated_attr(msg));
    }
    w.line(format!("public protocol {name}: AnyObject, Sendable {{"));
    w.indent();
    for (i, m) in cb.methods.iter().enumerate() {
        let m = camel_method(m);
        if i > 0 {
            w.blank();
        }
        let throws = match (m.throws, domain) {
            (true, Some(ty)) => vec![format!(
                "- Throws: ``{ty}`` to report a declared failure with its fields, or any other error to fail the call."
            )],
            _ => Vec::new(),
        };
        emit_fn_doc(w, &m.doc, &m.params, &throws);
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

/// How one method's result crosses back: the implementation call wrapped
/// into the trampoline's return expression, and the value returned on
/// failure.
fn return_shape(
    m: &CallbackMethodBinding,
    rp: &RetPass,
    call: &str,
    ctx: &SwiftCtx,
) -> (String, String) {
    let out = || {
        let slots = &m.abi_params[m.abi_params.len() - 3..m.abi_params.len() - 1];
        (slot_ident(&slots[0].name), slot_ident(&slots[1].name))
    };
    match rp {
        RetPass::Void => (format!("try {call}"), "()".to_string()),
        RetPass::Direct => match &m.ret {
            Some(Ty::Enum(name)) => {
                let c_type = ctx.c_enum_type(name);
                (
                    format!("try {c_type}(rawValue: numericCast({call}.rawValue))"),
                    format!("{c_type}(rawValue: 0)"),
                )
            }
            Some(Ty::Prim(Prim::Bool)) => (format!("try {call}"), "false".to_string()),
            _ => (format!("try {call}"), "0".to_string()),
        },
        // One strong reference the library adopts.
        RetPass::Object { nullable } => {
            let q = if *nullable { "?" } else { "" };
            (format!("try {call}{q}.clonePtr()"), "nil".to_string())
        }
        RetPass::String | RetPass::Bytes | RetPass::Buffer => {
            let helper = match rp {
                RetPass::String => "wvReturnString",
                RetPass::Bytes => "wvReturnBytes",
                _ => "wvReturnBuffer",
            };
            let (ptr, len) = out();
            (
                format!("try {helper}({call}, {ptr}, {len})"),
                "()".to_string(),
            )
        }
    }
}

/// Render one trampoline closure literal (the vtable entry for `m`) as a
/// labeled argument of the vtable struct initializer. `i` indexes the
/// method in `cb` and `protocol`; `report` names the domain reporter a
/// throwing method passes along.
fn render_trampoline(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    protocol: &CallbackProtocol<'_>,
    i: usize,
    report: Option<&str>,
    ctx: &SwiftCtx,
) {
    let m = &cb.methods[i];
    let proto = ctx.ty_name(&cb.name);
    let report = report.filter(|_| protocol.method_errors[i] == ErrorStrategy::Throws);
    // No trailing comma: argument lists accept one only from Swift 6.1.
    let sep = if i + 1 == cb.methods.len() { "" } else { "," };
    let camel = camel_method(m);
    let formals = m
        .abi_params
        .iter()
        .map(|s| slot_ident(&s.name))
        .collect::<Vec<_>>()
        .join(", ");
    let ctx_slot = slot_ident(&m.abi_params[0].name);
    let err_slot = slot_ident(&m.abi_params[m.abi_params.len() - 1].name);
    let call_args = camel
        .params
        .iter()
        .zip(&protocol.method_args[i])
        .map(|(p, rp)| format!("{}: {}", p.name, receive_arg(p, rp, ctx)))
        .collect::<Vec<_>>()
        .join(", ");
    let call = format!("wvImpl.{}({call_args})", swift_ident(&m.name));
    let (body, fallback) = return_shape(m, &protocol.method_returns[i], &call, ctx);
    let domain = report.map(|r| format!(", domain: {r}")).unwrap_or_default();
    // The vtable field is spelled exactly as the C header declares it, so a
    // method whose name is a C keyword carries the header's escape.
    w.line(format!("{}: {{ {formals} in", c_param_name(&m.name)));
    w.scope(|w| {
        w.line(format!(
            "wvInvoke({ctx_slot}, {err_slot}, as: (any {proto}).self, fallback: {fallback}{domain}) {{ wvImpl in"
        ));
        w.scope(|w| {
            w.line(body);
        });
        w.line("}");
    });
    w.line(format!("}}{sep}"));
}

/// Render the process-wide vtable namespace: `shared` holds the C struct
/// (its header, then capture-free trampolines, implicitly `@convention(c)`)
/// at a stable address the library can hold for the process lifetime.
fn render_vtable(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    protocol: &CallbackProtocol<'_>,
    report: Option<&str>,
    ctx: &SwiftCtx,
) {
    let proto = ctx.ty_name(&cb.name);
    let vtable_name = callback_vtable_name(&cb.name);
    let vtable_tag = &cb.vtable_tag;
    w.line(format!(
        "/// The process-wide vtable the library calls `{proto}` implementations"
    ));
    w.line(format!(
        "/// through (`{vtable_tag}`). Every entry recovers the implementation"
    ));
    w.line("/// from `ctx`, copies or adopts its arguments, hands its result back, and");
    w.line("/// reports a thrown error through `out_err` instead of unwinding.");
    w.line(format!("enum {vtable_name} {{"));
    w.indent();
    w.line(format!("static let shared = WvVtable({vtable_tag}("));
    w.indent();
    w.line(format!("size: UInt32(MemoryLayout<{vtable_tag}>.stride),"));
    w.line("flags: 0,");
    w.line("free: { ctx in");
    w.scope(|w| {
        w.line(format!("wvRelease(ctx, as: (any {proto}).self)"));
    });
    w.line("},");
    for i in 0..cb.methods.len() {
        render_trampoline(w, cb, protocol, i, report, ctx);
    }
    w.dedent();
    w.line("))");
    w.dedent();
    w.line("}");
    w.blank();
}

/// Render everything one callback interface contributes at file scope: the
/// public protocol and the vtable namespace whose `shared.pointer` every
/// call site passing this interface hands to the library. `domain` is the
/// `(type name, stem)` of the error domain in scope for its module, which
/// throwing methods report through `wvReport{Stem}`.
pub(crate) fn render_swift_callback_interface(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    domain: Option<(&str, &str)>,
    ctx: &SwiftCtx,
) {
    let protocol = cb.protocol();
    let report = domain.map(|(_, stem)| format!("wvReport{stem}"));
    render_protocol(w, cb, domain.map(|d| d.0), ctx);
    render_vtable(w, cb, &protocol, report.as_deref(), ctx);
}
