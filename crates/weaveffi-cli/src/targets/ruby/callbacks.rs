//! Callback-interface rendering: the consumer-facing Ruby module (duck-typed
//! method set with `NotImplementedError` defaults), the `FFI::Struct` vtable
//! layout with its `{size, flags, free}` header, one trampoline
//! `FFI::Function` per method plus `free`, and the single process-wide
//! vtable instance every registration passes to the producer.
//!
//! The trampolines follow [`CallbackProtocol`](weaveffi_model::plan::CallbackProtocol):
//! `ctx` is resolved through the module's implementation registry, object
//! arguments are adopted into wrappers first, borrowed string, bytes, and
//! buffer arguments are copied or decoded before the implementation runs,
//! and the return crosses back per its family (a direct value, a fresh
//! object reference, or a `{prefix}_alloc` run in the out slots). A domain
//! error raised by a method declared `throws` is reported with its code and
//! payload; any other exception is reported as `-4`. Nothing unwinds
//! through the C frame.

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use heck::ToShoutySnakeCase;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, ErrorBinding};
use weaveffi_model::plan::{ArgPass, ErrorStrategy, RetPass};
use weaveffi_model::ty::{Prim, Ty};

use crate::targets::ruby::docs::{emit_param_docs, emit_return_doc};
use crate::targets::ruby::entities::rb_error_payload_name;
use crate::targets::ruby::types::{rb_abi_types, rb_ffi_type, rb_param_name};
use crate::targets::ruby::RbCtx;

/// The `FFI::Struct` subclass laying out one callback interface's vtable:
/// `Listener` becomes `WvListenerVtable`.
fn rb_vtable_class(name: &str) -> String {
    format!("Wv{name}Vtable")
}

/// The constant holding the one process-wide vtable instance for a callback
/// interface: `Listener` becomes `WV_LISTENER_VTABLE`.
pub(crate) fn rb_vtable_const(name: &str) -> String {
    format!("WV_{}_VTABLE", name.to_shouty_snake_case())
}

/// The constant pinning one method's trampoline `FFI::Function`:
/// `Listener.on_change` becomes `WV_LISTENER_ON_CHANGE`.
fn rb_trampoline_const(cb: &str, method: &str) -> String {
    format!(
        "WV_{}_{}",
        cb.to_shouty_snake_case(),
        method.to_shouty_snake_case()
    )
}

/// The constant pinning the `free` trampoline.
fn rb_free_const(cb: &str) -> String {
    format!("WV_{}_FREE", cb.to_shouty_snake_case())
}

/// The trampoline's Ruby formal parameter names for one method, position
/// for position with [`CallbackMethodBinding::abi_params`]: `wv_ctx`, the
/// value of a direct parameter under its own name, `{name}_ptr` (and
/// `{name}_len`) for every other parameter, the `wv_out_ptr`/`wv_out_len`
/// slots of a string, bytes, or buffer return, then `wv_err`.
fn rb_tramp_formals(m: &CallbackMethodBinding) -> Vec<String> {
    let mut formals = vec!["wv_ctx".to_string()];
    for p in &m.params {
        let n = rb_param_name(&p.name);
        match p.arg_pass() {
            ArgPass::String { .. } | ArgPass::Bytes { .. } | ArgPass::Buffer { .. } => {
                formals.push(format!("{n}_ptr"));
                formals.push(format!("{n}_len"));
            }
            ArgPass::Object { .. } => formals.push(format!("{n}_ptr")),
            _ => formals.push(n),
        }
    }
    if matches!(
        RetPass::of(m.ret.as_ref()),
        RetPass::String | RetPass::Bytes | RetPass::Buffer
    ) {
        formals.push("wv_out_ptr".to_string());
        formals.push("wv_out_len".to_string());
    }
    formals.push("wv_err".to_string());
    formals
}

/// Render one callback interface: the consumer-facing module, the vtable
/// layout, the trampolines, and the static vtable instance. `error` is the
/// domain in scope for the interface's module, which its `throws` methods
/// may raise.
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    ctx: &RbCtx,
    error: Option<&ErrorBinding>,
    cb: &CallbackInterfaceBinding,
) {
    let local = &cb.name;
    let vtable_class = rb_vtable_class(&cb.name);
    let vtable_const = rb_vtable_const(&cb.name);

    // The consumer-facing module: documentation of the required methods, and
    // NotImplementedError defaults for consumers who include it.
    w.blank();
    w.doc(&cb.doc, DocCommentStyle::Hash);
    if cb.doc.is_some() {
        w.line("#");
    }
    w.line("# Consumer-implemented callback interface. Any object responding to the");
    w.line(format!(
        "# methods below is accepted wherever a {local} is expected; include"
    ));
    w.line("# this module to inherit NotImplementedError defaults. The library");
    w.line("# may call the methods from any thread until it releases the");
    w.line("# implementation.");
    if let Some(msg) = &cb.deprecated {
        w.line(format!("# @deprecated {msg}"));
    }
    w.block(format!("module {local}"), "end", |w| {
        for (idx, m) in cb.methods.iter().enumerate() {
            if idx > 0 {
                w.blank();
            }
            w.doc(&m.doc, DocCommentStyle::Hash);
            if let Some(msg) = &m.deprecated {
                w.line(format!("# @deprecated {msg}"));
            }
            emit_param_docs(w, &m.params);
            emit_return_doc(w, m.ret.as_ref());
            if let (ErrorStrategy::Throws, Some(eb)) = (m.error_strategy(), error) {
                w.line(format!(
                    "# @raise [{}] reported to the library with its code and fields",
                    eb.type_name
                ));
            }
            let formals: Vec<String> = m.params.iter().map(|p| rb_param_name(&p.name)).collect();
            let open = if formals.is_empty() {
                format!("def {}", m.name)
            } else {
                format!("def {}({})", m.name, formals.join(", "))
            };
            w.block(open, "end", |w| {
                w.line(format!(
                    "raise NotImplementedError, \"#{{self.class}}#{} is not implemented\"",
                    m.name
                ));
            });
        }
    });

    // The vtable layout: the fixed header, then one pointer per method in
    // declaration order.
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# The C vtable layout the library calls a {local} through."
    ));
    w.block(format!("class {vtable_class} < FFI::Struct"), "end", |w| {
        let mut fields = vec![
            ":wv_size, :uint32".to_string(),
            ":wv_flags, :uint32".to_string(),
            ":wv_free, :pointer".to_string(),
        ];
        fields.extend(cb.methods.iter().map(|m| format!(":{}, :pointer", m.name)));
        w.line(format!("layout {}", fields.join(",\n           ")));
    });

    // The free hook: drop the registry entry; the producer never touches
    // ctx again after this fires.
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# Releases a {local} implementation when the library drops its last"
    ));
    w.line("# reference. May run on any library thread.");
    w.block(
        format!(
            "{} = FFI::Function.new(:void, [:pointer]) do |ctx|",
            rb_free_const(&cb.name)
        ),
        "end",
        |w| {
            w.line("_wv_cb_free(ctx)");
        },
    );

    let protocol = cb.protocol();
    for (m, args) in cb.methods.iter().zip(&protocol.method_args) {
        render_trampoline(w, ctx, error, cb, m, args);
    }

    // The single static vtable instance, filled with the pinned trampolines.
    w.blank();
    w.line(format!(
        "# The one process-wide {local} vtable every registration hands the"
    ));
    w.line("# library; its entries live for the process lifetime.");
    w.line(format!("{vtable_const} = {vtable_class}.new"));
    w.line(format!("{vtable_const}[:wv_size] = {vtable_class}.size"));
    w.line(format!("{vtable_const}[:wv_flags] = 0"));
    w.line(format!(
        "{vtable_const}[:wv_free] = {}",
        rb_free_const(&cb.name)
    ));
    for m in &cb.methods {
        w.line(format!(
            "{vtable_const}[:{}] = {}",
            m.name,
            rb_trampoline_const(&cb.name, &m.name)
        ));
    }
}

/// The integer-range kind (`:i32`, ...) the runtime checks a direct
/// integer return against; C-style enums cross as `i32`.
fn int_kind(ty: &Ty) -> &'static str {
    match ty {
        Ty::Prim(p) if p.is_integer() => p.snake(),
        Ty::Enum(_) => "i32",
        other => unreachable!("{other} is not an integer type"),
    }
}

/// Render one method's trampoline: an `FFI::Function` matching the vtable
/// entry's C signature that receives each argument per its [`RetPass`]
/// plan, resolves `ctx`, invokes the implementation, and hands the return
/// back per its family. Object arguments are adopted first, so their
/// references are owned by a wrapper even when a later step raises. A
/// domain error from a `throws` method goes through `_wv_cb_throw`; any
/// other exception through `_wv_cb_fail` (code `-4`); either way the
/// method's zero value is returned.
fn render_trampoline(
    w: &mut CodeWriter,
    ctx: &RbCtx,
    error: Option<&ErrorBinding>,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    args: &[RetPass],
) {
    let ret_pass = RetPass::of(m.ret.as_ref());
    let default = match (&ret_pass, m.ret.as_ref()) {
        (RetPass::Direct, Some(Ty::Prim(Prim::Bool))) => "false",
        (RetPass::Direct, Some(Ty::Prim(Prim::F32 | Prim::F64))) => "0.0",
        (RetPass::Direct, _) => "0",
        (RetPass::Object { .. }, _) => "FFI::Pointer::NULL",
        _ => "nil",
    };

    w.blank();
    w.line("# @api private");
    w.line(format!("# Trampoline for {}#{}.", cb.name, m.name));
    w.line(format!(
        "{} = FFI::Function.new({}, [{}]) do |{}|",
        rb_trampoline_const(&cb.name, &m.name),
        rb_ffi_type(&m.abi_ret),
        rb_abi_types(&m.abi_params).join(", "),
        rb_tramp_formals(m).join(", ")
    ));
    w.scope(|w| {
        let params: Vec<_> = m.params.iter().zip(args).collect();
        let is_object = |pass: &RetPass| matches!(pass, RetPass::Object { .. });
        for (p, pass) in params.iter().filter(|(_, pass)| is_object(pass)) {
            render_tramp_arg(w, ctx, &p.name, &p.ty, pass);
        }
        for (p, pass) in params.iter().filter(|(_, pass)| !is_object(pass)) {
            render_tramp_arg(w, ctx, &p.name, &p.ty, pass);
        }
        let call_args: Vec<String> = m.params.iter().map(|p| rb_param_name(&p.name)).collect();
        let call = if call_args.is_empty() {
            format!("_wv_cb_lookup(wv_ctx).{}", m.name)
        } else {
            format!("_wv_cb_lookup(wv_ctx).{}({})", m.name, call_args.join(", "))
        };
        // Every conversion runs inside the rescue, so a wrong-typed return
        // surfaces as a callback failure rather than escaping through ffi's
        // own conversion.
        match (&ret_pass, m.ret.as_ref()) {
            (RetPass::Void, _) => {
                w.line(call);
                w.line("nil");
            }
            (RetPass::Direct, Some(Ty::Prim(Prim::Bool))) => {
                w.line(format!("{call} ? true : false"));
            }
            (RetPass::Direct, Some(Ty::Prim(Prim::F32 | Prim::F64))) => {
                w.line(format!("_wv_float({call})"));
            }
            (RetPass::Direct, Some(ty)) => {
                w.line(format!("_wv_int({call}, :{})", int_kind(ty)));
            }
            (RetPass::Object { .. }, Some(ty)) => {
                let class = ty.interface_name().expect("object plan names an interface");
                w.line(format!("_wv_cb_return_object({call}, {class})"));
            }
            (RetPass::String, _) => {
                w.line(format!(
                    "_wv_cb_return_bytes(wv_out_ptr, wv_out_len, _wv_str({call}))"
                ));
            }
            (RetPass::Bytes, _) => {
                w.line(format!(
                    "_wv_cb_return_bytes(wv_out_ptr, wv_out_len, _wv_bytes({call}))"
                ));
            }
            (RetPass::Buffer, Some(ty)) if ctx.codecs.carries_objects(ty) => {
                w.line(format!("result = {call}"));
                w.line("writer = WvBufferWriter.new");
                w.line(ctx.codecs.write(ty, "writer", "result", ""));
                w.line("_wv_cb_return_bytes(wv_out_ptr, wv_out_len, _wv_seal(writer).first)");
            }
            (RetPass::Buffer, Some(ty)) => {
                w.line(format!("result = {call}"));
                w.line(format!(
                    "_wv_cb_return_bytes(wv_out_ptr, wv_out_len, _wv_encode {{ |w| {} }})",
                    ctx.codecs.write(ty, "w", "result", "")
                ));
            }
            _ => unreachable!("a typed return has a type"),
        }
    });
    if let (ErrorStrategy::Throws, Some(eb)) = (m.error_strategy(), error) {
        w.line(format!("rescue {} => e", eb.type_name));
        w.scope(|w| {
            w.line(format!(
                "_wv_cb_throw(wv_err, e) {{ {}(e) }}",
                rb_error_payload_name(eb)
            ));
            w.line(default);
        });
    }
    // Rescue Exception, not StandardError: NotImplementedError is a
    // ScriptError, and nothing may unwind through the C frame.
    w.line("rescue Exception => e # rubocop:disable Lint/RescueException");
    w.scope(|w| {
        w.line("_wv_cb_fail(wv_err, e)");
        w.line(default);
    });
    w.line("end");
}

/// Emit the statement receiving one trampoline argument into the local
/// named after the parameter, per its [`RetPass`] plan. Strings, bytes, and
/// buffers are borrowed `(ptr, len)` pairs copied or decoded before the
/// implementation runs; objects transfer one strong reference that is
/// adopted into a wrapper (`nil` for a null optional slot); direct values
/// arrive under the parameter's own name already.
fn render_tramp_arg(w: &mut CodeWriter, ctx: &RbCtx, name: &str, ty: &Ty, pass: &RetPass) {
    let n = rb_param_name(name);
    match pass {
        RetPass::Void => unreachable!("callback parameters always have a type"),
        RetPass::Direct => {}
        RetPass::String => {
            w.line(format!("{n} = _wv_borrow_string({n}_ptr, {n}_len)"));
        }
        RetPass::Bytes => {
            w.line(format!("{n} = _wv_borrow_bytes({n}_ptr, {n}_len)"));
        }
        RetPass::Buffer => {
            w.line(format!(
                "{n} = _wv_decode_borrowed({n}_ptr, {n}_len) {{ |r| {} }}",
                ctx.codecs.read(ty, "r", "")
            ));
        }
        RetPass::Object { nullable } => {
            let class = ty.interface_name().expect("object plan names an interface");
            if *nullable {
                w.line(format!(
                    "{n} = {n}_ptr.null? ? nil : {class}._from_ptr({n}_ptr)"
                ));
            } else {
                w.line(format!("{n} = {class}._from_ptr({n}_ptr)"));
            }
        }
    }
}
