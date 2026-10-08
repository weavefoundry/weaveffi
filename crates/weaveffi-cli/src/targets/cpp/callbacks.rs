//! Callback interfaces: the abstract class the consumer implements, the
//! trampolines that adapt an implementation to the C vtable, and the
//! process-wide static vtable, satisfying
//! [`weaveffi_model::plan::CallbackProtocol`].

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, ErrorBinding};
use weaveffi_model::plan::{ArgPass, RetPass};
use weaveffi_model::ty::Ty;

use crate::targets::cpp::codec::{read_fn, write_stmt};
use crate::targets::cpp::entities::{doc_text, report_fn};
use crate::targets::cpp::types::{
    cpp_cb_param_decl, cpp_fn_name, cpp_ident, cpp_type, render_param_decls, slot_name,
    trampoline_struct, vtable_accessor,
};

/// The code a trampoline reports for any failure other than a declared
/// domain code.
const FOREIGN_ERROR_CODE: i32 = -4;

/// Append a callback interface: the abstract class, then (in `detail`) its
/// trampolines and static vtable. `domain` is the error domain in scope for
/// the interface's module, which its `throws` methods may report.
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    domain: Option<&ErrorBinding>,
    prefix: &str,
) {
    render_callback_class(w, cb, domain);
    render_trampolines(w, cb, domain, prefix);
}

/// Append the abstract class a consumer subclasses: a virtual destructor and
/// one pure virtual method per IDL method. Strings arrive as views valid for
/// the call, buffered values by const reference, and objects by value as
/// wrappers the implementation owns; every family can be returned.
fn render_callback_class(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    domain: Option<&ErrorBinding>,
) {
    let name = &cb.name;
    let throws = match domain.filter(|_| cb.methods.iter().any(|m| m.throws)) {
        Some(eb) => format!(
            " A method that declares errors may throw a `{}` code's exception, \
             which reaches the producer with its fields; any other exception \
             reaches it as a callback failure (code -4).",
            eb.type_name
        ),
        None => " An exception a method throws reaches the producer as a callback \
                  failure (code -4)."
            .to_string(),
    };
    let usage = format!(
        "Subclass it and pass a `std::shared_ptr<{name}>` where the API takes a \
         `{name}`. The producer may call the methods from any thread, so they must \
         be thread-safe, until it releases the implementation (also from any \
         thread).{throws}"
    );
    let doc = match doc_text(cb.doc.as_deref(), cb.deprecated.as_deref()) {
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
            let mut doc = doc_text(m.doc.as_deref(), m.deprecated.as_deref());
            if let Some(eb) = domain.filter(|_| m.throws) {
                let tag = format!(
                    "@throws {} to report one of the domain's codes to the producer.",
                    eb.type_name
                );
                doc = Some(match doc {
                    Some(d) => format!("{d}\n\n{tag}"),
                    None => tag,
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

/// Append, in `detail`, the trampolines and the static vtable of a callback
/// interface.
///
/// `ctx` is a heap-allocated `std::shared_ptr<Iface>` box made when the
/// implementation is passed to the producer; the vtable's `free` deletes
/// it. Each trampoline receives its arguments per the callback protocol,
/// calls the method, and hands the return back through the C return or the
/// `out_ptr`/`out_len` slots (a run from `{prefix}_alloc`). Any exception is
/// reported through `out_err` and nothing unwinds through the C frame.
fn render_trampolines(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    domain: Option<&ErrorBinding>,
    prefix: &str,
) {
    let name = &cb.name;
    let strukt = trampoline_struct(name);
    let vtable = &cb.vtable_tag;
    w.line("namespace detail {");
    w.blank();
    w.line(format!(
        "/** Trampolines adapting a `{name}` implementation to `{vtable}`. */"
    ));
    w.block(format!("struct {strukt} {{"), "};", |w| {
        for m in &cb.methods {
            render_trampoline(w, cb, m, domain.filter(|_| m.throws), prefix);
        }
        w.line("/** Deletes the implementation box once the producer releases it. */");
        w.block("static void free_ctx(void* ctx) {", "}", |w| {
            w.line(format!(
                "delete static_cast<std::shared_ptr<{name}>*>(ctx);"
            ));
        });
    });
    w.blank();
    w.line(format!(
        "/** The vtable every `{name}` implementation is passed with. */"
    ));
    w.block(
        format!("inline const {vtable}& {}() {{", vtable_accessor(name)),
        "}",
        |w| {
            w.block(format!("static const {vtable} vtable = {{"), "};", |w| {
                w.line(format!("static_cast<uint32_t>(sizeof({vtable})),"));
                w.line("0,");
                w.line(format!("&{strukt}::free_ctx,"));
                for m in &cb.methods {
                    w.line(format!("&{strukt}::{},", cpp_ident(&m.name)));
                }
            });
            w.line("return vtable;");
        },
    );
    w.blank();
    w.line("} // namespace detail");
    w.blank();
}

/// Append one trampoline: a static function with the vtable entry's exact C
/// signature. `domain` is set when the method declares errors.
fn render_trampoline(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    domain: Option<&ErrorBinding>,
    prefix: &str,
) {
    let iface = &cb.name;
    let method = cpp_fn_name(&m.name);
    let c_ret = m.abi_ret.render_c(prefix);
    let slots = &m.abi_params;
    let ctx = slot_name(&slots[0]);
    let out_err = slot_name(&slots[slots.len() - 1]);
    let ret_pass = RetPass::of(m.ret.as_ref());
    let returns_run = matches!(ret_pass, RetPass::String | RetPass::Bytes | RetPass::Buffer);
    let (out_ptr, out_len) = if returns_run {
        (
            slot_name(&slots[slots.len() - 3]),
            slot_name(&slots[slots.len() - 2]),
        )
    } else {
        (String::new(), String::new())
    };
    let params = render_param_decls(slots, prefix).join(", ");

    w.block(
        format!("static {c_ret} {}({params}) {{", cpp_ident(&m.name)),
        "}",
        |w| {
            // Object arguments each transfer one strong reference. Adopting
            // can't throw, so it comes first, which releases every reference
            // even when another argument fails to decode.
            for p in &m.params {
                if let ArgPass::Object { slot, nullable } = p.arg_pass() {
                    let class = p.ty.interface_name().expect("objects name an interface");
                    let slot = slot_name(slot);
                    let var = format!("{}_arg", p.name);
                    if nullable {
                        w.line(format!("std::optional<{class}> {var};"));
                        w.line(format!("if ({slot} != nullptr) {var}.emplace(adopt, {slot});"));
                    } else {
                        w.line(format!("{class} {var}(adopt, {slot});"));
                    }
                }
            }
            w.line("try {");
            w.scope(|w| {
                w.line(format!(
                    "{iface}& impl = **static_cast<std::shared_ptr<{iface}>*>({ctx});"
                ));
                // Buffers carrying object tokens decode first, so a failure
                // decoding another argument still adopts their references.
                let mut order: Vec<_> = m.params.iter().collect();
                order.sort_by_key(|p| !p.ty.contains_object());
                for p in order {
                    if let ArgPass::Buffer { ptr, len } = p.arg_pass() {
                        w.line(format!(
                            "{} {}_arg = detail::decode({}, {}, &{});",
                            cpp_type(&p.ty),
                            p.name,
                            slot_name(ptr),
                            slot_name(len),
                            read_fn(&p.ty)
                        ));
                    }
                }
                let args: Vec<String> = m.params.iter().map(trampoline_arg).collect();
                let call = format!("impl.{method}({})", args.join(", "));
                match (&m.ret, ret_pass.clone()) {
                    (None, _) => {
                        w.line(format!("{call};"));
                    }
                    (Some(Ty::Enum(_)), _) => {
                        w.line(format!(
                            "return static_cast<{c_ret}>(static_cast<int32_t>({call}));"
                        ));
                    }
                    (Some(_), RetPass::Direct) => {
                        w.line(format!("return {call};"));
                    }
                    (Some(_), RetPass::Object { nullable: false, .. }) => {
                        // A moved-from wrapper returns null, which the
                        // producer rejects (-3).
                        w.line(format!("return {call}.clone_handle();"));
                    }
                    (Some(ty), RetPass::Object { nullable: true, .. }) => {
                        w.line(format!("{} ret = {call};", cpp_type(ty)));
                        w.line("return ret.has_value() ? ret->clone_handle() : nullptr;");
                    }
                    (Some(ty), RetPass::String | RetPass::Bytes) => {
                        w.line(format!("{} ret = {call};", cpp_type(ty)));
                        w.line(format!(
                            "detail::hand_over(ret.data(), ret.size(), {out_ptr}, {out_len});"
                        ));
                    }
                    (Some(ty), RetPass::Buffer) => {
                        w.line(format!("{} ret = {call};", cpp_type(ty)));
                        w.line("detail::BufferWriter ret_buf;");
                        w.line(write_stmt(ty, "ret", "ret_buf"));
                        w.line(format!(
                            "detail::hand_over(ret_buf.data(), ret_buf.size(), {out_ptr}, {out_len});"
                        ));
                    }
                    (Some(_), RetPass::Void) => unreachable!("a return type is never void"),
                }
            });
            if let Some(eb) = domain {
                w.line(format!("}} catch (const {}& e) {{", eb.type_name));
                w.scope(|w| {
                    w.line(format!("{}(e, {out_err});", report_fn(eb)));
                });
            }
            w.line("} catch (const std::exception& e) {");
            w.scope(|w| {
                w.line(format!(
                    "{prefix}_error_set({out_err}, {FOREIGN_ERROR_CODE}, e.what());"
                ));
            });
            w.line("} catch (...) {");
            w.scope(|w| {
                w.line(format!(
                    "{prefix}_error_set({out_err}, {FOREIGN_ERROR_CODE}, \"{iface}::{method} threw a non-standard exception\");"
                ));
            });
            w.line("}");
            match ret_pass {
                RetPass::Direct => {
                    w.line(format!("return {c_ret}{{}};"));
                }
                RetPass::Object { .. } => {
                    w.line("return nullptr;");
                }
                _ => {}
            }
        },
    );
    w.blank();
}

/// The expression handing one argument to the implementation: a string
/// viewed in place, bytes copied, a decoded buffer or adopted object moved
/// in, and a direct value converted.
fn trampoline_arg(p: &weaveffi_model::model::ParamBinding) -> String {
    match p.arg_pass() {
        ArgPass::Buffer { .. } | ArgPass::Object { .. } => format!("std::move({}_arg)", p.name),
        ArgPass::String { ptr, len } => format!(
            "detail::borrow_string({}, {})",
            slot_name(ptr),
            slot_name(len)
        ),
        ArgPass::Bytes { ptr, len } => format!(
            "detail::borrow_bytes({}, {})",
            slot_name(ptr),
            slot_name(len)
        ),
        ArgPass::Direct { slot } => match &p.ty {
            Ty::Enum(e) => format!(
                "static_cast<{e}>(static_cast<int32_t>({}))",
                slot_name(slot)
            ),
            _ => slot_name(slot),
        },
        ArgPass::Callback { .. } => {
            unreachable!("callback interfaces are never callback-method parameters")
        }
    }
}
