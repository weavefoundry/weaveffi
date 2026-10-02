//! `JniBridge.kt`: the internal object declaring one `external fun` per C
//! symbol (named after the symbol, so two modules' functions never
//! collide), the error factory the shim throws through, and the static
//! dispatch shims callback trampolines call.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{
    BindingModel, CallShape, CallbackInterfaceBinding, CallbackMethodBinding, FnBinding,
};

use crate::targets::kotlin::calls::{item_slot, lift, lower};
use crate::targets::kotlin::names::{kt_member, kt_param, Names};

/// The `JniBridge` member name of a callback method's dispatch shim.
pub(crate) fn shim_name(
    n: &Names,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
) -> String {
    n.native(&format!("{}_{}", cb.c_tag, m.name))
}

/// The JVM descriptor of a callback method's dispatch shim.
pub(crate) fn shim_descriptor(
    n: &Names,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
) -> String {
    let mut sig = format!("(L{}/{};", n.package_path, n.ty(&cb.name));
    for p in &m.params {
        sig.push_str(&n.jni_descriptor(Some(&p.ty)));
    }
    sig.push(')');
    sig.push_str(&n.jni_descriptor(m.ret.as_ref()));
    sig
}

/// The `external fun` declarations backing callable `f`.
fn callable_natives(w: &mut CodeWriter, n: &Names, f: &FnBinding) {
    let mut params: Vec<String> = Vec::new();
    if f.has_self {
        params.push("_self: Long".to_string());
    }
    params.extend(
        f.params
            .iter()
            .map(|p| format!("{}: {}", kt_param(&p.name), n.jni_type(&p.ty))),
    );
    match &f.shape {
        CallShape::Sync(abi) => {
            let ret = f
                .ret
                .as_ref()
                .map(|t| format!(": {}", n.jni_type(t)))
                .unwrap_or_default();
            w.line(format!(
                "@JvmStatic external fun {}({}){ret}",
                n.native(&abi.symbol),
                params.join(", ")
            ));
        }
        CallShape::Async(ab) => {
            if f.cancellable {
                params.push("_token: Long".to_string());
            }
            params.push("_done: NativeCompletion<*>".to_string());
            w.line(format!(
                "@JvmStatic external fun {}({})",
                n.native(&ab.launch.symbol),
                params.join(", ")
            ));
        }
        CallShape::Iterator(it) => {
            w.line(format!(
                "@JvmStatic external fun {}({}): Long",
                n.native(&it.launch.symbol),
                params.join(", ")
            ));
            match item_slot(&it.elem) {
                None => w.line(format!(
                    "@JvmStatic external fun {}(_iter: Long): ByteArray?",
                    n.native(&it.next.symbol)
                )),
                Some(slot) => w.line(format!(
                    "@JvmStatic external fun {}(_iter: Long, _out: {}): Boolean",
                    n.native(&it.next.symbol),
                    slot.kotlin_array
                )),
            };
            w.line(format!(
                "@JvmStatic external fun {}(_iter: Long)",
                n.native(&it.destroy_symbol)
            ));
        }
    }
}

/// The static shim a trampoline calls for one callback method: lifts each
/// argument from its JNI form, calls the implementation, and lowers the
/// result.
fn callback_shim(
    w: &mut CodeWriter,
    n: &Names,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
) {
    let mut params = vec![format!("_impl: {}", n.ty(&cb.name))];
    params.extend(
        m.params
            .iter()
            .map(|p| format!("{}: {}", kt_param(&p.name), n.jni_type(&p.ty))),
    );
    let args: Vec<String> = m
        .params
        .iter()
        .map(|p| lift(n, &p.ty, &kt_param(&p.name)))
        .collect();
    let call = format!("_impl.{}({})", kt_member(&m.name), args.join(", "));
    let name = shim_name(n, cb, m);
    w.line("@JvmStatic");
    match &m.ret {
        None => w.line(format!("fun {name}({}) {{ {call} }}", params.join(", "))),
        Some(t) => w.line(format!(
            "fun {name}({}): {} = {}",
            params.join(", "),
            n.jni_type(t),
            lower(n, t, &call)
        )),
    };
}

/// Render the body of `JniBridge.kt` (after the package line).
pub(crate) fn render_bridge(n: &Names, model: &BindingModel) -> String {
    let mut w = CodeWriter::four_space();
    w.blank();
    w.line("/**");
    w.line(format!(
        " * The JNI entry points of `lib{}_jni`: one native per C symbol, named after",
        n.library
    ));
    w.line(" * it, plus the hooks the shim calls back into. Internal plumbing for the");
    w.line(" * public wrappers.");
    w.line(" */");
    w.line("internal object JniBridge {");
    w.scope(|w| {
        w.line("init {");
        w.line("    NativeLibrary.ensureLoaded()");
        w.line("}");
        w.blank();
        w.line("/** The exception for a native failure; domain 0 is the generic one. */");
        w.line("@JvmStatic");
        w.line("@Suppress(\"UNUSED_PARAMETER\")");
        w.line("fun error(domain: Int, code: Int, message: ByteArray, payload: ByteArray?): FfiException {");
        w.scope(|w| {
            w.line("val text = decodeUtf8(message)");
            w.line("return when (domain) {");
            w.scope(|w| {
                for (i, eb) in n.domains(model) {
                    w.line(format!(
                        "{i} -> {}.fromCode(code, text, payload)",
                        n.exception(eb)
                    ));
                }
                w.line("else -> FfiException(code, text)");
            });
            w.line("}");
        });
        w.line("}");
        if model.has_callback_interfaces() {
            w.blank();
            w.line("/** The message a failed callback reports to the producer (code -4). */");
            w.line("@JvmStatic");
            w.line("fun describe(error: Throwable): ByteArray = encodeUtf8(error.toString())");
        }
        w.blank();
        w.line("@JvmStatic external fun debug_live(kind: Int): Long");
        if model.has_async() {
            w.line("@JvmStatic external fun cancel_token_create(): Long");
            w.line("@JvmStatic external fun cancel_token_cancel(token: Long)");
            w.line("@JvmStatic external fun cancel_token_destroy(token: Long)");
        }
        for m in &model.modules {
            for i in &m.interfaces {
                w.line(format!(
                    "@JvmStatic external fun {}(_self: Long): Long",
                    n.native(&i.clone_symbol)
                ));
                w.line(format!(
                    "@JvmStatic external fun {}(_self: Long)",
                    n.native(&i.destroy_symbol)
                ));
            }
            for f in m.callables() {
                callable_natives(w, n, f);
            }
        }
        for (_, cb) in model.callback_interfaces() {
            for m in &cb.methods {
                w.blank();
                callback_shim(w, n, cb, m);
            }
        }
    });
    w.line("}");
    w.finish()
}
