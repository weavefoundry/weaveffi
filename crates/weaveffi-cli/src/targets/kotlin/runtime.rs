//! The fixed runtime sources under `runtime/`, spliced into the output with
//! `{{PLACEHOLDER}}` substitution: the Kotlin runtime (`FfiException`,
//! `NativeBugException`, `NativeIterator`, the loader, `NativeHandle`, the
//! phantom-reference cleaner), the value-buffer codec, the coroutine bridge,
//! and the JNI shim's helpers (the contract checker, thread attach and
//! detach, async completions, and callback dispatch).

use crate::cabi::ABI_VERSION;
use weaveffi_model::model::Model;
use weaveffi_model::ty::Family;

use crate::targets::kotlin::names::Names;

const RUNTIME_KT: &str = include_str!("runtime/Runtime.kt");
const BUFFERS_KT: &str = include_str!("runtime/Buffers.kt");
const ASYNC_KT: &str = include_str!("runtime/Async.kt");
const JNI_CORE_C: &str = include_str!("runtime/jni_core.c");
const JNI_THREADS_C: &str = include_str!("runtime/jni_threads.c");
const JNI_ASYNC_C: &str = include_str!("runtime/jni_async.c");
const JNI_CALLBACKS_C: &str = include_str!("runtime/jni_callbacks.c");
const JNI_CALLBACK_RUNS_C: &str = include_str!("runtime/jni_callback_runs.c");

/// Replace every `{{KEY}}` in `template` with its value.
fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    debug_assert!(
        !out.contains("{{"),
        "unfilled placeholder in a runtime template"
    );
    out
}

/// The JNI shim library the Kotlin side loads: `{library}_jni`.
pub(crate) fn jni_library(library: &str) -> String {
    format!("{library}_jni")
}

/// `Runtime.kt`: always emitted.
pub(crate) fn runtime_kt(n: &Names, env_var: &str) -> String {
    fill(
        RUNTIME_KT,
        &[
            ("PACKAGE", &n.package),
            ("LIBRARY", &n.library),
            ("JNI_LIBRARY", &jni_library(&n.library)),
            ("LIBRARY_ENV", env_var),
        ],
    )
}

/// `Buffers.kt`: emitted when any value crosses as a value buffer.
pub(crate) fn buffers_kt(n: &Names) -> String {
    fill(BUFFERS_KT, &[("PACKAGE", &n.package)])
}

/// `Async.kt`: emitted when any callable is async.
pub(crate) fn async_kt(n: &Names) -> String {
    fill(ASYNC_KT, &[("PACKAGE", &n.package)])
}

/// The fixed head of the JNI shim: the core helpers and `JNI_OnLoad`, plus
/// the thread, async, and callback support the model needs.
pub(crate) fn jni_runtime(n: &Names, model: &Model, header: &str, name: &str) -> String {
    let abi = ABI_VERSION.to_string();
    let jni_class = n.jni_class();
    let macro_prefix = n.prefix.to_ascii_uppercase();
    let vars: [(&str, &str); 8] = [
        ("PREFIX", &n.prefix),
        ("MACRO_PREFIX", &macro_prefix),
        ("HEADER", header),
        ("ABI_VERSION", &abi),
        ("NAME", name),
        ("PACKAGE_PATH", &n.package_path),
        ("JNI_CLASS", &jni_class),
        ("LIBRARY", &n.library),
    ];
    let mut out = fill(JNI_CORE_C, &vars);
    let has_async = model.has_async();
    let has_callbacks = model.has_callback_interfaces();
    if has_async || has_callbacks {
        out.push_str(&fill(JNI_THREADS_C, &vars));
    }
    if has_async {
        out.push_str(&fill(JNI_ASYNC_C, &vars));
    }
    if has_callbacks {
        out.push_str(&fill(JNI_CALLBACKS_C, &vars));
    }
    let returns_runs = model.callback_interfaces().any(|(_, cb)| {
        cb.methods.iter().any(|m| {
            m.ret.as_ref().is_some_and(|t| {
                matches!(t.family(), Family::String | Family::Bytes | Family::Buffer)
            })
        })
    });
    if returns_runs {
        out.push_str(&fill(JNI_CALLBACK_RUNS_C, &vars));
    }
    out
}
