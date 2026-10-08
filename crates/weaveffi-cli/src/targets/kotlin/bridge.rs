//! `JniBridge.kt`: the internal object declaring one `external fun` per C
//! symbol (named after the symbol, so two modules' functions never
//! collide), the error factory the shim throws through, and the static
//! dispatch shims callback trampolines call.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{
    CallShape, CallbackInterfaceBinding, CallbackMethodBinding, ErrorBinding, FnBinding, Model,
};
use weaveffi_model::ty::{Family, Ty};

use crate::targets::kotlin::calls::{item_slot, lift, lower};
use crate::targets::kotlin::names::{jni_kind, kt_member, kt_param, Names};

/// The `JniBridge` member name of a callback method's dispatch shim.
pub(crate) fn shim_name(
    n: &Names,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
) -> String {
    n.native(&format!("{}_{}", cb.c_tag, m.name))
}

/// The JVM descriptor of a callback method's dispatch shim: the
/// implementation, the `out_err` address, the parameters, then the return.
pub(crate) fn shim_descriptor(
    n: &Names,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
) -> String {
    let mut sig = format!("(L{}/{};J", n.package_path, n.ty(&cb.name));
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

/// The value a dispatch shim returns after reporting a failure (the
/// producer ignores it).
fn failure_value(t: &Ty) -> &'static str {
    match t.family() {
        Family::Direct => match jni_kind(t) {
            "Boolean" => "false",
            "Byte" => "0.toByte()",
            "Short" => "0.toShort()",
            "Int" => "0",
            "Float" => "0f",
            "Double" => "0.0",
            _ => "0L",
        },
        Family::Object { .. } => "0L",
        _ => "null",
    }
}

/// The static shim a trampoline calls for one callback method: adopts its
/// object arguments, lifts every other argument from its JNI form, calls
/// the implementation, and lowers the result. A failure never unwinds into
/// the producer: the shim reports it through `out_err` (`_err`) and returns
/// a zero value. `domain` is the error domain of a method declared
/// `throws`, whose typed errors keep their code and payload.
fn callback_shim(
    w: &mut CodeWriter,
    n: &Names,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    domain: Option<&ErrorBinding>,
) {
    let mut params = vec![format!("_impl: {}", n.ty(&cb.name)), "_err: Long".into()];
    params.extend(
        m.params
            .iter()
            .map(|p| format!("{}: {}", kt_param(&p.name), n.jni_type(&p.ty))),
    );
    // Objects are adopted before anything that can fail, so a failure never
    // strands a reference the producer handed over.
    let mut adopted = Vec::new();
    let args: Vec<String> = m
        .params
        .iter()
        .map(|p| {
            let name = kt_param(&p.name);
            match p.ty.family() {
                Family::Object { .. } => {
                    adopted.push(format!("val _{name} = {}", lift(n, &p.ty, &name)));
                    format!("_{name}")
                }
                _ => lift(n, &p.ty, &name),
            }
        })
        .collect();
    let call = format!("_impl.{}({})", kt_member(&m.name), args.join(", "));
    let typed = match domain {
        Some(eb) if m.throws => format!("_e as? {}", n.exception(eb)),
        _ => "null".to_string(),
    };
    let ret = m
        .ret
        .as_ref()
        .map(|t| match t.family() {
            Family::String | Family::Bytes | Family::Buffer => ": ByteArray?".to_string(),
            _ => format!(": {}", n.jni_type(t)),
        })
        .unwrap_or_default();
    let name = shim_name(n, cb, m);
    let sig = format!("fun {name}({}){ret}", params.join(", "));
    // `head` is the line opening the `try`.
    let attempt = |w: &mut CodeWriter, head: String| {
        w.line(head);
        w.scope(|w| {
            match &m.ret {
                Some(t) => w.line(lower(n, t, &call)),
                None => w.line(&call),
            };
        });
        w.line("} catch (_e: Throwable) {");
        w.scope(|w| {
            w.line(format!("fail(_err, _e, {typed})"));
            if let Some(t) = &m.ret {
                w.line(failure_value(t));
            }
        });
        w.line("}");
    };
    w.line("@JvmStatic");
    if adopted.is_empty() && m.ret.is_some() {
        attempt(w, format!("{sig} = try {{"));
    } else {
        w.line(format!("{sig} {{"));
        w.scope(|w| {
            for a in &adopted {
                w.line(a);
            }
            let head = if m.ret.is_some() {
                "return try {"
            } else {
                "try {"
            };
            attempt(w, head.to_string());
        });
        w.line("}");
    }
}

/// Render the body of `JniBridge.kt` (after the package line).
pub(crate) fn render_bridge(n: &Names, model: &Model) -> String {
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
    w.block("internal object JniBridge {", "}", |w| {
        w.block("init {", "}", |w| {
            w.line("NativeLibrary.ensureLoaded()");
        });
        w.blank();
        w.line("/**");
        w.line(" * The exception for a native failure: through error domain [domain] for a");
        w.line(" * throwing call, or a [NativeBugException] (domain 0) for one that can't fail.");
        w.line(" */");
        w.line("@JvmStatic");
        w.block(
            "fun error(domain: Int, code: Int, message: ByteArray, payload: ByteArray?): Throwable {",
            "}",
            |w| {
                w.line("val text = decodeUtf8(message)");
                w.block("return when (domain) {", "}", |w| {
                    for (i, eb) in n.domains(model) {
                        w.line(format!(
                            "{i} -> {}.fromCode(code, text, payload)",
                            n.exception(eb)
                        ));
                    }
                    w.line("else -> NativeBugException(code, text)");
                });
            },
        );
        if model.has_callback_interfaces() {
            w.blank();
            w.line("/**");
            w.line(" * Reports a failed callback through its vtable entry's `out_err` ([err]):");
            w.line(" * [typed], a domain error the method is declared to throw, with its code and");
            w.line(" * payload fields, and anything else as code -4 with its message.");
            w.line(" */");
            w.block(
                "private fun fail(err: Long, error: Throwable, typed: FfiException?) {",
                "}",
                |w| {
                    w.line("if (typed != null) {");
                    w.scope(|w| {
                        w.line("error_set(err, typed.code, encodeUtf8(typed.message ?: \"\"))");
                        w.line("typed.encodePayload()?.let { error_set_payload(err, it) }");
                    });
                    w.line("} else {");
                    w.scope(|w| {
                        w.line("error_set(err, -4, encodeUtf8(error.message ?: error.toString()))");
                    });
                    w.line("}");
                },
            );
            w.blank();
            w.line("@JvmStatic external fun error_set(err: Long, code: Int, message: ByteArray)");
            w.line("@JvmStatic external fun error_set_payload(err: Long, payload: ByteArray)");
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
        for (owner, cb) in model.callback_interfaces() {
            for m in &cb.methods {
                w.blank();
                callback_shim(w, n, cb, m, model.error_domain(owner));
            }
        }
    });
    w.finish()
}
