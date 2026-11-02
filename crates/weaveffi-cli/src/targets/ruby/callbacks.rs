//! Callback interfaces: the consumer-facing Ruby module (a duck-typed
//! method set with `NotImplementedError` defaults) and its vtable.
//!
//! A vtable is registered as data: each method's Ruby name, the error class
//! it may raise, its return kind, and its parameter kinds, in vtable order.
//! The runtime's `Bridge.vtable` builds one trampoline `FFI::Function` per
//! method from that row (every argument and return is one C value of its
//! kind, so one generic trampoline serves every method) and lays them out
//! behind the shared `{size, flags, free}` header. A trampoline reports an
//! exception through `out_err` per the method's [`ErrorStrategy`]; nothing
//! unwinds through the C frame.

use weaveffi_model::model::{CallbackBinding, ErrorStrategy};

use crate::codegen::common::{wrap, DocCommentStyle};
use crate::codegen::docs::Doc;
use crate::codegen::CodeWriter;
use crate::targets::ruby::docs::{self, CallableDoc, ParamDoc};
use crate::targets::ruby::types::{
    rb_callback_method_name, rb_const, rb_doc_type, rb_domain, rb_error_arg, rb_kind,
    rb_kind_or_nil, rb_param_name,
};
use crate::targets::ruby::RbCtx;

/// Render the consumer-facing module of one callback interface.
pub(crate) fn render_callback_module(w: &mut CodeWriter, ctx: &RbCtx, cb: &CallbackBinding) {
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
/// `Bridge.vtable(Iface, [name, error, return, [params]], ...)`, one row per
/// method in vtable order.
pub(crate) fn render_vtable(w: &mut CodeWriter, cb: &CallbackBinding) {
    w.blank();
    let about = format!(
        "The {} vtable, one row per method: its name, the error class it may \
         raise, its return, and its parameters.",
        rb_const(&cb.name)
    );
    w.doc(&Some(wrap(&about, 76)), DocCommentStyle::Hash);
    w.line("Bridge.vtable(");
    w.scope(|w| {
        w.line(format!("{},", rb_const(&cb.name)));
        for m in &cb.methods {
            let params: Vec<String> = m.params.iter().map(|p| rb_kind(&p.ty)).collect();
            w.line(format!(
                "[:{}, {}, {}, [{}]],",
                rb_callback_method_name(&m.name),
                rb_error_arg(&m.error),
                rb_kind_or_nil(m.ret.as_ref()),
                params.join(", ")
            ));
        }
    });
    w.line(")");
}
