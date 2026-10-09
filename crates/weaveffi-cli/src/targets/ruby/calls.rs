//! Callable rendering: FFI attachments, async completion trampolines, the
//! sync, async, and iterator wrapper bodies, and the parameter and return
//! marshalling they share.
//!
//! Marshalling dispatch goes through the shared plan layer ([`ArgPass`],
//! [`RetPass`]), so this backend can't drift from the others on
//! call-boundary semantics: strings, bytes, and value buffers cross as
//! borrowed `(ptr, len)` pairs, objects are borrowed as parameters (pinned
//! for the call through `_wv_pin`) and adopted as returns, and a callback
//! interface crosses as a handle-table key plus the interface's static
//! vtable (or two NULLs for an absent optional one).
//!
//! Every call into the library except the trivial runtime helpers releases
//! the GVL (`blocking: true`): a call may run for a long time, and a
//! producer thread may need the GVL to run a callback implementation while
//! the call is in flight.

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use heck::{ToShoutySnakeCase, ToSnakeCase};
use weaveffi_model::model::ParamBinding;
use weaveffi_model::model::{AsyncBinding, CallShape, ErrorBinding, FnBinding, IteratorBinding};
use weaveffi_model::plan::{ArgPass, ErrorStrategy, RetPass};
use weaveffi_model::ty::{Prim, Ty};

use crate::targets::ruby::callbacks::rb_vtable_const;
use crate::targets::ruby::docs::{emit_param_docs, emit_return_doc};
use crate::targets::ruby::entities::{rb_checker_name, rb_error_factory_name};
use crate::targets::ruby::types::{
    rb_abi_types, rb_direct_type, rb_ffi_type, rb_member_name, rb_param_name, rb_str_literal,
};
use crate::targets::ruby::RbCtx;

/// Where a rendered Ruby callable lives and how it's spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    /// A module-level free function (`def self.name` on the top-level module).
    Free,
    /// An instance method on an interface class: `def name`, borrowing the
    /// wrapper's own pointer as the leading C argument.
    Method,
    /// A static member of an interface class (`def self.name`).
    Static,
    /// A non-`new` constructor: a class method adopting the returned
    /// reference through `_from_ptr` (never re-running `initialize`).
    Factory,
    /// The canonical `new` constructor, emitted as `initialize`.
    Init,
}

/// The scope of one rendered callable: its kind, the top-level Ruby module,
/// and (for an interface member) the class it belongs to.
pub(crate) struct RbScope<'a> {
    /// Where the callable lives.
    pub(crate) kind: ScopeKind,
    /// The top-level Ruby module name.
    pub(crate) module: &'a str,
    /// The interface class of a member; `None` for a free function.
    pub(crate) class: Option<&'a str>,
}

impl<'a> RbScope<'a> {
    /// The scope of a free function.
    pub(crate) fn free(module: &'a str) -> Self {
        RbScope {
            kind: ScopeKind::Free,
            module,
            class: None,
        }
    }

    /// The scope of an interface member of the given kind.
    pub(crate) fn member(kind: ScopeKind, module: &'a str, class: &'a str) -> Self {
        RbScope {
            kind,
            module,
            class: Some(class),
        }
    }

    /// The receiver prefix for module singleton calls (attached C symbols,
    /// error checkers, runtime helpers): `"{ModuleName}."` inside a class
    /// body, empty at module scope, where `self` already is the module.
    fn qualifier(&self) -> String {
        match self.kind {
            ScopeKind::Free => String::new(),
            _ => format!("{}.", self.module),
        }
    }

    /// The Ruby name of `f` in this scope: snake_case, escaped away from
    /// the wrapper's own methods for an interface member.
    fn name(&self, f: &FnBinding) -> String {
        match self.kind {
            ScopeKind::Method => rb_member_name(&f.name, true),
            ScopeKind::Static | ScopeKind::Factory => rb_member_name(&f.name, false),
            ScopeKind::Free | ScopeKind::Init => f.name.to_snake_case(),
        }
    }

    /// The `def` opener for `f` with the given formal parameters.
    fn def_open(&self, f: &FnBinding, params: &[String]) -> String {
        let args = if params.is_empty() {
            String::new()
        } else {
            format!("({})", params.join(", "))
        };
        let name = self.name(f);
        match self.kind {
            ScopeKind::Method => format!("def {name}{args}"),
            ScopeKind::Free | ScopeKind::Static | ScopeKind::Factory => {
                format!("def self.{name}{args}")
            }
            ScopeKind::Init => format!("def initialize{args}"),
        }
    }

    /// How a caller spells `f`: `Kvstore.open_store`, `Kvstore::Store#size`,
    /// `Kvstore::Store.open`, or `Kvstore::Store.new`.
    fn display(&self, f: &FnBinding) -> String {
        let name = self.name(f);
        let module = self.module;
        match (self.kind, self.class) {
            (ScopeKind::Method, Some(class)) => format!("{module}::{class}#{name}"),
            (ScopeKind::Init, Some(class)) => format!("{module}::{class}.new"),
            (_, Some(class)) => format!("{module}::{class}.{name}"),
            (_, None) => format!("{module}.{name}"),
        }
    }
}

/// Render one callable: a free function or an interface member. `error` is
/// the domain in scope for throwing callables; `scope` picks the def
/// spelling, receiver, and result handling.
pub(crate) fn render_callable(
    w: &mut CodeWriter,
    ctx: &RbCtx,
    error: Option<&ErrorBinding>,
    f: &FnBinding,
    scope: &RbScope,
) {
    w.blank();
    w.doc(&f.doc, DocCommentStyle::Hash);
    match &f.shape {
        CallShape::Sync(_) => {}
        CallShape::Async(_) => {
            w.line("# Blocks until the call completes on a producer thread.");
        }
        CallShape::Iterator(_) => {
            w.line("# Returns a lazy Enumerator that pulls one element per step. The");
            w.line("# producer iterator starts on the first pull and is released when");
            w.line("# iteration finishes or is abandoned early.");
        }
    }
    emit_param_docs(w, &f.params);
    if f.cancellable {
        w.line("# @param cancel [CancelToken, nil] cancels the call; it then raises Cancelled");
    }
    if scope.kind != ScopeKind::Init {
        emit_return_doc(w, f.ret.as_ref());
    }
    if let Some(msg) = &f.deprecated {
        w.line(format!("# @deprecated {msg}"));
    }
    match &f.shape {
        CallShape::Sync(abi) => render_sync(w, ctx, error, f, &abi.symbol, scope),
        CallShape::Async(a) => render_async(w, ctx, f, a, scope),
        CallShape::Iterator(it) => render_iterator(w, ctx, error, f, it, scope),
    }
}

/// Attach the C symbols for one callable: the plain symbol for a sync shape,
/// the launcher for an async shape (its completion trampoline is rendered
/// by [`render_async_trampoline`]), and the launch/next/destroy triple for
/// an iterator.
///
/// Calls release the GVL (`blocking: true`) so a long call doesn't stall
/// other Ruby threads and a producer thread can run a callback
/// implementation while the call is in flight; an iterator's `_destroy`,
/// like an object's `_clone` and `_destroy`, is a trivial helper that keeps
/// it (and may therefore run from a GC finalizer).
pub(crate) fn render_attach_function(w: &mut CodeWriter, f: &FnBinding) {
    match &f.shape {
        CallShape::Sync(abi) => {
            w.line(format!(
                "attach_function :{}, [{}], {}, blocking: true",
                abi.symbol,
                rb_abi_types(&abi.params).join(", "),
                rb_ffi_type(&abi.ret),
            ));
        }
        CallShape::Async(a) => {
            w.line(format!(
                "attach_function :{}, [{}], :void, blocking: true",
                a.launch.symbol,
                rb_abi_types(&a.launch.params).join(", ")
            ));
        }
        CallShape::Iterator(it) => {
            w.line(format!(
                "attach_function :{}, [{}], :pointer, blocking: true",
                it.launch.symbol,
                rb_abi_types(&it.launch.params).join(", ")
            ));
            w.line(format!(
                "attach_function :{}, [{}], :int32, blocking: true",
                it.next.symbol,
                rb_abi_types(&it.next.params).join(", ")
            ));
            w.line(format!(
                "attach_function :{}, [:pointer], :void",
                it.destroy_symbol,
            ));
        }
    }
}

/// The constant pinning an async function's completion trampoline, named
/// after its C callback typedef: `kvstore_kv_Store_compact_callback` becomes
/// `KVSTORE_KV_STORE_COMPACT_CALLBACK`.
fn rb_async_const(a: &AsyncBinding) -> String {
    a.callback_type.to_shouty_snake_case()
}

/// The error a completion's `taken` triple becomes: the domain error for a
/// throwing function, the trap (`NativeBugError`, or `Cancelled`)
/// otherwise.
fn rb_error_from_taken(error: Option<&ErrorBinding>, f: &FnBinding) -> String {
    match (f.error_strategy(), error) {
        (ErrorStrategy::Throws, Some(eb)) => format!("{}(*taken)", rb_error_factory_name(eb)),
        _ => "_wv_trap(*taken)".to_string(),
    }
}

/// Render the module-level completion trampoline of an async function (a
/// no-op for any other shape). It resolves the call's queue from
/// `context`, converts the error or result (copying and releasing any owned
/// buffer), and pushes it; nothing raised while converting escapes into the
/// C frame, so the waiting call always wakes up.
pub(crate) fn render_async_trampoline(
    w: &mut CodeWriter,
    ctx: &RbCtx,
    error: Option<&ErrorBinding>,
    f: &FnBinding,
) {
    let CallShape::Async(a) = &f.shape else {
        return;
    };
    let mut formals = vec!["ctx".to_string(), "err".to_string()];
    formals.extend(a.callback_params.iter().skip(2).map(|p| p.name.clone()));
    w.blank();
    w.line("# @api private");
    w.line(format!("# Completion trampoline for {}.", f.name));
    w.block(
        format!(
            "{} = FFI::Function.new(:void, [{}]) do |{}|",
            rb_async_const(a),
            rb_abi_types(&a.callback_params).join(", "),
            formals.join(", ")
        ),
        "end",
        |w| {
            w.line("queue = _wv_async_finish(ctx)");
            w.line("begin");
            w.scope(|w| {
                w.line("taken = _wv_take_boxed_error(err)");
                w.line("if taken");
                w.scope(|w| {
                    w.line(format!("queue << {}", rb_error_from_taken(error, f)));
                });
                w.line("else");
                w.scope(|w| {
                    let ptr = match RetPass::of(f.ret.as_ref()) {
                        RetPass::String | RetPass::Bytes | RetPass::Buffer => "result_ptr",
                        _ => "result",
                    };
                    let value = receive_value(ctx, f.ret.as_ref(), ptr, "result_len", "");
                    w.line(format!("value = {value}"));
                    w.line("queue << value");
                });
                w.line("end");
            });
            w.line("rescue Exception => e # rubocop:disable Lint/RescueException");
            w.scope(|w| {
                w.line("queue << e");
            });
            w.line("end");
        },
    );
}

/// The expression turning a received value into its Ruby value: a sync
/// return (`ptr` names the C result, `len` the length read from `out_len`),
/// an async result, or an iterator element. Owned strings, bytes, and
/// buffers are copied and released; objects are adopted.
fn receive_value(ctx: &RbCtx, ty: Option<&Ty>, ptr: &str, len: &str, q: &str) -> String {
    match RetPass::of(ty) {
        RetPass::Void => "nil".to_string(),
        RetPass::Direct => ptr.to_string(),
        RetPass::String => format!("{q}_wv_take_string({ptr}, {len})"),
        RetPass::Bytes => format!("{q}_wv_take_bytes({ptr}, {len})"),
        RetPass::Buffer => {
            let ty = ty.expect("buffered value has a type");
            format!(
                "{q}_wv_decode({ptr}, {len}) {{ |r| {} }}",
                ctx.codecs.read(ty, "r", q)
            )
        }
        RetPass::Object { nullable } => {
            let class = ty
                .and_then(Ty::interface_name)
                .expect("object value names an interface");
            if nullable {
                format!("{ptr}.null? ? nil : {class}._from_ptr({ptr})")
            } else {
                format!("{class}._from_ptr({q}_wv_nonnull({ptr}))")
            }
        }
    }
}

/// The objects a call borrows, as `(Ruby expression, pinned pointer local)`
/// pairs: the receiver for an instance method, then each object parameter.
fn pinned_objects(f: &FnBinding, scope: &RbScope) -> Vec<(String, String)> {
    let mut pins = Vec::new();
    if scope.kind == ScopeKind::Method {
        pins.push(("self".to_string(), "_wv_self".to_string()));
    }
    for p in &f.params {
        if matches!(p.arg_pass(), ArgPass::Object { .. }) {
            let name = rb_param_name(&p.name);
            pins.push((name.clone(), format!("_wv_{name}")));
        }
    }
    pins
}

/// Run `body` inside a `_wv_pin` block over `pins` (each object stays alive,
/// and its wrapper can't release its reference, until the block returns),
/// or directly when nothing is borrowed. `lead` prefixes the block opener
/// (an assignment of the block's value, or empty).
fn with_pins(
    w: &mut CodeWriter,
    q: &str,
    lead: &str,
    pins: &[(String, String)],
    body: impl FnOnce(&mut CodeWriter),
) {
    if pins.is_empty() {
        body(w);
        return;
    }
    let exprs: Vec<&str> = pins.iter().map(|(e, _)| e.as_str()).collect();
    let vars: Vec<&str> = pins.iter().map(|(_, v)| v.as_str()).collect();
    w.block(
        format!(
            "{lead}{q}_wv_pin({}) do |{}|",
            exprs.join(", "),
            vars.join(", ")
        ),
        "end",
        body,
    );
}

/// Emit the statements validating the parameters and converting them into
/// the locals the C call borrows. Strings and bytes become private binary
/// copies and buffered values are encoded; object arguments are checked
/// against their interface and a required callback implementation against
/// `nil`. Callback implementations are registered later (see
/// [`render_handoff`]), so nothing that can raise runs between a
/// registration and the call that hands it to the producer.
fn render_param_prep(w: &mut CodeWriter, ctx: &RbCtx, f: &FnBinding, q: &str) {
    for p in &f.params {
        let name = rb_param_name(&p.name);
        match p.arg_pass() {
            ArgPass::String { .. } => {
                w.line(format!("{name}_s = {q}_wv_str({name})"));
            }
            ArgPass::Bytes { .. } => {
                w.line(format!("{name}_s = {q}_wv_bytes({name})"));
            }
            ArgPass::Buffer { .. } if ctx.codecs.carries_objects(&p.ty) => {
                w.line(format!("{name}_w = WvBufferWriter.new"));
                w.line(ctx.codecs.write(&p.ty, &format!("{name}_w"), &name, q));
            }
            ArgPass::Buffer { .. } => {
                w.line(format!(
                    "{name}_s = {q}_wv_encode {{ |w| {} }}",
                    ctx.codecs.write(&p.ty, "w", &name, q)
                ));
            }
            ArgPass::Object { nullable, .. } => {
                let class =
                    p.ty.interface_name()
                        .expect("object plan names an interface");
                let optional = if nullable { ", true" } else { "" };
                w.line(format!(
                    "{q}_wv_object!({name}, {class}, '{name}'{optional})"
                ));
            }
            ArgPass::Callback {
                nullable: false, ..
            } => {
                w.line(format!("{q}_wv_present!({name}, '{name}')"));
            }
            ArgPass::Callback { .. } | ArgPass::Direct { .. } => {}
        }
    }
}

/// Emit the statements that hand the producer what it adopts, immediately
/// before the call: seal the object-carrying buffers (minting the
/// references their tokens carry) and then register every callback
/// implementation. Registration can't raise, so nothing between here and
/// the call can strand a minted reference or a registration.
fn render_handoff(w: &mut CodeWriter, ctx: &RbCtx, f: &FnBinding, q: &str) {
    let sealed: Vec<String> = f
        .params
        .iter()
        .filter(|p| matches!(p.arg_pass(), ArgPass::Buffer { .. }))
        .filter(|p| ctx.codecs.carries_objects(&p.ty))
        .map(|p| rb_param_name(&p.name))
        .collect();
    match sealed.as_slice() {
        [] => {}
        [one] => {
            w.line(format!("{one}_s = {q}_wv_seal({one}_w).first"));
        }
        many => {
            let locals: Vec<String> = many.iter().map(|n| format!("{n}_s")).collect();
            let writers: Vec<String> = many.iter().map(|n| format!("{n}_w")).collect();
            w.line(format!(
                "{} = {q}_wv_seal({})",
                locals.join(", "),
                writers.join(", ")
            ));
        }
    }
    for p in &f.params {
        if matches!(p.arg_pass(), ArgPass::Callback { .. }) {
            let name = rb_param_name(&p.name);
            let cb =
                p.ty.callback_interface_name()
                    .expect("callback plan names a callback interface");
            w.line(format!(
                "{name}_ctx, {name}_vtable = {q}_wv_cb_register({name}, {})",
                rb_vtable_const(cb)
            ));
        }
    }
}

/// The Ruby argument expressions one parameter contributes to the C call,
/// per its [`ArgPass`] contract.
fn rb_call_args(p: &ParamBinding) -> Vec<String> {
    let name = rb_param_name(&p.name);
    match p.arg_pass() {
        ArgPass::String { .. } | ArgPass::Bytes { .. } | ArgPass::Buffer { .. } => {
            vec![format!("{name}_s"), format!("{name}_s.bytesize")]
        }
        ArgPass::Object { .. } => vec![format!("_wv_{name}")],
        ArgPass::Callback { .. } => vec![format!("{name}_ctx"), format!("{name}_vtable")],
        ArgPass::Direct { .. } if matches!(p.ty, Ty::Prim(Prim::Bool)) => {
            vec![format!("({name} ? true : false)")]
        }
        ArgPass::Direct { .. } => vec![name],
    }
}

/// The leading C arguments of a call: the pinned receiver, then every
/// parameter's slots.
fn call_args(f: &FnBinding, scope: &RbScope) -> Vec<String> {
    let mut args = Vec::new();
    if scope.kind == ScopeKind::Method {
        args.push("_wv_self".to_string());
    }
    for p in &f.params {
        args.extend(rb_call_args(p));
    }
    args
}

/// The Ruby formal parameters of `f`. Trailing optional parameters (`T?`,
/// `Cb?`) default to `nil`, so callers may leave them out.
fn formals(f: &FnBinding) -> Vec<String> {
    let optional = f
        .params
        .iter()
        .rev()
        .take_while(|p| matches!(p.ty, Ty::Optional(_)))
        .count();
    let first_optional = f.params.len() - optional;
    f.params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let name = rb_param_name(&p.name);
            if i >= first_optional {
                format!("{name} = nil")
            } else {
                name
            }
        })
        .collect()
}

/// Warn, pointing at the caller, when a deprecated callable is used (the
/// warning is silenced with `$VERBOSE = nil`, like any Ruby warning).
fn render_deprecation(w: &mut CodeWriter, f: &FnBinding, scope: &RbScope) {
    if let Some(msg) = &f.deprecated {
        w.line(format!(
            "warn('{} is deprecated: {}', uplevel: 1)",
            scope.display(f),
            rb_str_literal(msg)
        ));
    }
}

/// Render the sync wrapper: convert the parameters, pin the borrowed
/// objects, make the C call with a fresh `ErrorStruct`, route the out-err
/// slot through the function's checker, then receive the result per the
/// scope.
fn render_sync(
    w: &mut CodeWriter,
    ctx: &RbCtx,
    error: Option<&ErrorBinding>,
    f: &FnBinding,
    symbol: &str,
    scope: &RbScope,
) {
    let q = scope.qualifier();
    let checker = rb_checker_name(f, error);
    let has_out_len = matches!(
        RetPass::of(f.ret.as_ref()),
        RetPass::String | RetPass::Bytes | RetPass::Buffer
    );
    w.block(scope.def_open(f, &formals(f)), "end", |w| {
        render_deprecation(w, f, scope);
        render_param_prep(w, ctx, f, &q);
        with_pins(w, &q, "", &pinned_objects(f, scope), |w| {
            w.line("err = ErrorStruct.new");
            let mut args = call_args(f, scope);
            if has_out_len {
                w.line("out_len = FFI::MemoryPointer.new(:size_t)");
                args.push("out_len".into());
            }
            args.push("err".into());
            render_handoff(w, ctx, f, &q);
            let call = format!("{q}{symbol}({})", args.join(", "));
            if f.ret.is_some() {
                w.line(format!("result = {call}"));
            } else {
                w.line(call);
            }
            w.line(format!("{q}{checker}(err)"));
            match scope.kind {
                ScopeKind::Init => {
                    w.line(format!("_wv_init({q}_wv_nonnull(result))"));
                }
                ScopeKind::Factory => {
                    w.line(format!("_from_ptr({q}_wv_nonnull(result))"));
                }
                _ if f.ret.is_some() => {
                    w.line(receive_value(
                        ctx,
                        f.ret.as_ref(),
                        "result",
                        "out_len.read(:size_t)",
                        &q,
                    ));
                }
                _ => {
                    w.line("nil");
                }
            }
        });
    });
}

/// Render the async wrapper: launch the call with the function's completion
/// trampoline and block on a `Queue` until it fires (`Queue#pop` releases
/// the GVL; the ffi gem runs the trampoline on a Ruby thread). Blocking is
/// the idiomatic Ruby surface; callers wanting concurrency run the call in a
/// Thread.
///
/// A cancellable function takes a `cancel:` keyword. Without one, the
/// wrapper still passes a private token, so interrupting the wait
/// (`Thread#raise`, `Timeout`) cancels the producer's work too.
fn render_async(w: &mut CodeWriter, ctx: &RbCtx, f: &FnBinding, a: &AsyncBinding, scope: &RbScope) {
    let q = scope.qualifier();
    let mut params = formals(f);
    if f.cancellable {
        params.push("cancel: nil".into());
    }
    w.block(scope.def_open(f, &params), "end", |w| {
        render_deprecation(w, f, scope);
        render_param_prep(w, ctx, f, &q);
        if f.cancellable {
            w.line("own_token = CancelToken.new if cancel.nil?");
            w.line("token = cancel || own_token");
        }
        let mut args = call_args(f, scope);
        if f.cancellable {
            args.push("token._wv_ptr".into());
        }
        args.push(rb_async_const(a));
        args.push("ctx".into());
        let launch = format!("{q}{}({})", a.launch.symbol, args.join(", "));
        let pins = pinned_objects(f, scope);
        if pins.is_empty() {
            render_handoff(w, ctx, f, &q);
            w.line(format!("ctx, queue = {q}_wv_async_begin"));
            w.line(launch);
        } else {
            with_pins(w, &q, "queue = ", &pins, |w| {
                render_handoff(w, ctx, f, &q);
                w.line(format!("ctx, pending = {q}_wv_async_begin"));
                w.line(launch);
                w.line("pending");
            });
        }
        if f.cancellable {
            w.line(format!("{q}_wv_async_wait(queue, token)"));
            w.dedent();
            w.line("ensure");
            w.indent();
            w.line("own_token&.close");
        } else {
            w.line(format!("{q}_wv_async_wait(queue)"));
        }
    });
}

/// Render the iterator wrapper: a lazy `Enumerator` per the pull contract of
/// [`weaveffi_model::plan::IteratorProtocol`].
///
/// The producer iterator launches inside the enumerator block, on the first
/// pull, so nothing leaks when the enumerator is never started (launch
/// errors therefore raise on the first pull). Each step issues exactly one
/// `next` call, each element is received like a return of its type, and
/// `destroy` runs exactly once from an `ensure`, so an early `break` or an
/// error mid-iteration still releases the handle.
fn render_iterator(
    w: &mut CodeWriter,
    ctx: &RbCtx,
    error: Option<&ErrorBinding>,
    f: &FnBinding,
    it: &IteratorBinding,
    scope: &RbScope,
) {
    let q = scope.qualifier();
    let checker = rb_checker_name(f, error);
    let elem_pass = RetPass::of(Some(&it.elem));
    let needs_len = matches!(
        elem_pass,
        RetPass::String | RetPass::Bytes | RetPass::Buffer
    );
    w.block(scope.def_open(f, &formals(f)), "end", |w| {
        render_deprecation(w, f, scope);
        render_param_prep(w, ctx, f, &q);
        // The block closes over the converted arguments, so they stay
        // referenced until the launch call runs.
        w.block("Enumerator.new do |y|", "end", |w| {
            w.line("err = ErrorStruct.new");
            let mut args = call_args(f, scope);
            args.push("err".into());
            let launch = format!("{q}{}({})", it.launch.symbol, args.join(", "));
            let pins = pinned_objects(f, scope);
            if pins.is_empty() {
                render_handoff(w, ctx, f, &q);
                w.line(format!("iter = {launch}"));
            } else {
                with_pins(w, &q, "iter = ", &pins, |w| {
                    render_handoff(w, ctx, f, &q);
                    w.line(launch);
                });
            }
            w.line("begin");
            w.scope(|w| {
                w.line(format!("{q}{checker}(err)"));
                let item_type = match elem_pass {
                    RetPass::Direct => rb_direct_type(&it.elem),
                    _ => ":pointer",
                };
                w.line(format!("out_item = FFI::MemoryPointer.new({item_type})"));
                let mut next_args = vec!["iter", "out_item"];
                if needs_len {
                    w.line("out_len = FFI::MemoryPointer.new(:size_t)");
                    next_args.push("out_len");
                }
                next_args.push("err");
                w.block("loop do", "end", |w| {
                    w.line(format!(
                        "has_item = {q}{}({})",
                        it.next.symbol,
                        next_args.join(", ")
                    ));
                    w.line(format!("{q}{checker}(err)"));
                    w.line("break if has_item.zero?");
                    let item = match elem_pass {
                        RetPass::Direct => format!("out_item.read({item_type})"),
                        _ => "out_item.read_pointer".to_string(),
                    };
                    w.line(format!("item = {item}"));
                    w.line(format!(
                        "value = {}",
                        receive_value(ctx, Some(&it.elem), "item", "out_len.read(:size_t)", &q)
                    ));
                    w.line("y << value");
                });
            });
            w.line("ensure");
            w.scope(|w| {
                w.line(format!("{q}{}(iter) unless iter.null?", it.destroy_symbol));
            });
            w.line("end");
        });
    });
}
