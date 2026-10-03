//! Entity rendering: error domains, C-style enums, rich enums, records, and
//! interface wrapper classes, plus their value-buffer codec helpers.

use crate::codegen::CodeWriter;
use heck::ToUpperCamelCase;
use weaveffi_model::model::{
    EnumBinding, ErrorBinding, FieldBinding, InterfaceBinding, ModuleBinding, StructBinding, Ty,
};

use crate::targets::dart::calls::{
    err_ctx, finalizer, lookup, mapper_fn, render_callable, DartDecl,
};
use crate::targets::dart::codec::{fresh, pack_fn, read_expr, unpack_fn, write_stmts};
use crate::targets::dart::docs::{write_deprecated, write_doc};
use crate::targets::dart::types::{dart_class, dart_ident, dart_str_literal, dart_type, ffi_var};

/// The Dart exception class of an error domain or code: the PascalCase name
/// with a trailing `Error` swapped for `Exception` (`KvError` becomes
/// `KvException`, a code `IoError` becomes `IoException`, and a bare
/// `Error` becomes `ErrorException`).
pub(crate) fn dart_exception_name(raw: &str) -> String {
    let pascal = raw.to_upper_camel_case();
    let stem = pascal
        .strip_suffix("Error")
        .filter(|s| !s.is_empty())
        .unwrap_or(&pascal);
    if stem.ends_with("Exception") {
        dart_class(stem)
    } else {
        dart_class(&format!("{stem}Exception"))
    }
}

/// The indefinite article for a class name (`an Entry`, `a Contact`).
fn article(class: &str) -> &'static str {
    if class.starts_with(['A', 'E', 'I', 'O', 'U']) {
        "an"
    } else {
        "a"
    }
}

/// Render the final fields of a value class, with their docs.
fn render_fields(w: &mut CodeWriter, fields: &[FieldBinding]) {
    for f in fields {
        write_doc(w, &f.doc);
        w.line(format!(
            "final {} {};",
            dart_type(&f.ty),
            dart_ident(&f.name)
        ));
    }
}

/// Render one module's declared error domain: the domain exception (a
/// `NativeException` subclass), one subclass per code carrying its code,
/// default message, and decoded payload fields, and the `_map{Domain}`
/// mapper that throwing wrappers route codes through. Only declared codes
/// gain a `case`; every other code, including the negative runtime range,
/// maps onto the runtime exceptions.
pub(crate) fn render_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding) {
    let exc = dart_exception_name(&eb.type_name);
    let mut w = CodeWriter::two_space();
    w.blank();
    w.line(format!(
        "/// The `{}` error domain of module `{}`.",
        eb.name, module.dot_path
    ));
    w.block(
        format!("class {exc} extends NativeException {{"),
        "}",
        |w| {
            w.line("/// Creates a domain error carrying [code] and [message].");
            w.line(format!("{exc}(super.code, super.message);"));
        },
    );

    for c in &eb.codes {
        let class = dart_exception_name(&c.name);
        let message = dart_str_literal(&c.message);
        w.blank();
        write_doc(&mut w, &c.doc.clone().or_else(|| Some(c.message.clone())));
        w.block(format!("class {class} extends {exc} {{"), "}", |w| {
            if c.fields.is_empty() {
                w.line(format!(
                    "{class}([String message = '{message}']) : super({}, message);",
                    c.value
                ));
            } else {
                let params: Vec<String> = c
                    .fields
                    .iter()
                    .map(|f| format!("this.{}", dart_ident(&f.name)))
                    .collect();
                w.line(format!(
                    "{class}({}, [String message = '{message}'])",
                    params.join(", ")
                ));
                w.line(format!("    : super({}, message);", c.value));
                w.blank();
                render_fields(w, &c.fields);
            }
        });
    }

    w.blank();
    w.block(
        format!(
            "NativeException {}(int code, String message, Uint8List payload) {{",
            mapper_fn(&exc)
        ),
        "}",
        |w| {
            w.block("switch (code) {", "}", |w| {
                for c in &eb.codes {
                    let class = dart_exception_name(&c.name);
                    w.line(format!("case {}:", c.value));
                    w.scope(|w| {
                        if c.fields.is_empty() {
                            w.line(format!("return {class}(message);"));
                        } else {
                            w.line("final r = _BufferReader(payload);");
                            let args: Vec<String> =
                                c.fields.iter().map(|f| read_expr("r", &f.ty)).collect();
                            w.line(format!(
                                "final error = {class}({}, message);",
                                args.join(", ")
                            ));
                            w.line("r.expectEnd();");
                            w.line("return error;");
                        }
                    });
                }
                w.line("default:");
                w.scope(|w| {
                    w.line("return _runtimeError(code, message, payload);");
                });
            });
        },
    );
    out.push_str(&w.finish());
}

/// Render one interface as a wrapper class over the runtime's
/// `_NativeObject`: the canonical `new` constructor is an unnamed factory,
/// other constructors are named factories, methods borrow the wrapper's
/// pointer, and statics are `static`. Member bindings stay at file scope.
pub(crate) fn render_interface(
    out: &mut String,
    module: &ModuleBinding,
    i: &InterfaceBinding,
    leaf: bool,
) {
    let class = dart_class(&i.name);
    let destroy = ffi_var(&i.destroy_symbol);
    let clone = ffi_var(&i.clone_symbol);
    out.push_str(&lookup(
        &i.clone_symbol,
        "Pointer<Void> Function(Pointer<Void>)",
        "Pointer<Void> Function(Pointer<Void>)",
        leaf,
    ));
    out.push_str(&lookup(
        &i.destroy_symbol,
        "Void Function(Pointer<Void>)",
        "void Function(Pointer<Void>)",
        leaf,
    ));
    out.push_str(&finalizer(&i.destroy_symbol));

    let exc = module
        .error
        .as_ref()
        .map(|e| dart_exception_name(&e.type_name));
    let mut members = String::new();
    for c in &i.constructors {
        let kind = DartDecl::Factory {
            class_name: &class,
            named: c.name != "new",
        };
        let name = dart_ident(&c.name);
        render_callable(
            out,
            &mut members,
            c,
            &kind,
            &name,
            err_ctx(c, exc.as_deref()),
            leaf,
        );
    }
    for m in &i.methods {
        let name = dart_ident(&m.name);
        let err = err_ctx(m, exc.as_deref());
        render_callable(out, &mut members, m, &DartDecl::Method, &name, err, leaf);
    }
    for s in &i.statics {
        let name = dart_ident(&s.name);
        let err = err_ctx(s, exc.as_deref());
        render_callable(out, &mut members, s, &DartDecl::Static, &name, err, leaf);
    }

    let mut w = CodeWriter::two_space();
    w.blank();
    write_doc(&mut w, &i.doc);
    if i.doc.is_some() {
        w.line("///");
    }
    w.line("/// A reference-counted native object. Each instance holds one strong");
    w.line("/// reference, released by [dispose] or, when an undisposed instance is");
    w.line("/// collected, by a finalizer.");
    write_deprecated(&mut w, &i.deprecated);
    w.block(
        format!("final class {class} extends _NativeObject {{"),
        "}",
        |w| {
            w.line(format!("{class}._(super._ptr);"));
            w.blank();
            w.line("@override");
            w.line(format!(
                "NativeFinalizer get _finalizer => {destroy}Finalizer;"
            ));
            w.blank();
            w.line("@override");
            w.line(format!(
                "void _destroy(Pointer<Void> ptr) => {destroy}(ptr);"
            ));
            w.blank();
            w.line("@override");
            w.line(format!(
                "Pointer<Void> _clone(Pointer<Void> ptr) => {clone}(ptr);"
            ));
            w.block_raw(&members);
        },
    );
    out.push_str(&w.finish());
}

/// Enum members `enum` declarations already define; a variant spelled like
/// one gains a trailing `_`.
const ENUM_MEMBERS: &[&str] = &["fromValue", "index", "name", "value", "values"];

/// Render one enum: a C-style enum becomes an enhanced Dart `enum` carrying
/// its discriminant; a rich enum becomes a sealed class hierarchy.
pub(crate) fn render_enum(out: &mut String, e: &EnumBinding) {
    if e.is_rich() {
        render_rich_enum(out, e);
        return;
    }
    let name = dart_class(&e.name);
    let mut w = CodeWriter::two_space();
    w.blank();
    write_doc(&mut w, &e.doc);
    write_deprecated(&mut w, &e.deprecated);
    w.block(format!("enum {name} {{"), "}", |w| {
        for (i, v) in e.variants.iter().enumerate() {
            let mut variant = dart_ident(&v.name);
            if ENUM_MEMBERS.contains(&variant.as_str()) {
                variant.push('_');
            }
            write_doc(w, &v.doc);
            let end = if i + 1 == e.variants.len() { ';' } else { ',' };
            w.line(format!("{variant}({}){end}", v.value));
        }
        w.blank();
        w.line(format!("const {name}(this.value);"));
        w.blank();
        w.line("/// The C discriminant.");
        w.line("final int value;");
        w.blank();
        w.line("/// The variant whose discriminant is [value].");
        w.line(format!("static {name} fromValue(int value) => values.firstWhere("));
        w.line("    (e) => e.value == value,");
        w.line(format!(
            "    orElse: () => throw ArgumentError.value(value, 'value', 'not a {name} discriminant'));"
        ));
    });
    out.push_str(&w.finish());
}

/// Render one record as a plain Dart value class (final fields, one named
/// constructor argument per field, optional fields not required) plus its
/// `_pack{Name}`/`_unpack{Name}` helpers.
pub(crate) fn render_struct(out: &mut String, s: &StructBinding) {
    let class = dart_class(&s.name);
    let mut w = CodeWriter::two_space();
    w.blank();
    write_doc(&mut w, &s.doc);
    write_deprecated(&mut w, &s.deprecated);
    w.block(format!("class {class} {{"), "}", |w| {
        if s.fields.is_empty() {
            w.line(format!("/// Creates {} [{class}].", article(&class)));
            w.line(format!("const {class}();"));
            return;
        }
        let params: Vec<String> = s
            .fields
            .iter()
            .map(|f| {
                let n = dart_ident(&f.name);
                if matches!(f.ty, Ty::Optional(_)) {
                    format!("this.{n}")
                } else {
                    format!("required this.{n}")
                }
            })
            .collect();
        w.line(format!("/// Creates {} [{class}].", article(&class)));
        w.line(format!("{class}({{{}}});", params.join(", ")));
        w.blank();
        render_fields(w, &s.fields);
    });

    w.blank();
    w.block(
        format!("void {}(_BufferWriter w, {class} v) {{", pack_fn(&s.name)),
        "}",
        |w| {
            let mut tmp = 0usize;
            for f in &s.fields {
                let expr = format!("v.{}", dart_ident(&f.name));
                write_stmts(w, "w", &expr, &f.ty, &mut tmp);
            }
        },
    );

    // Named arguments evaluate in source order, which is the wire order.
    w.blank();
    if s.fields.is_empty() {
        w.line(format!(
            "{class} {}(_BufferReader r) => const {class}();",
            unpack_fn(&s.name)
        ));
    } else {
        w.line(format!(
            "{class} {}(_BufferReader r) => {class}(",
            unpack_fn(&s.name)
        ));
        w.scope(|w| {
            for f in &s.fields {
                w.line(format!(
                    "{}: {},",
                    dart_ident(&f.name),
                    read_expr("r", &f.ty)
                ));
            }
        });
        w.line(");");
    }
    out.push_str(&w.finish());
}

/// Render one rich enum as a sealed class hierarchy: a sealed base plus one
/// subclass per variant carrying its fields (positional constructor), and
/// `_pack{Name}`/`_unpack{Name}` helpers encoding the `i32` tag followed by
/// the active variant's fields.
fn render_rich_enum(out: &mut String, e: &EnumBinding) {
    let base = dart_class(&e.name);
    let variant_class = |name: &str| dart_class(&format!("{}{}", e.name, name));
    let mut w = CodeWriter::two_space();
    w.blank();
    write_doc(&mut w, &e.doc);
    write_deprecated(&mut w, &e.deprecated);
    w.block(format!("sealed class {base} {{"), "}", |w| {
        w.line(format!("const {base}();"));
    });

    for v in &e.variants {
        let cls = variant_class(&v.name);
        w.blank();
        write_doc(&mut w, &v.doc);
        if v.fields.is_empty() {
            w.block(format!("final class {cls} extends {base} {{"), "}", |w| {
                w.line(format!("/// Creates {} [{cls}].", article(&cls)));
                w.line(format!("const {cls}();"));
            });
            continue;
        }
        w.block(format!("final class {cls} extends {base} {{"), "}", |w| {
            let params: Vec<String> = v
                .fields
                .iter()
                .map(|f| format!("this.{}", dart_ident(&f.name)))
                .collect();
            w.line(format!("/// Creates {} [{cls}].", article(&cls)));
            w.line(format!("{cls}({});", params.join(", ")));
            w.blank();
            render_fields(w, &v.fields);
        });
    }

    // Pack: the tag, then the active variant's fields. The sealed base makes
    // the switch exhaustive. One temp counter spans every case so names stay
    // unique across the switch.
    w.blank();
    w.block(
        format!("void {}(_BufferWriter w, {base} v) {{", pack_fn(&e.name)),
        "}",
        |w| {
            w.block("switch (v) {", "}", |w| {
                let mut tmp = 0usize;
                for v in &e.variants {
                    let cls = variant_class(&v.name);
                    if v.fields.is_empty() {
                        w.line(format!("case {cls}():"));
                        w.scope(|w| {
                            w.line(format!("w.writeI32({});", v.value));
                        });
                        continue;
                    }
                    let b = fresh(&mut tmp);
                    w.line(format!("case final {cls} {b}:"));
                    w.scope(|w| {
                        w.line(format!("w.writeI32({});", v.value));
                        for f in &v.fields {
                            let expr = format!("{b}.{}", dart_ident(&f.name));
                            write_stmts(w, "w", &expr, &f.ty, &mut tmp);
                        }
                    });
                }
            });
        },
    );

    // Unpack: positional arguments evaluate left to right, in wire order.
    w.blank();
    w.block(
        format!("{base} {}(_BufferReader r) {{", unpack_fn(&e.name)),
        "}",
        |w| {
            w.line("final tag = r.readI32();");
            w.block("switch (tag) {", "}", |w| {
                for v in &e.variants {
                    let cls = variant_class(&v.name);
                    w.line(format!("case {}:", v.value));
                    w.scope(|w| {
                        let args: Vec<String> =
                            v.fields.iter().map(|f| read_expr("r", &f.ty)).collect();
                        let ctor = if v.fields.is_empty() { "const " } else { "" };
                        w.line(format!("return {ctor}{cls}({});", args.join(", ")));
                    });
                }
                w.line("default:");
                w.scope(|w| {
                    w.line(format!("_bufferError('unknown {base} tag $tag');"));
                });
            });
        },
    );
    out.push_str(&w.finish());
}
