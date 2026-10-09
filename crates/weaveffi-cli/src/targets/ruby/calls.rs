//! Callable rendering: the `attach_function` lines of the private `Native`
//! module and the idiomatic wrappers (module functions, interface
//! constructors, methods, and statics) in their sync, async, and iterator
//! shapes.
//!
//! A wrapper checks and converts its arguments per their [`ArgPass`], then
//! hands the C call to one runtime helper (`Bridge.call`, `Bridge.await`,
//! `Bridge.iterate`) as a block. The helper allocates the error and out
//! slots, seals the value-buffer arguments, raises per the callable's
//! [`ErrorStrategy`], and receives the
//! result per its kind, so no wrapper repeats that plumbing.
//!
//! Every call into the library except the trivial helpers releases the GVL
//! (`blocking: true`): a call may run for a long time, and a producer thread
//! may need the GVL to run a callback implementation while the call is in
//! flight.

use weaveffi_model::model::{CallShape, FnBinding};
use weaveffi_model::plan::{ArgPass, ErrorStrategy};
use weaveffi_model::ty::{Prim, Ty};

use crate::codegen::docs::Doc;
use crate::codegen::CodeWriter;
use crate::targets::ruby::docs::{self, CallableDoc, ParamDoc};
use crate::targets::ruby::types::{
    rb_checked_int, rb_const, rb_elem, rb_error_arg, rb_ffi_type, rb_ffi_types, rb_function_name,
    rb_item_kind, rb_member_name, rb_param_doc_type, rb_param_name, rb_result_kind,
    rb_ret_doc_type, rb_return_kind, rb_scalar, rb_slot, rb_str_literal, rb_wire,
};
use crate::targets::ruby::RbCtx;

/// Where a rendered Ruby callable lives and how it's spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    /// A module-level function (`def self.name` on the gem's module).
    Free,
    /// An instance method on an interface class (`def name`), pinning the
    /// wrapper's own pointer as the receiver slot.
    Method,
    /// A static member of an interface class (`def self.name`).
    Static,
    /// A constructor other than `new`: a class method returning a wrapper
    /// that adopted the returned reference (never re-running `initialize`).
    Factory,
    /// The `new` constructor, emitted as `initialize`.
    Init,
}

/// The scope of one rendered callable: its kind and, for an interface
/// member, the class it belongs to.
pub(crate) struct RbScope<'a> {
    /// Where the callable lives.
    pub(crate) kind: ScopeKind,
    /// The interface class of a member; `None` for a free function.
    pub(crate) class: Option<&'a str>,
}

impl RbScope<'_> {
    /// The Ruby name of `f` in this scope.
    fn name(&self, f: &FnBinding) -> String {
        match self.kind {
            ScopeKind::Method => rb_member_name(&f.name, true),
            ScopeKind::Static | ScopeKind::Factory => rb_member_name(&f.name, false),
            ScopeKind::Free => rb_function_name(&f.name),
            ScopeKind::Init => "initialize".to_string(),
        }
    }

    /// The `def` opener for `f` with the given formal parameters.
    fn def_open(&self, f: &FnBinding, formals: &[String]) -> String {
        let args = if formals.is_empty() {
            String::new()
        } else {
            format!("({})", formals.join(", "))
        };
        let name = self.name(f);
        match self.kind {
            ScopeKind::Method | ScopeKind::Init => format!("def {name}{args}"),
            ScopeKind::Free | ScopeKind::Static | ScopeKind::Factory => {
                format!("def self.{name}{args}")
            }
        }
    }

    /// How a caller spells `f`: `Kvstore.open_store`, `Kvstore::Store#size`,
    /// `Kvstore::Store.open`, or `Kvstore::Store.new`.
    fn display(&self, module: &str, f: &FnBinding) -> String {
        let name = self.name(f);
        match (self.kind, self.class) {
            (ScopeKind::Method, Some(class)) => format!("{module}::{class}#{name}"),
            (ScopeKind::Init, Some(class)) => format!("{module}::{class}.new"),
            (_, Some(class)) => format!("{module}::{class}.{name}"),
            (_, None) => format!("{module}.{name}"),
        }
    }
}

/// Emit the `attach_function` lines of one callable inside `Native`: the
/// symbol it calls (the sync entry, the async launcher, or the iterator
/// launcher) and, for an iterator, its `_next` and `_destroy`. An
/// iterator's `_destroy`, like an object's `_clone` and `_destroy`, is a
/// trivial helper that keeps the GVL (and may therefore run from a GC
/// finalizer).
pub(crate) fn render_attach(w: &mut CodeWriter, f: &FnBinding) {
    w.line(format!(
        "attach_function :{}, {}, {}, blocking: true",
        f.abi.symbol,
        rb_ffi_types(&f.abi.params),
        rb_ffi_type(&f.abi.ret),
    ));
    if let Some(it) = f.iterator() {
        w.line(format!(
            "attach_function :{}, {}, {}, blocking: true",
            it.next.symbol,
            rb_ffi_types(&it.next.params),
            rb_ffi_type(&it.next.ret),
        ));
        w.line(format!(
            "attach_function :{}, [:pointer], :void",
            it.destroy_symbol
        ));
    }
}

/// The Ruby formal parameter of each IDL parameter, in order. A
/// cancellable callable's `cancel:` keyword is reserved, so a parameter
/// named `cancel` there is `cancel_`.
fn formal_names(f: &FnBinding) -> Vec<String> {
    f.params
        .iter()
        .map(|p| {
            let name = rb_param_name(&p.name);
            if f.cancellable() && name == "cancel" {
                "cancel_".to_string()
            } else {
                name
            }
        })
        .collect()
}

/// The Ruby formal parameters of `f`. Trailing optional parameters (`T?`,
/// `Cb?`) default to `nil`, so callers may leave them out; a cancellable
/// callable ends with the `cancel:` keyword.
fn formals(f: &FnBinding) -> Vec<String> {
    let names = formal_names(f);
    let optional = f
        .params
        .iter()
        .rev()
        .take_while(|p| match &p.pass {
            ArgPass::Callback { nullable, .. } => *nullable,
            _ => matches!(p.ty.value(), Some(Ty::Optional(_))),
        })
        .count();
    let first_optional = f.params.len() - optional;
    let mut out: Vec<String> = names
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            if i >= first_optional {
                format!("{name} = nil")
            } else {
                name
            }
        })
        .collect();
    if f.cancellable() {
        out.push("cancel: nil".to_string());
    }
    out
}

/// What converting a callable's arguments produced: the statements to run
/// first, the value-buffer writers the call helper seals, the objects to
/// pin, the callback registrations to make immediately before the call,
/// and the C arguments in slot order.
#[derive(Default)]
struct Prepared {
    /// Checks and conversions, run before anything is pinned or sealed.
    lines: Vec<String>,
    /// The locals holding value-buffer writers; the helper yields their
    /// sealed encodings under the same names.
    writers: Vec<String>,
    /// `(Ruby expression, pinned pointer local)` pairs.
    pins: Vec<(String, String)>,
    /// Callback registrations, which never raise, so nothing can strand
    /// one between it and the call that hands it over.
    registers: Vec<String>,
    /// The C arguments, receiver first.
    args: Vec<String>,
}

/// Check and convert every argument of `f` per its [`ArgPass`].
fn prepare(f: &FnBinding, scope: &RbScope) -> Prepared {
    let mut out = Prepared::default();
    if let Some(receiver) = &f.receiver {
        if scope.kind == ScopeKind::Method {
            let local = rb_slot(receiver);
            out.pins.push(("self".to_string(), local.clone()));
            out.args.push(local);
        }
    }
    for (p, formal) in f.params.iter().zip(formal_names(f)) {
        match &p.pass {
            ArgPass::Direct { slot } => {
                let local = rb_slot(slot);
                let ty = p.ty.value().expect("a direct parameter is a value");
                if let Some(kind) = rb_checked_int(ty) {
                    out.lines
                        .push(format!("{local} = Bridge.scalar({formal}, {kind})"));
                    out.args.push(local);
                } else if matches!(ty, Ty::Prim(Prim::Bool)) {
                    out.args.push(format!("({formal} ? true : false)"));
                } else {
                    out.args.push(formal);
                }
            }
            ArgPass::OptDirect { has, value, inner } => {
                let (has, value) = (rb_slot(has), rb_slot(value));
                out.lines.push(format!(
                    "{has}, {value} = Bridge.opt_arg({formal}, {})",
                    rb_scalar(inner)
                ));
                out.args.extend([has, value]);
            }
            ArgPass::Slice { ptr, len, elem } => {
                let (ptr, len) = (rb_slot(ptr), rb_slot(len));
                out.lines.push(format!(
                    "{ptr}, {len} = Bridge.slice_arg({formal}, {})",
                    rb_elem(*elem)
                ));
                out.args.extend([ptr, len]);
            }
            ArgPass::String { ptr, .. } | ArgPass::Bytes { ptr, .. } => {
                let helper = if matches!(p.pass, ArgPass::String { .. }) {
                    "string_arg"
                } else {
                    "bytes_arg"
                };
                let ptr = rb_slot(ptr);
                out.lines.push(format!("{ptr} = Bridge.{helper}({formal})"));
                out.args.extend([ptr.clone(), format!("{ptr}.bytesize")]);
            }
            ArgPass::Buffer { ptr, .. } => {
                let ty = p.ty.value().expect("a buffered parameter is a value");
                let ptr = rb_slot(ptr);
                out.lines
                    .push(format!("{ptr} = Bridge.encode({}, {formal})", rb_wire(ty)));
                out.writers.push(ptr.clone());
                out.args.extend([ptr.clone(), format!("{ptr}.bytesize")]);
            }
            ArgPass::Object {
                slot,
                nullable,
                interface,
            } => {
                let nullable = if *nullable { ", nullable: true" } else { "" };
                out.lines.push(format!(
                    "Bridge.check_object({formal}, {}, '{formal}'{nullable})",
                    rb_const(interface)
                ));
                let local = rb_slot(slot);
                out.pins.push((formal, local.clone()));
                out.args.push(local);
            }
            ArgPass::Callback {
                ctx,
                vtable,
                nullable,
                interface,
            } => {
                if !nullable {
                    out.lines
                        .push(format!("Bridge.present!({formal}, '{formal}')"));
                }
                let (ctx, vtable) = (rb_slot(ctx), rb_slot(vtable));
                out.registers.push(format!(
                    "{ctx}, {vtable} = Bridge.register({formal}, {})",
                    rb_const(interface)
                ));
                out.args.extend([ctx, vtable]);
            }
        }
    }
    out
}

/// Run `body` inside a `Bridge.pin` block over `pins` (each object stays
/// alive, and its wrapper can't release its reference, until the block
/// returns), or directly when nothing is borrowed. `lead` prefixes the
/// opener (an assignment of the block's value, or empty).
fn with_pins(
    w: &mut CodeWriter,
    lead: &str,
    pins: &[(String, String)],
    body: impl FnOnce(&mut CodeWriter),
) {
    if pins.is_empty() {
        body(w);
        return;
    }
    let exprs: Vec<&str> = pins.iter().map(|(e, _)| e.as_str()).collect();
    let locals: Vec<&str> = pins.iter().map(|(_, l)| l.as_str()).collect();
    w.block(
        format!(
            "{lead}Bridge.pin({}) do |{}|",
            exprs.join(", "),
            locals.join(", ")
        ),
        "end",
        body,
    );
}

/// The longest line kept on one line before a call's arguments wrap.
const MAX_LINE: usize = 100;

/// Emit `{head}({args}){tail}` on one line, or with one argument per line
/// when that's too long.
fn emit_call(w: &mut CodeWriter, head: &str, args: &[String], tail: &str) {
    let one_line = format!("{head}({}){tail}", args.join(", "));
    if w.indent_str().len() + one_line.len() <= MAX_LINE {
        w.line(one_line);
        return;
    }
    w.line(format!("{head}("));
    w.scope(|w| {
        for a in args {
            w.line(format!("{a},"));
        }
    });
    w.line(format!("){tail}"));
}

/// Emit a helper call with a block: `{head}({args}) do |params|`, the
/// block's body, and its `end`.
fn emit_block_call(
    w: &mut CodeWriter,
    head: &str,
    args: &[String],
    params: &[String],
    body: impl FnOnce(&mut CodeWriter),
) {
    emit_call(w, head, args, &format!(" do{}", block_params(params)));
    w.scope(body);
    w.line("end");
}

/// Emit the C call itself, after the callback registrations that must
/// immediately precede it.
fn emit_native_call(w: &mut CodeWriter, f: &FnBinding, prep: &Prepared, args: &[String]) {
    for r in &prep.registers {
        w.line(r);
    }
    emit_call(w, &format!("Native.{}", f.abi.symbol), args, "");
}

/// The block parameter list `|a, b|` of a helper call.
fn block_params(params: &[String]) -> String {
    if params.is_empty() {
        String::new()
    } else {
        format!(" |{}|", params.join(", "))
    }
}

/// A helper call's arguments: the leading ones, then the writers it seals.
fn helper_args(lead: Vec<String>, writers: &[String]) -> Vec<String> {
    lead.into_iter().chain(writers.iter().cloned()).collect()
}

/// Render one callable: a free function or an interface member.
pub(crate) fn render_callable(w: &mut CodeWriter, ctx: &RbCtx, f: &FnBinding, scope: &RbScope) {
    render_doc(w, ctx, f, scope);
    let prepared = prepare(f, scope);
    w.block(scope.def_open(f, &formals(f)), "end", |w| {
        if f.deprecated.is_some() {
            let msg = Doc::new(&f.doc, &f.deprecated)
                .deprecation(|i| docs::spell(&ctx.names, i))
                .unwrap_or_default();
            w.line(format!(
                "warn('{} is deprecated: {}', uplevel: 1, category: :deprecated)",
                scope.display(ctx.module, f),
                rb_str_literal(&msg)
            ));
        }
        for line in &prepared.lines {
            w.line(line);
        }
        match &f.shape {
            CallShape::Async(_) => render_async(w, f, &prepared),
            CallShape::Sync if f.iterator().is_some() => render_iterator(w, f, &prepared),
            CallShape::Sync => render_sync(w, f, scope, &prepared),
        }
    });
}

/// The YARD comment of a callable: its doc, a note on its shape, every
/// parameter, the return, and what it raises.
fn render_doc(w: &mut CodeWriter, ctx: &RbCtx, f: &FnBinding, scope: &RbScope) {
    let mut extra = CallableDoc::default();
    if f.is_async() {
        extra.notes.push(
            "Blocks until the call completes on a library thread (under a Fiber \
             scheduler, only the calling Fiber waits)."
                .to_string(),
        );
    }
    if f.iterator().is_some() {
        extra.notes.push(
            "Returns a lazy Enumerator: each enumeration starts its own native \
             iterator on the first pull and releases it when iteration finishes \
             or stops early."
                .to_string(),
        );
    }
    for (p, name) in f.params.iter().zip(formal_names(f)) {
        extra.params.push(ParamDoc {
            name,
            ty: rb_param_doc_type(&p.ty),
            doc: p.doc.clone(),
        });
    }
    if f.cancellable() {
        extra.params.push(ParamDoc {
            name: "cancel".to_string(),
            ty: "CancelToken, nil".to_string(),
            doc: Some("cancels the call, which then raises Cancelled".to_string()),
        });
    }
    if scope.kind != ScopeKind::Init {
        extra.ret = f.ret.as_ref().map(rb_ret_doc_type);
    }
    match &f.error {
        ErrorStrategy::Trap => {}
        ErrorStrategy::Untyped => {
            extra
                .raises
                .push(("Error".to_string(), "when the call fails".to_string()));
        }
        ErrorStrategy::Domain(name) => {
            extra.raises.push((
                crate::targets::ruby::types::rb_domain(name),
                "for a domain error".to_string(),
            ));
        }
    }
    if f.cancellable() {
        extra.raises.push((
            "Cancelled".to_string(),
            "when `cancel` fires first".to_string(),
        ));
    }
    docs::emit(w, &ctx.names, &Doc::new(&f.doc, &f.deprecated), &extra);
}

/// The name of the error slot (the last C parameter of a sync call, an
/// iterator launcher, or `_next`).
fn err_slot(f: &FnBinding) -> String {
    rb_slot(f.abi.params.last().expect("a call carries out_err"))
}

/// Render a sync wrapper body: pin the borrowed objects, then hand the C
/// call to `Bridge.call`.
fn render_sync(w: &mut CodeWriter, f: &FnBinding, scope: &RbScope, prep: &Prepared) {
    let kind = if scope.kind == ScopeKind::Init {
        Some(":pointer".to_string())
    } else {
        rb_return_kind(&f.ret_pass, f.ret.as_ref().and_then(|r| r.value()))
    };
    let mut lead = vec![rb_error_arg(&f.error)];
    match kind {
        Some(kind) => lead.push(kind),
        None if !prep.writers.is_empty() => lead.push("nil".to_string()),
        None => {}
    }
    let mut params = prep.writers.clone();
    params.extend(f.ret_pass.out_slots().into_iter().map(rb_slot));
    params.push(err_slot(f));
    let mut args = prep.args.clone();
    args.extend(f.ret_pass.out_slots().into_iter().map(rb_slot));
    args.push(err_slot(f));
    let assign = if scope.kind == ScopeKind::Init {
        "ptr = "
    } else {
        ""
    };
    let call = |w: &mut CodeWriter, lead_text: &str| {
        emit_block_call(
            w,
            &format!("{lead_text}Bridge.call"),
            &helper_args(lead.clone(), &prep.writers),
            &params,
            |w| emit_native_call(w, f, prep, &args),
        );
    };
    if prep.pins.is_empty() {
        call(w, assign);
    } else {
        with_pins(w, assign, &prep.pins, |w| call(w, ""));
    }
    if scope.kind == ScopeKind::Init {
        w.line("_wv_init(ptr)");
    }
}

/// Render an async wrapper body: launch the call through `Bridge.await`
/// (or `Bridge.await_cancellable`), which blocks on a queue until the
/// completion delivers the result or the error.
fn render_async(w: &mut CodeWriter, f: &FnBinding, prep: &Prepared) {
    let a = f.async_binding().expect("an async shape has a binding");
    let ret = f.ret.as_ref().and_then(|r| r.value());
    let mut lead = vec![rb_error_arg(&f.error), rb_result_kind(&a.result, ret)];
    let mut params = prep.writers.clone();
    let mut args = prep.args.clone();
    let helper = if let Some(token) = &a.cancel_token {
        lead.push("cancel".to_string());
        params.push(rb_slot(token));
        args.push(rb_slot(token));
        "await_cancellable"
    } else {
        "await"
    };
    // The launcher's last two slots are the completion and its context.
    let tail = &f.abi.params[f.abi.params.len() - 2..];
    params.extend(tail.iter().map(rb_slot));
    args.extend(tail.iter().map(rb_slot));
    with_pins(w, "", &prep.pins, |w| {
        emit_block_call(
            w,
            &format!("Bridge.{helper}"),
            &helper_args(lead, &prep.writers),
            &params,
            |w| emit_native_call(w, f, prep, &args),
        );
    });
}

/// Render an iterator wrapper body: `Bridge.iterate` returns a lazy
/// Enumerator whose every enumeration runs the launch block (pinning,
/// sealing, and registering then) and pulls elements with `_next`.
fn render_iterator(w: &mut CodeWriter, f: &FnBinding, prep: &Prepared) {
    let it = f.iterator().expect("an iterator shape has a binding");
    let lead = vec![
        rb_error_arg(&f.error),
        rb_item_kind(&it.item, &it.elem),
        format!(":{}", it.next.symbol),
        format!(":{}", it.destroy_symbol),
    ];
    let mut params = prep.writers.clone();
    params.push(err_slot(f));
    let mut args = prep.args.clone();
    args.push(err_slot(f));
    emit_block_call(
        w,
        "Bridge.iterate",
        &helper_args(lead, &prep.writers),
        &params,
        |w| with_pins(w, "", &prep.pins, |w| emit_native_call(w, f, prep, &args)),
    );
}
