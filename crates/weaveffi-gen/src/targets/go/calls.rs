//! Call rendering: sync, async, and iterator wrappers, callback-interface
//! types and trampolines, and the argument/return marshalling they share.
//!
//! Marshalling dispatch follows the shared plan layer: each parameter's
//! passing contract comes from [`ParamBinding::arg_pass`], each result's
//! receiving contract from [`plan::ret_pass`], so this module only spells
//! those contracts in Go rather than re-deriving them from `Ty`.

use crate::codegen::CodeWriter;
use crate::lang;
use heck::ToUpperCamelCase;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::Ty;
use weaveffi_model::model::{
    AsyncBinding, BindingModel, CallShape, CallbackInterfaceBinding, CallbackMethodBinding,
    FnBinding, IteratorBinding, ParamBinding,
};
use weaveffi_model::plan::{self, ArgPass, ErrorStrategy, RetPass};

use crate::targets::go::codec::{emit_buffer_read, emit_buffer_write};
use crate::targets::go::docs::{emit_doc, emit_fn_doc};
use crate::targets::go::types::{
    cgo_type, from_c_direct, go_adopt_expr, go_param_ident, go_type, go_zero, strip_const,
    to_c_direct, vtable_accessor, vtable_var,
};

/// Package names the generated bindings use inside function bodies. A slot
/// or parameter spelled like one of them is escaped so it can't shadow the
/// package.
const SHADOWED_PACKAGES: &[&str] = &["cgo", "context", "fmt", "iter", "runtime", "unsafe"];

// ── Errors ──

/// How a wrapper body reports a non-zero error slot.
///
/// A callable with `throws == true` returns `(T, error)` and maps codes
/// through the declaring module's typed helper (`wvMapKv`), falling back to
/// the generic `Error` when no domain is in scope. A callable with
/// `throws == false` has a plain signature and panics via `wvTrap` instead,
/// since a reported error can only be a producer panic, an
/// argument-marshalling failure, or a callback implementation that panicked.
#[derive(Clone, Copy)]
pub(crate) struct ErrCtx<'a> {
    /// `true` when the wrapper returns `(T, error)` and surfaces typed errors.
    pub(crate) throws: bool,
    /// PascalCase stem of the domain in effect (`Kv` names `wvMapKv`); `None`
    /// falls back to the generic error.
    stem: Option<&'a str>,
}

impl<'a> ErrCtx<'a> {
    /// Build the wrapper error context for `f` from the shared plan's
    /// [`ErrorStrategy`].
    pub(crate) fn of(f: &FnBinding, stem: Option<&'a str>) -> Self {
        Self {
            throws: matches!(f.error_strategy(), ErrorStrategy::Throws),
            stem,
        }
    }

    /// The Go expression converting the `wvFailure` value `failure` into an
    /// `error`.
    fn map_call(&self, failure: &str) -> String {
        match self.stem {
            Some(stem) => format!("wvMap{stem}({failure})"),
            None => format!("{failure}.err()"),
        }
    }

    /// Emit the statement(s) checking the error slot named `slot`. A throwing
    /// wrapper returns `zero` (when the function has a result) plus the
    /// mapped error; a plain wrapper traps.
    fn emit_check(&self, w: &mut CodeWriter, slot: &str, zero: Option<&str>) {
        if self.throws {
            let map = self.map_call(&format!("wvTakeError(&{slot})"));
            w.block(format!("if {slot}.code != 0 {{"), "}", |w| {
                match zero {
                    Some(z) => w.line(format!("return {z}, {map}")),
                    None => w.line(format!("return {map}")),
                };
            });
        } else {
            w.line(format!("wvTrap(&{slot})"));
        }
    }

    /// The Go return-type suffix (including the leading space) of a sync
    /// wrapper returning `ret`.
    fn ret_sig(&self, ret: Option<&Ty>) -> String {
        match (ret, self.throws) {
            (Some(r), true) => format!(" ({}, error)", go_type(r)),
            (Some(r), false) => format!(" {}", go_type(r)),
            (None, true) => " error".into(),
            (None, false) => String::new(),
        }
    }

    /// The suffix of every successful `return`: `, nil` when the wrapper also
    /// returns an error.
    fn ok_tail(&self) -> &'static str {
        if self.throws {
            ", nil"
        } else {
            ""
        }
    }
}

// ── Trampolines and the cgo preamble ──

/// The Go spelling of one C ABI slot name inside an exported trampoline's
/// formal list: a slot named after a Go keyword or a package the body uses
/// gains a trailing underscore. The preamble `extern` uses the same spelling
/// so the two prototypes agree.
fn go_slot_ident(name: &str) -> String {
    let ident = lang::escape_ident(name, lang::GO_KEYWORDS);
    if SHADOWED_PACKAGES.contains(&ident.as_str()) {
        format!("{ident}_")
    } else {
        ident
    }
}

/// The Go spelling of a wrapper parameter, escaped against keywords, the
/// packages wrapper bodies use, and (in an async wrapper) the leading `ctx`.
fn param_ident(p: &ParamBinding, is_async: bool) -> String {
    let ident = go_param_ident(&p.name, is_async);
    if SHADOWED_PACKAGES.contains(&ident.as_str()) {
        format!("{ident}_")
    } else {
        ident
    }
}

/// The C name of the exported Go trampoline for an async completion typedef.
fn trampoline_name(c_type_name: &str) -> String {
    format!("goWv_{c_type_name}")
}

/// The C name of the exported Go trampoline behind one callback-interface
/// vtable entry (`goWv_{c_tag}_{method}`); `free` names the trailing entry.
fn cb_trampoline_name(c_tag: &str, method: &str) -> String {
    format!("goWv_{c_tag}_{method}")
}

/// The preamble `extern` declaration for one exported trampoline. Pointer
/// types are rendered const-free to match the prototypes cgo writes into
/// `_cgo_export.h` from the Go signature.
fn extern_decl(name: &str, ret: &CType, params: &[AbiParam], prefix: &str) -> String {
    let args: Vec<String> = params
        .iter()
        .map(|p| {
            format!(
                "{} {}",
                strip_const(&p.ty).render_c(prefix),
                go_slot_ident(&p.name)
            )
        })
        .collect();
    format!(
        "extern {} {name}({});",
        ret.render_c(prefix),
        args.join(", ")
    )
}

/// The preamble definition of the one process-wide static vtable for `cb`:
/// one trampoline per method in declaration order, then `free`. An entry
/// whose C signature carries `const` pointers is cast back to the vtable's
/// exact field type, since the exported Go function is declared const-free.
fn vtable_def(cb: &CallbackInterfaceBinding, prefix: &str) -> String {
    let mut s = format!(
        "static const {} {} = {{\n",
        cb.vtable_tag,
        vtable_var(&cb.vtable_tag)
    );
    for m in &cb.methods {
        let tramp = cb_trampoline_name(&cb.c_tag, &m.name);
        let needs_cast = m.abi_params.iter().any(|p| strip_const(&p.ty) != p.ty);
        if needs_cast {
            let types: Vec<String> = m.abi_params.iter().map(|p| p.ty.render_c(prefix)).collect();
            s.push_str(&format!(
                "    ({} (*)({})){tramp},\n",
                m.abi_ret.render_c(prefix),
                types.join(", ")
            ));
        } else {
            s.push_str(&format!("    {tramp},\n"));
        }
    }
    s.push_str(&format!(
        "    {},\n}};\n",
        cb_trampoline_name(&cb.c_tag, "free")
    ));
    // cgo reaches a C variable through `//go:cgo_import_static`, which needs
    // external linkage, so a `static const` table can't be named as
    // `&C.wvVtable_...` from Go. A static accessor function is callable
    // through the ordinary cgo stub in the same translation unit.
    s.push_str(&format!(
        "__attribute__((unused)) static const {}* {}(void) {{ return &{}; }}",
        cb.vtable_tag,
        vtable_accessor(&cb.vtable_tag),
        vtable_var(&cb.vtable_tag)
    ));
    s
}

/// Every declaration the cgo preamble needs beyond the header include: for
/// each callback interface, the `extern` prototypes of its method and `free`
/// trampolines followed by its static vtable and accessor; then one `extern`
/// per async completion callback, including async interface members.
///
/// A file that uses `//export` may only put declarations in its preamble
/// (the preamble is compiled into two C translation units); the vtable is a
/// `static const` so each unit gets a private copy and no symbol is
/// duplicated. Go takes the address of the copy in its own unit through the
/// static accessor, so the producer always sees one vtable whose entries
/// live for the process.
pub(crate) fn collect_preamble_decls(model: &BindingModel) -> Vec<String> {
    let prefix = model.prefix.as_str();
    let mut decls = Vec::new();
    for m in &model.modules {
        for cb in &m.callback_interfaces {
            for meth in &cb.methods {
                decls.push(extern_decl(
                    &cb_trampoline_name(&cb.c_tag, &meth.name),
                    &meth.abi_ret,
                    &meth.abi_params,
                    prefix,
                ));
            }
            decls.push(extern_decl(
                &cb_trampoline_name(&cb.c_tag, "free"),
                &CType::Void,
                &[AbiParam::new("ctx", CType::ptr(CType::Void))],
                prefix,
            ));
            decls.push(vtable_def(cb, prefix));
        }
        for f in m.callables() {
            if let CallShape::Async(ab) = &f.shape {
                decls.push(extern_decl(
                    &trampoline_name(&ab.callback_type),
                    &CType::Void,
                    &ab.callback_params,
                    prefix,
                ));
            }
        }
    }
    decls
}

// ── Callback interfaces ──

/// The Go method signature of one callback-interface method, as it appears
/// in the consumer-implemented interface type: `OnMessage(text string,
/// weight int32) int64`.
fn cb_method_sig(m: &CallbackMethodBinding) -> String {
    let params: Vec<String> = m
        .params
        .iter()
        .map(|p| format!("{} {}", param_ident(p, false), go_type(&p.ty)))
        .collect();
    let ret = match &m.ret {
        Some(ty) => format!(" {}", go_type(ty)),
        None => String::new(),
    };
    format!(
        "{}({}){ret}",
        m.name.to_upper_camel_case(),
        params.join(", ")
    )
}

/// Emit statements converting one callback-method parameter's C slots into
/// a Go value bound to `arg{idx}`, returning that local's name.
///
/// Strings, bytes, and buffers arriving in a trampoline are borrowed for the
/// dispatch: they're copied or decoded and nothing is freed. An object
/// argument transfers one strong reference, which is adopted into a wrapper
/// (a null `Interface?` adopts to nil).
fn emit_cb_param_arg(w: &mut CodeWriter, idx: usize, p: &ParamBinding) -> String {
    let arg = format!("arg{idx}");
    match p.arg_pass() {
        ArgPass::Buffer { ptr, len } => {
            let r = format!("rArg{idx}");
            w.line(format!(
                "{r} := &wvReader{{buf: wvBorrowBytes({}, {})}}",
                go_slot_ident(&ptr.name),
                go_slot_ident(&len.name)
            ));
            w.line(format!("var {arg} {}", go_type(&p.ty)));
            emit_buffer_read(w, &r, &arg, &p.ty, &format!("Arg{idx}"), 0);
            w.line(format!("{r}.expectEnd()"));
        }
        ArgPass::String { ptr, len } => {
            w.line(format!(
                "{arg} := wvBorrowString({}, {})",
                go_slot_ident(&ptr.name),
                go_slot_ident(&len.name)
            ));
        }
        ArgPass::Bytes { ptr, len } => {
            w.line(format!(
                "{arg} := wvBorrowBytes({}, {})",
                go_slot_ident(&ptr.name),
                go_slot_ident(&len.name)
            ));
        }
        ArgPass::Object { slot, .. } => {
            w.line(format!(
                "{arg} := {}",
                go_adopt_expr(&p.ty, &go_slot_ident(&slot.name))
            ));
        }
        ArgPass::Direct { slot } => {
            w.line(format!(
                "{arg} := {}",
                from_c_direct(&go_slot_ident(&slot.name), &p.ty)
            ));
        }
        ArgPass::Callback { .. } => {
            unreachable!("validation rejects callback interfaces as callback-method parameters")
        }
    }
    arg
}

/// Emit the exported trampoline behind one vtable entry. It recovers the
/// implementation from the `cgo.Handle` passed as `ctx`, converts the
/// borrowed arguments, calls the Go method, and writes a direct-family
/// result into the C return. A panic in the implementation is recovered and
/// reported through `{prefix}_error_set(out_err, -4, message)`, and the zero
/// value is returned; nothing ever unwinds through the C frame.
fn render_cb_trampoline(
    w: &mut CodeWriter,
    prefix: &str,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    iface_name: &str,
) {
    let tramp = cb_trampoline_name(&cb.c_tag, &m.name);
    let formals: Vec<String> = m
        .abi_params
        .iter()
        .map(|s| format!("{} {}", go_slot_ident(&s.name), cgo_type(&s.ty, prefix)))
        .collect();
    let ctx = go_slot_ident(&m.abi_params[0].name);
    let out_err = go_slot_ident(&m.abi_params[m.abi_params.len() - 1].name);
    let ret_sig = match &m.ret {
        Some(_) => format!(" (ret {})", cgo_type(&m.abi_ret, prefix)),
        None => String::new(),
    };
    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}){ret_sig} {{", formals.join(", ")),
        "}",
        |w| {
            w.block("defer func() {", "}()", |w| {
                w.block("if r := recover(); r != nil {", "}", |w| {
                    w.line(format!("wvForeignError({out_err}, r)"));
                });
            });
            w.line(format!(
                "impl := cgo.Handle(uintptr({ctx})).Value().({iface_name})"
            ));
            let args: Vec<String> = m
                .params
                .iter()
                .enumerate()
                .map(|(idx, p)| emit_cb_param_arg(w, idx, p))
                .collect();
            let call = format!("impl.{}({})", m.name.to_upper_camel_case(), args.join(", "));
            if m.ret.is_some() {
                w.line(format!("ret = {}", to_c_direct(&call, &m.abi_ret, prefix)));
                w.line("return");
            } else {
                w.line(call);
            }
        },
    );
    w.blank();
}

/// Render one callback interface: the Go `interface` type the consumer
/// implements (one method per IDL method, PascalCase, direct-family or void
/// returns), the exported trampoline behind each vtable entry, and the
/// `free` trampoline that deletes the `cgo.Handle` once the producer drops
/// its last reference to the callback.
///
/// Passing an implementation to a producer function stores it in a
/// `cgo.Handle` (the `void* ctx` slot) and passes the address of the
/// interface's static vtable from the cgo preamble (see
/// [`collect_preamble_decls`]). The producer may invoke any trampoline from
/// any thread; cgo attaches the calling thread to the Go runtime.
pub(crate) fn render_callback_interface(
    out: &mut String,
    prefix: &str,
    cb: &CallbackInterfaceBinding,
) {
    let name = cb.name.to_upper_camel_case();
    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &cb.doc, "", Some(&name));
    if d.is_empty() {
        w.line(format!(
            "// {name} is a callback interface: implement it in Go and pass the value to"
        ));
        w.line("// native functions that accept it.");
    } else {
        w.raw(d);
        w.line("//");
        w.line("// Implement this interface in Go and pass the value to native functions");
        w.line("// that accept it.");
    }
    w.line("//");
    w.line("// The native library may call any method from any thread until it releases");
    w.line("// the implementation. A panic in a method is reported to the native caller");
    w.line("// as a foreign error (code -4) instead of crashing the process.");
    if let Some(msg) = &cb.deprecated {
        w.line("//");
        w.line(format!("// Deprecated: {msg}"));
    }
    w.block(format!("type {name} interface {{"), "}", |w| {
        for m in &cb.methods {
            let mut md = String::new();
            emit_fn_doc(
                &mut md,
                &m.doc,
                &m.params,
                "\t",
                &m.name.to_upper_camel_case(),
            );
            let has_doc = !md.is_empty();
            w.raw(md);
            emit_deprecated(w, has_doc, &m.deprecated);
            w.line(cb_method_sig(m));
        }
    });
    w.blank();

    for m in &cb.methods {
        render_cb_trampoline(&mut w, prefix, cb, m, &name);
    }

    let free = cb_trampoline_name(&cb.c_tag, "free");
    w.line(format!("//export {free}"));
    w.block(format!("func {free}(ctx unsafe.Pointer) {{"), "}", |w| {
        w.line("cgo.Handle(uintptr(ctx)).Delete()");
    });
    w.blank();
    out.push_str(&w.finish());
}

// ── Shared wrapper pieces ──

/// The wrapper signature line: `func (s *Store) Name(params) ret {` or
/// `func Name(params) ret {`.
fn header(receiver: Option<&str>, go_name: &str, params: &[String], ret_sig: &str) -> String {
    match receiver {
        Some(ty) => format!(
            "func (s *{ty}) {go_name}({}){ret_sig} {{",
            params.join(", ")
        ),
        None => format!("func {go_name}({}){ret_sig} {{", params.join(", ")),
    }
}

/// The Go formal parameter list of a wrapper.
fn go_params(f: &FnBinding) -> Vec<String> {
    let mut params: Vec<String> = Vec::new();
    if f.is_async {
        params.push("ctx context.Context".into());
    }
    params.extend(
        f.params
            .iter()
            .map(|p| format!("{} {}", param_ident(p, f.is_async), go_type(&p.ty))),
    );
    params
}

/// Emit the receiver borrow every method opens with: `s.native()` panics
/// when the wrapper was already closed, and the deferred release keeps the
/// native object alive (even across a concurrent `Close`) until the call
/// returns.
fn emit_receiver(w: &mut CodeWriter, args: &mut Vec<String>) {
    w.line("cSelf := s.native()");
    w.line("defer s.ref.release()");
    args.push("cSelf".into());
}

/// Emit the staging statements and C argument expressions for one Go
/// parameter, dispatching on the shared [`ArgPass`] contract.
///
/// Strings and bytes pass a borrowed view of the Go memory (cgo pins it for
/// the call); a buffered parameter is packed into a `wvWriter` first. An
/// object parameter borrows the wrapper's pointer for the call, exactly like
/// a receiver; a nil `Interface?` passes NULL. A callback interface stores
/// the implementation in a `cgo.Handle` passed as `ctx` alongside the
/// address of the interface's static vtable.
fn emit_param(
    w: &mut CodeWriter,
    args: &mut Vec<String>,
    p: &ParamBinding,
    prefix: &str,
    is_async: bool,
) {
    let name = param_ident(p, is_async);
    let pascal = name.to_upper_camel_case();
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            let wv = format!("w{pascal}");
            w.line(format!("{wv} := &wvWriter{{}}"));
            emit_buffer_write(w, &wv, &name, &p.ty, &pascal, 0);
            w.line(format!("c{pascal}Ptr, c{pascal}Len := wvBytes({wv}.buf)"));
            args.push(format!("c{pascal}Ptr"));
            args.push(format!("c{pascal}Len"));
        }
        ArgPass::String { .. } => {
            w.line(format!("c{pascal}Ptr, c{pascal}Len := wvStr({name})"));
            args.push(format!("c{pascal}Ptr"));
            args.push(format!("c{pascal}Len"));
        }
        ArgPass::Bytes { .. } => {
            w.line(format!("c{pascal}Ptr, c{pascal}Len := wvBytes({name})"));
            args.push(format!("c{pascal}Ptr"));
            args.push(format!("c{pascal}Len"));
        }
        ArgPass::Object { slot, nullable } => {
            let cv = format!("c{pascal}");
            if nullable {
                w.line(format!(
                    "var {cv} {}",
                    cgo_type(&strip_const(&slot.ty), prefix)
                ));
                w.block(format!("if {name} != nil {{"), "}", |w| {
                    w.line(format!("{cv} = {name}.native()"));
                    w.line(format!("defer {name}.ref.release()"));
                });
            } else {
                w.line(format!("{cv} := {name}.native()"));
                w.line(format!("defer {name}.ref.release()"));
            }
            args.push(cv);
        }
        ArgPass::Callback { vtable, .. } => {
            let CType::Ptr { pointee, .. } = &vtable.ty else {
                unreachable!("a callback vtable slot is a pointer")
            };
            let hv = format!("h{pascal}");
            w.line(format!("{hv} := cgo.NewHandle({name})"));
            args.push(format!("C.wvHandlePtr(C.uintptr_t({hv}))"));
            args.push(format!(
                "C.{}()",
                vtable_accessor(&pointee.render_c(prefix))
            ));
        }
        ArgPass::Direct { slot } => args.push(to_c_direct(&name, &slot.ty, prefix)),
    }
}

/// Emit the statements receiving one producer-owned value per its
/// [`RetPass`] contract, binding it to the Go local `dst` (declared here).
/// `ptr` and `len` name the C slots holding it (`len` only for strings,
/// bytes, and buffers).
///
/// Strings and bytes are copied and released with `{prefix}_free_bytes`; a
/// buffer is copied, released, decoded, and checked for trailing bytes; an
/// object transfers one strong reference that the wrapper adopts (a null
/// `Interface?` adopts to nil).
fn emit_receive(w: &mut CodeWriter, ty: &Ty, pass: &RetPass, dst: &str, ptr: &str, len: &str) {
    match receive_expr(ty, pass, ptr, len) {
        Some(expr) => {
            w.line(format!("{dst} := {expr}"));
        }
        None => {
            let r = format!("r{}", dst.to_upper_camel_case());
            w.line(format!(
                "{r} := &wvReader{{buf: wvTakeBytes({ptr}, {len})}}"
            ));
            w.line(format!("var {dst} {}", go_type(ty)));
            emit_buffer_read(w, &r, dst, ty, &dst.to_upper_camel_case(), 0);
            w.line(format!("{r}.expectEnd()"));
        }
    }
}

/// The single Go expression receiving a value per `pass`, or `None` for a
/// buffer, which needs decoding statements (see [`emit_receive`]).
fn receive_expr(ty: &Ty, pass: &RetPass, ptr: &str, len: &str) -> Option<String> {
    match pass {
        RetPass::Void => unreachable!("a present value is never void"),
        RetPass::Direct => Some(from_c_direct(ptr, ty)),
        RetPass::String => Some(format!("wvTakeString({ptr}, {len})")),
        RetPass::Bytes => Some(format!("wvTakeBytes({ptr}, {len})")),
        RetPass::Object { .. } => Some(go_adopt_expr(ty, ptr)),
        RetPass::Buffer => None,
    }
}

/// Emit the statements returning a received value: `return <expr><tail>`,
/// or the decoding statements followed by `return ret<tail>` for a buffer.
fn emit_return(w: &mut CodeWriter, ty: &Ty, pass: &RetPass, ptr: &str, len: &str, tail: &str) {
    match receive_expr(ty, pass, ptr, len) {
        Some(expr) => {
            w.line(format!("return {expr}{tail}"));
        }
        None => {
            emit_receive(w, ty, pass, "ret", ptr, len);
            w.line(format!("return ret{tail}"));
        }
    }
}

/// Emit a `Deprecated:` paragraph after a doc comment that may be empty.
fn emit_deprecated(w: &mut CodeWriter, has_doc: bool, deprecated: &Option<String>) {
    if let Some(msg) = deprecated {
        if has_doc {
            w.line("//");
        }
        w.line(format!("// Deprecated: {msg}"));
    }
}

/// `true` when a value received per `pass` arrives as a `(ptr, len)` pair.
fn has_len(pass: &RetPass) -> bool {
    matches!(pass, RetPass::String | RetPass::Bytes | RetPass::Buffer)
}

// ── Async ──

/// An async callable: a Go wrapper taking a leading `context.Context` that
/// launches the C call with a completion trampoline and blocks on a buffered
/// channel, plus the exported trampoline itself. The channel travels to the
/// producer as a `cgo.Handle` in the `context` slot; `wvComplete` resolves
/// and deletes it, so the completion is delivered exactly once.
///
/// Every async wrapper returns an `error` (the context's, at least). A
/// cancellable function creates a native cancel token, cancels it when `ctx`
/// is done, and returns `ctx.Err()` for the cancelled (`-5`) completion; any
/// other function returns `ctx.Err()` as soon as `ctx` is done and abandons
/// the result. A producer error is mapped through the domain when the
/// function throws, and panics with the generic `Error` otherwise.
pub(crate) fn render_async_function(
    out: &mut String,
    prefix: &str,
    f: &FnBinding,
    ab: &AsyncBinding,
    go_name: &str,
    receiver: Option<&str>,
    err: ErrCtx,
) {
    let proto = ab.protocol(f, prefix);
    let tramp = trampoline_name(&ab.callback_type);
    let val_ty = f
        .ret
        .as_ref()
        .map_or_else(|| "struct{}".to_string(), go_type);

    let mut w = CodeWriter::tabs();

    // The exported completion trampoline.
    let formals: Vec<String> = ab
        .callback_params
        .iter()
        .map(|s| format!("{} {}", go_slot_ident(&s.name), cgo_type(&s.ty, prefix)))
        .collect();
    let handle = go_slot_ident(&ab.callback_params[0].name);
    let err_slot = go_slot_ident(&ab.callback_params[1].name);
    let results: Vec<String> = ab.callback_params[2..]
        .iter()
        .map(|s| go_slot_ident(&s.name))
        .collect();
    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}) {{", formals.join(", ")),
        "}",
        |w| {
            w.block(
                format!("wvComplete({handle}, {err_slot}, func() {val_ty} {{"),
                "})",
                |w| match &f.ret {
                    None => {
                        w.line("return struct{}{}");
                    }
                    Some(ty) => {
                        let len = results.get(1).map_or("", String::as_str);
                        emit_return(w, ty, &proto.result, &results[0], len, "");
                    }
                },
            );
        },
    );
    w.blank();

    // The blocking wrapper.
    let ret_sig = match &f.ret {
        Some(r) => format!(" ({}, error)", go_type(r)),
        None => " error".into(),
    };
    let zero_ret = |z: &str| match &f.ret {
        Some(_) => format!("return {z}, "),
        None => "return ".to_string(),
    };
    let zero = f.ret.as_ref().map(go_zero).unwrap_or_default();
    let mut doc = String::new();
    emit_fn_doc(&mut doc, &f.doc, &f.params, "", go_name);
    if doc.is_empty() {
        doc = format!("// {go_name} calls the native async function.\n");
    }
    w.raw(doc);
    w.line("//");
    if f.cancellable {
        w.line("// It blocks until the call completes. Cancelling ctx cancels the native");
        w.line("// call, which then returns ctx.Err().");
    } else {
        w.line("// It blocks until the call completes or ctx is done; in the latter case it");
        w.line("// returns ctx.Err() at once and the native result is discarded.");
    }
    emit_deprecated(&mut w, true, &f.deprecated);

    w.block(
        header(receiver, go_name, &go_params(f), &ret_sig),
        "}",
        |w| {
            w.block("if err := ctx.Err(); err != nil {", "}", |w| {
                w.line(format!("{}err", zero_ret(&zero)));
            });
            let mut args: Vec<String> = Vec::new();
            if receiver.is_some() {
                emit_receiver(w, &mut args);
            }
            for p in &f.params {
                emit_param(w, &mut args, p, prefix, true);
            }
            let token = if proto.cancellable {
                w.line(format!("wvToken := C.{prefix}_cancel_token_create()"));
                w.line(format!("defer C.{prefix}_cancel_token_destroy(wvToken)"));
                args.push("wvToken".into());
                "wvToken"
            } else {
                "nil"
            };
            w.line(format!("wvDone := make(chan wvOutcome[{val_ty}], 1)"));
            w.line("wvHandle := cgo.NewHandle(wvDone)");
            args.push(format!("C.{}(unsafe.Pointer(C.{tramp}))", ab.callback_type));
            args.push("C.wvHandlePtr(C.uintptr_t(wvHandle))".into());
            w.line(format!("C.{}({})", ab.launch.symbol, args.join(", ")));
            let res = if f.ret.is_some() { "res" } else { "_" };
            w.line(format!("{res}, fail, err := wvAwait(ctx, wvDone, {token})"));
            w.block("if err != nil {", "}", |w| {
                w.line(format!("{}err", zero_ret(&zero)));
            });
            w.block("if fail != nil {", "}", |w| {
                if err.throws {
                    w.line(format!("{}{}", zero_ret(&zero), err.map_call("*fail")));
                } else {
                    w.line("panic(fail.err())");
                }
            });
            match &f.ret {
                Some(_) => w.line("return res, nil"),
                None => w.line("return nil"),
            };
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

// ── Functions ──

/// A sync or iterator callable: the Go wrapper marshalling parameters in,
/// invoking the C symbol, checking the error slot per `err` (typed
/// `(T, error)` when throwing, `wvTrap` panic when plain), and converting the
/// result out. An iterator-returning callable renders through
/// [`render_iterator_fn`] as a lazy sequence instead. With `receiver` set,
/// the wrapper is a method on that wrapper type passing its native pointer
/// as the leading C argument.
pub(crate) fn render_function(
    out: &mut String,
    prefix: &str,
    f: &FnBinding,
    go_name: &str,
    receiver: Option<&str>,
    err: ErrCtx,
) {
    if let CallShape::Iterator(ib) = &f.shape {
        render_iterator_fn(out, prefix, f, ib, go_name, receiver, err);
        return;
    }

    let mut w = CodeWriter::tabs();
    let mut doc = String::new();
    emit_fn_doc(&mut doc, &f.doc, &f.params, "", go_name);
    let has_doc = !doc.is_empty();
    w.raw(doc);
    emit_deprecated(&mut w, has_doc, &f.deprecated);

    let ret_sig = err.ret_sig(f.ret.as_ref());
    let pass = plan::ret_pass(f.ret.as_ref(), prefix);
    w.block(
        header(receiver, go_name, &go_params(f), &ret_sig),
        "}",
        |w| {
            let mut args: Vec<String> = Vec::new();
            if receiver.is_some() {
                emit_receiver(w, &mut args);
            }
            for p in &f.params {
                emit_param(w, &mut args, p, prefix, false);
            }
            if has_len(&pass) {
                w.line("var cRetLen C.size_t");
                args.push("&cRetLen".into());
            }
            w.line(format!("var cErr C.{prefix}_error"));
            args.push("&cErr".into());
            let call = format!("C.{}({})", f.c_base, args.join(", "));
            match &f.ret {
                Some(_) => w.line(format!("cRet := {call}")),
                None => w.line(call),
            };
            err.emit_check(w, "cErr", f.ret.as_ref().map(go_zero).as_deref());
            match &f.ret {
                Some(ty) => {
                    emit_return(w, ty, &pass, "cRet", "cRetLen", err.ok_tail());
                }
                None if err.throws => {
                    w.line("return nil");
                }
                None => {}
            }
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// An `iter<T>`-returning callable, rendered per the shared
/// [`weaveffi_model::plan::IteratorProtocol`] pull contract as Go's standard
/// lazy iteration idiom (the `iter` package):
///
/// - A non-throwing function returns `iter.Seq[T]`. A launch or per-`next`
///   error can only be a producer bug, so it panics via `wvTrap`.
/// - A throwing function returns `iter.Seq2[T, error]`. A launch or
///   per-`next` domain error is yielded as the final `(zero, err)` pair and
///   iteration stops.
///
/// The producer iterator is launched lazily inside the returned closure, one
/// C `next` call runs per consumer step, and the destroy runs exactly once
/// through a `defer`, whether the sequence is exhausted, stops on an error,
/// or is abandoned by an early `break`. A receiver stays borrowed until then.
fn render_iterator_fn(
    out: &mut String,
    prefix: &str,
    f: &FnBinding,
    ib: &IteratorBinding,
    go_name: &str,
    receiver: Option<&str>,
    err: ErrCtx,
) {
    let proto = ib.protocol(f, prefix);
    let throws = matches!(proto.error, ErrorStrategy::Throws);
    let elem = &ib.elem;
    let elem_go = go_type(elem);
    let CType::Ptr { pointee, .. } = &ib.next.params[1].ty else {
        unreachable!("an iterator's out_item slot is always a pointer")
    };
    let item_ty = cgo_type(&strip_const(pointee), prefix);
    let with_len = has_len(&proto.elem);
    let zero = go_zero(elem);

    let (seq_ty, yield_ty) = if throws {
        (
            format!("iter.Seq2[{elem_go}, error]"),
            format!("func({elem_go}, error) bool"),
        )
    } else {
        (
            format!("iter.Seq[{elem_go}]"),
            format!("func({elem_go}) bool"),
        )
    };

    let mut w = CodeWriter::tabs();
    let mut doc = String::new();
    emit_fn_doc(&mut doc, &f.doc, &f.params, "", go_name);
    if doc.is_empty() {
        doc = format!("// {go_name} returns a lazy sequence over the native iterator.\n");
    }
    w.raw(doc);
    w.line("//");
    w.line("// The sequence launches the native iterator on each range and calls next once");
    w.line("// per element; the iterator is destroyed when the range ends, early or not.");
    if throws {
        w.line("// A failure is yielded as a final (zero value, error) pair.");
    }
    emit_deprecated(&mut w, true, &f.deprecated);

    // Statements surfacing a non-zero error slot: yield the mapped domain
    // error and stop when throwing, trap when plain.
    let emit_err_check = |w: &mut CodeWriter, slot: &str| {
        if throws {
            let map = err.map_call(&format!("wvTakeError(&{slot})"));
            w.block(format!("if {slot}.code != 0 {{"), "}", |w| {
                w.line(format!("yield({zero}, {map})"));
                w.line("return");
            });
        } else {
            w.line(format!("wvTrap(&{slot})"));
        }
    };

    w.block(
        header(receiver, go_name, &go_params(f), &format!(" {seq_ty}")),
        "}",
        |w| {
            w.block(format!("return func(yield {yield_ty}) {{"), "}", |w| {
                let mut args: Vec<String> = Vec::new();
                if receiver.is_some() {
                    emit_receiver(w, &mut args);
                }
                for p in &f.params {
                    emit_param(w, &mut args, p, prefix, false);
                }
                w.line(format!("var cErr C.{prefix}_error"));
                args.push("&cErr".into());
                w.line(format!(
                    "cIter := C.{}({})",
                    ib.launch.symbol,
                    args.join(", ")
                ));
                emit_err_check(w, "cErr");
                w.line(format!("defer C.{}(cIter)", ib.destroy_symbol));
                w.block("for {", "}", |w| {
                    w.line(format!("var cItem {item_ty}"));
                    let next_args = if with_len {
                        w.line("var cItemLen C.size_t");
                        "cIter, &cItem, &cItemLen, &cIterErr"
                    } else {
                        "cIter, &cItem, &cIterErr"
                    };
                    w.line(format!("var cIterErr C.{prefix}_error"));
                    w.line(format!("more := C.{}({next_args}) != 0", ib.next.symbol));
                    emit_err_check(w, "cIterErr");
                    w.block("if !more {", "}", |w| {
                        w.line("return");
                    });
                    emit_receive(w, elem, &proto.elem, "item", "cItem", "cItemLen");
                    let yield_call = if throws {
                        "if !yield(item, nil) {"
                    } else {
                        "if !yield(item) {"
                    };
                    w.block(yield_call, "}", |w| {
                        w.line("return");
                    });
                });
            });
        },
    );
    w.blank();
    out.push_str(&w.finish());
}
