//! Callback-interface rendering: the Swift `protocol` a consumer implements
//! and the process-wide vtable of `@convention(c)` trampolines the library
//! calls through.
//!
//! A callback argument is a `{p}_{Cb}` struct of a context (which retains the
//! implementation, released by the vtable's `free`) and a pointer to one
//! static vtable per interface. The vtable carries the `{size, flags, free}`
//! header (`size` is the vtable's as these bindings compiled it, so a newer
//! producer can tell which methods they lack; `flags` is 0: any thread may
//! call) and one trampoline per method. Arguments arrive as one C value each
//! (borrowed runs are copied; objects are adopted), results go back as one C
//! value (a run from `{p}_alloc` and an object as a fresh reference, both
//! adopted by the library), and failures are reported through `out_err`
//! instead of unwinding through the C frame, per the method's
//! [`ErrorStrategy`].

use crate::cabi::c_param_name;
use crate::codegen::common::{wrap, DocCommentStyle};
use crate::codegen::CodeWriter;
use weaveffi_model::model::{CallbackBinding, CallbackMethodBinding, ErrorStrategy};

use crate::codegen::values::crossing;
use crate::targets::swift::calls::{hand_back, receive};
use crate::targets::swift::docs::{emit_decl_doc, emit_fn_doc};
use crate::targets::swift::types::{callback_vtable_name, swift_ident, SwiftCtx};

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
            ctx.domain_type(name)
        )),
    }
}

/// Render the `public protocol` the consumer conforms to: one requirement
/// per method with idiomatic labels and types, `throws` when the IDL method
/// declares errors. Implementations are classes (`AnyObject`) the library
/// may call from any thread (`Sendable`).
fn render_protocol(w: &mut CodeWriter, cb: &CallbackBinding, ctx: &SwiftCtx) {
    let name = &cb.name;
    if ctx.doc(&cb.doc).is_some() {
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

/// Render one trampoline closure literal (the vtable entry for `m`) as a
/// labeled argument of the vtable struct initializer; `last` drops the
/// trailing comma.
///
/// The closure's formals are positional (`ctx`, `a0`, `a1`, ..., `outErr`)
/// rather than the IDL's parameter names, so no parameter can shadow a name
/// the body uses.
fn render_trampoline(
    w: &mut CodeWriter,
    cb: &CallbackBinding,
    m: &CallbackMethodBinding,
    last: bool,
    ctx: &SwiftCtx,
) {
    // No trailing comma: argument lists accept one only from Swift 6.1.
    let sep = if last { "" } else { "," };
    let args: Vec<String> = (0..m.params.len()).map(|i| format!("a{i}")).collect();
    let mut formals = vec!["ctx".to_string()];
    formals.extend(args.iter().cloned());
    formals.push("outErr".to_string());
    let call_args = m
        .params
        .iter()
        .zip(&args)
        .map(|(p, a)| {
            format!(
                "{}: {}",
                swift_ident(&p.name),
                receive(&p.ty, a, false, ctx)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let call = format!("wvImpl.{}({call_args})", swift_ident(&m.name));
    let (body, fallback) = match &m.ret {
        None => (call, "()".to_string()),
        Some(ty) => (hand_back(ty, &call, ctx), ctx.c_zero(&crossing(ty))),
    };
    let body = if m.error.throws() {
        format!("try {body}")
    } else {
        body
    };
    let throwing = match &m.error {
        ErrorStrategy::Domain(name) => format!(", throwing: {}.self", ctx.domain_type(name)),
        ErrorStrategy::Trap | ErrorStrategy::Untyped => String::new(),
    };
    // The vtable field is spelled exactly as the C header declares it, so a
    // method whose name is a C keyword carries the header's escape.
    w.line(format!(
        "{}: {{ {} in",
        c_param_name(&m.abi.symbol),
        formals.join(", ")
    ));
    w.scope(|w| {
        w.line(format!(
            "wvInvoke(ctx, outErr, as: (any {}).self{throwing}, fallback: {fallback}) {{ wvImpl in",
            cb.name
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
fn render_vtable(w: &mut CodeWriter, cb: &CallbackBinding, ctx: &SwiftCtx) {
    let proto = &cb.name;
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
    w.line(format!("size: UInt32(MemoryLayout<{vtable_tag}>.size),"));
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

/// Render everything one callback interface contributes: the public
/// protocol and the vtable namespace whose `shared.pointer` every call site
/// passing this interface hands to the library.
pub(crate) fn render_swift_callback(w: &mut CodeWriter, cb: &CallbackBinding, ctx: &SwiftCtx) {
    render_protocol(w, cb, ctx);
    render_vtable(w, cb, ctx);
}
