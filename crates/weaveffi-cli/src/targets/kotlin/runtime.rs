//! The fixed runtime sources under `runtime/`, spliced into the output with
//! `{{PLACEHOLDER}}` substitution: the Kotlin runtime (`FfiException`,
//! `NativeBugException`, `NativeSequence`, the JNA loader and its load-time
//! checks, `NativeHandle`, the phantom-reference cleaner, the callback
//! registry), the C shapes JNA maps (`Run`, `NativeError`, the callback
//! argument, one `{p}_opt_*` structure per scalar, and the runtime surface),
//! the value-buffer codec and content equality, and the coroutine bridge.

use weaveffi_model::model::ABI_VERSION;
use weaveffi_model::ty::Prim;

use crate::codegen::CodeWriter;
use crate::targets::kotlin::names::{jna_prim, kt_prim, Names};

const RUNTIME_KT: &str = include_str!("runtime/Runtime.kt");
const ABI_KT: &str = include_str!("runtime/Abi.kt");
const BUFFERS_KT: &str = include_str!("runtime/Buffers.kt");
const ASYNC_KT: &str = include_str!("runtime/Async.kt");

/// The scalars with a `{p}_opt_*` structure, in the header's order.
const OPT_PRIMS: [Prim; 11] = [
    Prim::Bool,
    Prim::I8,
    Prim::I16,
    Prim::I32,
    Prim::I64,
    Prim::U8,
    Prim::U16,
    Prim::U32,
    Prim::U64,
    Prim::F32,
    Prim::F64,
];

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

/// `Runtime.kt`: always emitted. `name` is the library's display name (for
/// load errors) and `env_var` its path override.
pub(crate) fn runtime_kt(n: &Names, library: &str, name: &str, env_var: &str) -> String {
    let abi = ABI_VERSION.to_string();
    fill(
        RUNTIME_KT,
        &[
            ("PACKAGE", &n.package),
            ("LIBRARY", library),
            ("LIBRARY_ENV", env_var),
            ("PREFIX", &n.prefix),
            ("NAME", name),
            ("ABI_VERSION", &abi),
        ],
    )
}

/// `Abi.kt`: always emitted.
pub(crate) fn abi_kt(n: &Names) -> String {
    let optionals = optionals(&n.prefix);
    fill(
        ABI_KT,
        &[
            ("PACKAGE", &n.package),
            ("PREFIX", &n.prefix),
            ("OPTIONALS", &optionals),
        ],
    )
}

/// `Buffers.kt`: emitted when any value crosses as a value buffer.
pub(crate) fn buffers_kt(n: &Names) -> String {
    fill(BUFFERS_KT, &[("PACKAGE", &n.package)])
}

/// `Async.kt`: emitted when any callable is async.
pub(crate) fn async_kt(n: &Names) -> String {
    fill(ASYNC_KT, &[("PACKAGE", &n.package), ("PREFIX", &n.prefix)])
}

/// The JNA structure of the optional scalar `p` (`OptI32`).
pub(crate) fn opt_class(p: Prim) -> String {
    format!("Opt{}", p.pascal())
}

/// The function building a `{p}_opt_*` value (`optI32`).
pub(crate) fn opt_fn(p: Prim) -> String {
    format!("opt{}", p.pascal())
}

/// One structure, its `get`, and its builder per optional scalar: the
/// `{p}_opt_{prim}` `{bool has; T value;}` shapes, with the value in its JNA
/// carrier and converted to the public type at the edges.
fn optionals(prefix: &str) -> String {
    let mut w = CodeWriter::four_space();
    for p in OPT_PRIMS {
        let class = opt_class(p);
        let jna = jna_prim(p);
        let public = kt_prim(p);
        let (lift, lower, zero) = match p {
            Prim::Bool => ("value.asBoolean()", "value.asByte()", "0"),
            Prim::U8 => ("value.toUByte()", "value.toByte()", "0"),
            Prim::U16 => ("value.toUShort()", "value.toShort()", "0"),
            Prim::U32 => ("value.toUInt()", "value.toInt()", "0"),
            Prim::U64 => ("value.toULong()", "value.toLong()", "0"),
            Prim::I64 => ("value", "value", "0L"),
            Prim::F32 => ("value", "value", "0f"),
            Prim::F64 => ("value", "value", "0.0"),
            _ => ("value", "value", "0"),
        };
        w.blank();
        w.line(format!(
            "/** A C `{prefix}_opt_{}`: `has`, then the value (0 when absent). */",
            p.snake()
        ));
        w.line("@Structure.FieldOrder(\"has\", \"value\")");
        w.block(
            format!("internal open class {class} : Structure() {{"),
            "}",
            |w| {
                w.line("@JvmField");
                w.line("var has: Byte = 0");
                w.blank();
                w.line("@JvmField");
                w.line(format!("var value: {jna} = {zero}"));
                w.blank();
                w.line("/** The value, or `null` when absent. */");
                w.line(format!(
                    "fun get(): {public}? = if (has.asBoolean()) {lift} else null"
                ));
                w.blank();
                w.line("/** The structure by value. */");
                w.line(format!("class ByValue : {class}(), Structure.ByValue"));
            },
        );
        w.blank();
        w.line(format!("/** [value] as a `{prefix}_opt_{}`. */", p.snake()));
        w.block(
            format!(
                "internal fun {}(value: {public}?): {class}.ByValue = {class}.ByValue().also {{",
                opt_fn(p)
            ),
            "}",
            |w| {
                w.block("if (value != null) {", "}", |w| {
                    w.line("it.has = 1");
                    w.line(format!("it.value = {lower}"));
                });
                w.line("it.write()");
            },
        );
    }
    w.finish()
}
