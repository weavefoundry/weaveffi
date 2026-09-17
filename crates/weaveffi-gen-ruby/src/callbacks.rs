//! Callback-interface rendering: the consumer-facing Ruby module (duck-typed
//! method set with `NotImplementedError` defaults), the `FFI::Struct` vtable
//! layout, one trampoline `FFI::Function` per method plus the trailing
//! `free`, and the single process-wide vtable instance every registration
//! passes to the producer.
//!
//! The trampolines follow [`CallbackProtocol`](weaveffi_core::plan::CallbackProtocol):
//! `ctx` is resolved through the module's implementation registry, borrowed
//! string, bytes, and buffer arguments are copied or decoded before the
//! implementation runs, object arguments are adopted into wrappers, and any
//! exception is reported through `{prefix}_error_set(out_err, -4, message)`
//! instead of unwinding through the C frame.

use heck::ToShoutySnakeCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{CallbackInterfaceBinding, CallbackMethodBinding, ModuleBinding, Ty};
use weaveffi_core::plan::{ArgPass, RetPass};
use weaveffi_core::utils::local_type_name;

use crate::docs::{emit_doc, emit_param_docs};
use crate::types::{rb_abi_types, rb_direct_default, rb_direct_from_c, rb_ffi_type, rb_param_name};

/// The `FFI::Struct` subclass laying out one callback interface's vtable:
/// `Listener` (or `events.Listener`) becomes `WvListenerVtable`.
fn rb_vtable_class(name: &str) -> String {
    format!("Wv{}Vtable", local_type_name(name))
}

/// The constant holding the one process-wide vtable instance for a callback
/// interface: `Listener` becomes `WV_LISTENER_VTABLE`.
pub(crate) fn rb_vtable_const(name: &str) -> String {
    format!("WV_{}_VTABLE", local_type_name(name).to_shouty_snake_case())
}

/// The constant pinning one method's trampoline `FFI::Function`:
/// `Listener.on_message` becomes `WV_LISTENER_ON_MESSAGE`.
fn rb_trampoline_const(cb: &str, method: &str) -> String {
    format!(
        "WV_{}_{}",
        local_type_name(cb).to_shouty_snake_case(),
        method.to_shouty_snake_case()
    )
}

/// The constant pinning the trailing `free` trampoline.
fn rb_free_const(cb: &str) -> String {
    format!("WV_{}_FREE", local_type_name(cb).to_shouty_snake_case())
}

/// The trampoline's Ruby formal parameter names for one method: `ctx`, one
/// formal per parameter slot (a `_ptr`/`_len` pair for bytes and buffered
/// values), then `out_err`. The Ruby spellings mirror the C slot names so the
/// list lines up with [`CallbackMethodBinding::abi_params`] position for
/// position.
fn rb_tramp_formals(m: &CallbackMethodBinding) -> Vec<String> {
    let mut formals = vec!["ctx".to_string()];
    for p in &m.params {
        let n = rb_param_name(&p.name);
        match p.arg_pass() {
            ArgPass::Bytes { .. } | ArgPass::Buffer { .. } => {
                formals.push(format!("{n}_ptr"));
                formals.push(format!("{n}_len"));
            }
            _ => formals.push(n),
        }
    }
    formals.push("out_err".to_string());
    formals
}

/// Render one callback interface: the consumer-facing module, the vtable
/// layout, the trampolines, and the static vtable instance. `prefix` is the
/// C symbol prefix the model was built with (needed to resolve each
/// parameter's receiving plan).
pub(crate) fn render_callback_interface(
    out: &mut String,
    module: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
    prefix: &str,
) {
    let protocol = cb.protocol(&module.path, prefix);
    let local = local_type_name(&cb.name);
    let vtable_class = rb_vtable_class(&cb.name);
    let vtable_const = rb_vtable_const(&cb.name);

    let mut w = CodeWriter::two_space().with_depth(1);

    // The consumer-facing module: documentation of the required methods, and
    // NotImplementedError defaults for consumers who include it.
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &cb.doc, "  ");
    w.raw(d);
    if let Some(msg) = &cb.deprecated {
        w.line(format!("# @deprecated {msg}"));
    }
    w.line("# Consumer-implemented callback interface. Any object responding to the");
    w.line(format!(
        "# methods below is accepted wherever a {local} parameter is expected;"
    ));
    w.line("# include this module to inherit NotImplementedError defaults. The");
    w.line("# producer may call the methods from any thread until it releases the");
    w.line("# implementation.");
    w.block(format!("module {local}"), "end", |w| {
        for (idx, m) in cb.methods.iter().enumerate() {
            if idx > 0 {
                w.blank();
            }
            let mut md = String::new();
            emit_doc(&mut md, &m.doc, "    ");
            w.raw(md);
            if let Some(msg) = &m.deprecated {
                w.line(format!("# @deprecated {msg}"));
            }
            emit_param_docs(w, &m.params);
            if let Some(ret) = &m.ret {
                w.line(format!("# @return [Object] a {ret}"));
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

    // The vtable layout: one pointer per method in declaration order, then
    // the trailing free.
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# The C vtable layout the producer calls a {local} through."
    ));
    w.block(format!("class {vtable_class} < FFI::Struct"), "end", |w| {
        let mut fields: Vec<String> = cb
            .methods
            .iter()
            .map(|m| format!(":{}, :pointer", m.name))
            .collect();
        fields.push(":free, :pointer".to_string());
        w.line(format!("layout {}", fields.join(",\n           ")));
    });

    // One trampoline per method.
    for (m, args) in cb.methods.iter().zip(&protocol.method_args) {
        render_trampoline(&mut w, cb, m, args);
    }

    // The trailing free: drop the registry entry; the producer never touches
    // ctx again after this fires.
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# Releases a {local} implementation when the producer drops its last"
    ));
    w.line("# reference.");
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

    // The single static vtable instance, filled with the pinned trampolines.
    w.blank();
    w.line(format!(
        "# The one process-wide {local} vtable every registration hands the"
    ));
    w.line("# producer; its entries live for the process lifetime.");
    w.line(format!("{vtable_const} = {vtable_class}.new"));
    for m in &cb.methods {
        w.line(format!(
            "{vtable_const}[:{}] = {}",
            m.name,
            rb_trampoline_const(&cb.name, &m.name)
        ));
    }
    w.line(format!(
        "{vtable_const}[:free] = {}",
        rb_free_const(&cb.name)
    ));

    out.push_str(&w.finish());
}

/// Render one method's trampoline: an `FFI::Function` matching the vtable
/// entry's C signature that resolves `ctx`, receives each argument per its
/// [`RetPass`] plan, invokes the implementation, and converts the direct
/// return. Any exception is reported through `_wv_cb_fail` (which calls
/// `{prefix}_error_set` with `-4`) and the method's default value is
/// returned instead.
fn render_trampoline(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    args: &[RetPass],
) {
    let local = local_type_name(&cb.name);
    let types = rb_abi_types(&m.abi_params, false);
    let formals = rb_tramp_formals(m);
    let default = rb_direct_default(m.ret.as_ref());

    w.blank();
    w.line("# @api private");
    w.line(format!("# Trampoline for {local}#{}.", m.name));
    w.block(
        format!(
            "{} = FFI::Function.new({}, [{}]) do |{}|",
            rb_trampoline_const(&cb.name, &m.name),
            rb_ffi_type(&m.abi_ret, true),
            types.join(", "),
            formals.join(", ")
        ),
        "end",
        |w| {
            w.line("begin");
            w.scope(|w| {
                w.line("impl = _wv_cb_lookup(ctx)");
                for (p, pass) in m.params.iter().zip(args) {
                    render_tramp_arg(w, &p.name, &p.ty, pass);
                }
                let call_args: Vec<String> = m
                    .params
                    .iter()
                    .map(|p| format!("{}_v", rb_param_name(&p.name)))
                    .collect();
                let call = if call_args.is_empty() {
                    format!("impl.{}", m.name)
                } else {
                    format!("impl.{}({})", m.name, call_args.join(", "))
                };
                match &m.ret {
                    None => {
                        w.line(call);
                        w.line("nil");
                    }
                    // Coerce inside the rescue so a wrong-typed return
                    // surfaces as a foreign error rather than escaping
                    // through ffi's own conversion.
                    Some(Ty::Bool) => {
                        w.line(format!("{call} ? 1 : 0"));
                    }
                    Some(Ty::F32 | Ty::F64) => {
                        w.line(format!("Float({call})"));
                    }
                    Some(_) => {
                        w.line(format!("Integer({call})"));
                    }
                }
            });
            // Rescue Exception, not StandardError: NotImplementedError is a
            // ScriptError, and nothing may unwind through the C frame.
            w.line("rescue Exception => e");
            w.scope(|w| {
                w.line("_wv_cb_fail(out_err, e)");
                w.line(default);
            });
            w.line("end");
        },
    );
}

/// Emit the statements receiving one trampoline argument into the local
/// `{name}_v`, per its [`RetPass`] plan. Strings arrive as Ruby Strings
/// (ffi copies `:string` slots); bytes and buffers are borrowed `(ptr, len)`
/// pairs copied or decoded before the implementation runs; objects transfer
/// one strong reference that is adopted into a wrapper (`nil` for a null
/// nullable slot); direct values convert per [`rb_direct_from_c`].
fn render_tramp_arg(w: &mut CodeWriter, name: &str, ty: &Ty, pass: &RetPass) {
    let n = rb_param_name(name);
    match pass {
        RetPass::Void => unreachable!("callback parameters always have a type"),
        RetPass::Direct => {
            w.line(format!("{n}_v = {}", rb_direct_from_c(ty, &n)));
        }
        RetPass::String => {
            // ffi copies a `:string` slot into a fresh BINARY String; the ABI
            // guarantees UTF-8, so retag it.
            w.line(format!(
                "{n}_v = {n}.nil? ? '' : {n}.force_encoding(Encoding::UTF_8)"
            ));
        }
        RetPass::Bytes => {
            w.line(format!(
                "{n}_v = {n}_ptr.null? ? ''.b : {n}_ptr.read_string({n}_len)"
            ));
        }
        RetPass::Buffer => {
            w.line(format!(
                "{n}_r = WvBufferReader.new({n}_ptr.null? ? ''.b : {n}_ptr.read_string({n}_len))"
            ));
            crate::codec::render_wv_read(w, &format!("{n}_r"), &format!("{n}_v"), ty, 0, "");
            w.line(format!("{n}_r.expect_end!"));
        }
        RetPass::Object { nullable, .. } => {
            let iface = ty.interface_name().expect("object plan names an interface");
            let class = local_type_name(iface);
            if *nullable {
                w.line(format!("{n}_v = {n}.null? ? nil : {class}._from_ptr({n})"));
            } else {
                w.line(format!("{n}_v = {class}._from_ptr({n})"));
            }
        }
    }
}
