//! Callable rendering: sync, async, and iterator-returning wrapper
//! functions.
//!
//! How a value crosses is read from the model's passing contracts
//! ([`ArgPass`], [`RetPass`], [`ResultPass`], [`ItemPass`]) and how a
//! failure surfaces from its [`ErrorStrategy`]; this module only renders the
//! Swift spelling.

use crate::codegen::CodeWriter;
use crate::lang;
use weaveffi_model::model::{AsyncBinding, FnBinding, IteratorBinding, ParamBinding};
use weaveffi_model::plan::{ArgPass, ErrorStrategy, ItemPass, ResultPass, RetPass};
use weaveffi_model::ty::{Prim, RetTy, Ty};

use crate::targets::swift::docs::emit_fn_doc;
use crate::targets::swift::types::{callback_vtable_name, prim_swift_type, swift_ident, SwiftCtx};

/// Locals a wrapper body declares next to its parameters. A parameter
/// spelled like one keeps its argument label and gets a `_`-suffixed local
/// name (`err err_: Int32`), so the body never confuses the two.
const BODY_LOCALS: &[&str] = &[
    "err",
    "rv",
    "outLen",
    "outValue",
    "token",
    "context",
    "continuation",
];

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
            domain_type: f
                .error
                .domain()
                .map(|d| ctx.ty_name(&ctx.model.error_domain(d).type_name)),
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

    /// The statement a completion runs for its boxed error `err`: resume
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
/// `swift_name`.
pub(crate) fn render_callable(
    w: &mut CodeWriter,
    f: &FnBinding,
    swift_name: &str,
    receiver: Receiver,
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
        let static_kw = if receiver == Receiver::Instance {
            ""
        } else {
            "static "
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

/// The Swift spelling of the C type of a direct scalar or C-style enum
/// (`Int32` for an enum, whose C type is an `int32_t` typedef), and its
/// zero value.
fn c_scalar(t: &Ty) -> (&'static str, &'static str) {
    match t {
        Ty::Prim(Prim::Bool) => ("Bool", "false"),
        Ty::Prim(p) => (prim_swift_type(*p), "0"),
        _ => ("Int32", "0"),
    }
}

/// The value type inside an optional (`T` of `T?`).
fn unwrap_optional(t: &Ty) -> &Ty {
    match t {
        Ty::Optional(inner) => inner,
        other => other,
    }
}

/// How a value the library hands over is received.
pub(crate) enum Recv<'a> {
    /// A scalar or C-style enum discriminant, by value.
    Direct,
    /// An optional scalar: the value and the expression saying it's present.
    Opt(&'a str),
    /// A typed array of `len` elements.
    Slice,
    /// UTF-8 bytes.
    String,
    /// Raw bytes.
    Bytes,
    /// A value buffer.
    Buffer,
    /// An object reference the wrapper adopts.
    Object {
        /// `true` for `I?`.
        nullable: bool,
        /// The interface's name.
        interface: &'a str,
    },
}

/// The Swift expression receiving a value of type `ty` that the library
/// handed over in `raw` (and `len`): `owned` values (returns, async
/// results, iterator elements) are released after copying, borrowed ones
/// (callback arguments) are only copied. An object argument is always
/// adopted.
pub(crate) fn receive(
    ty: &Ty,
    recv: &Recv,
    raw: &str,
    len: &str,
    owned: bool,
    ctx: &SwiftCtx,
) -> String {
    let take = if owned { "wvTake" } else { "wvBorrow" };
    match recv {
        Recv::Direct => match ty {
            Ty::Enum(name) => format!("wvEnumCase({}.self, {raw})", ctx.ty_name(name)),
            _ => raw.to_string(),
        },
        Recv::Opt(has) => {
            let value = receive(unwrap_optional(ty), &Recv::Direct, raw, len, owned, ctx);
            format!("{has} ? {value} : nil")
        }
        Recv::Slice => format!("{take}Slice({raw}, {len})"),
        Recv::String => format!("{take}String({raw}, {len})"),
        Recv::Bytes => format!("{take}Bytes({raw}, {len})"),
        Recv::Buffer => format!(
            "{take}Buffer({raw}, {len}, as: {}.self)",
            ctx.swift_type(ty)
        ),
        Recv::Object {
            nullable,
            interface,
        } => {
            let class = ctx.ty_name(interface);
            if *nullable {
                format!("{raw}.map {{ {class}(ptr: $0) }}")
            } else {
                format!("{class}(ptr: wvNonNull({raw}))")
            }
        }
    }
}

/// The C arguments for `params`, after the receiver's object for an
/// instance method.
fn c_call_args(params: &[Param], self_arg: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = self_arg.into_iter().map(str::to_string).collect();
    for p in params {
        let n = &p.local;
        match &p.binding.pass {
            ArgPass::Direct { .. } => match p.binding.ty.value() {
                Some(Ty::Enum(_)) => args.push(format!("{n}.rawValue")),
                _ => args.push(n.clone()),
            },
            ArgPass::OptDirect { inner, .. } => {
                args.push(format!("{n} != nil"));
                args.push(match inner {
                    Ty::Enum(_) => format!("{n}?.rawValue ?? 0"),
                    other => format!("{n} ?? {}", c_scalar(other).1),
                });
            }
            ArgPass::Slice { .. }
            | ArgPass::String { .. }
            | ArgPass::Bytes { .. }
            | ArgPass::Buffer { .. } => {
                args.push(format!("{n}_ptr"));
                args.push(format!("{n}_len"));
            }
            // The wrapper borrows its own reference for the call; Swift
            // keeps every argument alive until the wrapper returns.
            ArgPass::Object { nullable, .. } => {
                args.push(if *nullable {
                    format!("{n}?.ptr")
                } else {
                    format!("{n}.ptr")
                });
            }
            ArgPass::Callback {
                nullable,
                interface,
                ..
            } => {
                let vtable = format!("{}.shared.pointer", callback_vtable_name(interface));
                args.push(format!("{n}_ctx"));
                args.push(if *nullable {
                    format!("{n} == nil ? nil : {vtable}")
                } else {
                    vtable
                });
            }
        }
    }
    args
}

/// Emit the pre-call staging of callback parameters: each implementation is
/// retained for the library, whose vtable `free` entry releases it (also
/// when the call fails).
fn render_arg_staging(w: &mut CodeWriter, params: &[Param]) {
    for p in params {
        if let ArgPass::Callback { nullable, .. } = &p.binding.pass {
            let n = &p.local;
            if *nullable {
                w.line(format!("let {n}_ctx = {n}.map {{ wvRetain($0) }}"));
            } else {
                w.line(format!("let {n}_ctx = wvRetain({n})"));
            }
        }
    }
}

/// Open the closures lending the storage of every string, bytes, typed
/// array, and buffered parameter, binding `{local}_ptr`/`{local}_len`.
/// `bind` prefixes the first opening line (`let rv: T = ` or nothing);
/// nested ones return their inner value implicitly. Returns how many
/// closures were opened.
fn open_lending_closures(w: &mut CodeWriter, params: &[Param], bind: &str) -> usize {
    let mut opened = 0;
    for p in params {
        let n = &p.local;
        let open = match &p.binding.pass {
            ArgPass::String { .. } => format!("wvWithUTF8({n})"),
            ArgPass::Bytes { .. } => format!("wvWithBytes({n})"),
            ArgPass::Slice { .. } => format!("wvWithSlice({n})"),
            ArgPass::Buffer { .. } => format!("wvWithEncoded({n})"),
            _ => continue,
        };
        let prefix = if opened == 0 { bind } else { "" };
        w.line(format!("{prefix}{open} {{ {n}_ptr, {n}_len in"));
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

/// The Swift type of the raw C result of a synchronous call, which
/// annotates its binding when the call sits inside lending closures.
fn raw_return_swift(f: &FnBinding) -> Option<String> {
    let ret = f.ret.as_ref().map(RetTy::elem);
    Some(match &f.ret_pass {
        RetPass::Void => return None,
        RetPass::Direct => c_scalar(ret?).0.to_string(),
        RetPass::OptDirect { .. } => "Bool".to_string(),
        RetPass::Slice { elem, .. } => {
            format!("UnsafeMutablePointer<{}>?", prim_swift_type(*elem))
        }
        RetPass::String { .. } | RetPass::Bytes { .. } | RetPass::Buffer { .. } => {
            "UnsafePointer<UInt8>?".to_string()
        }
        RetPass::Object { .. } | RetPass::Iterator(_) => "OpaquePointer?".to_string(),
    })
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
    render_arg_staging(w, params);
    let ret = f.ret.as_ref().map(RetTy::elem);
    let mut args = c_call_args(
        params,
        (receiver == Receiver::Instance).then_some("self.ptr"),
    );
    match &f.ret_pass {
        RetPass::Slice { .. }
        | RetPass::String { .. }
        | RetPass::Bytes { .. }
        | RetPass::Buffer { .. } => {
            w.line("var outLen = 0");
            args.push("&outLen".to_string());
        }
        RetPass::OptDirect { .. } => {
            let (ty, zero) = c_scalar(unwrap_optional(ret.expect("an optional return")));
            w.line(format!("var outValue: {ty} = {zero}"));
            args.push("&outValue".to_string());
        }
        _ => {}
    }
    args.push("&err".to_string());
    let call = format!("{}({})", f.abi.symbol, args.join(", "));

    let raw = raw_return_swift(f);
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

    let value = match &f.ret_pass {
        RetPass::Void => return,
        // The constructor adopts the returned reference as its own.
        _ if receiver == Receiver::Init => {
            w.line("self.ptr = wvNonNull(rv)");
            return;
        }
        RetPass::Iterator(it) => {
            render_sequence(w, it, err, ctx);
            return;
        }
        RetPass::Direct => receive(
            ret.expect("a direct return"),
            &Recv::Direct,
            "rv",
            "",
            true,
            ctx,
        ),
        RetPass::OptDirect { .. } => receive(
            ret.expect("an optional return"),
            &Recv::Opt("rv"),
            "outValue",
            "",
            true,
            ctx,
        ),
        RetPass::Slice { .. } => "wvTakeSlice(rv, outLen)".to_string(),
        RetPass::String { .. } => "wvTakeString(rv, outLen)".to_string(),
        RetPass::Bytes { .. } => "wvTakeBytes(rv, outLen)".to_string(),
        RetPass::Buffer { .. } => receive(
            ret.expect("a buffered return"),
            &Recv::Buffer,
            "rv",
            "outLen",
            true,
            ctx,
        ),
        RetPass::Object {
            nullable,
            interface,
            ..
        } => receive(
            ret.expect("an object return"),
            &Recv::Object {
                nullable: *nullable,
                interface,
            },
            "rv",
            "",
            true,
            ctx,
        ),
    };
    w.line(format!("return {value}"));
}

/// Return the `NativeSequence` over the iterator handle in `rv`: its pull
/// closure calls `_next` once per element and receives the element like a
/// return of the same type.
fn render_sequence(w: &mut CodeWriter, it: &IteratorBinding, err: &ErrCtx, ctx: &SwiftCtx) {
    let elem = &it.elem;
    let mut decls = Vec::new();
    let (args, recv, len) = match &it.item {
        ItemPass::Direct { .. } => {
            let (ty, zero) = c_scalar(elem);
            decls.push(format!("var item: {ty} = {zero}"));
            ("&item", Recv::Direct, "")
        }
        ItemPass::OptDirect { .. } => {
            let (ty, zero) = c_scalar(unwrap_optional(elem));
            decls.push("var hasItem = false".to_string());
            decls.push(format!("var item: {ty} = {zero}"));
            ("&hasItem, &item", Recv::Opt("hasItem"), "")
        }
        ItemPass::Slice { elem: prim, .. } => {
            decls.push(format!(
                "var item: UnsafeMutablePointer<{}>? = nil",
                prim_swift_type(*prim)
            ));
            decls.push("var itemLen = 0".to_string());
            ("&item, &itemLen", Recv::Slice, "itemLen")
        }
        ItemPass::String { .. } | ItemPass::Bytes { .. } | ItemPass::Buffer { .. } => {
            decls.push("var item: UnsafePointer<UInt8>? = nil".to_string());
            decls.push("var itemLen = 0".to_string());
            let recv = match &it.item {
                ItemPass::String { .. } => Recv::String,
                ItemPass::Bytes { .. } => Recv::Bytes,
                _ => Recv::Buffer,
            };
            ("&item, &itemLen", recv, "itemLen")
        }
        ItemPass::Object {
            nullable,
            interface,
            ..
        } => {
            decls.push("var item: OpaquePointer? = nil".to_string());
            (
                "&item",
                Recv::Object {
                    nullable: *nullable,
                    interface,
                },
                "",
            )
        }
    };
    let mut value = receive(elem, &recv, "item", len, true, ctx);
    // An optional element must stay distinguishable from the end of the
    // stream (`nil`).
    if matches!(recv, Recv::Opt(_) | Recv::Object { nullable: true, .. }) {
        value = format!(".some({value})");
    }
    w.line(format!(
        "return NativeSequence(wvNonNull(rv), release: {{ {}($0) }}, failure: {}) {{ handle, err in",
        it.destroy_symbol,
        err.sequence_failure()
    ));
    w.scope(|w| {
        for d in &decls {
            w.line(d);
        }
        w.line(format!(
            "guard {}(handle, {args}, err) != 0 else {{ return nil }}",
            it.next.symbol
        ));
        w.line(format!("return {value}"));
    });
    w.line("}");
}

/// The completion closure's formals and the expression receiving its
/// result, per the async result's [`ResultPass`].
fn completion_shape(f: &FnBinding, binding: &AsyncBinding, ctx: &SwiftCtx) -> (String, String) {
    let formals = binding
        .callback_params
        .iter()
        .map(|p| lang::escape_ident(&p.name, lang::SWIFT_KEYWORDS))
        .collect::<Vec<_>>()
        .join(", ");
    let ret = f.ret.as_ref().map(RetTy::elem);
    let value = match &binding.result {
        ResultPass::Void => "()".to_string(),
        ResultPass::Direct { result } => receive(
            ret.expect("a direct result"),
            &Recv::Direct,
            &result.name,
            "",
            true,
            ctx,
        ),
        ResultPass::OptDirect { has, value } => receive(
            ret.expect("an optional result"),
            &Recv::Opt(&has.name),
            &value.name,
            "",
            true,
            ctx,
        ),
        ResultPass::Slice { ptr, len, .. } => format!("wvTakeSlice({}, {})", ptr.name, len.name),
        ResultPass::String { ptr, len } => format!("wvTakeString({}, {})", ptr.name, len.name),
        ResultPass::Bytes { ptr, len } => format!("wvTakeBytes({}, {})", ptr.name, len.name),
        ResultPass::Buffer { ptr, len } => receive(
            ret.expect("a buffered result"),
            &Recv::Buffer,
            &ptr.name,
            &len.name,
            true,
            ctx,
        ),
        ResultPass::Object {
            result,
            nullable,
            interface,
            ..
        } => receive(
            ret.expect("an object result"),
            &Recv::Object {
                nullable: *nullable,
                interface,
            },
            &result.name,
            "",
            true,
            ctx,
        ),
    };
    (formals, value)
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
    let cancellable = binding.cancellable();

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
    render_arg_staging(w, params);
    w.line("let context = Unmanaged.passRetained(WvContinuation(continuation)).toOpaque()");
    let opened = open_lending_closures(w, params, "");

    let mut args = c_call_args(
        params,
        (receiver == Receiver::Instance).then_some("self.ptr"),
    );
    if cancellable {
        args.push("token.raw".to_string());
    }
    let (formals, value) = completion_shape(f, binding, ctx);
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
