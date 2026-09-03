//! Per-callable Kotlin wrappers: free functions, `suspend fun` shims, and the
//! interface-member native declarations.

use std::fmt::Write as _;

use weaveffi_core::codegen::common::pascal_case;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{CallShape, ErrorBinding, FnBinding, ModuleBinding, ParamBinding};
use weaveffi_core::plan::ErrorStrategy;
use weaveffi_core::utils::local_type_name;

use crate::codec::{kt_decode_expr, kt_encode_expr};
use crate::entities::kotlin_exception_name;
use crate::types::{
    camel_params, kotlin_fn_name, kotlin_iterator_class_name, kotlin_jni_type, kotlin_type,
    kt_param, needs_wrapper_split,
};

/// The Kotlin lambda mapping an async error `(code, message, payload)` triple
/// to the exception the continuation resumes with: the typed domain exception
/// (which decodes the payload) when the callable's
/// [`ErrorStrategy`] is [`Throws`](ErrorStrategy::Throws), the generic brand
/// exception otherwise, so the runtime's reserved negative codes always trap
/// generically.
pub(crate) fn kotlin_error_mapper(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match (error, f.error_strategy()) {
        (Some(eb), ErrorStrategy::Throws) => {
            format!(
                "{{ code, message, payload -> {}.fromCode(code, message, payload) }}",
                kotlin_exception_name(eb)
            )
        }
        _ => format!(
            "{{ code, message, _ -> {}(code, message) }}",
            errors::EXCEPTION_BRAND
        ),
    }
}

/// Render one free function into the `WeaveFFI` companion: a bare `external
/// fun` when every type crosses JNI unchanged (callback interface
/// implementations included: they cross as the object itself), otherwise a
/// private `{name}Jni` external plus a public wrapper that unwraps handles and
/// enums on the way in and re-wraps class returns on the way out.
pub(crate) fn render_kotlin_free_fn(
    out: &mut String,
    m: &ModuleBinding,
    f: &FnBinding,
    strip: bool,
    c_prefix: &str,
) {
    let func_name = kotlin_fn_name(&m.path, &f.name, strip);
    crate::docs::emit_fn_doc(out, &f.doc, &camel_params(&f.params), "        ");
    if f.is_async {
        let native = format!("{func_name}Async");
        let mapper = kotlin_error_mapper(f, m.error.as_ref());
        render_kotlin_async_fun(
            out,
            f,
            &func_name,
            &native,
            false,
            "@JvmStatic ",
            true,
            2,
            &mapper,
        );
    } else if needs_wrapper_split(f) {
        let native_params: Vec<String> = f
            .params
            .iter()
            .map(|p| format!("{}: {}", kt_param(&p.name), kotlin_jni_type(&p.ty)))
            .collect();
        let native_ret = f
            .ret
            .as_ref()
            .map(kotlin_jni_type)
            .unwrap_or_else(|| "Unit".to_string());
        let _ = writeln!(
            out,
            "        @JvmStatic private external fun {}Jni({}): {}",
            func_name,
            native_params.join(", "),
            native_ret
        );
        let call_args: Vec<String> = f.params.iter().map(kotlin_unwrap_arg).collect();
        let call = format!("{}Jni({})", func_name, call_args.join(", "));
        let mut w = CodeWriter::four_space().with_depth(2);
        write_kotlin_sync_wrapper(
            &mut w,
            f,
            &format!("@JvmStatic fun {func_name}"),
            &call,
            c_prefix,
        );
        out.push_str(&w.finish());
    } else {
        let params_sig: Vec<String> = f
            .params
            .iter()
            .map(|p| format!("{}: {}", kt_param(&p.name), kotlin_type(&p.ty)))
            .collect();
        let ret = f
            .ret
            .as_ref()
            .map(kotlin_type)
            .unwrap_or_else(|| "Unit".to_string());
        if let Some(msg) = &f.deprecated {
            let _ = writeln!(out, "        @Deprecated(\"{}\")", msg.replace('"', "\\\""));
        }
        let _ = writeln!(
            out,
            "        @JvmStatic external fun {}({}): {}",
            func_name,
            params_sig.join(", "),
            ret
        );
    }
}

/// The Kotlin expression that lowers one public argument for a JNI call:
/// buffered values pack into a `ByteArray` (cloning any object they carry),
/// enums pass `.value`, interfaces lend the raw `.handle` (nullable via
/// `?.`), and a callback interface implementation passes as the object the
/// JNI shim pins.
pub(crate) fn kotlin_unwrap_arg(p: &ParamBinding) -> String {
    let n = kt_param(&p.name);
    if p.ty.is_buffered() {
        return kt_encode_expr(&p.ty, &n);
    }
    match &p.ty {
        Ty::Enum(_) => format!("{n}.value"),
        Ty::Interface(_) => format!("{n}.handle"),
        // Only `Interface?` reaches here (every other optional is buffered).
        Ty::Optional(_) => format!("{n}?.handle"),
        _ => n,
    }
}

/// The Kotlin expression re-wrapping a lowered JNI value `expr` into the
/// public return type, or `None` when the lowered value already is the public
/// type: buffered returns decode the `ByteArray`, enums round-trip through
/// `fromValue`, interfaces through the companion's `fromHandle` (nullable via
/// `?.let`).
pub(crate) fn kotlin_wrap_return(ret: Option<&Ty>, expr: &str) -> Option<String> {
    let ret = ret?;
    if ret.is_buffered() {
        return Some(kt_decode_expr(ret, expr));
    }
    match ret {
        Ty::Enum(name) => Some(format!("{}.fromValue({expr})", local_type_name(name))),
        Ty::Interface(name) => Some(format!("{}.fromHandle({expr})", local_type_name(name))),
        // Only `Interface?` reaches here (every other optional is buffered).
        Ty::Optional(inner) => match inner.as_ref() {
            Ty::Interface(name) => Some(format!(
                "{expr}?.let {{ {}.fromHandle(it) }}",
                local_type_name(name)
            )),
            _ => unreachable!("buffered optionals are handled above"),
        },
        _ => None,
    }
}

/// Write the public wrapper for a sync callable whose lowered JNI call is
/// `call`. `decl` carries everything before the parameter list (annotations
/// resolved by the caller, e.g. `"@JvmStatic fun createContact"` or
/// `"operator fun invoke"`).
pub(crate) fn write_kotlin_sync_wrapper(
    w: &mut CodeWriter,
    f: &FnBinding,
    decl: &str,
    call: &str,
    c_prefix: &str,
) {
    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", kt_param(&p.name), kotlin_type(&p.ty)))
        .collect();
    let public_ret = f
        .ret
        .as_ref()
        .map(kotlin_type)
        .unwrap_or_else(|| "Unit".to_string());
    if let Some(msg) = &f.deprecated {
        w.line(format!("@Deprecated(\"{}\")", msg.replace('"', "\\\"")));
    }
    // An iterator callable's native launcher returns the raw handle; the
    // public wrapper adopts it into the generated lazy iterator class.
    if let CallShape::Iterator(it) = &f.shape {
        let class = kotlin_iterator_class_name(it, c_prefix);
        w.line(format!(
            "{decl}({}): {public_ret} = {class}({call})",
            params_sig.join(", ")
        ));
        return;
    }
    match kotlin_wrap_return(f.ret.as_ref(), call) {
        Some(wrapped) => {
            w.line(format!(
                "{decl}({}): {public_ret} = {wrapped}",
                params_sig.join(", ")
            ));
        }
        None if f.ret.is_some() => {
            w.line(format!(
                "{decl}({}): {public_ret} = {call}",
                params_sig.join(", ")
            ));
        }
        None => {
            w.line(format!("{decl}({}) {{ {call} }}", params_sig.join(", ")));
        }
    }
}

/// The `external` JNI launcher parameter list for an async callable: the raw
/// `handle` receiver for methods, lowered input slots, the optional cancel
/// token, then the boxed continuation.
pub(crate) fn kotlin_async_native_params(f: &FnBinding, has_self: bool) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    if has_self {
        chain.push("selfHandle: Long".to_string());
    }
    chain.extend(
        f.params
            .iter()
            .map(|p| format!("{}: {}", kt_param(&p.name), kotlin_jni_type(&p.ty))),
    );
    if f.cancellable {
        chain.push("cancelToken: Long".to_string());
    }
    chain.push("callback: Any".to_string());
    chain
}

/// Render an async callable: the private `external` launcher declaration
/// (unless the caller declares it elsewhere, as interface companions do) plus
/// the public `suspend fun` wrapper that resumes through `WeaveContinuation`
/// and maps error codes to exceptions via `error_mapper`.
///
/// The external launcher crosses into JNI C, which declares raw JNI types
/// (`jlong` for handles/structs/interfaces, `jint` for enums), so its
/// signature uses the lowered types and the suspend wrapper unwraps
/// (`.handle` / `.value`) exactly like the sync path. Passing a wrapper object
/// where the C side reads a `jlong` is undefined behaviour (the pointer-sized
/// register holds a JVM reference).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_kotlin_async_fun(
    out: &mut String,
    f: &FnBinding,
    public_name: &str,
    native_name: &str,
    has_self: bool,
    modifier: &str,
    emit_native: bool,
    depth: usize,
    error_mapper: &str,
) {
    let mut w = CodeWriter::four_space().with_depth(depth);
    if emit_native {
        w.line(format!(
            "@JvmStatic private external fun {}({})",
            native_name,
            kotlin_async_native_params(f, has_self).join(", ")
        ));
    }

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", kt_param(&p.name), kotlin_type(&p.ty)))
        .collect();
    let public_ret = f
        .ret
        .as_ref()
        .map(kotlin_type)
        .unwrap_or_else(|| "Unit".to_string());
    // The continuation resumes with the value the JNI callback boxes (the
    // lowered type); buffered/enum/class returns are re-wrapped after the
    // await.
    let jni_ret = f
        .ret
        .as_ref()
        .map(kotlin_jni_type)
        .unwrap_or_else(|| "Unit".to_string());
    let mut call_args: Vec<String> = Vec::new();
    if has_self {
        call_args.push("handle".to_string());
    }
    call_args.extend(f.params.iter().map(kotlin_unwrap_arg));
    if f.cancellable {
        call_args.push("0L".to_string());
    }
    call_args.push(format!("WeaveContinuation(cont) {error_mapper}"));
    if let Some(msg) = &f.deprecated {
        w.line(format!("@Deprecated(\"{}\")", msg.replace('"', "\\\"")));
    }

    // Map the resumed (lowered) value back to the public type.
    match kotlin_wrap_return(f.ret.as_ref(), "raw") {
        Some(wrap) => {
            w.line(format!(
                "{modifier}suspend fun {public_name}({}): {public_ret} {{",
                params_sig.join(", ")
            ));
            w.scope(|w| {
                w.line(format!(
                    "val raw: {jni_ret} = suspendCancellableCoroutine {{ cont ->"
                ));
                w.scope(|w| {
                    w.line(format!("{}({})", native_name, call_args.join(", ")));
                });
                w.line("}");
                w.line(format!("return {wrap}"));
            });
            w.line("}");
        }
        None => {
            w.line(format!(
                "{modifier}suspend fun {public_name}({}): {public_ret} = suspendCancellableCoroutine {{ cont ->",
                params_sig.join(", ")
            ));
            w.scope(|w| {
                w.line(format!("{}({})", native_name, call_args.join(", ")));
            });
            w.line("}");
        }
    }
    out.push_str(&w.finish());
}

/// The Kotlin `external` declaration name for an interface member: `native` +
/// the member's PascalCase name, with an `Async` suffix for async members
/// (`nativeAdd`, `nativeFetchAsync`). The JNI C bridge exports the matching
/// `Java_<pkg>_<Class>_<name>` symbol.
pub(crate) fn interface_native_name(f: &FnBinding) -> String {
    let base = format!("native{}", pascal_case(&f.name));
    if f.is_async {
        format!("{base}Async")
    } else {
        base
    }
}

/// The full `external fun` declaration line for one interface member. Instance
/// methods take the raw receiver as a leading `selfHandle: Long`; every slot
/// uses the lowered JNI type, matching the C bridge exactly.
pub(crate) fn interface_native_decl(f: &FnBinding, has_self: bool) -> String {
    if f.is_async {
        return format!(
            "@JvmStatic private external fun {}({})",
            interface_native_name(f),
            kotlin_async_native_params(f, has_self).join(", ")
        );
    }
    let mut params: Vec<String> = Vec::new();
    if has_self {
        params.push("selfHandle: Long".to_string());
    }
    params.extend(
        f.params
            .iter()
            .map(|p| format!("{}: {}", kt_param(&p.name), kotlin_jni_type(&p.ty))),
    );
    let ret = f
        .ret
        .as_ref()
        .map(kotlin_jni_type)
        .unwrap_or_else(|| "Unit".to_string());
    format!(
        "@JvmStatic private external fun {}({}): {}",
        interface_native_name(f),
        params.join(", "),
        ret
    )
}

/// The lowered call expression for one interface member: the native name
/// applied to the receiver handle (when `self_arg` is set) and the unwrapped
/// public arguments.
pub(crate) fn interface_native_call(f: &FnBinding, self_arg: Option<&str>) -> String {
    let mut args: Vec<String> = Vec::new();
    if let Some(s) = self_arg {
        args.push(s.to_string());
    }
    args.extend(f.params.iter().map(kotlin_unwrap_arg));
    format!("{}({})", interface_native_name(f), args.join(", "))
}
