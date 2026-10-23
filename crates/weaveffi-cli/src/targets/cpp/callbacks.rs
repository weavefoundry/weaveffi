//! Callback interfaces: the abstract class the consumer implements, and its
//! `detail::Callbacks<I>` specialization holding the trampolines that adapt
//! an implementation to the C vtable and the process-wide static vtable.
//!
//! Each trampoline receives its arguments by their [`Crossing`] (runs are
//! borrowed for the call, objects move one strong reference in), hands its
//! return back as the vtable entry's one C return (a run from
//! `{p}_alloc`, which the producer adopts, or a fresh object reference), and
//! reports any exception through `out_err` per the method's
//! [`ErrorStrategy`] (through the runtime's `detail::callback`), so nothing
//! unwinds through the C frame.

use crate::codegen::common::DocCommentStyle;
use crate::codegen::values::{crossing, Crossing};
use crate::codegen::CodeWriter;
use weaveffi_model::abi::AbiParam;
use weaveffi_model::model::{CallbackBinding, CallbackMethodBinding, ErrorStrategy};
use weaveffi_model::ty::Ty;

use crate::targets::cpp::entities::type_doc;
use crate::targets::cpp::types::{
    cpp_cb_param_decl, cpp_error_class, cpp_fn_name, cpp_ident, cpp_prim, cpp_type, error_class,
    slot_name, trampoline_ident, Ctx,
};

/// Append a callback interface: the abstract class, then its
/// `detail::Callbacks` specialization.
pub(crate) fn render_callback_interface(w: &mut CodeWriter, ctx: &Ctx<'_>, cb: &CallbackBinding) {
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
fn render_callback_class(w: &mut CodeWriter, ctx: &Ctx<'_>, cb: &CallbackBinding) {
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
                .map(|p| cpp_cb_param_decl(&p.ty, &cpp_ident(&p.name)))
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
/// the `vtable()` accessor beside it and the helpers the trampolines call.
fn trampoline_name(m: &CallbackMethodBinding) -> String {
    trampoline_ident(&cpp_ident(&m.abi.symbol))
}

/// One trampoline C parameter's name, kept clear of the helpers the
/// trampoline body calls.
fn trampoline_slot(p: &AbiParam) -> String {
    trampoline_ident(&slot_name(p))
}

/// Append, in `detail`, the `Callbacks<I>` specialization of a callback
/// interface: one trampoline per method and the static vtable.
///
/// `ctx` is a heap-allocated `std::shared_ptr<I>` box made when the
/// implementation is passed to the producer; the vtable's `free` deletes
/// it. The vtable's `size` is its own `sizeof`, and its `flags` are 0: C++
/// implementations may be called from any thread.
fn render_trampolines(w: &mut CodeWriter, ctx: &Ctx<'_>, cb: &CallbackBinding) {
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

/// The expression handing one argument, the borrowed C value `slot`, to the
/// implementation: a string viewed in place, a typed array or bytes copied,
/// an optional scalar or enum unpacked, a decoded buffer or adopted object
/// (the local `local`) moved in, and a direct value converted.
fn trampoline_arg(ty: &Ty, slot: &str, local: &str) -> String {
    match crossing(ty) {
        Crossing::Buffer(_) | Crossing::Object { .. } => format!("std::move({local})"),
        Crossing::Utf8 => format!("borrow_string({slot})"),
        Crossing::Bytes => format!("borrow_bytes({slot})"),
        Crossing::Slice(p) => format!("borrow_slice<{}>({slot})", cpp_prim(p)),
        Crossing::OptScalar(p) => format!("from_opt<{}>({slot})", cpp_prim(p)),
        Crossing::OptEnum(name) => format!("from_opt<{name}>({slot})"),
        Crossing::Enum(name) => format!("static_cast<{name}>({slot})"),
        Crossing::Scalar(_) => slot.to_string(),
    }
}

/// The statement returning the implementation's `call` result to the
/// producer as the vtable entry's C return: a scalar as is, an enum as its
/// discriminant, an optional scalar as its `{p}_opt_*` struct, a run copied
/// into `{p}_alloc` memory, and an object as a fresh strong reference.
fn trampoline_return(ctx: &Ctx<'_>, ty: &Ty, call: &str) -> String {
    let prefix = ctx.prefix;
    match crossing(ty) {
        Crossing::Scalar(_) => format!("return {call};"),
        Crossing::Enum(_) => format!("return static_cast<int32_t>({call});"),
        Crossing::OptScalar(p) => format!("return to_opt<{prefix}_opt_{}>({call});", p.snake()),
        Crossing::OptEnum(_) => format!("return to_opt<{prefix}_opt_i32>({call});"),
        Crossing::Utf8 => format!("return give_string({call});"),
        Crossing::Bytes => format!("return give_bytes({call});"),
        Crossing::Slice(_) => format!("return give_slice({call});"),
        Crossing::Buffer(_) => format!("return give_buffer(encode({call}));"),
        // A moved-from wrapper returns null, which the producer rejects (-3).
        Crossing::Object {
            nullable: false, ..
        } => format!("return {call}.clone_handle();"),
        Crossing::Object { nullable: true, .. } => format!("return clone_of({call});"),
    }
}

/// Append one trampoline: a static function with the vtable entry's exact
/// C signature.
fn render_trampoline(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    cb: &CallbackBinding,
    m: &CallbackMethodBinding,
) {
    let iface = &cb.name;
    let c_ret = m.abi.ret.render_c(ctx.prefix);
    let params: Vec<String> = m
        .abi
        .params
        .iter()
        .map(|p| format!("{} {}", p.ty.render_c(ctx.prefix), trampoline_slot(p)))
        .collect();
    let ctx_slot = trampoline_slot(&m.abi.params[0]);
    let out_err = trampoline_slot(
        m.abi
            .params
            .last()
            .expect("a vtable entry ends with out_err"),
    );
    // The method's parameters fill the slots between `ctx` and `out_err`.
    let args: Vec<(&Ty, String, String)> = m
        .params
        .iter()
        .zip(&m.abi.params[1..])
        .map(|(p, slot)| {
            let local = format!("{}_arg", cpp_ident(&p.name));
            (&p.ty, trampoline_slot(slot), local)
        })
        .collect();

    w.block(
        format!("static {c_ret} {}({}) {{", trampoline_name(m), params.join(", ")),
        "}",
        |w| {
            // Object arguments each transfer one strong reference. Adopting
            // can't throw, so it comes first, which releases every reference
            // even when another argument fails to decode.
            for (ty, slot, local) in &args {
                if let Crossing::Object {
                    interface,
                    nullable,
                } = crossing(ty)
                {
                    if nullable {
                        w.line(format!(
                            "std::optional<{interface}> {local} = adopt_optional<{interface}>({slot});"
                        ));
                    } else {
                        w.line(format!("{interface} {local}(adopt, {slot});"));
                    }
                }
            }
            let errors = error_class(&m.error);
            let lead = if m.ret.is_some() { "return " } else { "" };
            w.line(format!("{lead}callback<{errors}>({out_err}, [&] {{"));
            w.scope(|w| {
                // Buffers carrying object tokens decode first, so a failure
                // decoding another argument still adopts their references.
                let mut order: Vec<_> = args.iter().collect();
                order.sort_by_key(|(ty, ..)| !ty.contains_object());
                for (ty, slot, local) in order {
                    if let Crossing::Buffer(ty) = crossing(ty) {
                        w.line(format!(
                            "auto {local} = decode<{}>({slot});",
                            cpp_type(ty)
                        ));
                    }
                }
                let call_args: Vec<String> = args
                    .iter()
                    .map(|(ty, slot, local)| trampoline_arg(ty, slot, local))
                    .collect();
                let call = format!(
                    "implementation<{iface}>({ctx_slot}).{}({})",
                    cpp_fn_name(&m.name),
                    call_args.join(", ")
                );
                match &m.ret {
                    None => w.line(format!("{call};")),
                    Some(ty) => w.line(trampoline_return(ctx, ty, &call)),
                };
            });
            w.line("});");
        },
    );
    w.blank();
}
