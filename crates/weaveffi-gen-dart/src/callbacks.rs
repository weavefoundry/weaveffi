//! Callback-interface rendering: the abstract class a consumer implements,
//! the `dart:ffi` `Struct` mirroring the C vtable, one trampoline per method
//! (plus `free`), and the single process-wide vtable instance whose entries
//! are `NativeCallable` function pointers pinned for the process lifetime.
//!
//! Trampolines follow the [`CallbackProtocol`] clauses: borrowed strings,
//! bytes, and buffers are copied or decoded inside the call and never freed;
//! object arguments transfer one strong reference and are adopted into a
//! wrapper; direct returns are written straight into the C return; a thrown
//! Dart exception is reported through `{prefix}_error_set` with the foreign
//! error code and a default value is returned, so nothing ever unwinds through
//! the C frame.
//!
//! [`CallbackProtocol`]: weaveffi_core::plan::CallbackProtocol

use heck::ToUpperCamelCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{CallbackInterfaceBinding, CallbackMethodBinding, Ty};
use weaveffi_core::plan::ArgPass;

use crate::calls::adopt_expr;
use crate::codec::read_expr;
use crate::docs::emit_doc;
use crate::types::{
    dart_class, dart_ident, dart_str_literal, dart_type, default_literal, input_slots, scalar_ffi,
    vtable_var,
};

/// The Dart `Struct` subclass mirroring one callback interface's C vtable.
fn vtable_struct(class: &str) -> String {
    format!("_{class}VtableStruct")
}

/// The native function typedef of one vtable entry.
fn entry_typedef(class: &str, method: &str) -> String {
    format!("_Native{class}Vt{}", method.to_upper_camel_case())
}

/// The Dart trampoline function bound into one vtable entry.
fn trampoline_fn(class: &str, method: &str) -> String {
    format!("_{}Vt{}", lower_first(class), method.to_upper_camel_case())
}

/// Lower-case the first character of a PascalCase class name.
fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The (native, dart) return types of one vtable entry; a void method is
/// `(Void, void)`, everything else is a direct scalar.
fn entry_ret(m: &CallbackMethodBinding) -> (String, String) {
    match &m.ret {
        None => ("Void".into(), "void".into()),
        Some(ty) => {
            let (n, d) = scalar_ffi(ty);
            (n.into(), d.into())
        }
    }
}

/// The `(native type, dart type, dart name)` slot triples of one vtable
/// entry, in ABI order: `ctx`, every parameter's slots, then `out_err`.
fn entry_slots(m: &CallbackMethodBinding) -> Vec<(String, String, String)> {
    let mut slots = vec![(
        "Pointer<Void>".to_string(),
        "Pointer<Void>".to_string(),
        "ctx".to_string(),
    )];
    for p in &m.params {
        for ((n, d), slot) in input_slots(p).into_iter().zip(p.abi.iter()) {
            slots.push((n, d, dart_ident(&slot.name)));
        }
    }
    slots.push((
        "Pointer<_WeaveFFIError>".into(),
        "Pointer<_WeaveFFIError>".into(),
        "outErr".into(),
    ));
    slots
}

/// Render one callback interface: the consumer-facing abstract class, the
/// vtable `Struct`, the per-method trampolines and `free`, and the one static
/// vtable instance.
pub(crate) fn render_callback_interface(out: &mut String, cb: &CallbackInterfaceBinding) {
    let class = dart_class(&cb.name);
    render_abstract_class(out, cb, &class);
    render_vtable_struct(out, cb, &class);
    for m in &cb.methods {
        render_trampoline(out, cb, m, &class);
    }
    render_free(out, &class);
    render_vtable_instance(out, cb, &class);
}

/// The abstract class a consumer extends or implements, one abstract method
/// per vtable entry with idiomatic Dart parameter and return types.
fn render_abstract_class(out: &mut String, cb: &CallbackInterfaceBinding, class: &str) {
    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &cb.doc, "");
        w.raw(d);
    }
    if cb.doc.is_some() {
        w.line("///");
    }
    w.line("/// A callback interface: implement this class and pass an instance to any");
    w.line(format!(
        "/// function taking a [{class}]. The producer keeps the instance alive"
    ));
    w.line("/// (through a private handle table) until it releases the callback, so no");
    w.line("/// other reference needs to be held.");
    w.line("///");
    w.line("/// Object arguments are owned by the implementation: call `dispose()` on");
    w.line("/// them (or let the finalizer run) when done. A thrown exception is reported");
    w.line("/// to the producer, which aborts the call it was making; the original Dart");
    w.line("/// caller then observes a [WeaveFFIException] with [WeaveFFIException.foreignCode]");
    w.line("/// carrying the exception's text.");
    w.line("///");
    w.line("/// Dart limitation: methods are bound with `NativeCallable.isolateLocal`, so");
    w.line("/// the producer may only invoke them on the thread of the isolate that");
    w.line("/// passed the callback. That holds when the producer calls them");
    w.line("/// synchronously during a call from Dart. A producer that invokes a method");
    w.line("/// from another thread (an async task, a background worker) is unsupported:");
    w.line("/// a `NativeCallable.listener` cannot return a value or read borrowed");
    w.line("/// arguments after the native frame returns.");
    if let Some(msg) = &cb.deprecated {
        w.line(format!("@Deprecated('{}')", dart_str_literal(msg)));
    }
    w.block(format!("abstract class {class} {{"), "}", |w| {
        for m in &cb.methods {
            {
                let mut d = String::new();
                emit_doc(&mut d, &m.doc, "  ");
                w.raw(d);
            }
            if let Some(msg) = &m.deprecated {
                w.line(format!("@Deprecated('{}')", dart_str_literal(msg)));
            }
            let params: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{} {}", dart_type(&p.ty), dart_ident(&p.name)))
                .collect();
            let ret = m.ret.as_ref().map_or("void".to_string(), dart_type);
            w.line(format!(
                "{ret} {}({});",
                dart_ident(&m.name),
                params.join(", ")
            ));
        }
    });
    out.push_str(&w.finish());
}

/// The `Struct` whose layout is the C vtable: one function pointer per
/// method in declaration order, then the trailing `free`.
fn render_vtable_struct(out: &mut String, cb: &CallbackInterfaceBinding, class: &str) {
    let mut w = CodeWriter::two_space();
    w.blank();
    for m in &cb.methods {
        let (native_ret, _) = entry_ret(m);
        let natives: Vec<String> = entry_slots(m).into_iter().map(|(n, _, _)| n).collect();
        w.line(format!(
            "typedef {} = {native_ret} Function({});",
            entry_typedef(class, &m.name),
            natives.join(", ")
        ));
    }
    w.line(format!(
        "typedef {} = Void Function(Pointer<Void>);",
        entry_typedef(class, "free")
    ));
    w.blank();
    w.line(format!("// The C `{}` layout.", cb.vtable_tag));
    w.block(
        format!("final class {} extends Struct {{", vtable_struct(class)),
        "}",
        |w| {
            for m in &cb.methods {
                w.line(format!(
                    "external Pointer<NativeFunction<{}>> {};",
                    entry_typedef(class, &m.name),
                    dart_ident(&m.name)
                ));
            }
            w.line(format!(
                "external Pointer<NativeFunction<{}>> free;",
                entry_typedef(class, "free")
            ));
        },
    );
    out.push_str(&w.finish());
}

/// Emit the statements converting one method's borrowed slots into the values
/// handed to the implementation, returning the argument expressions. Every
/// conversion happens inside the call (the borrow window); nothing is freed.
fn emit_trampoline_args(w: &mut CodeWriter, m: &CallbackMethodBinding) -> Vec<String> {
    let mut args = Vec::new();
    for p in &m.params {
        let base = dart_ident(&p.name);
        let n0 = dart_ident(&p.abi[0].name);
        args.push(match p.arg_pass() {
            // Decode the borrowed encoding now; object tokens inside it are
            // adopted by the wrappers the codec constructs.
            ArgPass::Buffer { len, .. } => {
                let n1 = dart_ident(&len.name);
                w.line(format!("final {base}Data = _copyNativeBytes({n0}, {n1});"));
                w.line(format!("final {base}Reader = _BufferReader({base}Data);"));
                w.line(format!(
                    "final {base}Value = {};",
                    read_expr(&format!("{base}Reader"), &p.ty)
                ));
                w.line(format!("{base}Reader.expectEnd();"));
                format!("{base}Value")
            }
            ArgPass::String { .. } => {
                format!("{n0} == nullptr ? '' : {n0}.toDartString()")
            }
            ArgPass::Bytes { len, .. } => {
                let n1 = dart_ident(&len.name);
                format!("{n0} == nullptr ? <int>[] : {n0}.asTypedList({n1}).toList()")
            }
            // One strong reference transfers to the implementation: adopt it
            // into a local first, so a later failure (a bad `ctx`, a malformed
            // buffer) still leaves the reference owned by a finalizable
            // wrapper instead of leaking it.
            ArgPass::Object { nullable, .. } => {
                w.line(format!(
                    "final {base}Value = {};",
                    adopt_expr(&n0, &p.ty, nullable)
                ));
                format!("{base}Value")
            }
            ArgPass::Callback { .. } => {
                unreachable!("validation rejects callback interfaces as callback-method parameters")
            }
            ArgPass::Direct { .. } => match &p.ty {
                Ty::Enum(name) => format!("{}.fromValue({n0})", dart_class(name)),
                _ => n0,
            },
        });
    }
    args
}

/// One method trampoline: look the implementation up by `ctx`, convert the
/// arguments, call the method, and write a direct return. Any exception is
/// reported through `_foreignError` (`{prefix}_error_set` with the foreign
/// code) and a default value is returned instead of unwinding.
fn render_trampoline(
    out: &mut String,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    class: &str,
) {
    let (_, dart_ret) = entry_ret(m);
    let decls: Vec<String> = entry_slots(m)
        .into_iter()
        .map(|(_, d, name)| format!("{d} {name}"))
        .collect();
    let method = dart_ident(&m.name);
    let default = m.ret.as_ref().map(default_literal);

    let mut w = CodeWriter::two_space();
    w.blank();
    w.line(format!("// Vtable entry `{}.{}`.", cb.vtable_tag, m.name));
    w.block(
        format!(
            "{dart_ret} {}({}) {{",
            trampoline_fn(class, &m.name),
            decls.join(", ")
        ),
        "}",
        |w| {
            w.line("try {");
            w.scope(|w| {
                // Arguments first: object arguments must be adopted before
                // anything that can throw.
                let args = emit_trampoline_args(w, m);
                w.line(format!("final impl = _callbackFor(ctx) as {class};"));
                let call = format!("impl.{method}({})", args.join(", "));
                match &m.ret {
                    None => w.line(format!("{call};")),
                    Some(Ty::Enum(_)) => w.line(format!("return {call}.value;")),
                    Some(_) => w.line(format!("return {call};")),
                };
            });
            w.line("} catch (e) {");
            w.scope(|w| {
                w.line("_foreignError(outErr, e);");
                if let Some(d) = default {
                    w.line(format!("return {d};"));
                }
            });
            w.line("}");
        },
    );
    out.push_str(&w.finish());
}

/// The `free(ctx)` trampoline: drop the handle-table entry. The producer never
/// touches `ctx` again after this fires. It's a `NativeCallable.listener`
/// because the producer may release its last reference from any thread (an
/// async completion, a background drop) and the removal can safely run
/// later on the isolate.
fn render_free(out: &mut String, class: &str) {
    let mut w = CodeWriter::two_space();
    w.blank();
    w.block(
        format!(
            "void {}(Pointer<Void> ctx) {{",
            trampoline_fn(class, "free")
        ),
        "}",
        |w| {
            w.line("_callbackTable.remove(ctx.address);");
        },
    );
    out.push_str(&w.finish());
}

/// The one static vtable for the interface: allocated once with `calloc`, its
/// entries `NativeCallable.isolateLocal` trampolines (and a `listener` for
/// `free`) pinned in `_callbackCallables` so their native thunks live for the
/// process lifetime.
fn render_vtable_instance(out: &mut String, cb: &CallbackInterfaceBinding, class: &str) {
    let strukt = vtable_struct(class);
    let mut w = CodeWriter::two_space();
    w.blank();
    w.line(format!(
        "// The process-wide static `{}` every {class} passed to the producer",
        cb.vtable_tag
    ));
    w.line("// shares; the per-instance state travels in `ctx`.");
    w.block(
        format!("final Pointer<{strukt}> {} = () {{", vtable_var(&cb.name)),
        "}();",
        |w| {
            w.line(format!("final vt = calloc<{strukt}>();"));
            for m in &cb.methods {
                let td = entry_typedef(class, &m.name);
                let exceptional = match &m.ret {
                    None => String::new(),
                    Some(ty) => format!(", exceptionalReturn: {}", default_literal(ty)),
                };
                w.line(format!(
                    "vt.ref.{} = _pinCallable(NativeCallable<{td}>.isolateLocal(",
                    dart_ident(&m.name)
                ));
                w.line(format!(
                    "    {}{exceptional}));",
                    trampoline_fn(class, &m.name)
                ));
            }
            w.line(format!(
                "vt.ref.free = _pinCallable(NativeCallable<{}>.listener(",
                entry_typedef(class, "free")
            ));
            w.line(format!("    {}));", trampoline_fn(class, "free")));
            w.line("return vt;");
        },
    );
    out.push_str(&w.finish());
}
