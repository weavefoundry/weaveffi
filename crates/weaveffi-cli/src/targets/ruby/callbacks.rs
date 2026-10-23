//! Callback interfaces: the consumer-facing Ruby module (a duck-typed
//! method set with `NotImplementedError` defaults) and its vtable, one
//! trampoline `FFI::Function` per method, which the runtime lays out behind
//! the shared `{size, flags, free}` header.
//!
//! A trampoline resolves `ctx` to the registered implementation, receives
//! each argument per its [`ArgPass`] (objects first, so their references
//! are owned by a wrapper even when a later step raises; strings, bytes,
//! buffers, and typed arrays copied before the implementation runs), calls
//! the method, and hands the return back per its [`CallbackRetPass`]. An
//! exception is reported through `out_err` per the method's
//! [`ErrorStrategy`]; nothing unwinds
//! through the C frame.

use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ErrorStrategy};
use weaveffi_model::ty::{Prim, Ty};

use crate::codegen::docs::Doc;
use crate::codegen::CodeWriter;
use crate::targets::ruby::docs::{self, CallableDoc, ParamDoc};
use crate::targets::ruby::types::{
    rb_callback_method_name, rb_const, rb_doc_type, rb_domain, rb_elem, rb_error_arg, rb_ffi_type,
    rb_ffi_types, rb_param_name, rb_scalar, rb_slot, rb_wire,
};
use crate::targets::ruby::RbCtx;

/// Render the consumer-facing module of one callback interface.
pub(crate) fn render_callback_module(
    w: &mut CodeWriter,
    ctx: &RbCtx,
    cb: &CallbackInterfaceBinding,
) {
    let name = rb_const(&cb.name);
    w.blank();
    let doc = Doc::new(&cb.doc, &cb.deprecated);
    let mut extra = CallableDoc::default();
    extra.notes.push(format!(
        "A callback interface: pass any object that responds to the methods \
         below wherever a {name} is expected. Including this module documents \
         the intent and supplies NotImplementedError defaults. The library may \
         call the methods from any thread until it releases the \
         implementation."
    ));
    docs::emit(w, &ctx.names, &doc, &extra);
    w.block(format!("module {name}"), "end", |w| {
        for (idx, m) in cb.methods.iter().enumerate() {
            if idx > 0 {
                w.blank();
            }
            let mut extra = CallableDoc::default();
            for p in &m.params {
                extra.params.push(ParamDoc {
                    name: rb_param_name(&p.name),
                    ty: rb_doc_type(&p.ty),
                    doc: p.doc.clone(),
                });
            }
            extra.ret = Some(m.ret.as_ref().map_or("void".to_string(), rb_doc_type));
            match &m.error {
                ErrorStrategy::Trap => {}
                ErrorStrategy::Untyped => extra.raises.push((
                    "StandardError".to_string(),
                    "reported to the library with its message".to_string(),
                )),
                ErrorStrategy::Domain(d) => extra.raises.push((
                    rb_domain(d),
                    "reported to the library with its code and fields".to_string(),
                )),
            }
            docs::emit(w, &ctx.names, &Doc::new(&m.doc, &m.deprecated), &extra);
            let formals: Vec<String> = m.params.iter().map(|p| rb_param_name(&p.name)).collect();
            let method = rb_callback_method_name(&m.name);
            let open = if formals.is_empty() {
                format!("def {method}")
            } else {
                format!("def {method}({})", formals.join(", "))
            };
            w.block(open, "end", |w| {
                w.line(format!(
                    "raise NotImplementedError, \"#{{self.class}}#{method} is not implemented\""
                ));
            });
        }
    });
}

/// Render the vtable registration of one callback interface:
/// `Bridge.vtable(Iface, trampoline, ...)`, with the trampolines in
/// declaration order.
pub(crate) fn render_vtable(w: &mut CodeWriter, cb: &CallbackInterfaceBinding) {
    w.blank();
    w.line(format!("# The {} vtable.", rb_const(&cb.name)));
    w.line("Bridge.vtable(");
    w.scope(|w| {
        w.line(format!("{},", rb_const(&cb.name)));
        for m in &cb.methods {
            render_trampoline(w, cb, m);
        }
    });
    w.line(")");
}

/// The value a trampoline returns when the method failed: the C zero of
/// its return.
fn failure_value(m: &CallbackMethodBinding) -> &'static str {
    match (&m.ret_pass, m.ret.as_ref()) {
        (CallbackRetPass::Direct, Some(Ty::Prim(Prim::Bool)))
        | (CallbackRetPass::OptDirect { .. }, _) => "false",
        (CallbackRetPass::Direct, Some(Ty::Prim(Prim::F32 | Prim::F64))) => "0.0",
        (CallbackRetPass::Direct, _) => "0",
        _ => "nil",
    }
}

/// Render one method's trampoline as an argument of `Bridge.vtable`.
fn render_trampoline(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, m: &CallbackMethodBinding) {
    let formals: Vec<String> = m.abi.params.iter().map(rb_slot).collect();
    let ctx_slot = formals
        .first()
        .expect("a callback method takes ctx")
        .clone();
    let err_slot = formals
        .last()
        .expect("a callback method takes out_err")
        .clone();
    w.line(format!("# {}#{}", rb_const(&cb.name), m.name));
    w.block(
        format!(
            "FFI::Function.new({}, {}) do |{}|",
            rb_ffi_type(&m.abi.ret),
            rb_ffi_types(&m.abi.params),
            formals.join(", ")
        ),
        "end,",
        |w| {
            let default = failure_value(m);
            let open = if default == "nil" {
                format!("Bridge.callback({err_slot}, {}) do", rb_error_arg(&m.error))
            } else {
                format!(
                    "Bridge.callback({err_slot}, {}, {default}) do",
                    rb_error_arg(&m.error)
                )
            };
            w.block(open, "end", |w| {
                let objects_first = m
                    .params
                    .iter()
                    .filter(|p| matches!(p.pass, ArgPass::Object { .. }))
                    .chain(
                        m.params
                            .iter()
                            .filter(|p| !matches!(p.pass, ArgPass::Object { .. })),
                    );
                let mut values = std::collections::HashMap::new();
                for p in objects_first {
                    let (line, value) = receive_arg(&p.name, &p.ty, &p.pass);
                    if let Some(line) = line {
                        w.line(line);
                    }
                    values.insert(p.name.clone(), value);
                }
                let args: Vec<&str> = m.params.iter().map(|p| values[&p.name].as_str()).collect();
                let method = rb_callback_method_name(&m.name);
                let call = if args.is_empty() {
                    format!("Bridge.impl({ctx_slot}).{method}")
                } else {
                    format!("Bridge.impl({ctx_slot}).{method}({})", args.join(", "))
                };
                return_value(w, m, &call);
            });
        },
    );
}

/// The statement receiving one trampoline argument (if it needs one) and
/// the expression the method is called with.
fn receive_arg(name: &str, ty: &Ty, pass: &ArgPass) -> (Option<String>, String) {
    let local = rb_param_name(name);
    let line = match pass {
        ArgPass::Direct { slot } => return (None, rb_slot(slot)),
        ArgPass::OptDirect { has, value, .. } => {
            format!("{local} = {} ? {} : nil", rb_slot(has), rb_slot(value))
        }
        ArgPass::Slice { ptr, len, elem } => format!(
            "{local} = Bridge.borrow_slice({}, {}, {})",
            rb_slot(ptr),
            rb_slot(len),
            rb_elem(*elem)
        ),
        ArgPass::String { ptr, len } => format!(
            "{local} = Bridge.borrow_string({}, {})",
            rb_slot(ptr),
            rb_slot(len)
        ),
        ArgPass::Bytes { ptr, len } => format!(
            "{local} = Bridge.borrow_bytes({}, {})",
            rb_slot(ptr),
            rb_slot(len)
        ),
        ArgPass::Buffer { ptr, len } => format!(
            "{local} = Bridge.decode_borrowed({}, {}, {})",
            rb_slot(ptr),
            rb_slot(len),
            rb_wire(ty)
        ),
        ArgPass::Object {
            slot,
            nullable,
            interface,
        } => {
            let slot = rb_slot(slot);
            let class = rb_const(interface);
            if *nullable {
                format!("{local} = {slot}.null? ? nil : Bridge.adopt({class}, {slot})")
            } else {
                format!("{local} = Bridge.adopt({class}, {slot})")
            }
        }
        ArgPass::Callback { .. } => unreachable!("a callback method takes no callback"),
    };
    (Some(line), local)
}

/// Emit the statement handing the method's return back per its
/// [`CallbackRetPass`] (the block's value is the C return).
fn return_value(w: &mut CodeWriter, m: &CallbackMethodBinding, call: &str) {
    let ty = || m.ret.as_ref().expect("a value return has a type");
    match &m.ret_pass {
        CallbackRetPass::Void => {
            w.line(call);
            w.line("nil");
        }
        CallbackRetPass::Direct => match ty() {
            Ty::Prim(Prim::Bool) => {
                w.line(format!("{call} ? true : false"));
            }
            other => {
                w.line(format!("Bridge.scalar({call}, {})", rb_scalar(other)));
            }
        },
        CallbackRetPass::OptDirect { out_value } => {
            let inner = match ty() {
                Ty::Optional(inner) => inner,
                other => unreachable!("{other} isn't optional"),
            };
            w.line(format!(
                "Bridge.opt_return({call}, {}, {})",
                rb_scalar(inner),
                rb_slot(out_value)
            ));
        }
        CallbackRetPass::Slice {
            out_ptr,
            out_len,
            elem,
        } => {
            w.line(format!(
                "Bridge.slice_return({call}, {}, {}, {})",
                rb_elem(*elem),
                rb_slot(out_ptr),
                rb_slot(out_len)
            ));
        }
        CallbackRetPass::String { out_ptr, out_len } => {
            w.line(format!(
                "Bridge.string_return({call}, {}, {})",
                rb_slot(out_ptr),
                rb_slot(out_len)
            ));
        }
        CallbackRetPass::Bytes { out_ptr, out_len } => {
            w.line(format!(
                "Bridge.bytes_return({call}, {}, {})",
                rb_slot(out_ptr),
                rb_slot(out_len)
            ));
        }
        CallbackRetPass::Buffer { out_ptr, out_len } => {
            w.line(format!(
                "Bridge.buffer_return({}, {call}, {}, {})",
                rb_wire(ty()),
                rb_slot(out_ptr),
                rb_slot(out_len)
            ));
        }
        CallbackRetPass::Object { interface, .. } => {
            w.line(format!(
                "Bridge.object_return({call}, {})",
                rb_const(interface)
            ));
        }
    }
}
