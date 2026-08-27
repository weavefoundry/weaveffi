//! Entity renderers: C-style enums, rich (algebraic) enums, records,
//! typed-handle wrapper structs, and interface wrapper classes.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    BindingModel, CallShape, EnumBinding, EnumVariantBinding, ErrorBinding, FieldBinding,
    FnBinding, InterfaceBinding, StructBinding,
};
use weaveffi_core::utils::local_type_name;
use weaveffi_ir::ir::TypeRef;

use crate::calls::{
    build_call_args, param_needs_marshal, render_marshal_cleanup, render_marshal_setup,
    render_wrapper_method, ErrCtx,
};
use crate::codec::{emit_buffer_read, emit_buffer_write};
use crate::docs::{writer_doc, writer_fn_doc};
use crate::types::{camel_fn, cs_type, safe_cs_name, typed_handle_cs};

/// Collect the local referent names of every `handle<T>` used anywhere in
/// the model (parameters, returns, fields, variant fields, callback
/// parameters, and error payload fields), so one `{T}Handle` wrapper struct
/// is emitted per referent. The `BTreeSet` keeps emission order stable.
pub(crate) fn collect_typed_handles(model: &BindingModel) -> std::collections::BTreeSet<String> {
    fn visit(ty: &TypeRef, acc: &mut std::collections::BTreeSet<String>) {
        match ty {
            TypeRef::TypedHandle(name) => {
                acc.insert(local_type_name(name).to_string());
            }
            TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
                visit(inner, acc);
            }
            TypeRef::Map(k, v) => {
                visit(k, acc);
                visit(v, acc);
            }
            _ => {}
        }
    }
    let mut acc = std::collections::BTreeSet::new();
    for m in &model.modules {
        for f in m.callables() {
            for p in &f.params {
                visit(&p.ty, &mut acc);
            }
            if let Some(r) = &f.ret {
                visit(r, &mut acc);
            }
        }
        for cb in &m.callbacks {
            for p in &cb.params {
                visit(&p.ty, &mut acc);
            }
        }
        for s in &m.structs {
            for f in &s.fields {
                visit(&f.ty, &mut acc);
            }
        }
        for e in &m.enums {
            for v in &e.variants {
                for f in &v.fields {
                    visit(&f.ty, &mut acc);
                }
            }
        }
        if let Some(eb) = &m.error {
            for c in &eb.codes {
                for f in &c.fields {
                    visit(&f.ty, &mut acc);
                }
            }
        }
    }
    acc
}

/// Render the `{T}Handle` wrapper struct for one typed-handle referent: a
/// readonly struct over the raw native pointer token. The token is opaque to
/// the consumer; the producer interprets it.
pub(crate) fn render_typed_handle_struct(out: &mut String, referent: &str) {
    let name = typed_handle_cs(referent);
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line(format!(
        "/// <summary>A typed native handle referencing a {referent}.</summary>"
    ));
    w.line(format!("public readonly struct {name}"));
    w.block("{", "}", |w| {
        w.line("internal readonly IntPtr Raw;");
        w.blank();
        w.line(format!("internal {name}(IntPtr raw)"));
        w.block("{", "}", |w| {
            w.line("Raw = raw;");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Render a plain C-style enum as a C# `enum` with its ABI discriminants.
pub(crate) fn render_enum(out: &mut String, e: &EnumBinding) {
    // A rich (algebraic) enum is not a plain C# `enum`; it surfaces as an
    // opaque-object class via `render_rich_enum_class`. Guard here so this
    // path only ever emits C-style enums.
    if e.is_rich() {
        return;
    }
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &e.doc);
    w.line(format!("public enum {}", e.name));
    w.block("{", "}", |w| {
        for v in &e.variants {
            writer_doc(w, &v.doc);
            w.line(format!("{} = {},", v.name, v.value));
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the get-only properties and the positional constructor shared by
/// record classes and rich-enum variant classes: one PascalCase property per
/// field plus a public constructor taking every field in declaration order.
pub(crate) fn render_value_members(w: &mut CodeWriter, class_name: &str, fields: &[FieldBinding]) {
    for field in fields {
        writer_doc(w, &field.doc);
        w.line(format!(
            "public {} {} {{ get; }}",
            cs_type(&field.ty),
            field.name.to_upper_camel_case()
        ));
        w.blank();
    }
    let params_sig: Vec<String> = fields
        .iter()
        .map(|f| {
            format!(
                "{} {}",
                cs_type(&f.ty),
                safe_cs_name(&f.name.to_lower_camel_case())
            )
        })
        .collect();
    w.line(format!("public {class_name}({})", params_sig.join(", ")));
    w.block("{", "}", |w| {
        for f in fields {
            w.line(format!(
                "{} = {};",
                f.name.to_upper_camel_case(),
                safe_cs_name(&f.name.to_lower_camel_case())
            ));
        }
    });
}

/// The `new {Class}(fField1, fField2, ...)` argument list matching the locals
/// [`emit_buffer_read`] declares for each field in `ReadFrom`.
pub(crate) fn read_ctor_args(fields: &[FieldBinding]) -> String {
    fields
        .iter()
        .map(|f| format!("f{}", f.name.to_upper_camel_case()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render a record as a plain sealed data class: typed get-only properties, a
/// positional constructor, and the internal `WriteTo`/`ReadFrom` pair
/// implementing the record's value-buffer encoding (fields in declaration
/// order). Records own no native resources, so there is no handle, `Dispose`,
/// builder, or getter symbol.
pub(crate) fn render_struct_class(out: &mut String, s: &StructBinding) {
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &s.doc);
    w.line(format!("public sealed class {}", s.name));
    w.line("{");
    w.indent();
    render_value_members(&mut w, &s.name, &s.fields);
    w.blank();
    w.line("internal void WriteTo(WeaveFFIBufferWriter writer)");
    w.block("{", "}", |w| {
        for f in &s.fields {
            emit_buffer_write(w, &f.ty, &f.name.to_upper_camel_case(), "writer", 0);
        }
    });
    w.blank();
    w.line(format!(
        "internal static {} ReadFrom(WeaveFFIBufferReader reader)",
        s.name
    ));
    w.block("{", "}", |w| {
        for f in &s.fields {
            emit_buffer_read(
                w,
                &f.ty,
                &format!("f{}", f.name.to_upper_camel_case()),
                "reader",
                0,
            );
        }
        w.line(format!(
            "return new {}({});",
            s.name,
            read_ctor_args(&s.fields)
        ));
    });
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as an idiomatic sum type: an abstract base
/// class with a private constructor and one nested sealed class per variant
/// (`Shape.Circle`), each carrying its fields as typed properties. The base
/// class hosts the internal `WriteTo`/`ReadFrom` pair implementing the
/// enum's value-buffer encoding: an `i32` tag followed by the active
/// variant's fields in declaration order. Rich enums own no native
/// resources and declare no C symbols.
pub(crate) fn render_rich_enum_class(out: &mut String, e: &EnumBinding) {
    let name = &e.name;
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &e.doc);
    w.line(format!("public abstract class {name}"));
    w.line("{");
    w.indent();
    // The private constructor closes the hierarchy: only the nested variant
    // classes can derive from the base.
    w.line(format!("private {name}()"));
    w.line("{");
    w.line("}");
    w.blank();

    for v in &e.variants {
        let mut vw = CodeWriter::four_space().with_depth(2);
        render_rich_variant_class(&mut vw, name, v);
        w.raw(vw.finish());
    }

    w.line("internal void WriteTo(WeaveFFIBufferWriter writer)");
    w.block("{", "}", |w| {
        w.line("switch (this)");
        w.block("{", "}", |w| {
            for v in &e.variants {
                if v.fields.is_empty() {
                    w.line(format!("case {} _:", v.name));
                    w.indent();
                    w.line(format!("writer.WriteI32({});", v.value));
                    w.line("break;");
                    w.dedent();
                } else {
                    w.line(format!("case {} v:", v.name));
                    w.indent();
                    w.line(format!("writer.WriteI32({});", v.value));
                    for f in &v.fields {
                        emit_buffer_write(
                            w,
                            &f.ty,
                            &format!("v.{}", f.name.to_upper_camel_case()),
                            "writer",
                            0,
                        );
                    }
                    w.line("break;");
                    w.dedent();
                }
            }
            w.line("default:");
            w.indent();
            w.line(format!(
                "throw new InvalidOperationException(\"unknown {name} variant\");"
            ));
            w.dedent();
        });
    });
    w.blank();
    w.line(format!(
        "internal static {name} ReadFrom(WeaveFFIBufferReader reader)"
    ));
    w.block("{", "}", |w| {
        w.line("var tag = reader.ReadI32();");
        w.line("switch (tag)");
        w.block("{", "}", |w| {
            for v in &e.variants {
                w.line(format!("case {}:", v.value));
                if v.fields.is_empty() {
                    w.indent();
                    w.line(format!("return new {}();", v.name));
                    w.dedent();
                } else {
                    w.block("{", "}", |w| {
                        for f in &v.fields {
                            emit_buffer_read(
                                w,
                                &f.ty,
                                &format!("f{}", f.name.to_upper_camel_case()),
                                "reader",
                                0,
                            );
                        }
                        w.line(format!(
                            "return new {}({});",
                            v.name,
                            read_ctor_args(&v.fields)
                        ));
                    });
                }
            }
            w.line("default:");
            w.indent();
            w.line(format!(
                "throw new InvalidOperationException(\"malformed WeaveFFI value buffer: unknown {name} tag \" + tag);"
            ));
            w.dedent();
        });
    });
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// One nested sealed variant class of a rich enum: typed get-only properties
/// and a positional constructor, exactly like a record. A unit variant has an
/// empty body with the compiler-provided constructor.
pub(crate) fn render_rich_variant_class(
    w: &mut CodeWriter,
    enum_name: &str,
    v: &EnumVariantBinding,
) {
    writer_doc(w, &v.doc);
    w.line(format!("public sealed class {} : {enum_name}", v.name));
    if v.fields.is_empty() {
        w.line("{");
        w.line("}");
    } else {
        w.line("{");
        w.indent();
        render_value_members(w, &v.name, &v.fields);
        w.dedent();
        w.line("}");
    }
    w.blank();
}

/// Render one interface as an opaque-handle class following the struct-wrapper
/// pattern: a private `IntPtr` handle with `IDisposable` plus a finalizer
/// calling the interface's destroy symbol. The `new` constructor maps to a
/// real C# constructor, other constructors become static factories, instance
/// methods pass the handle as the leading native argument, and statics are
/// plain static methods. All member shapes reuse the free-function
/// marshalling paths.
pub(crate) fn render_interface_class(
    out: &mut String,
    i: &InterfaceBinding,
    error: Option<&ErrorBinding>,
) {
    let name = &i.name;
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &i.doc);
    w.line(format!("public class {name} : IDisposable"));
    w.line("{");
    w.indent();
    w.line("private IntPtr _handle;");
    w.line("private bool _disposed;");
    w.blank();
    w.line(format!("internal {name}(IntPtr handle)"));
    w.block("{", "}", |w| {
        w.line("_handle = handle;");
    });
    w.blank();
    w.line("internal IntPtr Handle => _handle;");
    w.blank();

    for c in &i.constructors {
        let err = ErrCtx::for_fn(c, error);
        let mut tmp = String::new();
        if c.name == "new" && matches!(c.shape, CallShape::Sync(_)) {
            render_interface_ctor(&mut tmp, i, c, err);
        } else {
            render_wrapper_method(&mut tmp, c, &c.name.to_upper_camel_case(), None, err);
        }
        w.raw(tmp);
    }
    for m in &i.methods {
        let err = ErrCtx::for_fn(m, error);
        let mut tmp = String::new();
        render_wrapper_method(
            &mut tmp,
            m,
            &m.name.to_upper_camel_case(),
            Some("_handle"),
            err,
        );
        w.raw(tmp);
    }
    for s in &i.statics {
        let err = ErrCtx::for_fn(s, error);
        let mut tmp = String::new();
        render_wrapper_method(&mut tmp, s, &s.name.to_upper_camel_case(), None, err);
        w.raw(tmp);
    }

    w.line("public void Dispose()");
    w.block("{", "}", |w| {
        w.line("if (!_disposed)");
        w.block("{", "}", |w| {
            w.line(format!("NativeMethods.{}(_handle);", i.destroy_symbol));
            w.line("_disposed = true;");
        });
        w.line("GC.SuppressFinalize(this);");
    });
    w.blank();
    w.line(format!("~{name}()"));
    w.block("{", "}", |w| {
        w.line("Dispose();");
    });
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render the `new` constructor as a real C# constructor: the sync call path
/// with the checked result assigned to `_handle` instead of returned.
pub(crate) fn render_interface_ctor(
    out: &mut String,
    i: &InterfaceBinding,
    f: &FnBinding,
    err: ErrCtx,
) {
    let f = camel_fn(f);
    let c_sym = &f.c_base;
    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }
    w.line(format!("public {}({})", i.name, params_sig.join(", ")));
    w.line("{");
    w.scope(|w| {
        w.line("var err = new WeaveFFIError();");
        let call_args = build_call_args(&f.params);
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        let call = format!("var result = NativeMethods.{c_sym}({args_part}ref err);");

        let needs_try = f.params.iter().any(param_needs_marshal);
        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line(call.clone());
                w.line(err.check_stmt());
                w.line("_handle = result;");
            });
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            w.line(call);
            w.line(err.check_stmt());
            w.line("_handle = result;");
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}
