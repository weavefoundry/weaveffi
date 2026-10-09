//! Callback-interface rendering: the Swift `protocol` a consumer implements
//! and the process-wide vtable of `@convention(c)` trampolines the library
//! calls through.
//!
//! One static vtable per interface carries the `{size, flags, free}` header
//! (`flags` is 0: any thread may call), a context that retains the
//! implementation (released by `free`), and one trampoline per method.
//! Arguments arrive per the method's [`ArgPass`]es (borrowed strings, bytes,
//! typed arrays, and buffers are copied; objects are adopted), results go
//! back per its [`CallbackRetPass`] (a scalar by value, an optional scalar as
//! a presence flag plus out slot, an object as a fresh reference, and a
//! string, bytes, typed array, or buffer as a `{prefix}_alloc` run in the out
//! slots), and failures are reported through `out_err` instead of unwinding
//! through the C frame, per its [`ErrorStrategy`].

use crate::cabi::c_param_name;
use crate::codegen::common::{wrap, DocCommentStyle};
use crate::codegen::CodeWriter;
use crate::lang;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ErrorStrategy};
use weaveffi_model::ty::{Prim, Ty};

use crate::targets::swift::calls::{receive, Recv};
use crate::targets::swift::docs::{emit_decl_doc, emit_fn_doc};
use crate::targets::swift::types::{callback_vtable_name, swift_ident, SwiftCtx};

/// The Swift spelling of one trampoline closure formal: the ABI slot name,
/// keyword-escaped (a param named `in` yields a slot named `in`, which can't
/// bind as a closure formal unescaped).
fn slot_ident(name: &str) -> String {
    lang::escape_ident(name, lang::SWIFT_KEYWORDS)
}

/// The expression handing one trampoline argument to the implementation:
/// borrowed strings, bytes, typed arrays, and buffers are copied before the
/// implementation runs (the library owns them only for the call); object
/// arguments carry one strong reference the new wrapper adopts.
fn receive_arg(ty: &Ty, pass: &ArgPass, ctx: &SwiftCtx) -> String {
    let (recv, raw, len) = match pass {
        ArgPass::Direct { slot } => (Recv::Direct, slot_ident(&slot.name), String::new()),
        ArgPass::OptDirect { has, value, .. } => {
            let has = slot_ident(&has.name);
            let value = slot_ident(&value.name);
            return receive(ty, &Recv::Opt(&has), &value, "", false, ctx);
        }
        ArgPass::Slice { ptr, len, .. } => {
            (Recv::Slice, slot_ident(&ptr.name), slot_ident(&len.name))
        }
        ArgPass::String { ptr, len } => {
            (Recv::String, slot_ident(&ptr.name), slot_ident(&len.name))
        }
        ArgPass::Bytes { ptr, len } => (Recv::Bytes, slot_ident(&ptr.name), slot_ident(&len.name)),
        ArgPass::Buffer { ptr, len } => {
            (Recv::Buffer, slot_ident(&ptr.name), slot_ident(&len.name))
        }
        ArgPass::Object {
            slot,
            nullable,
            interface,
        } => {
            let recv = Recv::Object {
                nullable: *nullable,
                interface,
            };
            return receive(ty, &recv, &slot_ident(&slot.name), "", true, ctx);
        }
        ArgPass::Callback { .. } => unreachable!("a callback method never takes a callback"),
    };
    receive(ty, &recv, &raw, &len, false, ctx)
}

/// The `- Throws:` callout of a protocol requirement.
fn throws_doc(m: &CallbackMethodBinding, ctx: &SwiftCtx) -> Option<String> {
    match &m.error {
        ErrorStrategy::Trap => None,
        ErrorStrategy::Untyped => Some(
            "- Throws: Any error, which fails the library's call with the error's description."
                .to_string(),
        ),
        ErrorStrategy::Domain(name) => Some(format!(
            "- Throws: ``{}`` to fail the library's call with that error and its fields; any \
             other error fails it with the error's description.",
            ctx.ty_name(&ctx.model.error_domain(name).type_name)
        )),
    }
}

/// Render the `public protocol` the consumer conforms to: one requirement
/// per method with idiomatic labels and types, `throws` when the IDL method
/// declares errors. Implementations are classes (`AnyObject`) the library
/// may call from any thread (`Sendable`).
fn render_protocol(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, ctx: &SwiftCtx) {
    let name = ctx.ty_name(&cb.name);
    let doc = ctx.doc(&cb.doc);
    if doc.is_some() {
        emit_decl_doc(w, ctx, &cb.doc, &None);
        w.line("///");
    }
    w.line("/// Implement this protocol with a class and pass an instance where the API");
    w.line("/// expects it. The library keeps the instance until it no longer needs it and");
    w.line("/// may call any method from any thread.");
    if let Some(attr) = ctx.deprecated_attr(&cb.deprecated) {
        w.line(attr);
    }
    w.line(format!("public protocol {name}: AnyObject, Sendable {{"));
    w.indent();
    for (i, m) in cb.methods.iter().enumerate() {
        if i > 0 {
            w.blank();
        }
        let labels: Vec<_> = m.params.iter().map(|p| swift_ident(&p.name)).collect();
        let param_docs: Vec<_> = labels
            .iter()
            .cloned()
            .zip(m.params.iter().map(|p| &p.doc))
            .collect();
        emit_fn_doc(
            w,
            ctx,
            &m.doc,
            &param_docs,
            &throws_doc(m, ctx).into_iter().collect::<Vec<_>>(),
        );
        if let Some(attr) = ctx.deprecated_attr(&m.deprecated) {
            w.line(attr);
        }
        let params = labels
            .iter()
            .zip(&m.params)
            .map(|(label, p)| format!("{label}: {}", ctx.swift_type(&p.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let throws = if m.error.throws() { " throws" } else { "" };
        let ret = m
            .ret
            .as_ref()
            .map(|t| format!(" -> {}", ctx.swift_type(t)))
            .unwrap_or_default();
        w.line(format!(
            "func {}({params}){throws}{ret}",
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
fn return_shape(m: &CallbackMethodBinding, call: &str) -> (String, &'static str) {
    let optional_enum = matches!(
        &m.ret,
        Some(Ty::Optional(inner)) if matches!(**inner, Ty::Enum(_))
    );
    let out = |ptr: &str, len: &str| (slot_ident(ptr), slot_ident(len));
    match &m.ret_pass {
        CallbackRetPass::Void => (call.to_string(), "()"),
        CallbackRetPass::Direct => match &m.ret {
            Some(Ty::Enum(_)) => (format!("{call}.rawValue"), "0"),
            Some(Ty::Prim(Prim::Bool)) => (call.to_string(), "false"),
            _ => (call.to_string(), "0"),
        },
        CallbackRetPass::OptDirect { out_value } => {
            let value = if optional_enum {
                format!("{call}?.rawValue")
            } else {
                call.to_string()
            };
            (
                format!("wvReturnOptional({value}, {})", slot_ident(&out_value.name)),
                "false",
            )
        }
        // One strong reference the library adopts.
        CallbackRetPass::Object { nullable, .. } => {
            let q = if *nullable { "?" } else { "" };
            (format!("{call}{q}.clonePtr()"), "nil")
        }
        CallbackRetPass::Slice {
            out_ptr, out_len, ..
        }
        | CallbackRetPass::String { out_ptr, out_len }
        | CallbackRetPass::Bytes { out_ptr, out_len }
        | CallbackRetPass::Buffer { out_ptr, out_len } => {
            let helper = match &m.ret_pass {
                CallbackRetPass::Slice { .. } => "wvReturnSlice",
                CallbackRetPass::String { .. } => "wvReturnString",
                CallbackRetPass::Bytes { .. } => "wvReturnBytes",
                _ => "wvReturnBuffer",
            };
            let (ptr, len) = out(&out_ptr.name, &out_len.name);
            (format!("{helper}({call}, {ptr}, {len})"), "()")
        }
    }
}

/// Render one trampoline closure literal (the vtable entry for `m`) as a
/// labeled argument of the vtable struct initializer; `last` drops the
/// trailing comma.
fn render_trampoline(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    last: bool,
    ctx: &SwiftCtx,
) {
    let proto = ctx.ty_name(&cb.name);
    // No trailing comma: argument lists accept one only from Swift 6.1.
    let sep = if last { "" } else { "," };
    let slots = &m.abi.params;
    let formals = slots
        .iter()
        .map(|s| slot_ident(&s.name))
        .collect::<Vec<_>>()
        .join(", ");
    let ctx_slot = slot_ident(&slots[0].name);
    let err_slot = slot_ident(&slots[slots.len() - 1].name);
    let call_args = m
        .params
        .iter()
        .map(|p| {
            format!(
                "{}: {}",
                swift_ident(&p.name),
                receive_arg(&p.ty, &p.pass, ctx)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let call = format!("wvImpl.{}({call_args})", swift_ident(&m.name));
    let (body, fallback) = return_shape(m, &call);
    let body = if m.error.throws() {
        format!("try {body}")
    } else {
        body
    };
    let throwing = match &m.error {
        ErrorStrategy::Domain(name) => format!(
            ", throwing: {}.self",
            ctx.ty_name(&ctx.model.error_domain(name).type_name)
        ),
        ErrorStrategy::Trap | ErrorStrategy::Untyped => String::new(),
    };
    // The vtable field is spelled exactly as the C header declares it, so a
    // method whose name is a C keyword carries the header's escape.
    w.line(format!("{}: {{ {formals} in", c_param_name(&m.abi.symbol)));
    w.scope(|w| {
        w.line(format!(
            "wvInvoke({ctx_slot}, {err_slot}, as: (any {proto}).self{throwing}, fallback: {fallback}) {{ wvImpl in"
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
fn render_vtable(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, ctx: &SwiftCtx) {
    let proto = ctx.ty_name(&cb.name);
    let vtable_name = callback_vtable_name(&cb.name);
    let vtable_tag = &cb.vtable_tag;
    let doc = format!(
        "The process-wide vtable the library calls `{proto}` implementations through \
         (a `{vtable_tag}`). Every entry recovers the implementation from `ctx`, copies \
         or adopts its arguments, hands its result back, and reports a thrown error \
         through `out_err` instead of unwinding."
    );
    w.doc(&Some(wrap(&doc, 76)), DocCommentStyle::TripleSlash);
    w.line(format!("enum {vtable_name} {{"));
    w.indent();
    w.line(format!("static let shared = WvVtable({vtable_tag}("));
    w.indent();
    w.line(format!("size: UInt32(MemoryLayout<{vtable_tag}>.stride),"));
    w.line("flags: 0,");
    w.line(format!(
        "free: {{ ctx in wvRelease(ctx, as: (any {proto}).self) }}{}",
        if cb.methods.is_empty() { "" } else { "," }
    ));
    for (i, m) in cb.methods.iter().enumerate() {
        render_trampoline(w, cb, m, i + 1 == cb.methods.len(), ctx);
    }
    w.dedent();
    w.line("))");
    w.dedent();
    w.line("}");
    w.blank();
}

/// Render everything one callback interface contributes at file scope: the
/// public protocol and the vtable namespace whose `shared.pointer` every
/// call site passing this interface hands to the library.
pub(crate) fn render_swift_callback_interface(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    ctx: &SwiftCtx,
) {
    render_protocol(w, cb, ctx);
    render_vtable(w, cb, ctx);
}
