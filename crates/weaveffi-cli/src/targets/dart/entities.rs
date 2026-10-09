//! Entity rendering: error domains, C-style enums, rich enums, records, and
//! interface wrapper classes, plus their value-buffer codec helpers.

use crate::codegen::errors::ErrorTable;
use crate::codegen::CodeWriter;
use weaveffi_model::model::{
    EnumBinding, EnumVariantBinding, FieldBinding, InterfaceBinding, Model, StructBinding,
};
use weaveffi_model::ty::{Prim, Ty};

use crate::targets::dart::calls::{
    emit_bindings, emit_destroy, emit_wrapper, lookup, mapper_fn, DartDecl,
};
use crate::targets::dart::codec::{pack_fn, read_expr, unpack_fn, write_expr};
use crate::targets::dart::docs::Docs;
use crate::targets::dart::types::{
    dart_class, dart_field, dart_ident, dart_member, dart_str_literal, dart_type, error_field,
    ffi_var,
};

/// The `_payloadOf{Exception}` function encoding a domain exception's fields
/// for a callback that reports it.
pub(crate) fn payload_fn(exception: &str) -> String {
    format!("_payloadOf{exception}")
}

/// The indefinite article for a class name (`an Entry`, `a Contact`).
fn article(class: &str) -> &'static str {
    if class.starts_with(['A', 'E', 'I', 'O', 'U']) {
        "an"
    } else {
        "a"
    }
}

/// Render the final fields of a value class, with their docs; `name`
/// spells a field.
fn render_fields(
    w: &mut CodeWriter,
    docs: &Docs,
    fields: &[FieldBinding],
    name: fn(&str) -> String,
) {
    for f in fields {
        docs.write(w, &f.doc);
        w.line(format!("final {} {};", dart_type(&f.ty), name(&f.name)));
    }
}

/// Whether a field of `ty` compares element by element (a list, map, or
/// byte array, possibly optional) rather than with `==`.
fn is_collection(ty: &Ty) -> bool {
    match ty {
        Ty::Optional(inner) => is_collection(inner),
        Ty::List(_) | Ty::Map(..) | Ty::Prim(Prim::Bytes) => true,
        _ => false,
    }
}

/// Render `==`, `hashCode`, and `toString` for a value class: fields compare
/// by value, collections element by element.
fn render_value_members(w: &mut CodeWriter, class: &str, fields: &[FieldBinding]) {
    w.blank();
    w.line("@override");
    if fields.is_empty() {
        w.line(format!(
            "bool operator ==(Object other) => other is {class};"
        ));
        w.blank();
        w.line("@override");
        w.line(format!("int get hashCode => ({class}).hashCode;"));
        w.blank();
        w.line("@override");
        w.line(format!("String toString() => '{class}()';"));
        return;
    }
    w.line("bool operator ==(Object other) =>");
    w.scope(|w| {
        w.scope(|w| {
            w.line("identical(this, other) ||");
            w.line(format!("other is {class} &&"));
            for (i, f) in fields.iter().enumerate() {
                let n = dart_field(&f.name);
                let end = if i + 1 == fields.len() { ";" } else { " &&" };
                if is_collection(&f.ty) {
                    w.line(format!("_deepEquals({n}, other.{n}){end}"));
                } else {
                    w.line(format!("{n} == other.{n}{end}"));
                }
            }
        });
    });
    w.blank();
    w.line("@override");
    let hashes: Vec<String> = fields
        .iter()
        .map(|f| {
            let n = dart_field(&f.name);
            if is_collection(&f.ty) {
                format!("_deepHash({n})")
            } else {
                n
            }
        })
        .collect();
    w.line(format!(
        "int get hashCode => Object.hashAll([{class}, {}]);",
        hashes.join(", ")
    ));
    w.blank();
    w.line("@override");
    let shown: Vec<String> = fields
        .iter()
        .map(|f| {
            let n = dart_field(&f.name);
            format!("{n}: ${n}")
        })
        .collect();
    w.line(format!(
        "String toString() => '{class}({})';",
        shown.join(", ")
    ));
}

/// Render one error domain: an open domain exception (a `NativeException`
/// subclass) with one final subclass per code carrying its code, default
/// message, and decoded payload fields; the `_map{Domain}` mapper the
/// domain's callables route codes through (a declared code is its subclass,
/// any other positive code the domain class itself with its code and
/// message, and a negative code a runtime exception); and, when `reported`
/// (a callback method reports this domain), the encoder of each code's
/// fields.
pub(crate) fn render_error(w: &mut CodeWriter, docs: &Docs, table: &ErrorTable, reported: bool) {
    let exc = dart_class(&table.type_name);
    w.blank();
    w.line(format!(
        "/// The `{}` error domain of module `{}`: catch it to handle any of",
        table.domain.name, table.module.dot_path
    ));
    w.line("/// its codes, or a subclass for one code. The domain is open: a code");
    w.line("/// these bindings don't declare (from a newer library) arrives as this");
    w.line("/// class itself, with its [code] and [message].");
    w.block(
        format!("class {exc} extends NativeException {{"),
        "}",
        |w| {
            w.line("/// Creates a domain error carrying [code] and [message].");
            w.line(format!("{exc}(super.code, super.message);"));
        },
    );

    for row in &table.codes {
        let c = row.code;
        let class = dart_class(&row.type_name);
        let message = dart_str_literal(&c.message);
        w.blank();
        docs.write(w, &c.doc.clone().or_else(|| Some(c.message.clone())));
        w.block(format!("final class {class} extends {exc} {{"), "}", |w| {
            w.line(format!(
                "/// Creates {} [{class}] (code {}).",
                article(&class),
                c.value
            ));
            if c.fields.is_empty() {
                w.line(format!(
                    "{class}([String message = '{message}']) : super({}, message);",
                    c.value
                ));
            } else {
                let params: Vec<String> = c
                    .fields
                    .iter()
                    .map(|f| format!("this.{}", error_field(&f.name)))
                    .collect();
                w.line(format!(
                    "{class}({}, [String message = '{message}'])",
                    params.join(", ")
                ));
                w.line(format!("    : super({}, message);", c.value));
                w.blank();
                render_fields(w, docs, &c.fields, error_field);
            }
        });
    }

    w.blank();
    w.line(format!(
        "NativeException {}(int code, String message, Uint8List payload) =>",
        mapper_fn(&exc)
    ));
    w.scope(|w| {
        w.scope(|w| {
            w.line("switch (code) {");
            w.scope(|w| {
                for row in &table.codes {
                    let c = row.code;
                    let class = dart_class(&row.type_name);
                    if c.fields.is_empty() {
                        w.line(format!("{} => {class}(message),", c.value));
                    } else {
                        let args: Vec<String> =
                            c.fields.iter().map(|f| read_expr(&f.ty, "r")).collect();
                        w.line(format!(
                            "{} => _decode(payload, (r) => {class}({}, message)),",
                            c.value,
                            args.join(", ")
                        ));
                    }
                }
                w.line(format!("> 0 => {exc}(code, message),"));
                w.line("_ => _runtimeException(code, message, payload),");
            });
            w.line("};");
        });
    });

    if !reported {
        return;
    }
    w.blank();
    w.line("/// The value buffer of [e]'s fields, or null for a code without any.");
    w.block(
        format!("Uint8List? {}({exc} e) {{", payload_fn(&exc)),
        "}",
        |w| {
            w.line("final w = _BufferWriter();");
            w.block("switch (e) {", "}", |w| {
                for row in table.codes.iter().filter(|r| !r.code.fields.is_empty()) {
                    w.line(format!("case {}():", dart_class(&row.type_name)));
                    w.scope(|w| {
                        for f in &row.code.fields {
                            let expr = format!("e.{}", error_field(&f.name));
                            w.line(format!("{};", write_expr(&f.ty, "w", &expr)));
                        }
                    });
                }
                w.line("default:");
                w.scope(|w| {
                    w.line("return null;");
                });
            });
            w.line("return w.takeBytes();");
        },
    );
}

/// Render one interface: its lifecycle and member bindings at top level,
/// then a wrapper class over the runtime's `_NativeObject` where the
/// canonical `new` constructor is an unnamed factory, other constructors are
/// named factories, methods borrow the wrapper's pointer, and statics are
/// `static`.
pub(crate) fn render_interface(
    w: &mut CodeWriter,
    model: &Model,
    docs: &Docs,
    i: &InterfaceBinding,
    leaf: bool,
) {
    let class = dart_class(&i.name);
    let destroy = ffi_var(&i.destroy_symbol);
    let clone = ffi_var(&i.clone_symbol);
    lookup(
        w,
        &i.clone_symbol,
        "Pointer<Void> Function(Pointer<Void>)",
        "Pointer<Void> Function(Pointer<Void>)",
        leaf,
    );
    emit_destroy(w, &i.destroy_symbol, leaf);
    let members = || {
        let ctors = i.constructors.iter().map(|c| {
            let kind = DartDecl::Factory {
                class_name: &class,
                named: c.name != "new",
            };
            (c, kind)
        });
        let methods = i.methods.iter().map(|m| (m, DartDecl::Method));
        let statics = i.statics.iter().map(|s| (s, DartDecl::Static));
        ctors.chain(methods).chain(statics)
    };
    for (f, _) in members() {
        emit_bindings(w, f, leaf);
    }

    w.blank();
    docs.write(w, &i.doc);
    if i.doc.is_some() {
        w.line("///");
    }
    w.line("/// A reference-counted native object. Each instance holds one strong");
    w.line("/// reference, released by [dispose] or, when an undisposed instance is");
    w.line("/// collected, by a finalizer.");
    docs.write_deprecated(w, &i.deprecated);
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
            for (f, kind) in members() {
                emit_wrapper(w, model, docs, f, &kind, &dart_member(&f.name));
            }
        },
    );
}

/// Enum members `enum` declarations already define; a variant spelled like
/// one gains a trailing `_`.
const ENUM_MEMBERS: &[&str] = &["fromValue", "index", "name", "value", "values"];

/// Render one enum: a C-style enum becomes an enhanced Dart `enum` carrying
/// its discriminant; a rich enum becomes a sealed class hierarchy.
pub(crate) fn render_enum(w: &mut CodeWriter, docs: &Docs, e: &EnumBinding) {
    if e.is_rich() {
        render_rich_enum(w, docs, e);
        return;
    }
    let name = dart_class(&e.name);
    w.blank();
    docs.write(w, &e.doc);
    docs.write_deprecated(w, &e.deprecated);
    w.block(format!("enum {name} {{"), "}", |w| {
        for (i, v) in e.variants.iter().enumerate() {
            let mut variant = dart_ident(&v.name);
            if ENUM_MEMBERS.contains(&variant.as_str()) {
                variant.push('_');
            }
            docs.write(w, &v.doc);
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
            "    orElse: () => throw ArgumentError.value(value, 'value', 'not {} {name} discriminant'));",
            article(&name)
        ));
    });
}

/// Render one record as a Dart value class (final fields, one named
/// constructor argument per field, optional fields not required, value
/// equality) plus its `_pack_{Name}`/`_unpack_{Name}` helpers.
pub(crate) fn render_struct(w: &mut CodeWriter, docs: &Docs, s: &StructBinding) {
    let class = dart_class(&s.name);
    let ty = Ty::Record(s.name.clone());
    w.blank();
    docs.write(w, &s.doc);
    docs.write_deprecated(w, &s.deprecated);
    w.block(format!("final class {class} {{"), "}", |w| {
        w.line(format!("/// Creates {} [{class}].", article(&class)));
        if s.fields.is_empty() {
            w.line(format!("const {class}();"));
        } else {
            let params: Vec<String> = s
                .fields
                .iter()
                .map(|f| {
                    let n = dart_field(&f.name);
                    if matches!(f.ty, Ty::Optional(_)) {
                        format!("this.{n}")
                    } else {
                        format!("required this.{n}")
                    }
                })
                .collect();
            w.line(format!("{class}({{{}}});", params.join(", ")));
            w.blank();
            render_fields(w, docs, &s.fields, dart_field);
        }
        render_value_members(w, &class, &s.fields);
    });

    w.blank();
    if s.fields.is_empty() {
        w.line(format!(
            "void {}(_BufferWriter w, {class} v) {{}}",
            pack_fn(&ty)
        ));
    } else {
        w.block(
            format!("void {}(_BufferWriter w, {class} v) {{", pack_fn(&ty)),
            "}",
            |w| {
                for f in &s.fields {
                    let expr = format!("v.{}", dart_field(&f.name));
                    w.line(format!("{};", write_expr(&f.ty, "w", &expr)));
                }
            },
        );
    }

    // Named arguments evaluate in source order, which is the wire order.
    w.blank();
    if s.fields.is_empty() {
        w.line(format!(
            "{class} {}(_BufferReader r) => const {class}();",
            unpack_fn(&ty)
        ));
    } else {
        w.line(format!(
            "{class} {}(_BufferReader r) => {class}(",
            unpack_fn(&ty)
        ));
        w.indent().indent().indent();
        for f in &s.fields {
            w.line(format!(
                "{}: {},",
                dart_field(&f.name),
                read_expr(&f.ty, "r")
            ));
        }
        w.dedent().dedent().dedent();
        w.line("    );");
    }
}

/// The class of one rich-enum variant.
fn variant_class(e: &EnumBinding, v: &EnumVariantBinding) -> String {
    dart_class(&format!("{}{}", e.name, v.name))
}

/// Render one rich enum as a sealed class hierarchy: a sealed base plus one
/// final subclass per variant carrying its fields (positional constructor,
/// value equality), and `_pack_{Name}`/`_unpack_{Name}` helpers encoding the
/// `i32` tag followed by the active variant's fields.
fn render_rich_enum(w: &mut CodeWriter, docs: &Docs, e: &EnumBinding) {
    let base = dart_class(&e.name);
    let ty = Ty::RichEnum(e.name.clone());
    w.blank();
    docs.write(w, &e.doc);
    docs.write_deprecated(w, &e.deprecated);
    w.block(format!("sealed class {base} {{"), "}", |w| {
        w.line(format!("const {base}();"));
    });

    for v in &e.variants {
        let cls = variant_class(e, v);
        w.blank();
        docs.write(w, &v.doc);
        w.block(format!("final class {cls} extends {base} {{"), "}", |w| {
            w.line(format!("/// Creates {} [{cls}].", article(&cls)));
            if v.fields.is_empty() {
                w.line(format!("const {cls}();"));
            } else {
                let params: Vec<String> = v
                    .fields
                    .iter()
                    .map(|f| format!("this.{}", dart_field(&f.name)))
                    .collect();
                w.line(format!("{cls}({});", params.join(", ")));
                w.blank();
                render_fields(w, docs, &v.fields, dart_field);
            }
            render_value_members(w, &cls, &v.fields);
        });
    }

    // Pack: the tag, then the active variant's fields. The sealed base makes
    // the switch exhaustive.
    w.blank();
    w.block(
        format!("void {}(_BufferWriter w, {base} v) {{", pack_fn(&ty)),
        "}",
        |w| {
            w.block("switch (v) {", "}", |w| {
                for var in &e.variants {
                    let cls = variant_class(e, var);
                    w.line(format!("case {cls}():"));
                    w.scope(|w| {
                        w.line(format!("w.writeI32({});", var.value));
                        for f in &var.fields {
                            let expr = format!("v.{}", dart_field(&f.name));
                            w.line(format!("{};", write_expr(&f.ty, "w", &expr)));
                        }
                    });
                }
            });
        },
    );

    // Unpack: positional arguments evaluate left to right, in wire order.
    w.blank();
    w.block(
        format!("{base} {}(_BufferReader r) {{", unpack_fn(&ty)),
        "}",
        |w| {
            w.line("final tag = r.readI32();");
            w.block("switch (tag) {", "}", |w| {
                for v in &e.variants {
                    let cls = variant_class(e, v);
                    w.line(format!("case {}:", v.value));
                    w.scope(|w| {
                        let args: Vec<String> =
                            v.fields.iter().map(|f| read_expr(&f.ty, "r")).collect();
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
}
