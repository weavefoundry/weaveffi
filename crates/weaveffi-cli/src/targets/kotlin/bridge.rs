//! `JniBridge.kt`: the internal object declaring one `external fun` per C
//! symbol (named after the symbol, so two modules' functions never
//! collide), the error factory the shim throws through, and the static
//! dispatch shims callback trampolines call.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{
    CallShape, CallbackInterfaceBinding, CallbackMethodBinding, FnBinding, Model,
};
use weaveffi_model::plan::{ArgPass, ErrorStrategy, RetPass};

use crate::targets::kotlin::calls::{has_name, jni_params, lift, lift_split, lower};
use crate::targets::kotlin::carrier::Carrier;
use crate::targets::kotlin::names::{kt_member, kt_param, Names, TRAP_DOMAIN, UNTYPED_DOMAIN};

/// The `JniBridge` member name of a callback method's dispatch shim.
pub(crate) fn shim_name(
    n: &Names,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
) -> String {
    n.native(&format!("{}_{}", cb.c_tag, m.name))
}

/// The carrier of a callback method's return, or `None` for void.
fn shim_ret(m: &CallbackMethodBinding) -> Option<Carrier> {
    Carrier::of_callback_ret(&m.ret_pass, m.ret.as_ref())
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
        if let Some(c) = Carrier::of_arg(&p.pass) {
            sig.push_str(&c.sig());
        }
    }
    sig.push(')');
    sig.push_str(&shim_ret(m).map_or_else(|| "V".to_string(), Carrier::sig));
    sig
}

/// The `external fun` declarations backing callable `f`.
fn callable_natives(w: &mut CodeWriter, n: &Names, f: &FnBinding) {
    let mut params = jni_params(n, f);
    let native = n.native(&f.abi.symbol);
    match (&f.shape, &f.ret_pass) {
        (CallShape::Async(ab), _) => {
            if ab.cancellable() {
                params.push("_token: Long".to_string());
            }
            params.push("_done: NativeCompletion<*>".to_string());
            w.line(format!(
                "@JvmStatic external fun {native}({})",
                params.join(", ")
            ));
        }
        (CallShape::Sync, RetPass::Iterator(it)) => {
            w.line(format!(
                "@JvmStatic external fun {native}({}): Long",
                params.join(", ")
            ));
            w.line(format!(
                "@JvmStatic external fun {}(_iter: Long, _out: Array<Any?>): Boolean",
                n.native(&it.next.symbol)
            ));
            w.line(format!(
                "@JvmStatic external fun {}(_iter: Long)",
                n.native(&it.destroy_symbol)
            ));
        }
        (CallShape::Sync, pass) => {
            let ret = Carrier::of_ret(pass, f.ret.as_ref().and_then(|r| r.value()))
                .map(|c| format!(": {}", c.kotlin()))
                .unwrap_or_default();
            w.line(format!(
                "@JvmStatic external fun {native}({}){ret}",
                params.join(", ")
            ));
        }
    }
}

/// The static shim a trampoline calls for one callback method: adopts its
/// object arguments, lifts every other argument from its JNI form, calls
/// the implementation, and lowers the result. A failure never unwinds into
/// the producer: the shim reports it through `out_err` (`_err`) and returns
/// a zero value. A method that throws a domain reports that domain's
/// exceptions with their code and payload.
fn callback_shim(
    w: &mut CodeWriter,
    n: &Names,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
) {
    let mut params = vec![format!("_impl: {}", n.ty(&cb.name)), "_err: Long".into()];
    // Objects are adopted before anything that can fail, so a failure never
    // strands a reference the producer handed over.
    let mut adopted = Vec::new();
    let mut args = Vec::new();
    for p in &m.params {
        let name = kt_param(&p.name);
        match Carrier::of_arg(&p.pass) {
            Some(Carrier::Split(k)) => {
                let has = has_name(&name);
                params.push(format!("{has}: Boolean"));
                params.push(format!("{name}: {}", k.name()));
                args.push(lift_split(n, &p.ty, k, &has, &name));
            }
            Some(c) => {
                params.push(format!("{name}: {}", c.kotlin()));
                if matches!(p.pass, ArgPass::Object { .. }) {
                    adopted.push(format!("val _{name} = {}", lift(n, &p.ty, c, &name)));
                    args.push(format!("_{name}"));
                } else {
                    args.push(lift(n, &p.ty, c, &name));
                }
            }
            None => unreachable!("callback method parameters are values"),
        }
    }
    let call = format!("_impl.{}({})", kt_member(&m.name), args.join(", "));
    let (typed, fallback) = match &m.error {
        ErrorStrategy::Domain(d) => (format!("_e as? {}", n.exception(d)), -1),
        ErrorStrategy::Untyped => ("null".to_string(), -1),
        ErrorStrategy::Trap => ("null".to_string(), -4),
    };
    let ret = shim_ret(m);
    let ret_sig = match ret {
        Some(c @ (Carrier::Prim(_) | Carrier::Handle)) => format!(": {}", c.kotlin()),
        Some(c) => format!(": {}?", c.kotlin().trim_end_matches('?')),
        None => String::new(),
    };
    let failure = match ret {
        Some(Carrier::Prim(k)) => Some(k.zero()),
        Some(Carrier::Handle) => Some("0L"),
        Some(_) => Some("null"),
        None => None,
    };
    let name = shim_name(n, cb, m);
    let sig = format!("fun {name}({}){ret_sig}", params.join(", "));
    let attempt = |w: &mut CodeWriter, head: String| {
        w.line(head);
        w.scope(|w| {
            match (ret, &m.ret) {
                (Some(c), Some(t)) => w.line(lower(n, t, c, &call)),
                _ => w.line(&call),
            };
        });
        w.line("} catch (_e: Throwable) {");
        w.scope(|w| {
            w.line(format!("fail(_err, _e, {typed}, {fallback})"));
            if let Some(v) = failure {
                w.line(v);
            }
        });
        w.line("}");
    };
    w.line("@JvmStatic");
    if adopted.is_empty() && ret.is_some() {
        attempt(w, format!("{sig} = try {{"));
    } else {
        w.line(format!("{sig} {{"));
        w.scope(|w| {
            for a in &adopted {
                w.line(a);
            }
            let head = if ret.is_some() {
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
            w.line("NativeLibrary.load()");
        });
        w.blank();
        w.line("/**");
        w.line(" * The exception for a native failure of a call whose errors map through");
        w.line(format!(
            " * [domain]: {TRAP_DOMAIN} for a call that can't fail ([NativeBugException]), {UNTYPED_DOMAIN} for"
        ));
        w.line(" * `throws: any` ([FfiException]), or an error domain, whose unknown codes");
        w.line(" * map to the domain's base exception. Runtime (negative) codes are");
        w.line(" * [FfiException]s.");
        w.line(" */");
        w.line("@JvmStatic");
        w.block(
            "fun error(domain: Int, code: Int, message: ByteArray, payload: ByteArray?): Throwable {",
            "}",
            |w| {
                w.line("val text = decodeUtf8(message)");
                w.block("return when {", "}", |w| {
                    w.line(format!(
                        "domain == {TRAP_DOMAIN} -> NativeBugException(code, text)"
                    ));
                    w.line("code < 0 -> FfiException(code, text)");
                    for (i, exc) in n.domains() {
                        w.line(format!(
                            "domain == {i} -> {exc}.fromCode(code, text, payload)"
                        ));
                    }
                    w.line("else -> FfiException(code, text)");
                });
            },
        );
        if model.has_callback_interfaces() {
            w.blank();
            w.line("/**");
            w.line(" * Reports a failed callback through its vtable entry's `out_err` ([err]):");
            w.line(" * [typed], an exception of the domain the method throws, with its code and");
            w.line(" * payload fields, and anything else with code [fallback] and its message.");
            w.line(" */");
            w.block(
                "private fun fail(err: Long, error: Throwable, typed: FfiException?, fallback: Int) {",
                "}",
                |w| {
                    w.line("if (typed != null) {");
                    w.scope(|w| {
                        w.line("error_set(err, typed.code, encodeUtf8(typed.message ?: \"\"))");
                        w.line("typed.encodePayload()?.let { error_set_payload(err, it) }");
                    });
                    w.line("} else {");
                    w.scope(|w| {
                        w.line("error_set(err, fallback, encodeUtf8(error.message ?: error.toString()))");
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
        for (_, cb) in model.callback_interfaces() {
            for m in &cb.methods {
                w.blank();
                callback_shim(w, n, cb, m);
            }
        }
    });
    w.finish()
}
