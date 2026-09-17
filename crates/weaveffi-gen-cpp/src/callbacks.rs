//! Callback-interface rendering: the abstract class the consumer implements,
//! the `extern "C"`-compatible trampolines that adapt an implementation to
//! the C vtable, and the process-wide static vtable, per
//! `weaveffi_core::plan::CallbackProtocol`.

use weaveffi_core::codegen::common::DocCommentStyle;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{CallbackInterfaceBinding, CallbackMethodBinding, ParamBinding, Ty};
use weaveffi_core::plan::ArgPass;
use weaveffi_core::utils::{c_abi_struct_name, local_type_name};

use crate::codec::emit_read_decl;
use crate::types::{
    cpp_cb_param_decl, cpp_fn_name, cpp_ident, cpp_type, render_param_decls, slot_name,
    trampoline_struct, vtable_accessor,
};

/// The foreign-error code a trampoline reports when the implementation throws.
const FOREIGN_ERROR_CODE: i32 = -4;

/// Render the abstract class a consumer subclasses to implement a callback
/// interface: a virtual destructor and one pure virtual method per IDL
/// method, with idiomatic C++ signatures (strings and buffered values by
/// const reference, object arguments by value as the adopted wrapper).
pub(crate) fn render_callback_class(out: &mut String, cb: &CallbackInterfaceBinding) {
    let name = &cb.name;
    let usage = format!(
        "Implement this interface and pass a `std::shared_ptr<{name}>` to any \
         function that accepts one. The producer may call the methods from any \
         thread until it releases its last reference to the implementation. An \
         exception thrown by a method is reported to the producer as a foreign \
         error (code -4) and aborts the call that triggered it."
    );
    let doc = match &cb.doc {
        Some(d) => format!("{d}\n\n{usage}"),
        None => usage,
    };
    let mut w = CodeWriter::four_space();
    w.doc(&Some(doc), DocCommentStyle::Javadoc);
    w.line(format!("class {name} {{"));
    w.line("public:");
    w.scope(|w| {
        w.line(format!("virtual ~{name}() = default;"));
        w.blank();
        for m in &cb.methods {
            w.doc(&m.doc, DocCommentStyle::Javadoc);
            if let Some(msg) = &m.deprecated {
                let escaped = msg.replace('"', "\\\"");
                w.line(format!("[[deprecated(\"{escaped}\")]]"));
            }
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
    out.push_str(&w.finish());
}

/// Render, inside `detail`, the trampolines and the static vtable for one
/// callback interface.
///
/// `ctx` is a heap-allocated `std::shared_ptr<Iface>` box created when the
/// implementation is passed to the producer; the trailing `free` entry
/// deletes it. Each method trampoline receives its arguments per the
/// callback protocol (strings, bytes, and buffers borrowed and copied or
/// decoded; objects adopted), calls the virtual method, and on any exception
/// reports `{prefix}_error_set(out_err, -4, message)` and returns a default
/// value, so nothing ever unwinds through the C frame.
pub(crate) fn render_callback_trampolines(
    out: &mut String,
    cb: &CallbackInterfaceBinding,
    module: &str,
    prefix: &str,
) {
    let name = &cb.name;
    let strukt = trampoline_struct(name);
    let mut w = CodeWriter::four_space();
    w.line("namespace detail {");
    w.blank();
    w.line(format!(
        "/** Vtable trampolines adapting a `{name}` implementation to `{}`. */",
        cb.vtable_tag
    ));
    w.line(format!("struct {strukt} {{"));
    w.scope(|w| {
        for m in &cb.methods {
            render_trampoline(w, cb, m, module, prefix);
        }
        w.line(
            "/** Releases the implementation box once the producer drops its last reference. */",
        );
        w.line("static void free_ctx(void* ctx) {");
        w.scope(|w| {
            w.line(format!(
                "delete static_cast<std::shared_ptr<{name}>*>(ctx);"
            ));
        });
        w.line("}");
    });
    w.line("};");
    w.blank();

    w.line(format!(
        "/** The process-wide vtable every `{name}` implementation is passed with. */"
    ));
    w.line(format!(
        "inline const {}& {}() {{",
        cb.vtable_tag,
        vtable_accessor(name)
    ));
    w.scope(|w| {
        w.line(format!("static const {} vtable = {{", cb.vtable_tag));
        w.scope(|w| {
            for m in &cb.methods {
                w.line(format!("&{strukt}::{},", cpp_ident(&m.name)));
            }
            w.line(format!("&{strukt}::free_ctx,"));
        });
        w.line("};");
        w.line("return vtable;");
    });
    w.line("}");
    w.blank();
    w.line("} // namespace detail");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit one method trampoline: a static function with the vtable entry's
/// exact C signature.
fn render_trampoline(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    module: &str,
    prefix: &str,
) {
    let iface = &cb.name;
    let method = cpp_fn_name(&m.name);
    let c_ret = m.abi_ret.render_c(prefix);
    let params = render_param_decls(&m.abi_params, prefix).join(", ");

    w.line(format!(
        "static {c_ret} {}({params}) {{",
        cpp_ident(&m.name)
    ));
    w.scope(|w| {
        // Object arguments transfer one strong reference each. Adopting them
        // cannot throw, so it happens before anything that can, which
        // guarantees every reference is released even when a later argument
        // fails to decode.
        for p in &m.params {
            if let ArgPass::Object { slot, nullable } = p.arg_pass() {
                let class = local_type_name(
                    p.ty
                        .interface_name()
                        .expect("object arguments name an interface"),
                );
                let slot = slot_name(slot);
                let var = arg_var(p);
                if nullable {
                    w.line(format!("std::optional<{class}> {var};"));
                    w.line(format!("if ({slot}) {var}.emplace({slot});"));
                } else {
                    w.line(format!("{class} {var}({slot});"));
                }
            }
        }
        w.line("try {");
        w.scope(|w| {
            w.line(format!(
                "{iface}& impl = **static_cast<std::shared_ptr<{iface}>*>(ctx);"
            ));
            let args: Vec<String> = m
                .params
                .iter()
                .map(|p| emit_trampoline_arg(w, p, module, prefix))
                .collect();
            let call = format!("impl.{method}({})", args.join(", "));
            // Validation restricts callback returns to the direct family, so
            // the value is written straight into the C return.
            match &m.ret {
                None => {
                    w.line(format!("{call};"));
                    w.line("return;");
                }
                Some(Ty::Enum(e)) => {
                    w.line(format!(
                        "return static_cast<{}>(static_cast<int32_t>({call}));",
                        c_abi_struct_name(e, module, prefix)
                    ));
                }
                Some(_) => {
                    w.line(format!("return {call};"));
                }
            }
        });
        w.line("} catch (const std::exception& e) {");
        w.scope(|w| {
            w.line(format!(
                "{prefix}_error_set(out_err, {FOREIGN_ERROR_CODE}, e.what());"
            ));
        });
        w.line("} catch (...) {");
        w.scope(|w| {
            w.line(format!(
                "{prefix}_error_set(out_err, {FOREIGN_ERROR_CODE}, \"{iface}::{method} threw a non-standard exception\");"
            ));
        });
        w.line("}");
        if m.ret.is_some() {
            w.line(format!("return {c_ret}{{}};"));
        }
    });
    w.line("}");
    w.blank();
}

/// The local variable holding one decoded or adopted trampoline argument.
fn arg_var(p: &ParamBinding) -> String {
    format!("{}_val", p.name)
}

/// Emit any decode statements for one trampoline argument and return the
/// expression handed to the virtual method. Strings, bytes, and buffered
/// values are borrowed for the call, so they are copied or decoded into owned
/// C++ values and nothing is freed. Objects were adopted before the `try`
/// block and are moved into the call.
fn emit_trampoline_arg(w: &mut CodeWriter, p: &ParamBinding, module: &str, prefix: &str) -> String {
    let var = arg_var(p);
    match p.arg_pass() {
        ArgPass::Buffer { ptr, len } => {
            let rdr = format!("{}_r", p.name);
            w.line(format!(
                "detail::BufferReader {rdr}({}, {});",
                slot_name(ptr),
                slot_name(len)
            ));
            emit_read_decl(w, &p.ty, &var, &rdr, module, prefix);
            w.line(format!("{rdr}.expect_end();"));
            var
        }
        ArgPass::String { slot } => {
            let n = slot_name(slot);
            w.line(format!("std::string {var}({n} ? {n} : \"\");"));
            var
        }
        ArgPass::Bytes { ptr, len } => {
            let n0 = slot_name(ptr);
            let n1 = slot_name(len);
            w.line(format!("std::vector<uint8_t> {var}({n0}, {n0} + {n1});"));
            var
        }
        ArgPass::Object { .. } => format!("std::move({var})"),
        ArgPass::Direct { slot } => {
            let n = slot_name(slot);
            match &p.ty {
                Ty::Enum(e) => format!(
                    "static_cast<{}>(static_cast<int32_t>({n}))",
                    local_type_name(e)
                ),
                _ => n,
            }
        }
        ArgPass::Callback { .. } => {
            unreachable!("callback interfaces are never callback-method parameters")
        }
    }
}
