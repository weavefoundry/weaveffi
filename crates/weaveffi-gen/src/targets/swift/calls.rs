//! Callable rendering: sync and async wrapper functions and the lazy
//! sequence classes backing `iter<T>` returns.
//!
//! Parameter marshalling dispatches on the shared [`ArgPass`] plan and
//! return handling on [`RetPass`], so how a value crosses the ABI is decided
//! centrally; this module only renders the Swift spelling.

use std::fmt::Write;

use crate::codegen::CodeWriter;
use crate::utils::local_type_name;
use weaveffi_model::model::{CallShape, FnBinding, IteratorBinding, ParamBinding, Ty};
use weaveffi_model::plan::{ret_pass, ArgPass, ErrorStrategy, RetPass};

use crate::targets::swift::codec::{decode_closure, write_value_stmts};
use crate::targets::swift::docs::emit_fn_doc;
use crate::targets::swift::types::{
    callback_box_name, callback_vtable_name, deprecated_attr, iterator_class_name,
    scalar_swift_type, swift_ident, SwiftCtx,
};

/// How a wrapper reports a filled error slot.
///
/// A callable declaring `throws` maps codes through its domain's typed
/// mapper (`checkKv`/`mapKv`) and throws; a cancellable async callable also
/// throws (`CancellationError` for code `-5`). Any other callable has a plain
/// signature and stops the process with `fatalError`, naming the code and
/// message, since a reported error can only be a runtime trap.
#[derive(Clone, Copy)]
pub(crate) struct ErrCtx<'a> {
    /// `true` when the Swift signature `throws`.
    throws: bool,
    /// `true` when the IDL declares `throws` (domain codes are typed).
    typed: bool,
    /// `true` for a cancellable async callable.
    cancellable: bool,
    /// PascalCase stem of the domain in effect (`Kv` names `checkKv` and
    /// `mapKv`); `None` falls back to the runtime helpers.
    domain: Option<&'a str>,
}

impl<'a> ErrCtx<'a> {
    /// Build the error context for `f` from its [`ErrorStrategy`] and
    /// cancellability.
    pub(crate) fn for_fn(f: &FnBinding, domain: Option<&'a str>) -> Self {
        let typed = f.error_strategy() == ErrorStrategy::Throws;
        let cancellable = f.is_async && f.cancellable;
        Self {
            throws: typed || cancellable,
            typed,
            cancellable,
            domain,
        }
    }

    /// The mapper turning an error slot into a Swift error, when the wrapper
    /// throws: the domain's `map{Stem}`, else the runtime's.
    fn mapper(&self) -> String {
        match (self.typed, self.domain) {
            (true, Some(stem)) => format!("map{stem}"),
            (true, None) => "wvRuntimeError".to_string(),
            (false, _) => "wvCancelledOrTrap".to_string(),
        }
    }

    /// The statement checking the error slot of a synchronous call.
    fn check_stmt(&self) -> String {
        match (self.typed, self.domain) {
            (true, Some(stem)) => format!("try check{stem}(&err)"),
            (true, None) => "try wvCheck(&err)".to_string(),
            (false, _) => "wvTrap(&err)".to_string(),
        }
    }
}

/// Clone a callable with its parameter names camel-cased and keyword-escaped,
/// so the Swift argument labels, bound locals, and every staged
/// `_ptr`/`_len` variable derived from them agree (and never collide with a
/// reserved word).
pub(crate) fn camel_params(f: &FnBinding) -> FnBinding {
    let mut f = f.clone();
    for p in &mut f.params {
        p.name = swift_ident(&p.name);
    }
    f
}

/// Where a wrapper sits, which decides how it reaches its object and whether
/// it runs the contract check.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Receiver {
    /// A free function or static member: runs the contract check first.
    Static,
    /// An instance method passing its own object as the leading argument.
    Instance,
    /// The `new` constructor, rendered as `public init`.
    Init,
}

/// Render one wrapper for `f` (sync, async, or iterator-returning) named
/// `swift_name`, at the indentation depth `depth`.
pub(crate) fn render_callable(
    out: &mut String,
    f: &FnBinding,
    swift_name: &str,
    receiver: Receiver,
    err: ErrCtx,
    ctx: &SwiftCtx,
    depth: usize,
) {
    let mut w = CodeWriter::four_space().with_depth(depth);
    {
        let mut tmp = String::new();
        emit_fn_doc(&mut tmp, &f.doc, &f.params, &w.indent_str());
        w.raw(tmp);
    }
    if let CallShape::Iterator(_) = &f.shape {
        w.line("/// - Returns: A lazy sequence that pulls one element per step from the");
        w.line("///   library; the underlying iterator is destroyed when the sequence is");
        w.line("///   exhausted or deinitialized.");
    }
    if err.cancellable {
        w.line("/// - Throws: `CancellationError` when the calling task is cancelled, which");
        w.line("///   also cancels the native call.");
    }
    if let Some(msg) = &f.deprecated {
        w.line(deprecated_attr(msg));
    }
    let mut sig = String::new();
    for (i, p) in f.params.iter().enumerate() {
        if i > 0 {
            sig.push_str(", ");
        }
        let _ = write!(sig, "{}: {}", p.name, ctx.swift_type(&p.ty));
    }
    let is_async = matches!(f.shape, CallShape::Async(_));
    let effects = match (is_async, err.throws) {
        (true, true) => " async throws",
        (true, false) => " async",
        (false, true) => " throws",
        (false, false) => "",
    };
    if receiver == Receiver::Init {
        w.line(format!("public init({sig}){effects} {{"));
    } else {
        let ret = match (&f.shape, &f.ret) {
            (CallShape::Iterator(it), _) => format!(
                " -> {}",
                ctx.ty_name(&iterator_class_name(it, ctx.c_prefix))
            ),
            (_, Some(t)) => format!(" -> {}", ctx.swift_type(t)),
            (_, None) => String::new(),
        };
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
    if is_async {
        render_async_body(&mut w, f, receiver, err, ctx);
    } else {
        render_sync_body(&mut w, f, receiver, err, ctx);
    }
    w.dedent();
    w.line("}");
    out.push_str(&w.finish());
}

/// The C argument list for `params`, prefixed with the receiver's object for
/// an instance method.
fn c_call_args(params: &[ParamBinding], self_arg: Option<&str>, ctx: &SwiftCtx) -> Vec<String> {
    let mut args: Vec<String> = self_arg.into_iter().map(str::to_string).collect();
    for p in params {
        let n = &p.name;
        match p.arg_pass() {
            ArgPass::String { .. } | ArgPass::Bytes { .. } | ArgPass::Buffer { .. } => {
                args.push(format!("{n}_ptr"));
                args.push(format!("{n}_len"));
            }
            // The wrapper borrows its own reference for the call; Swift
            // keeps every parameter alive until the wrapper returns.
            ArgPass::Object { nullable, .. } => {
                args.push(if nullable {
                    format!("{n}?.ptr")
                } else {
                    format!("{n}.ptr")
                });
            }
            ArgPass::Callback { .. } => {
                let cb =
                    p.ty.callback_interface_name()
                        .expect("callback family names a callback interface");
                args.push(format!("{n}_ctx"));
                args.push(format!("{}.shared.pointer", callback_vtable_name(cb)));
            }
            ArgPass::Direct { .. } => match &p.ty {
                Ty::Enum(name) => args.push(format!(
                    "{}(rawValue: numericCast({n}.rawValue))",
                    ctx.c_enum_type(name)
                )),
                _ => args.push(n.clone()),
            },
        }
    }
    args
}

/// Emit the pre-call staging: buffered params are packed into a writer, and
/// callback interfaces are boxed with a +1 retain the library's `free` entry
/// releases.
fn render_arg_staging(w: &mut CodeWriter, params: &[ParamBinding]) {
    let mut counter = 0usize;
    for p in params {
        match p.arg_pass() {
            ArgPass::Buffer { .. } => {
                let writer = format!("{}Writer", p.name);
                w.line(format!("var {writer} = WvWriter()"));
                write_value_stmts(w, &p.ty, &p.name, &writer, &mut counter);
            }
            ArgPass::Callback { .. } => {
                let cb =
                    p.ty.callback_interface_name()
                        .expect("callback family names a callback interface");
                w.line(format!(
                    "let {n}_ctx = Unmanaged.passRetained({}({n})).toOpaque()",
                    callback_box_name(cb),
                    n = p.name
                ));
            }
            _ => {}
        }
    }
}

/// Open the pointer-lending closures every string, bytes, and buffered
/// parameter needs, binding `{name}_ptr`/`{name}_len`. `bind` prefixes the
/// first opening line (`let rv: T = ` or nothing); nested ones return their
/// inner value implicitly. Returns how many closures were opened.
fn open_lending_closures(w: &mut CodeWriter, params: &[ParamBinding], bind: &str) -> usize {
    let mut opened = 0;
    for p in params {
        let n = &p.name;
        let open = match p.arg_pass() {
            ArgPass::String { .. } => format!("wvWithUTF8({n})"),
            ArgPass::Bytes { .. } => format!("wvWithBytes({n})"),
            ArgPass::Buffer { .. } => format!("wvWithBuffer({n}Writer)"),
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

/// The Swift type of the raw C result of a synchronous call, used to annotate
/// its binding when the call sits inside lending closures.
fn raw_return_swift(f: &FnBinding, rp: Option<&RetPass>, ctx: &SwiftCtx) -> String {
    match rp {
        None | Some(RetPass::Object { .. }) => "OpaquePointer?".to_string(),
        Some(RetPass::Void) => "Void".to_string(),
        Some(RetPass::String | RetPass::Bytes | RetPass::Buffer) => {
            "UnsafePointer<UInt8>?".to_string()
        }
        Some(RetPass::Direct) => match f.ret.as_ref() {
            Some(Ty::Enum(name)) => ctx.c_enum_type(name).to_string(),
            Some(other) => scalar_swift_type(other).to_string(),
            None => unreachable!("a direct return carries a type"),
        },
    }
}

/// The interface name behind an object-returning callable (direct or
/// nullable).
fn ret_interface_name(ty: Option<&Ty>) -> &str {
    ty.and_then(Ty::interface_name)
        .expect("an object return names an interface")
}

/// Render the body of a synchronous (or iterator-returning) callable: the
/// error slot, input staging, the C call wrapped in whatever lending closures
/// the inputs need, the error check, and the return conversion.
fn render_sync_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    receiver: Receiver,
    err: ErrCtx,
    ctx: &SwiftCtx,
) {
    w.line("var err = WvError()");
    render_arg_staging(w, &f.params);

    // An iterator launch returns the handle; its plan is the iterator
    // protocol rather than a `RetPass`.
    let (symbol, rp) = match &f.shape {
        CallShape::Iterator(it) => (&it.launch.symbol, None),
        CallShape::Sync(abi) => (&abi.symbol, Some(ret_pass(f.ret.as_ref(), ctx.c_prefix))),
        CallShape::Async(_) => unreachable!("async callables render through render_async_body"),
    };
    let needs_out_len = matches!(rp, Some(RetPass::String | RetPass::Bytes | RetPass::Buffer));
    if needs_out_len {
        w.line("var outLen = 0");
    }

    let self_arg = (receiver == Receiver::Instance).then_some("ptr");
    let mut args = c_call_args(&f.params, self_arg, ctx);
    if needs_out_len {
        args.push("&outLen".to_string());
    }
    args.push("&err".to_string());
    let call = format!("{symbol}({})", args.join(", "));

    let has_ret = !matches!(rp, Some(RetPass::Void));
    let bind = if has_ret {
        format!("let rv: {} = ", raw_return_swift(f, rp.as_ref(), ctx))
    } else {
        String::new()
    };
    let opened = open_lending_closures(w, &f.params, &bind);
    if opened == 0 {
        if has_ret {
            w.line(format!("let rv = {call}"));
        } else {
            w.line(call);
        }
    } else {
        w.line(call);
        close_closures(w, opened);
    }
    w.line(err.check_stmt());

    match rp {
        None => {
            let CallShape::Iterator(it) = &f.shape else {
                unreachable!("only an iterator launch has no return plan")
            };
            let class_name = ctx.ty_name(&iterator_class_name(it, ctx.c_prefix));
            w.line(format!("return {class_name}(handle: wvNonNull(rv))"));
        }
        Some(RetPass::Void) => {}
        // The constructor adopts the returned reference as its own.
        Some(_) if receiver == Receiver::Init => {
            w.line("self.ptr = wvNonNull(rv)");
        }
        Some(rp) => {
            let value = receive_value(w, f.ret.as_ref(), &rp, "rv", "outLen", ctx);
            w.line(format!("return {value}"));
        }
    }
}

/// The Swift expression converting a value the library returned (an owned
/// result) into its Swift form, releasing what the conversion copied.
/// `raw` names the result slot and `len` its length slot, if any.
fn receive_value(
    w: &CodeWriter,
    ty: Option<&Ty>,
    rp: &RetPass,
    raw: &str,
    len: &str,
    ctx: &SwiftCtx,
) -> String {
    match rp {
        RetPass::Void => "()".to_string(),
        RetPass::Direct => match ty {
            Some(Ty::Enum(name)) => format!(
                "wvEnumCase({}.self, numericCast({raw}.rawValue))",
                ctx.ty_name(local_type_name(name))
            ),
            _ => raw.to_string(),
        },
        RetPass::String => format!("wvTakeString({raw}, {len})"),
        RetPass::Bytes => format!("wvTakeBytes({raw}, {len})"),
        RetPass::Buffer => {
            let ty = ty.expect("a buffered value carries a type");
            let decode = decode_closure(w, ty, ctx);
            if decode.starts_with('{') {
                format!("wvTakeBuffer({raw}, {len}) {decode}")
            } else {
                format!("wvTakeBuffer({raw}, {len}, {decode})")
            }
        }
        RetPass::Object { nullable, .. } => {
            let class = ctx.ty_name(local_type_name(ret_interface_name(ty)));
            if *nullable {
                format!("{raw}.map {{ {class}(ptr: $0) }}")
            } else {
                format!("{class}(ptr: wvNonNull({raw}))")
            }
        }
    }
}

/// Render the body of an async callable: a checked continuation resumed
/// exactly once from the completion callback, wrapped in a cancellation
/// handler that cancels the native token for a cancellable callable.
fn render_async_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    receiver: Receiver,
    err: ErrCtx,
    ctx: &SwiftCtx,
) {
    let CallShape::Async(binding) = &f.shape else {
        unreachable!("render_async_body renders async callables")
    };
    let protocol = binding.protocol(f, ctx.c_prefix);
    let ret = f
        .ret
        .as_ref()
        .map_or_else(|| "Void".to_string(), |t| ctx.swift_type(t));
    let err_ty = if err.throws { "Error" } else { "Never" };

    if protocol.cancellable {
        w.line("let token = WvCancelToken()");
        w.line("return try await withTaskCancellationHandler {");
        w.indent();
    }
    if err.throws {
        w.line(format!(
            "{}try await withCheckedThrowingContinuation {{ (continuation: CheckedContinuation<{ret}, Error>) in",
            if protocol.cancellable { "" } else { "return " }
        ));
    } else {
        w.line(format!(
            "return await withCheckedContinuation {{ (continuation: CheckedContinuation<{ret}, Never>) in"
        ));
    }
    w.indent();

    // The library copies every input during the launch, so lending them for
    // the launch call's duration is enough.
    render_arg_staging(w, &f.params);
    w.line("let context = Unmanaged.passRetained(WvContinuation(continuation)).toOpaque()");
    let opened = open_lending_closures(w, &f.params, "");

    let self_arg = (receiver == Receiver::Instance).then_some("self.ptr");
    let mut args = c_call_args(&f.params, self_arg, ctx);
    if protocol.cancellable {
        args.push("token.raw".to_string());
    }
    let slots = match &protocol.result {
        RetPass::Void => "context, err",
        RetPass::String | RetPass::Bytes | RetPass::Buffer => "context, err, resultPtr, resultLen",
        _ => "context, err, result",
    };
    let mut launch = args.join(", ");
    if !launch.is_empty() {
        launch.push_str(", ");
    }
    w.line(format!("{}({launch}{{ {slots} in", binding.launch.symbol));
    w.indent();
    w.line(format!(
        "let cont = Unmanaged<WvContinuation<{ret}, {err_ty}>>.fromOpaque(context!).takeRetainedValue().value"
    ));
    w.line("if let err = err {");
    w.indent();
    if err.throws {
        w.line(format!(
            "cont.resume(throwing: wvTakeError(err, {}))",
            err.mapper()
        ));
        w.line("return");
    } else {
        w.line("wvTrapBoxed(err)");
    }
    w.dedent();
    w.line("}");
    let raw = if slots.ends_with("resultLen") {
        "resultPtr"
    } else {
        "result"
    };
    let value = receive_value(w, f.ret.as_ref(), &protocol.result, raw, "resultLen", ctx);
    w.line(format!("cont.resume(returning: {value})"));
    w.dedent();
    w.line("}, context)");
    close_closures(w, opened);
    w.dedent();
    w.line("}");
    if protocol.cancellable {
        w.dedent();
        w.line("} onCancel: {");
        w.scope(|w| {
            w.line("token.cancel()");
        });
        w.line("}");
    }
}

/// Emit the lazy sequence class backing one `iter<T>` function.
///
/// The class conforms to `Sequence & IteratorProtocol` and owns the C
/// iterator handle. Each `next()` issues exactly one library `next` call;
/// the handle is destroyed eagerly on exhaustion (or on a mid-stream error)
/// and from `deinit` when iteration is abandoned early. Elements are received
/// exactly like returns of the same type.
///
/// `next()` can't throw under `IteratorProtocol`, so for a throwing function
/// a mid-stream error ends iteration and is stored in the sequence's public
/// `error` property; for a non-throwing one a reported error stops the
/// process.
pub(crate) fn render_swift_iterator_class(
    out: &mut String,
    f: &FnBinding,
    it: &IteratorBinding,
    err: ErrCtx,
    ctx: &SwiftCtx,
) {
    let protocol = it.protocol(f, ctx.c_prefix);
    let class_name = iterator_class_name(it, ctx.c_prefix);
    let elem = &it.elem;
    let elem_swift = ctx.swift_type(elem);
    let has_len_slot = matches!(
        protocol.elem,
        RetPass::String | RetPass::Bytes | RetPass::Buffer
    );

    // The `out_item` slot declaration.
    let item_decl = match &protocol.elem {
        RetPass::String | RetPass::Bytes | RetPass::Buffer => {
            "var item: UnsafePointer<UInt8>? = nil".to_string()
        }
        RetPass::Object { .. } => "var item: OpaquePointer? = nil".to_string(),
        RetPass::Direct => match elem {
            Ty::Enum(name) => format!("var item = {}(rawValue: 0)", ctx.c_enum_type(name)),
            Ty::Bool => "var item = false".to_string(),
            other => format!("var item: {} = 0", scalar_swift_type(other)),
        },
        RetPass::Void => unreachable!("an iterator element is never void"),
    };

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "/// A lazy sequence over the `{elem_swift}` elements streamed by `{}`.",
        it.launch.symbol
    ));
    w.line("///");
    w.line("/// Each `next()` call pulls exactly one element from the library. The");
    w.line("/// underlying iterator is destroyed eagerly on exhaustion and from `deinit`");
    w.line("/// when iteration is abandoned early.");
    if err.throws {
        w.line("///");
        w.line("/// If the library reports an error mid-stream, iteration ends and the");
        w.line("/// error is stored in ``error`` for the caller to inspect after the loop.");
    }
    w.line(format!(
        "public final class {class_name}: Sequence, IteratorProtocol {{"
    ));
    w.indent();
    w.line("private var handle: OpaquePointer?");
    if err.throws {
        w.line("/// The error that ended iteration early, if any.");
        w.line("public private(set) var error: Error?");
    }
    w.blank();
    w.line("init(handle: OpaquePointer) {");
    w.scope(|w| {
        w.line("self.handle = handle");
    });
    w.line("}");
    w.blank();
    w.line("deinit {");
    w.scope(|w| {
        w.line("destroyHandle()");
    });
    w.line("}");
    w.blank();
    w.line("private func destroyHandle() {");
    w.scope(|w| {
        w.line("guard let handle = handle else { return }");
        w.line(format!("{}(handle)", it.destroy_symbol));
        w.line("self.handle = nil");
    });
    w.line("}");
    w.blank();
    w.line("/// Pulls the next element from the library, or returns `nil` once the");
    w.line("/// stream is exhausted (destroying the underlying iterator).");
    w.line(format!("public func next() -> {elem_swift}? {{"));
    w.indent();
    w.line("guard let handle = handle else { return nil }");
    w.line(item_decl);
    if has_len_slot {
        w.line("var itemLen = 0");
    }
    w.line("var err = WvError()");
    let len_arg = if has_len_slot { ", &itemLen" } else { "" };
    w.line(format!(
        "if {}(handle, &item{len_arg}, &err) == 0 {{",
        it.next.symbol
    ));
    w.indent();
    if err.throws {
        w.line("if err.code != 0 {");
        w.scope(|w| {
            w.line(format!("error = {}(err)", err.mapper()));
            w.line(format!("{}_error_clear(&err)", ctx.c_prefix));
        });
        w.line("}");
    } else {
        w.line("wvTrap(&err)");
    }
    w.line("destroyHandle()");
    w.line("return nil");
    w.dedent();
    w.line("}");
    let value = receive_value(&w, Some(elem), &protocol.elem, "item", "itemLen", ctx);
    w.line(format!("return {value}"));
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}
