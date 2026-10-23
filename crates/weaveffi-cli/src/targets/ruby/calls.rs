//! Callable rendering: the `attach_function` lines of the private `Native`
//! module and the idiomatic wrappers (library functions, interface
//! constructors, methods, and statics) in their sync, async, and iterator
//! shapes.
//!
//! Every value is one C value, so a wrapper converts each argument per its
//! [`Crossing`] into exactly one C argument, then hands the C call to one
//! runtime helper (`Bridge.call`, `Bridge.await`, `Bridge.iterate`) as a
//! block. The helper allocates the error slot, seals the value-buffer
//! arguments into runs, raises per the callable's [`ErrorStrategy`], and
//! receives the result per its kind, so no wrapper repeats that plumbing.
//!
//! Every call into the library except the trivial helpers releases the GVL
//! (`blocking: true`): a call may run for a long time, and a producer thread
//! may need the GVL to run a callback implementation while the call is in
//! flight.

use weaveffi_model::model::{ErrorStrategy, FnBinding, Shape};
use weaveffi_model::ty::{ParamTy, Prim, Ty};

use crate::codegen::docs::Doc;
use crate::codegen::values::{crossing, Crossing};
use crate::codegen::CodeWriter;
use crate::targets::ruby::docs::{self, CallableDoc, ParamDoc};
use crate::targets::ruby::types::{
    rb_const, rb_domain, rb_error_arg, rb_ffi_type, rb_ffi_types, rb_function_name, rb_kind,
    rb_kind_or_nil, rb_member_name, rb_param_doc_type, rb_param_name, rb_ret_doc_type, rb_scalar,
    rb_str_literal, rb_wire,
};
use crate::targets::ruby::RbCtx;

/// Where a rendered Ruby callable lives and how it's spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    /// A library function (`def self.name` on the gem's module).
    Free,
    /// An instance method on an interface class (`def name`), pinning the
    /// wrapper's own pointer as the receiver.
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
    /// The interface class of a member; `None` for a library function.
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
        .take_while(|p| match &p.ty {
            ParamTy::Callback { nullable, .. } => *nullable,
            ParamTy::Value(ty) => matches!(ty, Ty::Optional(_)),
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
/// pin, and the C arguments in slot order.
#[derive(Default)]
struct Prepared {
    /// Checks and conversions, run before anything is pinned or sealed.
    lines: Vec<String>,
    /// The locals holding value-buffer writers; the helper yields their
    /// sealed runs under the same names.
    writers: Vec<String>,
    /// `(Ruby expression, pinned pointer local)` pairs.
    pins: Vec<(String, String)>,
    /// The C arguments, receiver first. A callback argument is its
    /// registration, which never raises, evaluated as the call is made so
    /// nothing can strand it.
    args: Vec<String>,
}

/// The receiver's pinned pointer local.
const SELF_LOCAL: &str = "self_";

/// Check and convert every argument of `f` per its [`Crossing`].
fn prepare(f: &FnBinding, scope: &RbScope) -> Prepared {
    let mut out = Prepared::default();
    if scope.kind == ScopeKind::Method {
        out.pins.push(("self".to_string(), SELF_LOCAL.to_string()));
        out.args.push(SELF_LOCAL.to_string());
    }
    for (p, formal) in f.params.iter().zip(formal_names(f)) {
        let ty = match &p.ty {
            ParamTy::Callback { name, nullable } => {
                if !nullable {
                    out.lines
                        .push(format!("Bridge.present!({formal}, '{formal}')"));
                }
                out.args
                    .push(format!("Bridge.register({formal}, {})", rb_const(name)));
                continue;
            }
            ParamTy::Value(ty) => ty,
        };
        let convert = |expr: String| format!("{formal} = {expr}");
        match crossing(ty) {
            Crossing::Scalar(Prim::Bool) => {
                out.args.push(format!("({formal} ? true : false)"));
                continue;
            }
            Crossing::Scalar(p) if !p.is_integer() => {}
            Crossing::Scalar(p) => out.lines.push(convert(format!(
                "Bridge.scalar({formal}, {})",
                rb_scalar(p)
            ))),
            Crossing::Enum(_) => out
                .lines
                .push(convert(format!("Bridge.scalar({formal}, :i32)"))),
            Crossing::OptScalar(p) => out.lines.push(convert(format!(
                "Bridge.opt_arg({formal}, {})",
                rb_scalar(p)
            ))),
            Crossing::OptEnum(_) => out
                .lines
                .push(convert(format!("Bridge.opt_arg({formal}, :i32)"))),
            Crossing::Utf8 => out
                .lines
                .push(convert(format!("Bridge.string_arg({formal})"))),
            Crossing::Bytes => out
                .lines
                .push(convert(format!("Bridge.bytes_arg({formal})"))),
            Crossing::Slice(p) => out.lines.push(convert(format!(
                "Bridge.slice_arg({formal}, {})",
                rb_scalar(p)
            ))),
            Crossing::Buffer(ty) => {
                out.lines
                    .push(convert(format!("Bridge.encode({}, {formal})", rb_wire(ty))));
                out.writers.push(formal.clone());
            }
            Crossing::Object {
                interface,
                nullable,
            } => {
                let nullable = if nullable { ", nullable: true" } else { "" };
                out.lines.push(format!(
                    "Bridge.check_object({formal}, {}, '{formal}'{nullable})",
                    rb_const(interface)
                ));
                out.pins.push((formal.clone(), formal.clone()));
            }
        }
        out.args.push(formal);
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
    let params = if params.is_empty() {
        String::new()
    } else {
        format!(" |{}|", params.join(", "))
    };
    emit_call(w, head, args, &format!(" do{params}"));
    w.scope(body);
    w.line("end");
}

/// A helper call's arguments: the leading ones, then the writers it seals.
fn helper_args(lead: Vec<String>, writers: &[String]) -> Vec<String> {
    lead.into_iter().chain(writers.iter().cloned()).collect()
}

/// Render one callable: a library function or an interface member.
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
            Shape::Async(_) => render_async(w, f, &prepared),
            Shape::Iterator(_) => render_iterator(w, f, &prepared),
            Shape::Sync => render_sync(w, f, scope, &prepared),
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
            extra
                .raises
                .push((rb_domain(name), "for a domain error".to_string()));
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

/// The error slot's local, the last C argument of a sync call, an iterator
/// launcher, and `_next`.
const ERR: &str = "out_err";

/// The C call itself, with the helper's block parameters appended.
fn native_call(w: &mut CodeWriter, f: &FnBinding, prep: &Prepared, tail: &[&str]) {
    let args: Vec<String> = prep
        .args
        .iter()
        .cloned()
        .chain(tail.iter().map(|s| (*s).to_string()))
        .collect();
    emit_call(w, &format!("Native.{}", f.abi.symbol), &args, "");
}

/// Render a sync wrapper body: pin the borrowed objects, then hand the C
/// call to `Bridge.call`.
fn render_sync(w: &mut CodeWriter, f: &FnBinding, scope: &RbScope, prep: &Prepared) {
    let kind = if scope.kind == ScopeKind::Init {
        Some(":pointer".to_string())
    } else {
        f.ret_value().map(rb_kind)
    };
    let mut lead = vec![rb_error_arg(&f.error)];
    match kind {
        Some(kind) => lead.push(kind),
        None if !prep.writers.is_empty() => lead.push("nil".to_string()),
        None => {}
    }
    let mut params = prep.writers.clone();
    params.push(ERR.to_string());
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
            |w| native_call(w, f, prep, &[ERR]),
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
    let mut lead = vec![rb_error_arg(&f.error), rb_kind_or_nil(f.ret_value())];
    let mut params = prep.writers.clone();
    let mut tail = Vec::new();
    let helper = if f.cancellable() {
        lead.push("cancel".to_string());
        tail.push("cancel_token");
        "await_cancellable"
    } else {
        "await"
    };
    tail.extend(["callback", "context"]);
    params.extend(tail.iter().map(|s| (*s).to_string()));
    with_pins(w, "", &prep.pins, |w| {
        emit_block_call(
            w,
            &format!("Bridge.{helper}"),
            &helper_args(lead, &prep.writers),
            &params,
            |w| native_call(w, f, prep, &tail),
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
        rb_kind(&it.elem),
        format!(":{}", it.next.symbol),
        format!(":{}", it.destroy_symbol),
    ];
    let mut params = prep.writers.clone();
    params.push(ERR.to_string());
    emit_block_call(
        w,
        "Bridge.iterate",
        &helper_args(lead, &prep.writers),
        &params,
        |w| with_pins(w, "", &prep.pins, |w| native_call(w, f, prep, &[ERR])),
    );
}
