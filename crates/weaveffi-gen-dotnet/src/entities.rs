//! Entity renderers: C-style enums, rich (algebraic) enums, records, and
//! the reference-counted interface wrapper classes.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    CallShape, EnumBinding, EnumVariantBinding, ErrorBinding, FieldBinding, FnBinding,
    InterfaceBinding, StructBinding,
};

use crate::calls::{
    build_call_args, params_sig, render_marshalled_call, render_wrapper_method, write_obsolete,
    ErrCtx,
};
use crate::codec::{emit_buffer_read, emit_buffer_write};
use crate::docs::{writer_doc, writer_fn_doc};
use crate::types::{camel_fn, cs_type, safe_cs_name};

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

/// Render one interface as a reference-counted object wrapper: a private
/// `IntPtr` holding one strong reference, `IDisposable` plus a finalizer
/// backstop that release it through the interface's `destroy` symbol exactly
/// once, and an internal `CloneHandle` that mints a second strong reference
/// through the `clone` symbol (what the value-buffer codec writes as an
/// object token). The `new` constructor maps to a real C# constructor, other
/// constructors become static factories, instance methods pass the handle as
/// the leading native argument, and statics are plain static methods. All
/// member shapes reuse the free-function marshalling paths.
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
    w.line("private int _released;");
    w.blank();
    w.line("/// <summary>Adopts one strong reference to a native object.</summary>");
    w.line(format!("internal {name}(WeaveFFIHandle handle)"));
    w.block("{", "}", |w| {
        w.line("_handle = handle.Value;");
    });
    w.blank();
    w.line("/// <summary>The borrowed native pointer for the duration of a call.</summary>");
    w.line("/// <exception cref=\"ObjectDisposedException\">The wrapper was disposed.</exception>");
    w.line("internal IntPtr Handle");
    w.block("{", "}", |w| {
        w.line("get");
        w.block("{", "}", |w| {
            w.line("var h = _handle;");
            w.line("if (h == IntPtr.Zero)");
            w.block("{", "}", |w| {
                w.line(format!(
                    "throw new ObjectDisposedException(nameof({name}));"
                ));
            });
            w.line("return h;");
        });
    });
    w.blank();
    w.line("/// <summary>Mints a second strong reference the caller owns (for");
    w.line("/// example to write this object into a value buffer).</summary>");
    w.line("internal IntPtr CloneHandle()");
    w.block("{", "}", |w| {
        w.line(format!("return NativeMethods.{}(Handle);", i.clone_symbol));
    });
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
    // Methods borrow the receiver through the checked `Handle` property, so a
    // call on a disposed wrapper throws ObjectDisposedException instead of
    // handing the producer a null self pointer.
    for m in &i.methods {
        let err = ErrCtx::for_fn(m, error);
        let mut tmp = String::new();
        render_wrapper_method(
            &mut tmp,
            m,
            &m.name.to_upper_camel_case(),
            Some("Handle"),
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

    w.line("/// <summary>Releases this wrapper's reference. The native object is");
    w.line("/// dropped when the producer releases its last reference; other");
    w.line("/// wrappers to the same object stay valid.</summary>");
    w.line("public void Dispose()");
    w.block("{", "}", |w| {
        w.line("Release();");
        w.line("GC.SuppressFinalize(this);");
    });
    w.blank();
    w.line(format!("~{name}()"));
    w.block("{", "}", |w| {
        w.line("Release();");
    });
    w.blank();
    // Interlocked makes the release idempotent even when Dispose races the
    // finalizer or a concurrent Dispose; `destroy` runs at most once.
    w.line("private void Release()");
    w.block("{", "}", |w| {
        w.line("if (System.Threading.Interlocked.Exchange(ref _released, 1) != 0)");
        w.block("{", "}", |w| {
            w.line("return;");
        });
        w.line("var h = _handle;");
        w.line("_handle = IntPtr.Zero;");
        w.line("if (h != IntPtr.Zero)");
        w.block("{", "}", |w| {
            w.line(format!("NativeMethods.{}(h);", i.destroy_symbol));
        });
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

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    write_obsolete(&mut w, &f.deprecated);
    w.line(format!("public {}({})", i.name, params_sig(&f.params)));
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
        render_marshalled_call(w, &f.params, |w| {
            w.line(call);
            w.line(err.check_stmt());
            w.line("_handle = result;");
        });
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}
