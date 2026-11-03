//! Callable rendering: sync, async, and iterator-returning wrapper
//! functions, and the per-[`Crossing`] marshalling every position shares.
//!
//! In C ABI revision 6 every value crosses as one C value, so a value is
//! marshalled the same way whether it's a parameter, a return, an async
//! result, an iterator item, or a callback argument or return; only who owns
//! a run differs (see [`crate::codegen::values`]). How a failure surfaces
//! comes from the callable's [`ErrorStrategy`]; this module only renders the
//! Swift spelling.

use crate::codegen::values::{crossing, Crossing};
use crate::codegen::CodeWriter;
use crate::lang;
use weaveffi_model::model::{
    AsyncBinding, ErrorStrategy, FnBinding, IteratorBinding, ParamBinding,
};
use weaveffi_model::ty::{ParamTy, Ty};

use crate::targets::swift::docs::emit_fn_doc;
use crate::targets::swift::types::{callback_vtable_name, prim_swift_type, swift_ident, SwiftCtx};

/// Locals a wrapper body declares next to its parameters. A parameter
/// spelled like one keeps its argument label and gets a `_`-suffixed local
/// name (`err err_: Int32`), so the body never confuses the two. (The
/// `{local}_run` and `{local}_cb` locals can't collide with another
/// parameter's: a lowerCamelCase local never contains an underscore but a
/// trailing escape.)
const BODY_LOCALS: &[&str] = &["err", "rv", "token", "context", "continuation"];

/// One parameter of a wrapper: its argument label and the local name the
/// body uses (they differ only when the label is a body local).
pub(crate) struct Param<'a> {
    /// The argument label (lowerCamelCase, keyword-escaped).
    pub(crate) label: String,
    /// The name the body refers to the argument by.
    pub(crate) local: String,
    /// The parameter as lowered.
    pub(crate) binding: &'a ParamBinding,
}

impl Param<'_> {
    /// The declaration in a signature: `label: Type`, or `label local: Type`.
    fn decl(&self, ctx: &SwiftCtx) -> String {
        let ty = ctx.param_type(&self.binding.ty);
        if self.label == self.local {
            format!("{}: {ty}", self.label)
        } else {
            format!("{} {}: {ty}", self.label, self.local)
        }
    }
}

/// The wrapper parameters of `f`.
pub(crate) fn swift_params(f: &FnBinding) -> Vec<Param<'_>> {
    f.params
        .iter()
        .map(|binding| {
            let label = swift_ident(&binding.name);
            let local = lang::escape_member(&label, BODY_LOCALS);
            Param {
                label,
                local,
                binding,
            }
        })
        .collect()
}

// MARK: Marshalling

/// The runtime helper that lends a run-crossing value of `crossing` for one
/// call, binding the run to the closure's parameter, or `None` for a value
/// that crosses without one.
fn lender(crossing: &Crossing) -> Option<&'static str> {
    Some(match crossing {
        Crossing::Utf8 => "wvWithUTF8",
        Crossing::Bytes => "wvWithBytes",
        Crossing::Slice(_) => "wvWithSlice",
        Crossing::Buffer(_) => "wvWithEncoded",
        _ => return None,
    })
}

/// The C value of `value` (a Swift expression of the value's Swift type)
/// for a position that doesn't take a run: a scalar as itself, an enum as
/// its raw value, an optional scalar as its `{p}_opt_{prim}`, and an object
/// as its pointer, `fresh` (a fresh strong reference, for a callback return)
/// or borrowed (the wrapper's own, for a parameter).
fn pass(crossing: &Crossing, value: &str, fresh: bool, ctx: &SwiftCtx) -> String {
    match crossing {
        Crossing::Scalar(_) => value.to_string(),
        Crossing::Enum(_) => format!("{value}.rawValue"),
        Crossing::OptScalar(_) => format!("{}(wv: {value})", ctx.c_value_type(crossing)),
        Crossing::OptEnum(_) => {
            format!("{}(wv: {value}?.rawValue)", ctx.c_value_type(crossing))
        }
        Crossing::Object { nullable, .. } => {
            let q = if *nullable { "?" } else { "" };
            let member = if fresh { "clonePtr()" } else { "ptr" };
            format!("{value}{q}.{member}")
        }
        Crossing::Utf8 | Crossing::Bytes | Crossing::Slice(_) | Crossing::Buffer(_) => {
            unreachable!("a run crosses through its lender")
        }
    }
}

/// The C value of `value` handed to the library as a callback method's
/// return: runs are allocated with `{p}_alloc` and adopted by the library,
/// and an object is a fresh strong reference.
pub(crate) fn hand_back(ty: &Ty, value: &str, ctx: &SwiftCtx) -> String {
    let c = crossing(ty);
    let helper = match c {
        Crossing::Utf8 => "wvHandOverString",
        Crossing::Bytes => "wvHandOverBytes",
        Crossing::Slice(_) => "wvHandOverSlice",
        Crossing::Buffer(_) => "wvHandOverBuffer",
        _ => return pass(&c, value, true, ctx),
    };
    format!("{helper}({value})")
}

/// The Swift value of type `ty` that the library handed over as the C value
/// `raw`. An `owned` run (a return, async result, or iterator item) is
/// released after it's copied; a borrowed one (a callback argument) is only
/// copied. An object always moves one strong reference, which the new
/// wrapper adopts.
pub(crate) fn receive(ty: &Ty, raw: &str, owned: bool, ctx: &SwiftCtx) -> String {
    let take = if owned { "wvTake" } else { "wvBorrow" };
    match crossing(ty) {
        Crossing::Scalar(_) => raw.to_string(),
        Crossing::Enum(name) => format!("wvEnumCase({name}.self, {raw})"),
        Crossing::OptScalar(_) => format!("{raw}.wvValue"),
        Crossing::OptEnum(name) => format!("{raw}.wvValue.map {{ wvEnumCase({name}.self, $0) }}"),
        Crossing::Utf8 => format!("{take}String({raw})"),
        Crossing::Bytes => format!("{take}Bytes({raw})"),
        Crossing::Slice(p) => format!("{take}Slice({raw}, as: {}.self)", prim_swift_type(p)),
        Crossing::Buffer(ty) => format!("{take}Buffer({raw}, as: {}.self)", ctx.swift_type(ty)),
        Crossing::Object {
            interface,
            nullable: true,
        } => format!("{raw}.map {{ {interface}(ptr: $0) }}"),
        Crossing::Object {
            interface,
            nullable: false,
        } => format!("{interface}(ptr: wvNonNull({raw}))"),
    }
}

// MARK: Errors

/// How a wrapper reports a failure, from the callable's [`ErrorStrategy`]
/// and cancellability.
///
/// A callable declared `throws: Domain` throws the domain's enum for its
/// positive codes; one declared `throws: any` throws the runtime error; a
/// cancellable async callable also throws (`CancellationError` for code
/// `-5`). Any other callable has a plain signature and stops the process
/// with `fatalError`, naming the code and message, since a reported error
/// can only be a producer bug.
pub(crate) struct ErrCtx {
    error: ErrorStrategy,
    cancellable: bool,
    /// The Swift type of the domain, for `throws: Domain`.
    domain_type: Option<String>,
}

impl ErrCtx {
    /// The error context of `f`.
    pub(crate) fn for_fn(f: &FnBinding, ctx: &SwiftCtx) -> Self {
        Self {
            domain_type: f.error.domain().map(|d| ctx.domain_type(d)),
            error: f.error.clone(),
            cancellable: f.cancellable(),
        }
    }

    /// `true` when the Swift signature `throws`.
    pub(crate) fn throws(&self) -> bool {
        self.error.throws() || self.cancellable
    }

    /// The statement checking the error slot of a synchronous call.
    fn check_stmt(&self) -> String {
        match (&self.error, &self.domain_type) {
            (ErrorStrategy::Domain(_), Some(ty)) => format!("try wvCheck(&err, {ty}.self)"),
            (ErrorStrategy::Trap, _) => "wvTrap(&err)".to_string(),
            _ => "try wvCheck(&err)".to_string(),
        }
    }

    /// The statements a completion runs for its boxed error `err`: resume
    /// the continuation by throwing, or stop the process.
    fn completion_failure(&self) -> Vec<String> {
        let take = match (&self.error, &self.domain_type) {
            (ErrorStrategy::Domain(_), Some(ty)) => format!("wvTakeError(err, {ty}.self)"),
            (ErrorStrategy::Trap, _) if self.cancellable => "wvTakeCancellation(err)".to_string(),
            (ErrorStrategy::Trap, _) => return vec!["wvFatal(err.pointee)".to_string()],
            _ => "wvTakeError(err)".to_string(),
        };
        vec![
            format!("cont.resume(throwing: {take})"),
            "return".to_string(),
        ]
    }

    /// The closure a sequence maps a mid-stream failure through.
    fn sequence_failure(&self) -> String {
        match (&self.error, &self.domain_type) {
            (ErrorStrategy::Domain(_), Some(ty)) => format!("{{ wvDomainError($0, {ty}.self) }}"),
            (ErrorStrategy::Trap, _) => "{ wvFatal($0) }".to_string(),
            _ => "wvRuntimeError".to_string(),
        }
    }

    /// The `- Throws:` doc callout of a throwing wrapper.
    fn throws_doc(&self, runtime_error: &str) -> Option<String> {
        let mut doc = match (&self.error, &self.domain_type) {
            (ErrorStrategy::Domain(_), Some(ty)) => {
                format!("- Throws: ``{ty}`` for a declared failure, ``{runtime_error}`` otherwise")
            }
            (ErrorStrategy::Trap, _) if self.cancellable => {
                "- Throws: `CancellationError` when the calling task is cancelled, which \
                 cancels the native call."
                    .to_string()
            }
            (ErrorStrategy::Trap, _) => return None,
            _ => format!("- Throws: ``{runtime_error}`` with the library's message"),
        };
        if self.error.throws() {
            if self.cancellable {
                doc.push_str(
                    ", and `CancellationError` when the calling task is cancelled, which \
                     cancels the native call",
                );
            }
            doc.push('.');
        }
        Some(doc)
    }
}

// MARK: Wrappers

/// Where a wrapper sits, which decides how it reaches its object and whether
/// it runs the load-time checks.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Receiver {
    /// A free function or static member: runs the load-time checks first.
    Static,
    /// An instance method passing its own object as the leading argument.
    Instance,
    /// The `new` constructor, rendered as `public init`.
    Init,
}

/// Render one wrapper for `f` (sync, async, or iterator-returning) named
/// `swift_name`. A free function is a top-level `func`; an interface
/// member, a member of its class.
pub(crate) fn render_callable(
    w: &mut CodeWriter,
    f: &FnBinding,
    swift_name: &str,
    receiver: Receiver,
    member: bool,
    ctx: &SwiftCtx,
) {
    let err = ErrCtx::for_fn(f, ctx);
    let params = swift_params(f);
    let mut callouts = Vec::new();
    if f.iterator().is_some() {
        callouts.push(
            "- Returns: A lazy sequence that pulls one element per step from the library."
                .to_string(),
        );
    }
    callouts.extend(err.throws_doc(&ctx.runtime_error));
    let param_docs: Vec<_> = params
        .iter()
        .map(|p| (p.label.clone(), &p.binding.doc))
        .collect();
    emit_fn_doc(w, ctx, &f.doc, &param_docs, &callouts);
    if let Some(attr) = ctx.deprecated_attr(&f.deprecated) {
        w.line(attr);
    }
    let sig = params
        .iter()
        .map(|p| p.decl(ctx))
        .collect::<Vec<_>>()
        .join(", ");
    let effects = match (f.is_async(), err.throws()) {
        (true, true) => " async throws",
        (true, false) => " async",
        (false, true) => " throws",
        (false, false) => "",
    };
    if receiver == Receiver::Init {
        w.line(format!("public init({sig}){effects} {{"));
    } else {
        let ret = f
            .ret
            .as_ref()
            .map(|t| format!(" -> {}", ctx.ret_type(t)))
            .unwrap_or_default();
        let static_kw = if member && receiver == Receiver::Static {
            "static "
        } else {
            ""
        };
        w.line(format!(
            "public {static_kw}func {swift_name}({sig}){effects}{ret} {{"
        ));
    }
    w.indent();
    if receiver != Receiver::Instance {
        w.line("wvLoad()");
    }
    match f.async_binding() {
        Some(binding) => render_async_body(w, f, binding, &params, receiver, &err, ctx),
        None => render_sync_body(w, f, &params, receiver, &err, ctx),
    }
    w.dedent();
    w.line("}");
}

/// The C arguments for `params`, after the receiver's object for an
/// instance method: a run-crossing value as the run its lending closure
/// binds (`{local}_run`), a callback interface as its staged argument struct
/// (`{local}_cb`), and anything else as its C value.
fn c_call_args(params: &[Param], receiver: Receiver, ctx: &SwiftCtx) -> Vec<String> {
    let mut args = Vec::new();
    if receiver == Receiver::Instance {
        args.push("self.ptr".to_string());
    }
    for p in params {
        let n = &p.local;
        args.push(match &p.binding.ty {
            ParamTy::Callback { .. } => format!("{n}_cb"),
            ParamTy::Value(ty) => {
                let c = crossing(ty);
                if c.is_run() {
                    format!("{n}_run")
                } else {
                    // The wrapper borrows its own reference for the call;
                    // Swift keeps every argument alive until it returns.
                    pass(&c, n, false, ctx)
                }
            }
        });
    }
    args
}

/// Emit the pre-call staging of callback parameters: each implementation is
/// retained for the library behind the `{p}_{Cb}` argument struct, whose
/// vtable's `free` entry releases it (also when the call fails). A `nil`
/// optional callback is the zero struct, whose null vtable means none.
fn render_arg_staging(w: &mut CodeWriter, params: &[Param], ctx: &SwiftCtx) {
    for p in params {
        let ParamTy::Callback { name, nullable } = &p.binding.ty else {
            continue;
        };
        let n = &p.local;
        let tag = ctx.model.callback(name).c_tag.clone();
        let vtable = format!("{}.shared.pointer", callback_vtable_name(name));
        if *nullable {
            w.line(format!(
                "let {n}_cb = {n}.map {{ {tag}(ctx: wvRetain($0), vtable: {vtable}) }} ?? {tag}()"
            ));
        } else {
            w.line(format!(
                "let {n}_cb = {tag}(ctx: wvRetain({n}), vtable: {vtable})"
            ));
        }
    }
}

/// Open the closures lending every run-crossing parameter (a string, bytes,
/// typed array, or value buffer), binding `{local}_run`. `bind` prefixes the
/// first opening line (`let rv: T = ` or nothing); nested ones return their
/// inner value implicitly. Returns how many closures were opened.
fn open_lending_closures(w: &mut CodeWriter, params: &[Param], bind: &str) -> usize {
    let mut opened = 0;
    for p in params {
        let Some(ty) = p.binding.ty.value() else {
            continue;
        };
        let Some(lend) = lender(&crossing(ty)) else {
            continue;
        };
        let n = &p.local;
        let prefix = if opened == 0 { bind } else { "" };
        w.line(format!("{prefix}{lend}({n}) {{ {n}_run in"));
        w.indent();
        opened += 1;
    }
    opened
}

/// Close `n` closures opened by [`open_lending_closures`].
fn close_closures(w: &mut CodeWriter, n: usize) {
    for _ in 0..n {
        w.dedent();
        w.line("}");
    }
}

/// The Swift type of the raw C result of a synchronous call (the one C
/// value of its return, or the iterator handle), which annotates its
/// binding when the call sits inside lending closures.
fn raw_return_swift(f: &FnBinding, ctx: &SwiftCtx) -> Option<String> {
    if f.iterator().is_some() {
        return Some("OpaquePointer?".to_string());
    }
    f.ret_value().map(|ty| ctx.c_value_type(&crossing(ty)))
}

/// Render the body of a synchronous (or iterator-returning) callable: the
/// error slot, callback staging, the C call wrapped in whatever lending
/// closures the inputs need, the error check, and the return conversion.
fn render_sync_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    params: &[Param],
    receiver: Receiver,
    err: &ErrCtx,
    ctx: &SwiftCtx,
) {
    w.line("var err = WvError()");
    render_arg_staging(w, params, ctx);
    let mut args = c_call_args(params, receiver, ctx);
    args.push("&err".to_string());
    let call = format!("{}({})", f.abi.symbol, args.join(", "));

    let raw = raw_return_swift(f, ctx);
    let bind = raw
        .as_ref()
        .map(|t| format!("let rv: {t} = "))
        .unwrap_or_default();
    let opened = open_lending_closures(w, params, &bind);
    if opened == 0 {
        if raw.is_some() {
            w.line(format!("let rv = {call}"));
        } else {
            w.line(call);
        }
    } else {
        w.line(call);
        close_closures(w, opened);
    }
    w.line(err.check_stmt());

    if let Some(it) = f.iterator() {
        render_sequence(w, it, err, ctx);
        return;
    }
    let Some(ty) = f.ret_value() else {
        return;
    };
    if receiver == Receiver::Init {
        // The constructor adopts the returned reference as its own.
        w.line("self.ptr = wvNonNull(rv)");
        return;
    }
    w.line(format!("return {}", receive(ty, "rv", true, ctx)));
}

/// Return the `NativeSequence` over the iterator handle in `rv`: its pull
/// closure calls `_next` once per element and receives the element like a
/// return of the same type.
fn render_sequence(w: &mut CodeWriter, it: &IteratorBinding, err: &ErrCtx, ctx: &SwiftCtx) {
    let elem = &it.elem;
    let c = crossing(elem);
    let mut value = receive(elem, "item", true, ctx);
    // An optional element must stay distinguishable from the end of the
    // stream (`nil`).
    if matches!(elem, Ty::Optional(_)) {
        value = format!(".some({value})");
    }
    w.line(format!(
        "return NativeSequence(wvNonNull(rv), release: {{ {}($0) }}, failure: {}) {{ handle, err in",
        it.destroy_symbol,
        err.sequence_failure()
    ));
    w.scope(|w| {
        w.line(format!(
            "var item: {} = {}",
            ctx.c_value_type(&c),
            ctx.c_zero(&c)
        ));
        w.line(format!(
            "guard {}(handle, &item, err) != 0 else {{ return nil }}",
            it.next.symbol
        ));
        w.line(format!("return {value}"));
    });
    w.line("}");
}

/// Render the body of an async callable: a checked continuation resumed
/// exactly once from the completion callback, wrapped in a cancellation
/// handler that cancels the native token for a cancellable callable.
fn render_async_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    binding: &AsyncBinding,
    params: &[Param],
    receiver: Receiver,
    err: &ErrCtx,
    ctx: &SwiftCtx,
) {
    let ret = f
        .ret
        .as_ref()
        .map_or_else(|| "Void".to_string(), |t| ctx.ret_type(t));
    let err_ty = if err.throws() { "Error" } else { "Never" };
    let cancellable = binding.cancellable;

    if cancellable {
        w.line("let token = WvCancelToken()");
        w.line("return try await withTaskCancellationHandler {");
        w.indent();
    }
    let ret_kw = if cancellable { "" } else { "return " };
    if err.throws() {
        w.line(format!(
            "{ret_kw}try await withCheckedThrowingContinuation {{ (continuation: CheckedContinuation<{ret}, Error>) in"
        ));
    } else {
        w.line(format!(
            "{ret_kw}await withCheckedContinuation {{ (continuation: CheckedContinuation<{ret}, Never>) in"
        ));
    }
    w.indent();

    // The library copies or retains every input during the launch, so
    // lending them for the launch call's duration is enough.
    render_arg_staging(w, params, ctx);
    w.line("let context = Unmanaged.passRetained(WvContinuation(continuation)).toOpaque()");
    let opened = open_lending_closures(w, params, "");

    let mut args = c_call_args(params, receiver, ctx);
    if cancellable {
        args.push("token.raw".to_string());
    }
    let (formals, value) = match f.ret_value() {
        Some(ty) => ("context, err, result", receive(ty, "result", true, ctx)),
        None => ("context, err", "()".to_string()),
    };
    let mut launch = args.join(", ");
    if !launch.is_empty() {
        launch.push_str(", ");
    }
    w.line(format!("{}({launch}{{ {formals} in", f.abi.symbol));
    w.indent();
    w.line(format!(
        "let cont = Unmanaged<WvContinuation<{ret}, {err_ty}>>.fromOpaque(context!).takeRetainedValue().value"
    ));
    w.line("if let err = err {");
    w.scope(|w| {
        for line in err.completion_failure() {
            w.line(line);
        }
    });
    w.line("}");
    w.line(format!("cont.resume(returning: {value})"));
    w.dedent();
    w.line("}, context)");
    close_closures(w, opened);
    w.dedent();
    w.line("}");
    if cancellable {
        w.dedent();
        w.line("} onCancel: {");
        w.scope(|w| {
            w.line("token.cancel()");
        });
        w.line("}");
    }
}
