//! Kotlin declarations for one top-level module: enums, records, rich
//! enums, callback interfaces, interface wrapper classes, error domains,
//! and the module object holding the free functions (nested modules become
//! nested objects).

use crate::codegen::common::pascal_case;
use crate::codegen::CodeWriter;
use weaveffi_model::errors;
use weaveffi_model::model::{
    BindingModel, CallbackInterfaceBinding, EnumBinding, ErrorBinding, FieldBinding,
    InterfaceBinding, ModuleBinding, StructBinding,
};

use crate::targets::kotlin::calls::{deprecated_line, emit_callable};
use crate::targets::kotlin::codec::{read_expr, write_expr};
use crate::targets::kotlin::docs::{camel_params, writer_doc, writer_fn_doc};
use crate::targets::kotlin::names::{kt_escape, kt_member, kt_param, Names};

/// Emit `@Deprecated` for a deprecated declaration.
fn deprecated(w: &mut CodeWriter, msg: &Option<String>) {
    if let Some(msg) = msg {
        w.line(deprecated_line(msg));
    }
}

/// The `val name: Type` constructor properties of a record or variant.
fn field_decls(n: &Names, fields: &[FieldBinding]) -> Vec<String> {
    fields
        .iter()
        .map(|f| format!("val {}: {}", kt_escape(&f.name), n.kt_type(&f.ty)))
        .collect()
}

/// Emit a class header with constructor properties: on one line when no
/// field is documented, one property per line (each with its KDoc)
/// otherwise. `head` is everything before `(`, `tail` everything after `)`.
fn emit_properties(w: &mut CodeWriter, n: &Names, head: &str, fields: &[FieldBinding], tail: &str) {
    if fields.iter().any(|f| f.doc.is_some()) {
        w.line(format!("{head}("));
        w.scope(|w| {
            for (f, decl) in fields.iter().zip(field_decls(n, fields)) {
                writer_doc(w, &f.doc);
                w.line(format!("{decl},"));
            }
        });
        w.line(format!("){tail}"));
    } else {
        w.line(format!(
            "{head}({}){tail}",
            field_decls(n, fields).join(", ")
        ));
    }
}

/// A C-style enum: an `enum class` carrying its ABI value.
fn render_enum(w: &mut CodeWriter, n: &Names, e: &EnumBinding) {
    let name = n.ty(&e.name);
    w.blank();
    writer_doc(w, &e.doc);
    deprecated(w, &e.deprecated);
    w.line(format!("enum class {name}(val value: Int) {{"));
    w.scope(|w| {
        for (i, v) in e.variants.iter().enumerate() {
            writer_doc(w, &v.doc);
            let end = if i + 1 == e.variants.len() { ";" } else { "," };
            w.line(format!("{}({}){end}", kt_escape(&v.name), v.value));
        }
        w.blank();
        w.line("companion object {");
        w.scope(|w| {
            w.line("/** The variant whose ABI value is [value]. */");
            w.line(format!(
                "fun fromValue(value: Int): {name} = entries.firstOrNull {{ it.value == value }}"
            ));
            w.line(format!(
                "    ?: throw FfiException(-3, \"unknown {name} value $value\")"
            ));
        });
        w.line("}");
    });
    w.line("}");
}

/// A rich enum: a sealed class with one subclass per variant, plus its
/// buffer codec (an `i32` tag, then the variant's fields).
fn render_rich_enum(w: &mut CodeWriter, n: &Names, e: &EnumBinding) {
    let name = n.ty(&e.name);
    w.blank();
    writer_doc(w, &e.doc);
    deprecated(w, &e.deprecated);
    w.line(format!("sealed class {name} {{"));
    w.scope(|w| {
        for v in &e.variants {
            writer_doc(w, &v.doc);
            let vn = kt_escape(&pascal_case(&v.name));
            if v.fields.is_empty() {
                w.line(format!("object {vn} : {name}()"));
            } else {
                emit_properties(
                    w,
                    n,
                    &format!("data class {vn}"),
                    &v.fields,
                    &format!(" : {name}()"),
                );
            }
        }
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "internal fun pack{name}(_w: BufferWriter, _v: {name}) {{"
    ));
    w.scope(|w| {
        w.line("when (_v) {");
        w.scope(|w| {
            for v in &e.variants {
                let vn = kt_escape(&pascal_case(&v.name));
                if v.fields.is_empty() {
                    w.line(format!("is {name}.{vn} -> _w.writeI32({})", v.value));
                } else {
                    w.line(format!("is {name}.{vn} -> {{"));
                    w.scope(|w| {
                        w.line(format!("_w.writeI32({})", v.value));
                        for f in &v.fields {
                            let expr = format!("_v.{}", kt_escape(&f.name));
                            w.line(write_expr(n, &f.ty, "_w", &expr, 0));
                        }
                    });
                    w.line("}");
                }
            }
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "internal fun unpack{name}(_r: BufferReader): {name} = when (val _tag = _r.readI32()) {{"
    ));
    w.scope(|w| {
        for v in &e.variants {
            let vn = kt_escape(&pascal_case(&v.name));
            if v.fields.is_empty() {
                w.line(format!("{} -> {name}.{vn}", v.value));
            } else {
                let reads: Vec<String> =
                    v.fields.iter().map(|f| read_expr(n, &f.ty, "_r")).collect();
                w.line(format!("{} -> {name}.{vn}({})", v.value, reads.join(", ")));
            }
        }
        w.line(format!(
            "else -> throw FfiException(-3, \"malformed value buffer: unknown {name} tag $_tag\")"
        ));
    });
    w.line("}");
}

/// A record: a `data class` (a plain `class` when it has no fields), plus
/// its buffer codec (the fields in declaration order).
fn render_struct(w: &mut CodeWriter, n: &Names, s: &StructBinding) {
    let name = n.ty(&s.name);
    w.blank();
    writer_doc(w, &s.doc);
    deprecated(w, &s.deprecated);
    if s.fields.is_empty() {
        w.line(format!("class {name}"));
        w.blank();
        w.line(format!(
            "internal fun pack{name}(_w: BufferWriter, _v: {name}) {{}}"
        ));
        w.blank();
        w.line(format!(
            "internal fun unpack{name}(_r: BufferReader): {name} = {name}()"
        ));
        return;
    }
    emit_properties(w, n, &format!("data class {name}"), &s.fields, "");
    w.blank();
    w.line(format!(
        "internal fun pack{name}(_w: BufferWriter, _v: {name}) {{"
    ));
    w.scope(|w| {
        for f in &s.fields {
            let expr = format!("_v.{}", kt_escape(&f.name));
            w.line(write_expr(n, &f.ty, "_w", &expr, 0));
        }
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "internal fun unpack{name}(_r: BufferReader): {name} = {name}("
    ));
    w.scope(|w| {
        for f in &s.fields {
            w.line(format!("{},", read_expr(n, &f.ty, "_r")));
        }
    });
    w.line(")");
}

/// A callback interface: the Kotlin `interface` the consumer implements.
/// Its JNI dispatch shims live on `JniBridge`.
fn render_callback_interface(w: &mut CodeWriter, n: &Names, cb: &CallbackInterfaceBinding) {
    w.blank();
    writer_doc(w, &cb.doc);
    deprecated(w, &cb.deprecated);
    w.line(format!("interface {} {{", n.ty(&cb.name)));
    w.scope(|w| {
        for (i, m) in cb.methods.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            writer_fn_doc(w, &m.doc, &camel_params(&m.params));
            deprecated(w, &m.deprecated);
            let params: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{}: {}", kt_param(&p.name), n.kt_type(&p.ty)))
                .collect();
            let ret = m
                .ret
                .as_ref()
                .map(|t| format!(": {}", n.kt_type(t)))
                .unwrap_or_default();
            w.line(format!(
                "fun {}({}){ret}",
                kt_member(&m.name),
                params.join(", ")
            ));
        }
    });
    w.line("}");
}

/// An interface: an `AutoCloseable` wrapper owning one strong reference.
/// Every call borrows the reference (so neither `close()` nor the cleaner
/// can release it mid-call); constructors and statics live on the
/// companion, with the `new` constructor as `operator fun invoke`.
fn render_interface(
    w: &mut CodeWriter,
    n: &Names,
    i: &InterfaceBinding,
    error: Option<&ErrorBinding>,
) {
    let name = n.ty(&i.name);
    let destroy = n.native(&i.destroy_symbol);
    let clone = n.native(&i.clone_symbol);
    w.blank();
    writer_doc(w, &i.doc);
    deprecated(w, &i.deprecated);
    // The address constructor is private so a `new` constructor taking one
    // `Long` (exposed as `invoke`) is never shadowed by it.
    w.line(format!(
        "class {name} private constructor(address: Long) : AutoCloseable {{"
    ));
    w.scope(|w| {
        w.line(format!(
            "internal val handle: NativeHandle = NativeCleaner.register(this, NativeHandle(address, JniBridge::{destroy}))"
        ));
        w.blank();
        w.line("/** A new strong reference to this object, as written into a value buffer. */");
        w.line(format!(
            "internal fun cloneHandle(): Long = handle.borrow {{ JniBridge.{clone}(it) }}"
        ));
        for f in &i.methods {
            w.blank();
            writer_fn_doc(w, &f.doc, &camel_params(&f.params));
            emit_callable(w, n, f, &format!("fun {}", kt_member(&f.name)), true, error);
        }
        w.blank();
        w.line("/** Releases this wrapper's native reference; safe to call more than once. */");
        w.line("override fun close() = handle.close()");
        w.blank();
        w.line("companion object {");
        w.scope(|w| {
            w.line("/** Adopts one strong reference (a return value or a buffer token). */");
            w.line(format!("internal fun fromHandle(address: Long): {name} = {name}(address)"));
            w.blank();
            w.line("/** [fromHandle], with address 0 meaning no object. */");
            w.line(format!(
                "internal fun fromHandleOrNull(address: Long): {name}? = if (address == 0L) null else {name}(address)"
            ));
            for c in &i.constructors {
                w.blank();
                writer_fn_doc(w, &c.doc, &camel_params(&c.params));
                let decl = if c.name == "new" {
                    "operator fun invoke".to_string()
                } else {
                    format!("fun {}", kt_member(&c.name))
                };
                emit_callable(w, n, c, &decl, false, error);
            }
            for f in &i.statics {
                w.blank();
                writer_fn_doc(w, &f.doc, &camel_params(&f.params));
                emit_callable(w, n, f, &format!("fun {}", kt_member(&f.name)), false, error);
            }
        });
        w.line("}");
    });
    w.line("}");
}

/// An error domain: a sealed exception class with one subclass per code
/// (payload fields as constructor properties) and the `fromCode` factory
/// the bridge maps raw codes through. Unknown codes, including every
/// runtime (negative) code, map to the generic `FfiException`.
fn render_error_domain(w: &mut CodeWriter, n: &Names, eb: &ErrorBinding) {
    let exc = n.exception(eb);
    w.blank();
    w.line(format!("/** Errors of the `{}` domain. */", eb.name));
    w.line(format!(
        "sealed class {exc}(code: Int, message: String) : FfiException(code, message) {{"
    ));
    w.scope(|w| {
        for ec in &eb.codes {
            writer_doc(w, &ec.doc);
            let class = errors::pascal(&ec.name);
            let default = ec.message.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$");
            let mut params = vec![format!("message: String = \"{default}\"")];
            params.extend(field_decls(n, &ec.fields));
            w.line(format!(
                "class {class}({}) : {exc}({}, message)",
                params.join(", "),
                ec.value
            ));
        }
        w.blank();
        w.line("internal companion object {");
        w.scope(|w| {
            w.line("/** The exception for a raw code and its serialized payload. */");
            w.line("fun fromCode(code: Int, message: String, payload: ByteArray?): FfiException = when (code) {");
            w.scope(|w| {
                for ec in &eb.codes {
                    let class = errors::pascal(&ec.name);
                    if ec.fields.is_empty() {
                        w.line(format!("{} -> {class}(message)", ec.value));
                    } else {
                        let reads: Vec<String> =
                            ec.fields.iter().map(|f| read_expr(n, &f.ty, "_r")).collect();
                        w.line(format!(
                            "{} -> if (payload == null) FfiException(code, message) else decodeBuffer(payload) {{ _r -> {class}(message, {}) }}",
                            ec.value,
                            reads.join(", ")
                        ));
                    }
                }
                w.line("else -> FfiException(code, message)");
            });
            w.line("}");
        });
        w.line("}");
    });
    w.line("}");
}

/// Whether `m` or any module below it declares a free function.
fn has_functions(model: &BindingModel, m: &ModuleBinding) -> bool {
    !m.functions.is_empty() || model.children(m).any(|c| has_functions(model, c))
}

/// The module object for `m`: its free functions, then one nested object
/// per submodule that has functions of its own.
fn render_module_object(w: &mut CodeWriter, n: &Names, model: &BindingModel, m: &ModuleBinding) {
    // The model falls back to a function's doc for an undocumented module;
    // that reads wrong on the object, so only a module's own doc is kept.
    if !m.functions.iter().any(|f| f.doc == m.doc) {
        writer_doc(w, &m.doc);
    }
    w.line(format!("object {} {{", n.object(m)));
    w.scope(|w| {
        let mut first = true;
        for f in &m.functions {
            if !first {
                w.blank();
            }
            first = false;
            writer_fn_doc(w, &f.doc, &camel_params(&f.params));
            emit_callable(
                w,
                n,
                f,
                &format!("fun {}", n.function(m, f)),
                false,
                m.error.as_ref(),
            );
        }
        for child in model.children(m).filter(|c| has_functions(model, c)) {
            if !first {
                w.blank();
            }
            first = false;
            render_module_object(w, n, model, child);
        }
    });
    w.line("}");
}

/// Render every declaration of the top-level module `root` and its
/// submodules (the body of `{Root}.kt`, after the package line).
pub(crate) fn render_root(n: &Names, model: &BindingModel, root: &ModuleBinding) -> String {
    let mut w = CodeWriter::four_space();
    let tree = model
        .modules
        .iter()
        .filter(|m| m.segments.first() == root.segments.first());
    for m in tree {
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_error_domain(&mut w, n, eb);
        }
        for e in &m.enums {
            if e.is_rich() {
                render_rich_enum(&mut w, n, e);
            } else {
                render_enum(&mut w, n, e);
            }
        }
        for s in &m.structs {
            render_struct(&mut w, n, s);
        }
        for cb in &m.callback_interfaces {
            render_callback_interface(&mut w, n, cb);
        }
        for i in &m.interfaces {
            render_interface(&mut w, n, i, m.error.as_ref());
        }
    }
    if has_functions(model, root) {
        w.blank();
        render_module_object(&mut w, n, model, root);
    }
    w.finish()
}
