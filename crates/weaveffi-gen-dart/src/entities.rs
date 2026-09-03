//! Entity rendering: error domains, C-style enums, rich enums, records, and
//! interface wrapper classes, plus their value-buffer codec helpers.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    EnumBinding, ErrorBinding, InterfaceBinding, ModuleBinding, StructBinding,
};

use crate::calls::{err_ctx, render_callable, DartDecl};
use crate::codec::{fresh, read_expr, write_stmts};
use crate::docs::emit_doc;
use crate::runtime::emit_typedef_and_lookup;
use crate::types::{dart_ident, dart_str_literal, dart_type};

/// The Dart exception class named by an error domain or one of its codes: the
/// PascalCase name with a trailing `Error` swapped for `Exception`, so
/// `KvError` becomes `KvException` and a code `IoError` becomes `IoException`.
pub(crate) fn dart_exception_name(raw: &str) -> String {
    errors::exception_type_name(raw)
}

/// Render one module's declared error domain: the domain exception extending
/// the generic [`errors::EXCEPTION_BRAND`], one exception subclass per code
/// carrying its stable code, default message, and any decoded payload fields,
/// and the `_map`/`_check` helpers that throwing wrappers route their out-err
/// slots through. When a code declares payload fields, the mapper decodes the
/// error's payload buffer into the exception's typed properties. Only
/// declared (positive) codes gain a `case`; every other code, including the
/// reserved negative runtime range (generic error, panic, marshalling
/// failure, and a callback-interface implementation that threw), falls
/// through to the generic exception.
pub(crate) fn render_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding) {
    let exc = dart_exception_name(&eb.type_name);
    let brand = errors::EXCEPTION_BRAND;

    let mut w = CodeWriter::two_space();
    w.blank();
    w.line(format!(
        "/// Typed error domain `{}` declared by module `{}`.",
        eb.name, module.path
    ));
    w.block(format!("class {exc} extends {brand} {{"), "}", |w| {
        w.line(format!("{exc}(super.code, super.message);"));
    });

    for c in &eb.codes {
        let class = dart_exception_name(&c.name);
        let message = dart_str_literal(&c.message);
        w.blank();
        let doc = c.doc.clone().or_else(|| Some(c.message.clone()));
        {
            let mut d = String::new();
            emit_doc(&mut d, &doc, "");
            w.raw(d);
        }
        w.block(format!("class {class} extends {exc} {{"), "}", |w| {
            if c.fields.is_empty() {
                w.line(format!(
                    "{class}([String message = '{message}']) : super({}, message);",
                    c.value
                ));
            } else {
                for f in &c.fields {
                    let mut fd = String::new();
                    emit_doc(&mut fd, &f.doc, "  ");
                    w.raw(fd);
                    w.line(format!(
                        "final {} {};",
                        dart_type(&f.ty),
                        dart_ident(&f.name)
                    ));
                }
                w.blank();
                let params: Vec<String> = c
                    .fields
                    .iter()
                    .map(|f| format!("this.{}", dart_ident(&f.name)))
                    .collect();
                w.line(format!(
                    "{class}({}, [String message = '{message}']) : super({}, message);",
                    params.join(", "),
                    c.value
                ));
            }
        });
    }

    w.blank();
    w.line(format!(
        "{brand} _map{exc}(int code, String message, Uint8List payload) {{"
    ));
    w.scope(|w| {
        w.block("switch (code) {", "}", |w| {
            for c in &eb.codes {
                let class = dart_exception_name(&c.name);
                if c.fields.is_empty() {
                    w.line(format!("case {}:", c.value));
                    w.scope(|w| {
                        w.line(format!("return {class}(message);"));
                    });
                } else {
                    // Braces give each payload-decoding case its own scope,
                    // so the reader and field locals never collide between
                    // cases (a Dart switch otherwise shares one scope).
                    w.line(format!("case {}: {{", c.value));
                    w.scope(|w| {
                        w.line("final r = _BufferReader(payload);");
                        let mut args: Vec<String> = Vec::new();
                        for (i, f) in c.fields.iter().enumerate() {
                            w.line(format!("final v{i} = {};", read_expr("r", &f.ty)));
                            args.push(format!("v{i}"));
                        }
                        w.line("r.expectEnd();");
                        w.line(format!("return {class}({}, message);", args.join(", ")));
                    });
                    w.line("}");
                }
            }
            w.line("default:");
            w.scope(|w| {
                w.line(format!("return {brand}(code, message);"));
            });
        });
    });
    w.line("}");

    w.blank();
    w.block(
        format!("void _check{exc}(Pointer<_WeaveFFIError> err) {{"),
        "}",
        |w| {
            w.block("if (err.ref.code != 0) {", "}", |w| {
                w.line("final code = err.ref.code;");
                w.line("final msg = err.ref.message.toDartString();");
                w.line("final payload = _copyNativeBytes(err.ref.payloadPtr, err.ref.payloadLen);");
                w.line("_weaveffiErrorClear(err);");
                w.line(format!("throw _map{exc}(code, msg, payload);"));
            });
        },
    );
    out.push_str(&w.finish());
}

/// Render one interface as a reference-counted object wrapper class.
///
/// The wrapper adopts one strong reference (the pointer behind the private
/// `_handle`) and owes exactly one `_destroy` for it: `dispose()` releases it
/// eagerly and a `NativeFinalizer` attached in the constructor releases it
/// when an undisposed wrapper is collected. The two paths never both fire
/// because `dispose()` detaches the finalizer before destroying, and a second
/// `dispose()` is a no-op. Using a disposed wrapper throws a `StateError`.
/// The private `_cloneRef()` calls the interface's `_clone` symbol to mint a
/// second strong reference, which the value-buffer codec writes as an object
/// token.
///
/// The canonical `new` constructor renders as an unnamed factory
/// (`Store(...)`), every other constructor as a named factory
/// (`Store.open(...)`), instance methods pass `_handle` as the implicit
/// leading FFI argument, and statics are `static` methods. Member FFI
/// typedefs and lookups stay at file scope.
pub(crate) fn render_interface(out: &mut String, module: &ModuleBinding, i: &InterfaceBinding) {
    let class_name = i.name.to_upper_camel_case();
    emit_typedef_and_lookup(
        out,
        &i.clone_symbol,
        "Pointer<Void>",
        "Pointer<Void>",
        "Pointer<Void>",
        "Pointer<Void>",
    );
    emit_typedef_and_lookup(
        out,
        &i.destroy_symbol,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );
    let destroy_var = i.destroy_symbol.to_lower_camel_case();
    let clone_var = i.clone_symbol.to_lower_camel_case();
    let finalizer = format!("_{destroy_var}Finalizer");
    out.push_str(&format!(
        "final {finalizer} = NativeFinalizer(\n    \
         _lib.lookup<NativeFunction<Void Function(Pointer<Void>)>>('{}'));\n",
        i.destroy_symbol
    ));

    let exc = module
        .error
        .as_ref()
        .map(|e| dart_exception_name(&e.type_name));

    // Members render exactly like free functions (depth 0), with the lookups
    // going to file scope and the declarations collected for the class body.
    let mut members = String::new();
    for c in &i.constructors {
        let kind = DartDecl::Factory {
            class_name: &class_name,
            named: c.name != "new",
        };
        render_callable(
            out,
            &mut members,
            c,
            &kind,
            &dart_ident(&c.name),
            err_ctx(c, exc.as_deref()),
        );
    }
    for m in &i.methods {
        render_callable(
            out,
            &mut members,
            m,
            &DartDecl::Method,
            &dart_ident(&m.name),
            err_ctx(m, exc.as_deref()),
        );
    }
    for s in &i.statics {
        render_callable(
            out,
            &mut members,
            s,
            &DartDecl::Static,
            &dart_ident(&s.name),
            err_ctx(s, exc.as_deref()),
        );
    }

    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &i.doc, "");
        w.raw(d);
    }
    if i.doc.is_some() {
        w.line("///");
    }
    w.line("/// A reference-counted native object. Each instance holds one strong");
    w.line("/// reference, released by [dispose] or, if the instance is collected");
    w.line("/// without being disposed, by a GC finalizer. Instances stay valid for as");
    w.line("/// long as the Dart object is reachable; the producer keeps its own");
    w.line("/// references to any object it retains.");
    if let Some(msg) = &i.deprecated {
        w.line(format!("@Deprecated('{}')", dart_str_literal(msg)));
    }
    w.block(
        format!("class {class_name} implements Finalizable {{"),
        "}",
        |w| {
            w.line("final Pointer<Void> _ptr;");
            w.line("bool _disposed = false;");
            w.blank();
            w.line("/// Adopts one strong reference to the native object.");
            w.block(format!("{class_name}._(this._ptr) {{"), "}", |w| {
                w.line(format!("{finalizer}.attach(this, _ptr, detach: this);"));
            });
            w.blank();
            w.line("/// The borrowed native pointer for the duration of a call.");
            w.block("Pointer<Void> get _handle {", "}", |w| {
                w.line(format!(
                    "if (_disposed) throw StateError('{class_name} used after dispose()');"
                ));
                w.line("return _ptr;");
            });
            w.blank();
            w.line("/// Mints a second strong reference (the interface's `_clone` symbol) for");
            w.line("/// an object token written into a value buffer.");
            w.line(format!(
                "Pointer<Void> _cloneRef() => _{clone_var}(_handle);"
            ));
            w.blank();
            w.line("/// Releases this instance's native reference. Safe to call more than");
            w.line("/// once; the native object is dropped when its last reference (this");
            w.line("/// one, any other wrapper's, or the producer's) is released.");
            w.block("void dispose() {", "}", |w| {
                w.line("if (_disposed) return;");
                w.line("_disposed = true;");
                w.line(format!("{finalizer}.detach(this);"));
                w.line(format!("_{destroy_var}(_ptr);"));
            });
            // Reindent the depth-0 member declarations into the class body.
            w.block_raw(&members);
        },
    );
    out.push_str(&w.finish());
}

/// Render one enum. A C-style enum becomes an enhanced Dart `enum`; a rich
/// (algebraic) enum is a value type and becomes a sealed class hierarchy with
/// pack/unpack helpers.
pub(crate) fn render_enum(out: &mut String, e: &EnumBinding) {
    if e.is_rich() {
        render_rich_enum(out, e);
        return;
    }
    let name = e.name.to_upper_camel_case();
    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &e.doc, "");
        w.raw(d);
    }
    w.block(format!("enum {name} {{"), "}", |w| {
        for v in &e.variants {
            let vname = dart_ident(&v.name);
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "  ");
            w.raw(vd);
            w.line(format!("{vname}({}),", v.value));
        }
        w.line(";");
        w.line(format!("const {name}(this.value);"));
        w.line("final int value;");
        w.blank();
        w.line(format!(
            "static {name} fromValue(int value) =>\n      {name}.values.firstWhere((e) => e.value == value);"
        ));
    });
    out.push_str(&w.finish());
}

/// Render one record as a plain Dart value class (final typed fields, a named
/// constructor argument per field), plus its `_pack{Name}`/`_unpack{Name}`
/// buffer helpers. Records declare no C symbols: no destroy, no getters, no
/// builders; instances cross the ABI serialized in value buffers.
pub(crate) fn render_struct(out: &mut String, s: &StructBinding) {
    let class_name = s.name.to_upper_camel_case();
    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &s.doc, "");
        w.raw(d);
    }
    w.block(format!("class {class_name} {{"), "}", |w| {
        for f in &s.fields {
            let mut fd = String::new();
            emit_doc(&mut fd, &f.doc, "  ");
            w.raw(fd);
            w.line(format!(
                "final {} {};",
                dart_type(&f.ty),
                dart_ident(&f.name)
            ));
        }
        if !s.fields.is_empty() {
            w.blank();
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
            w.line(format!("{class_name}({{{}}});", params.join(", ")));
        }
    });

    // Pack: each field in declaration (wire) order.
    w.blank();
    w.line(format!(
        "void _pack{class_name}(_BufferWriter w, {class_name} v) {{"
    ));
    w.scope(|w| {
        let mut tmp = 0usize;
        for f in &s.fields {
            write_stmts(
                w,
                "w",
                &format!("v.{}", dart_ident(&f.name)),
                &f.ty,
                &mut tmp,
            );
        }
    });
    w.line("}");

    // Unpack: named constructor arguments evaluate in source order, which is
    // the field declaration (wire) order.
    w.blank();
    w.line(format!(
        "{class_name} _unpack{class_name}(_BufferReader r) {{"
    ));
    w.scope(|w| {
        if s.fields.is_empty() {
            w.line(format!("return {class_name}();"));
        } else {
            w.line(format!("return {class_name}("));
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
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// The Dart subclass name of one rich-enum variant: `{Enum}{Variant}`.
fn variant_class(base: &str, variant: &str) -> String {
    format!("{base}{}", variant.to_upper_camel_case())
}

/// Render one rich (algebraic) enum as an idiomatic sealed class hierarchy:
/// a sealed base class plus one subclass per variant carrying that variant's
/// fields, and `_pack{Name}`/`_unpack{Name}` helpers encoding the `i32` tag
/// followed by the active variant's fields. Rich enums declare no C symbols;
/// values cross the ABI serialized in value buffers.
fn render_rich_enum(out: &mut String, e: &EnumBinding) {
    let base = e.name.to_upper_camel_case();
    let mut w = CodeWriter::two_space();
    w.blank();
    {
        let mut d = String::new();
        emit_doc(&mut d, &e.doc, "");
        w.raw(d);
    }
    w.block(format!("sealed class {base} {{"), "}", |w| {
        w.line(format!("const {base}();"));
    });

    for v in &e.variants {
        let cls = variant_class(&base, &v.name);
        w.blank();
        {
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "");
            w.raw(vd);
        }
        if v.fields.is_empty() {
            w.line(format!("class {cls} extends {base} {{}}"));
        } else {
            w.block(format!("class {cls} extends {base} {{"), "}", |w| {
                for f in &v.fields {
                    let mut fd = String::new();
                    emit_doc(&mut fd, &f.doc, "  ");
                    w.raw(fd);
                    w.line(format!(
                        "final {} {};",
                        dart_type(&f.ty),
                        dart_ident(&f.name)
                    ));
                }
                w.blank();
                let params: Vec<String> = v
                    .fields
                    .iter()
                    .map(|f| format!("this.{}", dart_ident(&f.name)))
                    .collect();
                w.line(format!("{cls}({});", params.join(", ")));
            });
        }
    }

    // Pack: the i32 tag, then the active variant's fields in order. The
    // sealed base makes the switch exhaustive without a default arm. One
    // temp counter spans all cases: a Dart switch shares a single scope for
    // plain declarations, so names must stay unique across cases.
    w.blank();
    w.line(format!("void _pack{base}(_BufferWriter w, {base} v) {{"));
    w.scope(|w| {
        w.line("switch (v) {");
        w.scope(|w| {
            let mut tmp = 0usize;
            for v in &e.variants {
                let cls = variant_class(&base, &v.name);
                if v.fields.is_empty() {
                    w.line(format!("case {cls}():"));
                    w.scope(|w| {
                        w.line(format!("w.writeInt32({});", v.value));
                    });
                } else {
                    let b = fresh(&mut tmp);
                    w.line(format!("case final {cls} {b}:"));
                    w.scope(|w| {
                        w.line(format!("w.writeInt32({});", v.value));
                        for f in &v.fields {
                            write_stmts(
                                w,
                                "w",
                                &format!("{b}.{}", dart_ident(&f.name)),
                                &f.ty,
                                &mut tmp,
                            );
                        }
                    });
                }
            }
        });
        w.line("}");
    });
    w.line("}");

    // Unpack: constructor arguments evaluate left to right, preserving the
    // wire order of the variant's fields.
    w.blank();
    w.line(format!("{base} _unpack{base}(_BufferReader r) {{"));
    w.scope(|w| {
        w.line("final tag = r.readInt32();");
        w.line("switch (tag) {");
        w.scope(|w| {
            for v in &e.variants {
                let cls = variant_class(&base, &v.name);
                w.line(format!("case {}:", v.value));
                w.scope(|w| {
                    let args: Vec<String> =
                        v.fields.iter().map(|f| read_expr("r", &f.ty)).collect();
                    w.line(format!("return {cls}({});", args.join(", ")));
                });
            }
            w.line("default:");
            w.scope(|w| {
                w.line(format!("_bufferError('unknown {base} tag $tag');"));
            });
        });
        w.line("}");
    });
    w.line("}");
    out.push_str(&w.finish());
}
