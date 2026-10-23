//! Callback interfaces: the abstract class the consumer implements, and its
//! `detail::Callbacks<I>` specialization holding the trampolines that adapt
//! an implementation to the C vtable and the process-wide static vtable.
//!
//! Each trampoline receives its arguments per the method's [`ArgPass`]
//! slots, hands its return back per the method's [`CallbackRetPass`], and
//! reports any exception through `out_err` per the method's
//! [`ErrorStrategy`] (through the runtime's `detail::callback`), so nothing
//! unwinds through the C frame.

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use crate::lang;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ErrorStrategy};
use weaveffi_model::ty::Ty;

use crate::targets::cpp::entities::type_doc;
use crate::targets::cpp::types::{
    cpp_cb_param_decl, cpp_error_class, cpp_fn_name, cpp_ident, cpp_type, error_class, slot_decl,
    slot_name, Ctx,
};

/// Append a callback interface: the abstract class, then its
/// `detail::Callbacks` specialization.
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    cb: &CallbackInterfaceBinding,
) {
    render_callback_class(w, ctx, cb);
    render_trampolines(w, ctx, cb);
}

/// The `@throws` paragraph of a callback method that declares errors.
fn throws_note(error: &ErrorStrategy) -> Option<String> {
    match error {
        ErrorStrategy::Trap => None,
        ErrorStrategy::Untyped => {
            Some("@throws std::exception to report a failure, with `what()` as its message.".into())
        }
        ErrorStrategy::Domain(name) => Some(format!(
            "@throws {} (or one of its codes' classes) to report one of the domain's codes, \
             fields included; any other exception reports a plain failure.",
            cpp_error_class(name)
        )),
    }
}

/// Append the abstract class a consumer subclasses: a virtual destructor and
/// one pure virtual method per IDL method. Strings arrive as views valid for
/// the call, typed arrays, bytes, and buffered values by const reference,
/// optional scalars by value, and objects by value as wrappers the
/// implementation owns.
fn render_callback_class(w: &mut CodeWriter, ctx: &Ctx<'_>, cb: &CallbackInterfaceBinding) {
    let name = &cb.name;
    let usage = format!(
        "Subclass it and pass a `std::shared_ptr<{name}>` where the API takes a \
         `{name}`. The producer may call the methods from any thread, so they must \
         be thread-safe, and releases the implementation (also from any thread) \
         once it's done with it. An exception a method throws never unwinds into \
         the producer: a method that declares errors reports it as described on \
         the method, and any other method reports a callback failure (-4)."
    );
    let doc = match type_doc(ctx, &cb.doc, &cb.deprecated) {
        Some(d) => format!("{d}\n\n{usage}"),
        None => usage,
    };
    w.doc(&Some(doc), DocCommentStyle::Javadoc);
    w.line(format!("class {name} {{"));
    w.line("public:");
    w.scope(|w| {
        w.line(format!("virtual ~{name}() = default;"));
        for m in &cb.methods {
            w.blank();
            let mut doc = type_doc(ctx, &m.doc, &m.deprecated);
            if let Some(note) = throws_note(&m.error) {
                doc = Some(match doc {
                    Some(d) => format!("{d}\n\n{note}"),
                    None => note,
                });
            }
            w.doc(&doc, DocCommentStyle::Javadoc);
            let ret = m.ret.as_ref().map_or("void".to_string(), cpp_type);
            let params: Vec<String> = m
                .params
                .iter()
                .map(|p| cpp_cb_param_decl(&p.ty, &p.pass, &cpp_ident(&p.name)))
                .collect();
            w.line(format!(
                "virtual {ret} {}({}) = 0;",
                cpp_fn_name(&m.name),
                params.join(", ")
            ));
        }
    });
    w.line("};");
    w.blank();
}

/// The trampoline name of a method: its vtable field name, kept clear of
/// the `vtable()` accessor beside it.
fn trampoline_name(m: &CallbackMethodBinding) -> String {
    lang::escape_member(&cpp_ident(&m.abi.symbol), &["vtable"])
}

/// Append, in `detail`, the `Callbacks<I>` specialization of a callback
/// interface: one trampoline per method and the static vtable.
///
/// `ctx` is a heap-allocated `std::shared_ptr<I>` box made when the
/// implementation is passed to the producer; the vtable's `free` deletes
/// it. The vtable's `flags` are 0: C++ implementations may be called from
/// any thread.
fn render_trampolines(w: &mut CodeWriter, ctx: &Ctx<'_>, cb: &CallbackInterfaceBinding) {
    let name = &cb.name;
    let vtable = &cb.vtable_tag;
    w.line("namespace detail {");
    w.blank();
    w.line(format!(
        "/** Adapts a `{name}` implementation to `{vtable}`. */"
    ));
    w.line("template <>");
    w.block(format!("struct Callbacks<{name}> {{"), "};", |w| {
        for m in &cb.methods {
            render_trampoline(w, ctx, cb, m);
        }
        w.line(format!(
            "/** The vtable every `{name}` implementation is passed with. */"
        ));
        w.block(
            format!("static const {vtable}& vtable() noexcept {{"),
            "}",
            |w| {
                let mut entries = vec![
                    format!("sizeof({vtable})"),
                    "0".to_string(),
                    format!("&release<{name}>"),
                ];
                entries.extend(
                    cb.methods
                        .iter()
                        .map(|m| format!("&{}", trampoline_name(m))),
                );
                w.block(format!("static const {vtable} table = {{"), "};", |w| {
                    for entry in &entries {
                        w.line(format!("{entry},"));
                    }
                });
                w.line("return table;");
            },
        );
    });
    w.blank();
    w.line("} // namespace detail");
    w.blank();
}

/// The expression handing one argument to the implementation, per its
/// [`ArgPass`]: a string viewed in place, a typed array or bytes copied, an
/// optional scalar or enum lifted from its flag and value, a decoded buffer
/// or adopted object moved in, and a direct value converted.
fn trampoline_arg(ty: &Ty, pass: &ArgPass, local: &str) -> String {
    match pass {
        ArgPass::Buffer { .. } | ArgPass::Object { .. } => format!("std::move({local})"),
        ArgPass::String { ptr, len } => {
            format!("borrow_string({}, {})", slot_name(ptr), slot_name(len))
        }
        ArgPass::Bytes { ptr, len } => {
            format!("borrow_bytes({}, {})", slot_name(ptr), slot_name(len))
        }
        ArgPass::Slice { ptr, len, .. } => {
            format!("borrow_slice({}, {})", slot_name(ptr), slot_name(len))
        }
        ArgPass::OptDirect { has, value, inner } => format!(
            "lift_optional<{}>({}, {})",
            cpp_type(inner),
            slot_name(has),
            slot_name(value)
        ),
        ArgPass::Direct { slot } => match ty {
            Ty::Enum(e) => format!("static_cast<{e}>({})", slot_name(slot)),
            _ => slot_name(slot),
        },
        ArgPass::Callback { .. } => {
            unreachable!("callback interfaces are never callback-method parameters")
        }
    }
}

/// Append one trampoline: a static function with the vtable entry's exact
/// C signature.
fn render_trampoline(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
) {
    let iface = &cb.name;
    let c_ret = m.abi.ret.render_c(ctx.prefix);
    let params: Vec<String> = m
        .abi
        .params
        .iter()
        .map(|p| slot_decl(p, ctx.prefix))
        .collect();
    let local = |name: &str| format!("{}_arg", cpp_ident(name));

    w.block(
        format!("static {c_ret} {}({}) {{", trampoline_name(m), params.join(", ")),
        "}",
        |w| {
            // Object arguments each transfer one strong reference. Adopting
            // can't throw, so it comes first, which releases every reference
            // even when another argument fails to decode.
            for p in &m.params {
                if let ArgPass::Object {
                    slot,
                    nullable,
                    interface,
                } = &p.pass
                {
                    let slot = slot_name(slot);
                    let arg = local(&p.name);
                    if *nullable {
                        w.line(format!(
                            "std::optional<{interface}> {arg} = adopt_optional<{interface}>({slot});"
                        ));
                    } else {
                        w.line(format!("{interface} {arg}(adopt, {slot});"));
                    }
                }
            }
            let errors = error_class(&m.error);
            let returns = !matches!(
                m.ret_pass,
                CallbackRetPass::Void
                    | CallbackRetPass::Slice { .. }
                    | CallbackRetPass::String { .. }
                    | CallbackRetPass::Bytes { .. }
                    | CallbackRetPass::Buffer { .. }
            );
            let lead = if returns { "return " } else { "" };
            w.line(format!("{lead}callback<{errors}>(out_err, [&] {{"));
            w.scope(|w| {
                // Buffers carrying object tokens decode first, so a failure
                // decoding another argument still adopts their references.
                let mut order: Vec<_> = m.params.iter().collect();
                order.sort_by_key(|p| !p.ty.contains_object());
                for p in order {
                    if let ArgPass::Buffer { ptr, len } = &p.pass {
                        w.line(format!(
                            "auto {} = decode<{}>({}, {});",
                            local(&p.name),
                            cpp_type(&p.ty),
                            slot_name(ptr),
                            slot_name(len)
                        ));
                    }
                }
                let args: Vec<String> = m
                    .params
                    .iter()
                    .map(|p| trampoline_arg(&p.ty, &p.pass, &local(&p.name)))
                    .collect();
                let call = format!(
                    "implementation<{iface}>(ctx).{}({})",
                    cpp_fn_name(&m.name),
                    args.join(", ")
                );
                let line = match &m.ret_pass {
                    CallbackRetPass::Void => format!("{call};"),
                    CallbackRetPass::Direct => match &m.ret {
                        Some(Ty::Enum(_)) => format!("return static_cast<int32_t>({call});"),
                        _ => format!("return {call};"),
                    },
                    CallbackRetPass::OptDirect { out_value } => {
                        format!("return give_optional({call}, {});", slot_name(out_value))
                    }
                    CallbackRetPass::Slice {
                        out_ptr, out_len, ..
                    } => format!(
                        "give_slice({call}, {}, {});",
                        slot_name(out_ptr),
                        slot_name(out_len)
                    ),
                    CallbackRetPass::String { out_ptr, out_len }
                    | CallbackRetPass::Bytes { out_ptr, out_len } => format!(
                        "give({call}, {}, {});",
                        slot_name(out_ptr),
                        slot_name(out_len)
                    ),
                    CallbackRetPass::Buffer { out_ptr, out_len } => format!(
                        "give(encode({call}), {}, {});",
                        slot_name(out_ptr),
                        slot_name(out_len)
                    ),
                    // A moved-from wrapper returns null, which the producer
                    // rejects (-3).
                    CallbackRetPass::Object {
                        nullable: false, ..
                    } => format!("return {call}.clone_handle();"),
                    CallbackRetPass::Object { nullable: true, .. } => {
                        format!("return clone_of({call});")
                    }
                };
                w.line(line);
            });
            w.line("});");
        },
    );
    w.blank();
}
