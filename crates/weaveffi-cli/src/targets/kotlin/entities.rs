//! Kotlin declarations for one top-level module: error domains, enums,
//! records, rich enums, callback interfaces, interface wrapper classes, and
//! the module object holding the free functions (nested modules become
//! nested objects).

use crate::codegen::common::pascal_case;
use crate::codegen::CodeWriter;
use weaveffi_model::errors;
use weaveffi_model::model::{
    CallbackInterfaceBinding, EnumBinding, ErrorBinding, FieldBinding, InterfaceBinding, Model,
    ModuleBinding, StructBinding,
};
use weaveffi_model::plan::ErrorStrategy;
use weaveffi_model::ty::{Prim, Ty};

use crate::targets::kotlin::calls::{deprecated_line, emit_callable, kt_string, Decl};
use crate::targets::kotlin::codec::{read_expr, write_expr};
use crate::targets::kotlin::docs::Speller;
use crate::targets::kotlin::names::{kt_escape, kt_fields, kt_member, kt_param, Names};

/// Emit `@Deprecated` for a deprecated declaration.
fn deprecated(w: &mut CodeWriter, sp: &Speller, msg: &Option<String>) {
    if let Some(msg) = sp.deprecation(msg) {
        w.line(deprecated_line(&msg));
    }
}

/// One constructor property of a record, variant, or error code: its Kotlin
/// name and type, and the IR field.
struct Prop<'a> {
    name: String,
    ty: String,
    field: &'a FieldBinding,
}

/// The constructor properties of `fields`, named for Kotlin (unique, and
/// away from `Throwable`'s members for an error code).
fn props<'a>(n: &Names, fields: &'a [FieldBinding], error_payload: bool) -> Vec<Prop<'a>> {
    kt_fields(fields.iter().map(|f| f.name.as_str()), error_payload)
        .into_iter()
        .zip(fields)
        .map(|(name, field)| Prop {
            name,
            ty: n.kt_type(&field.ty),
            field,
        })
        .collect()
}

/// Emit a class header with constructor properties: on one line when no
/// field is documented, one property per line (each with its KDoc)
/// otherwise. `head` is everything before `(`, `tail` everything after `)`.
fn emit_properties(w: &mut CodeWriter, sp: &Speller, head: &str, props: &[Prop], tail: &str) {
    if props.iter().any(|p| p.field.doc.is_some()) {
        w.line(format!("{head}("));
        w.scope(|w| {
            for p in props {
                sp.doc(w, &p.field.doc);
                w.line(format!("val {}: {},", p.name, p.ty));
            }
        });
        w.line(format!("){tail}"));
    } else {
        let decls: Vec<String> = props
            .iter()
            .map(|p| format!("val {}: {}", p.name, p.ty))
            .collect();
        w.line(format!("{head}({}){tail}", decls.join(", ")));
    }
}

/// How a field compares: by `==` unless its type holds a `ByteArray`,
/// whose `equals` is identity.
enum Compare {
    /// No array inside: the data class default.
    Value,
    /// A `bytes` or `bytes?` field: the stdlib `content*` functions.
    Array,
    /// Arrays nested in a list, map, or optional: the runtime's `deep*`
    /// helpers.
    Deep,
}

fn compare(t: &Ty) -> Compare {
    let bytes = |t: &Ty| matches!(t, Ty::Prim(Prim::Bytes));
    match t {
        _ if bytes(t) => Compare::Array,
        Ty::Optional(inner) if bytes(inner) => Compare::Array,
        _ if t.any(&bytes) => Compare::Deep,
        _ => Compare::Value,
    }
}

/// Emit a data class (`head` is `data class Name`, `tail` its supertype),
/// overriding `equals`, `hashCode`, and `toString` with content semantics
/// when a field holds a `ByteArray` (whose own `equals` is identity).
fn emit_data_class(
    w: &mut CodeWriter,
    sp: &Speller,
    class: &str,
    head: &str,
    props: &[Prop],
    tail: &str,
) {
    if props
        .iter()
        .all(|p| matches!(compare(&p.field.ty), Compare::Value))
    {
        emit_properties(w, sp, head, props, tail);
        return;
    }
    emit_properties(w, sp, head, props, &format!("{tail} {{"));
    w.scope(|w| {
        w.line("override fun equals(other: Any?): Boolean {");
        w.scope(|w| {
            w.line("if (this === other) return true");
            w.line(format!("if (other !is {class}) return false"));
            let terms: Vec<String> = props
                .iter()
                .map(|p| {
                    let x = &p.name;
                    match compare(&p.field.ty) {
                        Compare::Value => format!("{x} == other.{x}"),
                        Compare::Array => format!("{x}.contentEquals(other.{x})"),
                        Compare::Deep => format!("deepEquals({x}, other.{x})"),
                    }
                })
                .collect();
            for (i, t) in terms.iter().enumerate() {
                match (i, i + 1 == terms.len()) {
                    (0, true) => w.line(format!("return {t}")),
                    (0, false) => w.line(format!("return {t} &&")),
                    (_, last) => {
                        w.indent();
                        w.line(if last { t.clone() } else { format!("{t} &&") });
                        w.dedent()
                    }
                };
            }
        });
        w.line("}");
        w.blank();
        w.line("override fun hashCode(): Int {");
        w.scope(|w| {
            for (i, p) in props.iter().enumerate() {
                let x = &p.name;
                let h = match compare(&p.field.ty) {
                    Compare::Value => format!("{x}.hashCode()"),
                    Compare::Array => format!("{x}.contentHashCode()"),
                    Compare::Deep => format!("deepHashCode({x})"),
                };
                if i == 0 {
                    w.line(format!("var result = {h}"));
                } else {
                    w.line(format!("result = 31 * result + {h}"));
                }
            }
            w.line("return result");
        });
        w.line("}");
        w.blank();
        let parts: Vec<String> = props
            .iter()
            .map(|p| {
                let x = &p.name;
                match compare(&p.field.ty) {
                    Compare::Value => format!("{x}=${{{x}}}"),
                    Compare::Array => format!("{x}=${{{x}.contentToString()}}"),
                    Compare::Deep => format!("{x}=${{deepToString({x})}}"),
                }
            })
            .collect();
        let simple = class.rsplit('.').next().unwrap_or(class);
        w.line(format!(
            "override fun toString(): String = \"{simple}({})\"",
            parts.join(", ")
        ));
    });
    w.line("}");
}

/// A C-style enum: an `enum class` carrying its ABI value.
fn render_enum(w: &mut CodeWriter, n: &Names, sp: &Speller, e: &EnumBinding) {
    let name = n.ty(&e.name);
    w.blank();
    sp.doc(w, &e.doc);
    deprecated(w, sp, &e.deprecated);
    w.line(format!("enum class {name}(val value: Int) {{"));
    w.scope(|w| {
        for (i, v) in e.variants.iter().enumerate() {
            sp.doc(w, &v.doc);
            let end = if i + 1 == e.variants.len() { ";" } else { "," };
            w.line(format!("{}({}){end}", kt_escape(&v.name), v.value));
        }
        w.blank();
        w.line("companion object {");
        w.scope(|w| {
            w.line("/** The variant whose ABI value is [value]. */");
            w.line("@JvmStatic");
            w.line(format!(
                "fun fromValue(value: Int): {name} = entries.firstOrNull {{ it.value == value }}"
            ));
            w.line(format!(
                "    ?: throw NativeBugException(-3, \"unknown {name} value $value\")"
            ));
        });
        w.line("}");
    });
    w.line("}");
}

/// A rich enum: a sealed class with one subclass per variant, plus its
/// buffer codec (an `i32` tag, then the variant's fields).
fn render_rich_enum(w: &mut CodeWriter, n: &Names, sp: &Speller, e: &EnumBinding) {
    let name = n.ty(&e.name);
    let variant = |v: &str| kt_escape(&pascal_case(v));
    w.blank();
    sp.doc(w, &e.doc);
    deprecated(w, sp, &e.deprecated);
    w.line(format!("sealed class {name} {{"));
    w.scope(|w| {
        for v in &e.variants {
            sp.doc(w, &v.doc);
            let vn = variant(&v.name);
            if v.fields.is_empty() {
                w.line(format!("data object {vn} : {name}()"));
            } else {
                let props = props(n, &v.fields, false);
                emit_data_class(
                    w,
                    sp,
                    &format!("{name}.{vn}"),
                    &format!("data class {vn}"),
                    &props,
                    &format!(" : {name}()"),
                );
            }
        }
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "internal fun pack_{}(_w: BufferWriter, _v: {name}) {{",
        e.name
    ));
    w.scope(|w| {
        w.line("when (_v) {");
        w.scope(|w| {
            for v in &e.variants {
                let vn = variant(&v.name);
                if v.fields.is_empty() {
                    w.line(format!("is {name}.{vn} -> _w.writeI32({})", v.value));
                } else {
                    w.line(format!("is {name}.{vn} -> {{"));
                    w.scope(|w| {
                        w.line(format!("_w.writeI32({})", v.value));
                        for p in props(n, &v.fields, false) {
                            let expr = format!("_v.{}", p.name);
                            w.line(write_expr(n, &p.field.ty, "_w", &expr));
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
        "internal fun unpack_{}(_r: BufferReader): {name} = when (val _tag = _r.readI32()) {{",
        e.name
    ));
    w.scope(|w| {
        for v in &e.variants {
            let vn = variant(&v.name);
            if v.fields.is_empty() {
                w.line(format!("{} -> {name}.{vn}", v.value));
            } else {
                let reads: Vec<String> =
                    v.fields.iter().map(|f| read_expr(n, &f.ty, "_r")).collect();
                w.line(format!("{} -> {name}.{vn}({})", v.value, reads.join(", ")));
            }
        }
        w.line(format!(
            "else -> throw NativeBugException(-3, \"malformed value buffer: unknown {name} tag $_tag\")"
        ));
    });
    w.line("}");
}

/// A record: a `data class` (a plain `class` when it has no fields), plus
/// its buffer codec (the fields in declaration order).
fn render_struct(w: &mut CodeWriter, n: &Names, sp: &Speller, s: &StructBinding) {
    let name = n.ty(&s.name);
    let stem = &s.name;
    w.blank();
    sp.doc(w, &s.doc);
    deprecated(w, sp, &s.deprecated);
    if s.fields.is_empty() {
        w.line(format!("class {name} {{"));
        w.scope(|w| {
            w.line(format!(
                "override fun equals(other: Any?): Boolean = other is {name}"
            ));
            w.blank();
            w.line("override fun hashCode(): Int = 0");
            w.blank();
            w.line(format!("override fun toString(): String = \"{name}()\""));
        });
        w.line("}");
        w.blank();
        w.line(format!(
            "internal fun pack_{stem}(_w: BufferWriter, _v: {name}) {{}}"
        ));
        w.blank();
        w.line(format!(
            "internal fun unpack_{stem}(_r: BufferReader): {name} = {name}()"
        ));
        return;
    }
    let props = props(n, &s.fields, false);
    emit_data_class(w, sp, &name, &format!("data class {name}"), &props, "");
    w.blank();
    w.line(format!(
        "internal fun pack_{stem}(_w: BufferWriter, _v: {name}) {{"
    ));
    w.scope(|w| {
        for p in &props {
            let expr = format!("_v.{}", p.name);
            w.line(write_expr(n, &p.field.ty, "_w", &expr));
        }
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "internal fun unpack_{stem}(_r: BufferReader): {name} = {name}("
    ));
    w.scope(|w| {
        for f in &s.fields {
            w.line(format!("{},", read_expr(n, &f.ty, "_r")));
        }
    });
    w.line(")");
}

/// A callback interface: the Kotlin `interface` the consumer implements (a
/// `fun interface` when it has one method, so a lambda can implement it).
/// Its JNI dispatch shims live on `JniBridge`. A method that throws a
/// domain may throw that domain's exceptions, which reach the producer
/// with their code and fields; anything else it throws reaches the
/// producer as a failure with its message.
fn render_callback_interface(
    w: &mut CodeWriter,
    n: &Names,
    sp: &Speller,
    cb: &CallbackInterfaceBinding,
) {
    w.blank();
    sp.doc(w, &cb.doc);
    deprecated(w, sp, &cb.deprecated);
    let kind = if cb.methods.len() == 1 {
        "fun interface"
    } else {
        "interface"
    };
    w.line(format!("{kind} {} {{", n.ty(&cb.name)));
    w.scope(|w| {
        for (i, m) in cb.methods.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            let note = match &m.error {
                ErrorStrategy::Domain(d) => Some(format!(
                    "Throw a [{}] to report a typed error to the library; anything else\nthrown reaches it as an untyped failure with its message.",
                    n.exception(d)
                )),
                ErrorStrategy::Untyped => Some(
                    "Anything thrown reaches the library as a failure with its message."
                        .to_string(),
                ),
                ErrorStrategy::Trap => None,
            };
            let doc = match (sp.text(&m.doc), note) {
                (Some(d), Some(note)) => Some(format!("{d}\n\n{note}")),
                (d, note) => d.or(note),
            };
            sp.fn_doc(w, doc, m.params.iter().map(|p| (p.name.as_str(), &p.doc)));
            deprecated(w, sp, &m.deprecated);
            match &m.error {
                ErrorStrategy::Domain(d) => w.line(format!("@Throws({}::class)", n.exception(d))),
                ErrorStrategy::Untyped => w.line("@Throws(Exception::class)"),
                ErrorStrategy::Trap => w,
            };
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
/// companion (as `@JvmStatic` functions), with the `new` constructor as
/// `operator fun invoke`.
fn render_interface(w: &mut CodeWriter, n: &Names, sp: &Speller, i: &InterfaceBinding) {
    let name = n.ty(&i.name);
    let destroy = n.native(&i.destroy_symbol);
    let clone = n.native(&i.clone_symbol);
    w.blank();
    sp.doc(w, &i.doc);
    deprecated(w, sp, &i.deprecated);
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
            let name = kt_member(&f.name);
            emit_callable(
                w,
                n,
                sp,
                f,
                Decl {
                    name: &name,
                    operator: false,
                    jvm_static: false,
                },
            );
        }
        w.blank();
        w.line("/** Releases this wrapper's native reference; safe to call more than once. */");
        w.line("override fun close() = handle.close()");
        w.blank();
        w.line("companion object {");
        w.scope(|w| {
            w.line("/** Adopts one strong reference (a return value or a buffer token). */");
            w.line(format!(
                "internal fun fromHandle(address: Long): {name} = {name}(address)"
            ));
            w.blank();
            w.line("/** [fromHandle], with address 0 meaning no object. */");
            w.line(format!(
                "internal fun fromHandleOrNull(address: Long): {name}? = if (address == 0L) null else {name}(address)"
            ));
            for c in &i.constructors {
                w.blank();
                let (cname, operator) = if c.name == "new" {
                    ("invoke".to_string(), true)
                } else {
                    (kt_member(&c.name), false)
                };
                emit_callable(
                    w,
                    n,
                    sp,
                    c,
                    Decl {
                        name: &cname,
                        operator,
                        jvm_static: true,
                    },
                );
            }
            for f in &i.statics {
                w.blank();
                let name = kt_member(&f.name);
                emit_callable(
                    w,
                    n,
                    sp,
                    f,
                    Decl {
                        name: &name,
                        operator: false,
                        jvm_static: true,
                    },
                );
            }
        });
        w.line("}");
    });
    w.line("}");
}

/// An error domain: an exception class (constructible only by the
/// bindings) with one nested subclass per code (payload fields as
/// constructor properties, then the message with the documented default)
/// and the `fromCode` factory the bridge maps positive codes through.
/// Domains are open: a code these bindings don't know (from a newer
/// library) is the base class itself, with its code and message. A
/// subclass with fields encodes them for a callback that throws it.
fn render_error_domain(w: &mut CodeWriter, n: &Names, sp: &Speller, eb: &ErrorBinding) {
    let exc = n.exception(&eb.name);
    w.blank();
    w.line("/**");
    w.line(format!(
        " * Errors of the `{}` domain, one subclass per code.",
        eb.name
    ));
    w.line(" *");
    w.line(format!(
        " * A code these bindings don't declare (from a newer library) is a plain `{exc}`"
    ));
    w.line(" * carrying that [code] and the library's message.");
    w.line(" */");
    w.line(format!(
        "open class {exc} internal constructor(code: Int, message: String) : FfiException(code, message) {{"
    ));
    w.scope(|w| {
        for ec in &eb.codes {
            sp.doc(w, &ec.doc);
            let class = errors::pascal(&ec.name);
            let props = props(n, &ec.fields, true);
            let mut params: Vec<String> = props
                .iter()
                .map(|p| format!("val {}: {}", p.name, p.ty))
                .collect();
            params.push(format!("message: String = \"{}\"", kt_string(&ec.message)));
            let head = format!(
                "class {class}({}) : {exc}({}, message)",
                params.join(", "),
                ec.value
            );
            if ec.fields.is_empty() {
                w.line(head);
                continue;
            }
            w.block(format!("{head} {{"), "}", |w| {
                w.block(
                    "override fun encodePayload(): ByteArray = encodeBuffer { _w ->",
                    "}",
                    |w| {
                        for p in &props {
                            w.line(write_expr(n, &p.field.ty, "_w", &p.name));
                        }
                    },
                );
            });
        }
        w.blank();
        w.line("internal companion object {");
        w.scope(|w| {
            w.line("/** The exception for a positive code and its serialized payload. */");
            w.line(format!(
                "fun fromCode(code: Int, message: String, payload: ByteArray?): {exc} = when (code) {{"
            ));
            w.scope(|w| {
                for ec in &eb.codes {
                    let class = errors::pascal(&ec.name);
                    if ec.fields.is_empty() {
                        w.line(format!("{} -> {class}(message)", ec.value));
                    } else {
                        let reads: Vec<String> =
                            ec.fields.iter().map(|f| read_expr(n, &f.ty, "_r")).collect();
                        w.line(format!(
                            "{} -> if (payload == null) {exc}(code, message) else decodeBuffer(payload) {{ _r -> {class}({}, message) }}",
                            ec.value,
                            reads.join(", ")
                        ));
                    }
                }
                w.line(format!("else -> {exc}(code, message)"));
            });
            w.line("}");
        });
        w.line("}");
    });
    w.line("}");
}

/// Whether `m` or any module below it declares a free function.
fn has_functions(model: &Model, m: &ModuleBinding) -> bool {
    !m.functions.is_empty() || model.children(m).any(|c| has_functions(model, c))
}

/// The module object for `m`: its free functions (`@JvmStatic`, so Java
/// sees static methods), then one nested object per submodule that has
/// functions of its own.
fn render_module_object(
    w: &mut CodeWriter,
    n: &Names,
    sp: &Speller,
    model: &Model,
    m: &ModuleBinding,
) {
    sp.doc(w, &m.doc);
    w.line(format!("object {} {{", n.object(m)));
    w.scope(|w| {
        let mut first = true;
        for f in &m.functions {
            if !first {
                w.blank();
            }
            first = false;
            let name = n.function(f);
            emit_callable(
                w,
                n,
                sp,
                f,
                Decl {
                    name: &name,
                    operator: false,
                    jvm_static: true,
                },
            );
        }
        for child in model.children(m).filter(|c| has_functions(model, c)) {
            if !first {
                w.blank();
            }
            first = false;
            render_module_object(w, n, sp, model, child);
        }
    });
    w.line("}");
}

/// Render every declaration of the top-level module `root` and its
/// submodules (the body of `{Root}.kt`, after the package line).
pub(crate) fn render_root(n: &Names, sp: &Speller, model: &Model, root: &ModuleBinding) -> String {
    let mut w = CodeWriter::four_space();
    let tree = model
        .modules
        .iter()
        .filter(|m| m.segments.first() == root.segments.first());
    for m in tree {
        for eb in &m.errors {
            render_error_domain(&mut w, n, sp, eb);
        }
        for e in &m.enums {
            if e.is_rich() {
                render_rich_enum(&mut w, n, sp, e);
            } else {
                render_enum(&mut w, n, sp, e);
            }
        }
        for s in &m.structs {
            render_struct(&mut w, n, sp, s);
        }
        for cb in &m.callback_interfaces {
            render_callback_interface(&mut w, n, sp, cb);
        }
        for i in &m.interfaces {
            render_interface(&mut w, n, sp, i);
        }
    }
    if has_functions(model, root) {
        w.blank();
        render_module_object(&mut w, n, sp, model, root);
    }
    w.finish()
}
