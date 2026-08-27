//! Callable rendering: sync and async wrapper functions, listener
//! register/unregister pairs, and the lazy sequence classes backing
//! `iter<T>` returns.
//!
//! Parameter marshalling dispatches on the shared [`ArgPass`] plan and
//! return handling on [`RetPass`], so how a value crosses the ABI is decided
//! centrally; this module only renders the Swift spelling.

use std::fmt::Write;

use heck::ToLowerCamelCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::lang;
use weaveffi_core::model::{
    CallShape, FnBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding,
};
use weaveffi_core::plan::{ret_pass, ArgPass, ElemFree, ErrorStrategy, RetPass};
use weaveffi_core::utils::{local_type_name, wrapper_name};
use weaveffi_ir::ir::TypeRef;

use crate::codec::{fresh, read_value_stmts, write_value_stmts};
use crate::docs::emit_fn_doc;
use crate::entities::domain_stem;
use crate::types::{
    c_enum_type, iterator_class_name, swift_ident, swift_scalar_default, swift_type_ctx,
    swift_type_for, SwiftCtx,
};

/// How a wrapper body reports a non-zero `weaveffi_error` slot.
///
/// A callable with `throws == true` maps codes through the declaring module's
/// typed checker (`checkKv`) and surfaces marshalling failures as thrown
/// [`ERROR_BRAND`] values; a callable with `throws == false` has a plain
/// signature and traps (`fatalError`) instead, since a reported error can only
/// be a producer panic or an argument-marshalling failure.
#[derive(Clone, Copy)]
pub(crate) struct ErrCtx<'a> {
    /// `true` when the wrapper is `throws` and surfaces typed errors.
    pub(crate) throws: bool,
    /// PascalCase stem of the domain in effect (`Kv` names `checkKv` and
    /// `mapKv`); `None` falls back to the generic `check` helper.
    pub(crate) domain: Option<&'a str>,
}

impl<'a> ErrCtx<'a> {
    /// Build the error context for `f` from its [`ErrorStrategy`]:
    /// [`ErrorStrategy::Throws`] surfaces typed errors, [`ErrorStrategy::Trap`]
    /// traps on the (panic-only) error path.
    pub(crate) fn for_fn(f: &FnBinding, domain: Option<&'a str>) -> Self {
        Self {
            throws: f.error_strategy() == ErrorStrategy::Throws,
            domain,
        }
    }

    /// The statement checking the error slot named `slot`.
    fn check_stmt(&self, slot: &str) -> String {
        if !self.throws {
            return format!("trap(&{slot})");
        }
        match self.domain {
            Some(stem) => format!("try check{stem}(&{slot})"),
            None => format!("try check(&{slot})"),
        }
    }

    /// The statement reporting a marshalling failure (`code`, `msg` are
    /// literals): a thrown [`ERROR_BRAND`] for a throwing wrapper, a trap
    /// otherwise.
    fn fail_stmt(&self, code: i32, msg: &str) -> String {
        if self.throws {
            format!("throw {ERROR_BRAND}.error(code: {code}, message: \"{msg}\")")
        } else {
            format!("fatalError(\"{code}: {msg}\")")
        }
    }

    /// A `guard let {name} = {name} else {{ ... }}` line reporting a
    /// marshalling failure through [`Self::fail_stmt`].
    fn guard_stmt(&self, name: &str, code: i32, msg: &str) -> String {
        format!(
            "guard let {name} = {name} else {{ {} }}",
            self.fail_stmt(code, msg)
        )
    }

    /// The statements an async completion callback runs (after copying the
    /// runtime `code`/`msg` locals) when the error slot reports: copy the
    /// payload and resume throwing the mapped domain error, resume with the
    /// generic brand error, or trap.
    fn async_err_lines(&self) -> Vec<String> {
        if !self.throws {
            return vec!["fatalError(\"\\(code): \\(msg)\")".to_string()];
        }
        match self.domain {
            Some(stem) => vec![
                "let payload: [UInt8]? = err.pointee.payload_ptr.map { [UInt8](UnsafeBufferPointer(start: $0, count: err.pointee.payload_len)) }".to_string(),
                format!("contRef.value.resume(throwing: map{stem}(code: code, message: msg, payload: payload))"),
            ],
            None => vec![format!(
                "contRef.value.resume(throwing: {ERROR_BRAND}.error(code: code, message: msg))"
            )],
        }
    }

    /// The statement an async completion callback uses for a marshalling
    /// failure with literal `code`/`msg`.
    fn async_fail_stmt(&self, code: i32, msg: &str) -> String {
        if self.throws {
            format!(
                "contRef.value.resume(throwing: {ERROR_BRAND}.error(code: {code}, message: \"{msg}\"))"
            )
        } else {
            format!("fatalError(\"{code}: {msg}\")")
        }
    }

    /// The Swift error type parameter of the continuation: `Error` for a
    /// throwing wrapper, `Never` for a plain one.
    fn continuation_err_ty(&self) -> &'static str {
        if self.throws {
            "Error"
        } else {
            "Never"
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

/// Prepend an instance receiver's pointer to a rendered C argument list.
fn with_self_arg(call_args: String, self_arg: Option<&str>) -> String {
    match self_arg {
        Some(recv) if call_args.is_empty() => recv.to_string(),
        Some(recv) => format!("{recv}, {call_args}"),
        None => call_args,
    }
}

/// `true` when `p` is staged through a pointer-borrowing closure
/// (`withCString` or `withUnsafeBufferPointer`) rather than passed directly.
fn needs_staging_closure(p: &ParamBinding) -> bool {
    matches!(
        p.arg_pass(),
        ArgPass::String { .. } | ArgPass::Bytes { .. } | ArgPass::Buffer { .. }
    )
}

/// Render one synchronous (or iterator-returning) callable. `swift_name` is
/// the already-cased wrapper name; `self_arg` is `Some("ptr")` for an
/// instance method, making the wrapper a member `func` that passes its own
/// handle as the leading C argument.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_swift_function(
    out: &mut String,
    c_prefix: &str,
    module_name: &str,
    f: &FnBinding,
    swift_name: &str,
    self_arg: Option<&str>,
    err: ErrCtx,
    ctx: SwiftCtx,
) {
    let mut w = CodeWriter::four_space().with_depth(1);
    {
        let mut tmp = String::new();
        emit_fn_doc(&mut tmp, &f.doc, &f.params, "    ");
        w.raw(tmp);
    }
    if let CallShape::Iterator(_) = &f.shape {
        w.line("/// - Returns: A lazy sequence that pulls one element per step from the");
        w.line("///   producer; the underlying iterator is destroyed when the sequence is");
        w.line("///   exhausted or deinitialized.");
        if err.throws {
            w.line("/// - Throws: The module's typed error if the launch fails. Mid-stream");
            w.line("///   errors end iteration and are stored in the sequence's `error`");
            w.line("///   property instead of being thrown.");
        }
    }
    if let Some(msg) = &f.deprecated {
        w.line(format!(
            "@available(*, deprecated, message: \"{}\")",
            msg.replace('"', "\\\"")
        ));
    }
    let ret_swift = match &f.shape {
        CallShape::Iterator(it) => ctx.ty_name(&iterator_class_name(it, c_prefix)),
        _ => f
            .ret
            .as_ref()
            .map(|t| swift_type_ctx(t, ctx))
            .unwrap_or_else(|| "Void".to_string()),
    };
    let mut sig = String::new();
    write_swift_params_sig(&mut sig, &f.params, ctx);
    let static_kw = if self_arg.is_some() { "" } else { "static " };
    let throws_kw = if err.throws { " throws" } else { "" };
    w.line(format!(
        "public {static_kw}func {swift_name}({sig}){throws_kw} -> {ret_swift} {{"
    ));
    w.indent();
    render_call_body(&mut w, f, c_prefix, module_name, self_arg, err, ctx, false);
    w.dedent();
    w.line("}");
    out.push_str(&w.finish());
}

/// Render the constructor named `new` as `public init`: the body is the
/// shared call body with an assign-to-`self.ptr` tail instead of a
/// wrapper-returning one. Throwing before `self.ptr` is assigned is legal in
/// a root-class initializer, so the error paths carry over unchanged.
pub(crate) fn render_swift_ctor_init(
    out: &mut String,
    c_prefix: &str,
    module_name: &str,
    f: &FnBinding,
    err: ErrCtx,
    ctx: SwiftCtx,
) {
    let mut w = CodeWriter::four_space().with_depth(1);
    {
        let mut tmp = String::new();
        emit_fn_doc(&mut tmp, &f.doc, &f.params, "    ");
        w.raw(tmp);
    }
    let mut sig = String::new();
    write_swift_params_sig(&mut sig, &f.params, ctx);
    let throws_kw = if err.throws { " throws" } else { "" };
    w.line(format!("public init({sig}){throws_kw} {{"));
    w.indent();
    render_call_body(&mut w, f, c_prefix, module_name, None, err, ctx, true);
    w.dedent();
    w.line("}");
    out.push_str(&w.finish());
}

/// Write `name: SwiftType, name: SwiftType, ...` directly into `out`,
/// avoiding the per-call `format!` and intermediate `Vec<String>` allocations
/// that `params.iter().map(format!).collect::<Vec<_>>().join(", ")` would
/// require. Parameters carry real argument labels (their camel-cased,
/// keyword-escaped names).
fn write_swift_params_sig(out: &mut String, params: &[ParamBinding], ctx: SwiftCtx) {
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{}: {}", p.name, swift_type_ctx(&p.ty, ctx));
    }
}

/// Render the C argument list for `params`: staged params contribute their
/// `_ptr`/`_len` bindings, direct params their converted expressions.
fn build_c_call_args(params: &[ParamBinding], c_prefix: &str, module_name: &str) -> String {
    let mut args: Vec<String> = Vec::new();
    for p in params {
        match p.arg_pass() {
            // Strings are a single NUL-terminated `const char*`.
            ArgPass::String { .. } => args.push(format!("{}_ptr", p.name)),
            // Bytes and buffered values pass an explicit (ptr, len) pair.
            ArgPass::Bytes { .. } | ArgPass::Buffer { .. } => {
                args.push(format!("{}_ptr", p.name));
                args.push(format!("{}_len", p.name));
            }
            // An interface param borrows the wrapper's handle for the call;
            // the receiver stays alive for the call frame. A nullable one
            // passes null for "none".
            ArgPass::Object { nullable, .. } => {
                if nullable {
                    args.push(format!("{}?.ptr", p.name));
                } else {
                    args.push(format!("{}.ptr", p.name));
                }
            }
            ArgPass::Direct { .. } => match &p.ty {
                TypeRef::Enum(enum_name) => args.push(format!(
                    "{}({}.rawValue)",
                    c_enum_type(enum_name, c_prefix, module_name),
                    p.name
                )),
                // A typed handle is a `UInt64` token in Swift; the C slot is
                // an opaque typed pointer, so reinterpret the bits.
                TypeRef::TypedHandle(_) => {
                    args.push(format!("OpaquePointer(bitPattern: UInt({}))", p.name));
                }
                _ => args.push(p.name.clone()),
            },
        }
    }
    args.join(", ")
}

/// The Swift spelling of the raw C return value of `f`, used to annotate the
/// binding when the call sits inside multi-statement staging closures (whose
/// return type Swift cannot infer).
fn raw_return_swift(
    f: &FnBinding,
    rp: Option<&RetPass>,
    c_prefix: &str,
    module_name: &str,
) -> String {
    // An iterator launch returns the opaque iterator handle.
    let Some(rp) = rp else {
        return "OpaquePointer?".to_string();
    };
    match rp {
        RetPass::Void => "Void".to_string(),
        RetPass::Buffer | RetPass::Bytes => "UnsafePointer<UInt8>?".to_string(),
        RetPass::String => "UnsafePointer<CChar>?".to_string(),
        RetPass::Object { .. } => "OpaquePointer?".to_string(),
        RetPass::Direct => match f.ret.as_ref() {
            Some(TypeRef::TypedHandle(_)) => "OpaquePointer?".to_string(),
            Some(TypeRef::Enum(name)) => c_enum_type(name, c_prefix, module_name),
            Some(other) => swift_type_for(other),
            None => unreachable!("a direct return carries a type"),
        },
    }
}

/// The interface name behind an object-returning callable (direct or
/// nullable).
fn ret_interface_name(f: &FnBinding) -> &str {
    match f.ret.as_ref() {
        Some(TypeRef::Interface(name)) => name,
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::Interface(name) => name,
            _ => unreachable!("non-interface optional is buffered"),
        },
        _ => unreachable!("object return implies an interface type"),
    }
}

/// Render the shared body of a synchronous callable: the error slot, input
/// staging (byte copies and buffer packing), the C call wrapped in whatever
/// pointer-staging closures the inputs need, the error check, and the return
/// conversion. With `ctor` set, an interface-returning tail assigns
/// `self.ptr` instead of wrapping the pointer.
#[allow(clippy::too_many_arguments)]
fn render_call_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    c_prefix: &str,
    module_name: &str,
    self_arg: Option<&str>,
    err: ErrCtx,
    ctx: SwiftCtx,
    ctor: bool,
) {
    let mut counter = 0usize;
    w.line("var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)");

    // Staging: byte params are copied into `[UInt8]`, buffered params are
    // packed into a writer; both are handed to the C call via
    // `withUnsafeBufferPointer` below.
    for p in &f.params {
        match p.arg_pass() {
            ArgPass::Bytes { .. } => {
                w.line(format!("let {n}Bytes = Array({n})", n = p.name));
            }
            ArgPass::Buffer { .. } => {
                w.line(format!("var {n}Writer = WvWriter()", n = p.name));
                let writer = format!("{}Writer", p.name);
                write_value_stmts(w, &p.ty, &p.name, &writer, ctx, &mut counter);
            }
            _ => {}
        }
    }

    // An iterator launch returns the handle, not a value; its return plan is
    // the iterator protocol rather than a `RetPass`.
    let rp = match &f.shape {
        CallShape::Iterator(_) => None,
        _ => Some(ret_pass(f.ret.as_ref(), module_name, c_prefix)),
    };
    let needs_out_len = matches!(rp, Some(RetPass::Bytes | RetPass::Buffer));
    if needs_out_len {
        w.line("var outLen: Int = 0");
    }

    let c_sym = &f.c_base;
    let mut all_args = with_self_arg(
        build_c_call_args(&f.params, c_prefix, module_name),
        self_arg,
    );
    if needs_out_len {
        if all_args.is_empty() {
            all_args.push_str("&outLen");
        } else {
            all_args.push_str(", &outLen");
        }
    }
    let call = if all_args.is_empty() {
        format!("{c_sym}(&err)")
    } else {
        format!("{c_sym}({all_args}, &err)")
    };

    let closure_params: Vec<&ParamBinding> = f
        .params
        .iter()
        .filter(|p| needs_staging_closure(p))
        .collect();
    let has_ret = f.ret.is_some();

    if closure_params.is_empty() {
        if has_ret {
            w.line(format!("let rv = {call}"));
        } else {
            w.line(call);
        }
    } else {
        let raw_ty = raw_return_swift(f, rp.as_ref(), c_prefix, module_name);
        for (i, p) in closure_params.iter().enumerate() {
            let bind = if !has_ret {
                String::new()
            } else if i == 0 {
                format!("let rv: {raw_ty} = ")
            } else {
                "return ".to_string()
            };
            let n = &p.name;
            match p.arg_pass() {
                ArgPass::String { .. } => {
                    w.line(format!("{bind}{n}.withCString {{ {n}_ptr in"));
                    w.indent();
                }
                ArgPass::Bytes { .. } => {
                    w.line(format!(
                        "{bind}{n}Bytes.withUnsafeBufferPointer {{ {n}_buf in"
                    ));
                    w.indent();
                    w.line(format!("let {n}_ptr = {n}_buf.baseAddress"));
                    w.line(format!("let {n}_len = {n}_buf.count"));
                }
                ArgPass::Buffer { .. } => {
                    w.line(format!(
                        "{bind}{n}Writer.bytes.withUnsafeBufferPointer {{ {n}_buf in"
                    ));
                    w.indent();
                    w.line(format!("let {n}_ptr = {n}_buf.baseAddress"));
                    w.line(format!("let {n}_len = {n}_buf.count"));
                }
                ArgPass::Direct { .. } | ArgPass::Object { .. } => unreachable!(),
            }
        }
        if has_ret {
            w.line(format!("return {call}"));
        } else {
            w.line(call);
        }
        for _ in 0..closure_params.len() {
            w.dedent();
            w.line("}");
        }
    }

    w.line(err.check_stmt("err"));
    render_return_tail(w, f, rp, err, ctx, ctor, &mut counter);
}

/// Render the post-check return conversion of a callable body, consuming the
/// raw call result bound as `rv`. `rp` is `None` for an iterator launch,
/// whose handle wraps into the per-function sequence class.
fn render_return_tail(
    w: &mut CodeWriter,
    f: &FnBinding,
    rp: Option<RetPass>,
    err: ErrCtx,
    ctx: SwiftCtx,
    ctor: bool,
    counter: &mut usize,
) {
    match rp {
        None => {
            let CallShape::Iterator(it) = &f.shape else {
                unreachable!("iterator return implies iterator shape")
            };
            let class_name = ctx.ty_name(&iterator_class_name(it, ctx.c_prefix));
            w.line(err.guard_stmt("rv", -1, "null iterator"));
            w.line(format!("return {class_name}(handle: rv)"));
        }
        Some(RetPass::Void) => {}
        Some(RetPass::Buffer) => {
            let ty = f.ret.as_ref().expect("a buffered return carries a type");
            // Copy the encoding, release the producer buffer, then decode.
            w.line(err.guard_stmt("rv", -1, "null buffer"));
            w.line("let rvBytes = [UInt8](UnsafeBufferPointer(start: rv, count: outLen))");
            w.line("weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen)");
            w.line("var rvReader = WvReader(bytes: rvBytes)");
            let v = fresh(counter, "v");
            read_value_stmts(w, ty, &v, "rvReader", ctx, counter);
            w.line("rvReader.finish()");
            w.line(format!("return {v}"));
        }
        Some(RetPass::String) => {
            w.line(err.guard_stmt("rv", -1, "null string"));
            w.line("defer { weaveffi_free_string(rv) }");
            w.line("return String(cString: rv)");
        }
        Some(RetPass::Bytes) => {
            w.line("guard let rv = rv else { return Data() }");
            w.line("defer { weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen) }");
            w.line("return Data(bytes: rv, count: outLen)");
        }
        Some(RetPass::Object {
            nullable: false, ..
        }) => {
            let name = ret_interface_name(f);
            w.line(err.guard_stmt("rv", -1, "null pointer"));
            if ctor {
                w.line("self.ptr = rv");
            } else {
                w.line(format!(
                    "return {}(ptr: rv)",
                    ctx.ty_name(local_type_name(name))
                ));
            }
        }
        // A nullable owned object pointer: null means none.
        Some(RetPass::Object { nullable: true, .. }) => {
            let name = ret_interface_name(f);
            w.line(format!(
                "return rv.map {{ {}(ptr: $0) }}",
                ctx.ty_name(local_type_name(name))
            ));
        }
        Some(RetPass::Direct) => match f.ret.as_ref() {
            Some(TypeRef::Enum(name)) => {
                let ty_name = ctx.ty_name(local_type_name(name));
                w.line(format!("return {ty_name}(rawValue: rv.rawValue)!"));
            }
            Some(TypeRef::TypedHandle(_)) => {
                w.line("return UInt64(UInt(bitPattern: rv))");
            }
            _ => {
                w.line("return rv");
            }
        },
    }
}

/// The Swift type one callback parameter surfaces as in the user closure.
/// Interface parameters stay raw (`OpaquePointer?`): wrapping them in the
/// owning Swift class would `*_destroy` a borrowed handle on ARC release.
/// Buffered parameters are decoded to their idiomatic value types before the
/// closure is invoked.
fn swift_cb_param_type(ty: &TypeRef, ctx: SwiftCtx) -> String {
    match ty {
        TypeRef::Interface(_) => "OpaquePointer?".into(),
        TypeRef::Optional(inner) if matches!(inner.as_ref(), TypeRef::Interface(_)) => {
            "OpaquePointer?".into()
        }
        other => swift_type_ctx(other, ctx),
    }
}

/// The Swift spelling of one trampoline closure formal: the ABI slot name,
/// keyword-escaped (a param named `in` yields a slot named `in`, which can't
/// bind as a closure formal unescaped).
fn cb_slot_ident(name: &str) -> String {
    lang::escape_ident(name, lang::SWIFT_KEYWORDS)
}

/// The expression converting one *direct* callback parameter's C slots into
/// the value handed to the user closure. Slot names follow the parameter's
/// precomputed ABI slots. Buffered parameters are decoded via statements
/// instead (see [`render_swift_listener`]).
fn swift_cb_direct_arg(p: &ParamBinding, ctx: SwiftCtx) -> String {
    let n0 = cb_slot_ident(&p.abi[0].name);
    match &p.ty {
        TypeRef::Enum(_) => {
            let local = swift_type_ctx(&p.ty, ctx);
            format!("{local}(rawValue: {n0}.rawValue)!")
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("String(cString: {n0}!)"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let n1 = cb_slot_ident(&p.abi[1].name);
            format!("{n0}.map {{ Data(bytes: $0, count: {n1}) }} ?? Data()")
        }
        TypeRef::TypedHandle(_) => format!("UInt64(UInt(bitPattern: {n0}))"),
        // Interfaces (and nullable interfaces) stay raw borrowed pointers.
        _ => n0,
    }
}

/// The register/unregister pair for one listener. The user closure is boxed
/// (`WvCallbackBox`) and retained through the C `context` pointer; the
/// capture-free trampoline closure decodes any buffered arguments, unboxes
/// the user closure, and invokes it.
pub(crate) fn render_swift_listener(
    out: &mut String,
    module_path: &str,
    mb: &ModuleBinding,
    l: &ListenerBinding,
    strip_module_prefix: bool,
    ctx: SwiftCtx,
) {
    let Some(cb) = mb.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };
    let register_fn = swift_ident(&wrapper_name(
        module_path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    ));
    let unregister_fn = swift_ident(&wrapper_name(
        module_path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    ));

    let closure_type = format!(
        "({}) -> Void",
        cb.params
            .iter()
            .map(|p| swift_cb_param_type(&p.ty, ctx))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Trampoline closure formals: every ABI slot, context last.
    let slot_names: Vec<String> = cb
        .abi_params
        .iter()
        .map(|s| cb_slot_ident(&s.name))
        .collect();

    let mut w = CodeWriter::four_space().with_depth(1);
    {
        let mut tmp = String::new();
        emit_fn_doc(&mut tmp, &l.doc, &[], "    ");
        w.raw(tmp);
    }
    w.line(format!(
        "/// - Returns: A subscription id for ``{unregister_fn}(_:)``."
    ));
    w.line(format!(
        "public static func {register_fn}(_ callback: @escaping {closure_type}) -> UInt64 {{"
    ));
    w.indent();
    w.line("let box = WvCallbackBox(callback)");
    w.line("let ctx = Unmanaged.passRetained(box).toOpaque()");
    w.line(format!(
        "let id = {}({{ {} in",
        l.register_symbol,
        slot_names.join(", ")
    ));
    w.indent();
    w.line(format!(
        "let cb = Unmanaged<WvCallbackBox<{closure_type}>>.fromOpaque(context!).takeUnretainedValue().value"
    ));
    // Buffered arguments are borrowed (ptr, len) pairs, valid only for the
    // dispatch: decode them before invoking the user closure.
    let mut counter = 0usize;
    let mut args: Vec<String> = Vec::new();
    for p in &cb.params {
        if let ArgPass::Buffer { ptr, len } = p.arg_pass() {
            let base = p.name.to_lower_camel_case();
            w.line(format!(
                "let {base}Buf = [UInt8](UnsafeBufferPointer(start: {}, count: {}))",
                ptr.name, len.name
            ));
            w.line(format!("var {base}Reader = WvReader(bytes: {base}Buf)"));
            let v = fresh(&mut counter, "v");
            let reader = format!("{base}Reader");
            read_value_stmts(&mut w, &p.ty, &v, &reader, ctx, &mut counter);
            w.line(format!("{base}Reader.finish()"));
            args.push(v);
        } else {
            args.push(swift_cb_direct_arg(p, ctx));
        }
    }
    w.line(format!("cb({})", args.join(", ")));
    w.dedent();
    w.line("}, ctx)");
    w.line("wvListenerLock.lock()");
    w.line("wvListenerContexts[id] = ctx");
    w.line("wvListenerLock.unlock()");
    w.line("return id");
    w.dedent();
    w.line("}");

    w.line(format!(
        "/// Unregisters a listener previously registered with ``{register_fn}(_:)``."
    ));
    w.line(format!(
        "public static func {unregister_fn}(_ id: UInt64) {{"
    ));
    w.indent();
    w.line(format!("{}(id)", l.unregister_symbol));
    w.line("wvListenerLock.lock()");
    w.line("let ctx = wvListenerContexts.removeValue(forKey: id)");
    w.line("wvListenerLock.unlock()");
    w.line("if let ctx = ctx {");
    w.scope(|w| {
        w.line(format!(
            "Unmanaged<WvCallbackBox<{closure_type}>>.fromOpaque(ctx).release()"
        ));
    });
    w.line("}");
    w.dedent();
    w.line("}");
    out.push_str(&w.finish());
}

/// Render one async callable as a continuation-backed `async` wrapper. A
/// throwing callable is `async throws` over a throwing continuation resuming
/// the module's typed error; a plain one is `async` over a never-throwing
/// continuation that traps on the (panic-only) error path. `self_arg` works
/// as in [`render_swift_function`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_swift_async_function(
    out: &mut String,
    c_prefix: &str,
    module_name: &str,
    f: &FnBinding,
    swift_name: &str,
    self_arg: Option<&str>,
    err: ErrCtx,
    ctx: SwiftCtx,
) {
    let mut w = CodeWriter::four_space().with_depth(1);
    {
        let mut tmp = String::new();
        emit_fn_doc(&mut tmp, &f.doc, &f.params, "    ");
        w.raw(tmp);
    }
    if let Some(msg) = &f.deprecated {
        w.line(format!(
            "@available(*, deprecated, message: \"{}\")",
            msg.replace('"', "\\\"")
        ));
    }
    let ret_swift = f
        .ret
        .as_ref()
        .map(|t| swift_type_ctx(t, ctx))
        .unwrap_or_else(|| "Void".to_string());
    let err_ty = err.continuation_err_ty();
    let mut sig = String::new();
    write_swift_params_sig(&mut sig, &f.params, ctx);
    let static_kw = if self_arg.is_some() { "" } else { "static " };
    if err.throws {
        w.line(format!(
            "public {static_kw}func {swift_name}({sig}) async throws -> {ret_swift} {{"
        ));
        w.indent();
        w.line(format!(
            "try await withCheckedThrowingContinuation {{ (continuation: CheckedContinuation<{ret_swift}, Error>) in"
        ));
    } else {
        w.line(format!(
            "public {static_kw}func {swift_name}({sig}) async -> {ret_swift} {{"
        ));
        w.indent();
        w.line(format!(
            "await withCheckedContinuation {{ (continuation: CheckedContinuation<{ret_swift}, Never>) in"
        ));
    }
    w.indent();
    w.line("let ctx = Unmanaged.passRetained(ContinuationRef(continuation)).toOpaque()");

    // Staging: identical to the sync path. The producer copies every input
    // synchronously during the launch, so pointer validity for the launch
    // call's duration is sufficient.
    let mut counter = 0usize;
    for p in &f.params {
        match p.arg_pass() {
            ArgPass::Bytes { .. } => {
                w.line(format!("let {n}Bytes = Array({n})", n = p.name));
            }
            ArgPass::Buffer { .. } => {
                w.line(format!("var {n}Writer = WvWriter()", n = p.name));
                let writer = format!("{}Writer", p.name);
                write_value_stmts(&mut w, &p.ty, &p.name, &writer, ctx, &mut counter);
            }
            _ => {}
        }
    }

    // The launch returns void, so the staging closures carry no binding.
    let closure_params: Vec<&ParamBinding> = f
        .params
        .iter()
        .filter(|p| needs_staging_closure(p))
        .collect();
    for p in &closure_params {
        let n = &p.name;
        match p.arg_pass() {
            ArgPass::String { .. } => {
                w.line(format!("{n}.withCString {{ {n}_ptr in"));
                w.indent();
            }
            ArgPass::Bytes { .. } => {
                w.line(format!("{n}Bytes.withUnsafeBufferPointer {{ {n}_buf in"));
                w.indent();
                w.line(format!("let {n}_ptr = {n}_buf.baseAddress"));
                w.line(format!("let {n}_len = {n}_buf.count"));
            }
            ArgPass::Buffer { .. } => {
                w.line(format!(
                    "{n}Writer.bytes.withUnsafeBufferPointer {{ {n}_buf in"
                ));
                w.indent();
                w.line(format!("let {n}_ptr = {n}_buf.baseAddress"));
                w.line(format!("let {n}_len = {n}_buf.count"));
            }
            ArgPass::Direct { .. } | ArgPass::Object { .. } => unreachable!(),
        }
    }

    let c_sym = format!("{}_async", f.c_base);
    let call_args = with_self_arg(
        build_c_call_args(&f.params, c_prefix, module_name),
        self_arg,
    );
    let rp = ret_pass(f.ret.as_ref(), module_name, c_prefix);
    let cb_param_names = async_callback_param_names(&rp);

    let mut launch_prefix = String::new();
    if !call_args.is_empty() {
        launch_prefix.push_str(&call_args);
        launch_prefix.push_str(", ");
    }
    if f.cancellable {
        launch_prefix.push_str("nil, ");
    }
    w.line(format!("{c_sym}({launch_prefix}{{ {cb_param_names} in"));
    w.indent();
    w.line(format!(
        "let contRef = Unmanaged<ContinuationRef<{ret_swift}, {err_ty}>>.fromOpaque(context!).takeRetainedValue()"
    ));
    w.line("if let err = err, err.pointee.code != 0 {");
    w.indent();
    w.line("let code = err.pointee.code");
    w.line("let msg = err.pointee.message.flatMap { String(cString: $0) } ?? \"\"");
    for line in err.async_err_lines() {
        w.line(line);
    }
    w.dedent();
    w.line("} else {");
    w.indent();
    render_async_resume_result(&mut w, f, &rp, err, ctx, &mut counter);
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}, ctx)");

    for _ in 0..closure_params.len() {
        w.dedent();
        w.line("}");
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    out.push_str(&w.finish());
}

/// The formal names of the async completion callback's slots, by return
/// plan: `context` and `err` always, plus the result slot(s) the return
/// shape adds.
fn async_callback_param_names(rp: &RetPass) -> &'static str {
    match rp {
        RetPass::Void => "context, err",
        RetPass::Buffer => "context, err, resultPtr, resultLen",
        RetPass::Bytes => "context, err, result, resultLen",
        _ => "context, err, result",
    }
}

/// Render the success branch of an async completion callback: convert the
/// callback's result slots and resume the continuation exactly once.
///
/// Result buffers (strings, bytes, and buffered values) are borrowed for the
/// callback's duration: they're deep-copied or decoded before the callback
/// returns and never freed here. Owned-object results are the exception; the
/// callback receives ownership and the pointer is adopted by its wrapper
/// class.
fn render_async_resume_result(
    w: &mut CodeWriter,
    f: &FnBinding,
    rp: &RetPass,
    err: ErrCtx,
    ctx: SwiftCtx,
    counter: &mut usize,
) {
    match rp {
        RetPass::Void => {
            w.line("contRef.value.resume(returning: ())");
        }
        RetPass::Buffer => {
            let ty = f.ret.as_ref().expect("a buffered return carries a type");
            // Borrowed for the callback's duration: copy the bytes and decode
            // inside the callback; the producer frees its own buffer after.
            w.line("guard let resultPtr = resultPtr else {");
            w.indent();
            w.line(err.async_fail_stmt(-1, "null buffer"));
            if err.throws {
                w.line("return");
            }
            w.dedent();
            w.line("}");
            w.line(
                "let resultBytes = [UInt8](UnsafeBufferPointer(start: resultPtr, count: resultLen))",
            );
            w.line("var resultReader = WvReader(bytes: resultBytes)");
            let v = fresh(counter, "v");
            read_value_stmts(w, ty, &v, "resultReader", ctx, counter);
            w.line("resultReader.finish()");
            w.line(format!("contRef.value.resume(returning: {v})"));
        }
        RetPass::String => {
            w.line("guard let result = result else {");
            w.indent();
            w.line(err.async_fail_stmt(-1, "null string"));
            // `fatalError` already never returns; only the resuming
            // (throwing) flavor needs an explicit exit from the guard.
            if err.throws {
                w.line("return");
            }
            w.dedent();
            w.line("}");
            // The string is borrowed for the callback's duration: copy it,
            // don't free it (the producer releases its own buffer).
            w.line("contRef.value.resume(returning: String(cString: result))");
        }
        RetPass::Bytes => {
            w.line("if let result = result {");
            w.scope(|w| {
                w.line("contRef.value.resume(returning: Data(bytes: result, count: resultLen))");
            });
            w.line("} else {");
            w.scope(|w| {
                w.line("contRef.value.resume(returning: Data())");
            });
            w.line("}");
        }
        // An owned interface result is adopted: the consumer owns it and the
        // wrapper's deinit calls `_destroy`.
        RetPass::Object {
            nullable: false, ..
        } => {
            let ty_name = ctx.ty_name(local_type_name(ret_interface_name(f)));
            w.line("guard let result = result else {");
            w.indent();
            w.line(err.async_fail_stmt(-1, "null pointer"));
            if err.throws {
                w.line("return");
            }
            w.dedent();
            w.line("}");
            w.line(format!(
                "contRef.value.resume(returning: {ty_name}(ptr: result))"
            ));
        }
        // A nullable owned object pointer: null means none.
        RetPass::Object { nullable: true, .. } => {
            let ty_name = ctx.ty_name(local_type_name(ret_interface_name(f)));
            w.line(format!(
                "contRef.value.resume(returning: result.map {{ {ty_name}(ptr: $0) }})"
            ));
        }
        RetPass::Direct => match f.ret.as_ref() {
            Some(TypeRef::Enum(name)) => {
                let ty_name = ctx.ty_name(local_type_name(name));
                w.line(format!(
                    "contRef.value.resume(returning: {ty_name}(rawValue: result.rawValue)!)"
                ));
            }
            Some(TypeRef::TypedHandle(_)) => {
                w.line("contRef.value.resume(returning: UInt64(UInt(bitPattern: result)))");
            }
            _ => {
                w.line("contRef.value.resume(returning: result)");
            }
        },
    }
}

/// Emit the lazy sequence class backing one `iter<T>` function.
///
/// The class conforms to `Sequence & IteratorProtocol` and owns the C
/// iterator handle. Each `next()` issues exactly one producer `next` call;
/// the handle is destroyed eagerly on exhaustion (or on a mid-stream error)
/// and again, guarded against double-destroy by the nulled handle, from
/// `deinit` when iteration is abandoned early. Elements are converted and
/// released per the [`weaveffi_core::plan::elem_free`] contract: strings are
/// copied then freed, bytes and buffered elements are copied or decoded then
/// released with `weaveffi_free_bytes`, owned interface pointers are adopted
/// by their wrapper classes, and by-value elements need no release.
///
/// Errors follow the owning function's [`ErrorStrategy`]. `next()` cannot
/// throw under `IteratorProtocol`, so for a throwing function a mid-stream
/// domain error ends iteration and is stored in the sequence's public
/// `error` property for the caller to inspect; for a non-throwing function
/// a reported error can only be a producer bug and traps via `fatalError`.
pub(crate) fn render_swift_iterator_class(
    out: &mut String,
    mb: &ModuleBinding,
    f: &FnBinding,
    it: &IteratorBinding,
    ctx: SwiftCtx,
) {
    let protocol = it.protocol(f);
    let class_name = iterator_class_name(it, ctx.c_prefix);
    let next_fn = &it.next.symbol;
    let destroy_fn = &it.destroy_symbol;
    let inner = &it.elem;
    let elem_swift = swift_type_ctx(inner, ctx);
    let stem = domain_stem(mb);
    let throws = protocol.error == ErrorStrategy::Throws;
    let has_len_slot = protocol.elem_free == ElemFree::Bytes;
    let is_bytes_elem = matches!(inner, TypeRef::Bytes | TypeRef::BorrowedBytes);
    // A `(ptr, len)` element that isn't raw bytes is a buffered value the
    // wrapper decodes.
    let is_buffered_elem = has_len_slot && !is_bytes_elem;

    // `out_item` is the slot after the iterator handle; render its pointee as
    // the element C type so enum slots get the imported C enum
    // (`{prefix}_{module}_{Name}`).
    let elem_c_type = it
        .next
        .params
        .get(1)
        .map(|p| {
            p.ty.render_c(ctx.c_prefix)
                .trim_end_matches('*')
                .trim()
                .to_string()
        })
        .unwrap_or_default();

    // The `out_item` slot declaration.
    let (c_var, default): (String, String) = match inner {
        _ if has_len_slot => ("UnsafePointer<UInt8>?".to_string(), "nil".to_string()),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            ("UnsafePointer<CChar>?".to_string(), "nil".to_string())
        }
        TypeRef::Interface(_) | TypeRef::TypedHandle(_) | TypeRef::Optional(_) => {
            ("OpaquePointer?".to_string(), "nil".to_string())
        }
        TypeRef::Enum(_) => (elem_c_type.clone(), format!("{elem_c_type}(0)")),
        _ => (swift_type_for(inner), swift_scalar_default(inner)),
    };

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "/// A lazy sequence over the `{elem_swift}` elements streamed by `{}`.",
        it.launch.symbol
    ));
    w.line("///");
    w.line("/// Each `next()` call pulls exactly one element from the producer. The");
    w.line("/// underlying C iterator is destroyed eagerly on exhaustion and from");
    w.line("/// `deinit` when iteration is abandoned early.");
    if throws {
        w.line("///");
        w.line("/// If the producer reports an error mid-stream, iteration ends and the");
        w.line("/// error is stored in ``error`` for the caller to inspect after the loop.");
    }
    w.line(format!(
        "public final class {class_name}: Sequence, IteratorProtocol {{"
    ));
    w.indent();
    w.line("private var handle: OpaquePointer?");
    if throws {
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
        w.line(format!("{destroy_fn}(handle)"));
        w.line("self.handle = nil");
    });
    w.line("}");
    w.blank();
    w.line("/// Pulls the next element from the producer, or returns `nil` once the");
    w.line("/// stream is exhausted (destroying the underlying iterator).");
    w.line(format!("public func next() -> {elem_swift}? {{"));
    w.indent();
    w.line("guard let handle = handle else { return nil }");
    w.line(format!("var item: {c_var} = {default}"));
    if has_len_slot {
        w.line("var itemLen: Int = 0");
    }
    w.line("var err = weaveffi_error(code: 0, message: nil, payload_ptr: nil, payload_len: 0)");
    if has_len_slot {
        w.line(format!(
            "if {next_fn}(handle, &item, &itemLen, &err) == 0 {{"
        ));
    } else {
        w.line(format!("if {next_fn}(handle, &item, &err) == 0 {{"));
    }
    w.indent();
    w.line("if err.code != 0 {");
    w.indent();
    w.line("let code = err.code");
    w.line("let message = err.message.flatMap { String(cString: $0) } ?? \"\"");
    if throws {
        match &stem {
            Some(stem) => {
                w.line("let payload: [UInt8]? = err.payload_ptr.map { [UInt8](UnsafeBufferPointer(start: $0, count: err.payload_len)) }");
                w.line("weaveffi_error_clear(&err)");
                w.line(format!(
                    "self.error = map{stem}(code: code, message: message, payload: payload)"
                ));
            }
            None => {
                w.line("weaveffi_error_clear(&err)");
                w.line(format!(
                    "self.error = {ERROR_BRAND}.error(code: code, message: message)"
                ));
            }
        }
    } else {
        w.line("weaveffi_error_clear(&err)");
        w.line("fatalError(\"\\(code): \\(message)\")");
    }
    w.dedent();
    w.line("}");
    w.line("destroyHandle()");
    w.line("return nil");
    w.dedent();
    w.line("}");

    if is_buffered_elem {
        // Decode the element buffer, then release it (ElemFree::Bytes).
        w.line("let itemBytes = [UInt8](UnsafeBufferPointer(start: item, count: itemLen))");
        w.line("weaveffi_free_bytes(UnsafeMutablePointer(mutating: item), itemLen)");
        w.line("var itemReader = WvReader(bytes: itemBytes)");
        let mut counter = 0usize;
        let v = fresh(&mut counter, "v");
        read_value_stmts(&mut w, inner, &v, "itemReader", ctx, &mut counter);
        w.line("itemReader.finish()");
        w.line(format!("return {v}"));
    } else if is_bytes_elem {
        w.line("let element = Data(bytes: item!, count: itemLen)");
        w.line("weaveffi_free_bytes(UnsafeMutablePointer(mutating: item), itemLen)");
        w.line("return element");
    } else {
        let convert = match inner {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => "String(cString: item!)".to_string(),
            // An owned interface element is adopted by the wrapper class,
            // whose deinit owes the `_destroy`.
            TypeRef::Interface(name) => {
                format!("{}(ptr: item!)", ctx.ty_name(local_type_name(name)))
            }
            TypeRef::TypedHandle(_) => "UInt64(UInt(bitPattern: item))".to_string(),
            TypeRef::Enum(name) => format!(
                "{}(rawValue: item.rawValue)!",
                ctx.ty_name(local_type_name(name))
            ),
            _ => "item".to_string(),
        };
        w.line(format!("let element = {convert}"));
        if protocol.elem_free == ElemFree::String {
            w.line("weaveffi_free_string(item)");
        }
        w.line("return element");
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}
