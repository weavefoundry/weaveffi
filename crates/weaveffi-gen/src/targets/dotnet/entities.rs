//! Entity renderers: C-style enums, records, rich (algebraic) enums, and the
//! interface wrapper classes.

use crate::codegen::CodeWriter;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::model::{
    CallShape, EnumBinding, EnumVariantBinding, ErrorBinding, FieldBinding, InterfaceBinding,
    StructBinding,
};

use crate::targets::dotnet::calls::{render_callable, write_obsolete, ErrCtx, Receiver};
use crate::targets::dotnet::codec::{emit_read, emit_write};
use crate::targets::dotnet::docs::write_doc;
use crate::targets::dotnet::types::{cs_type, safe_cs_name, Cx};

/// Render a C-style enum as a C# `enum` with its ABI discriminants.
pub(crate) fn render_enum(w: &mut CodeWriter, e: &EnumBinding) {
    write_doc(w, &e.doc);
    write_obsolete(w, &e.deprecated);
    w.line(format!("public enum {}", e.name));
    w.block("{", "}", |w| {
        for v in &e.variants {
            write_doc(w, &v.doc);
            w.line(format!("{} = {},", v.name, v.value));
        }
    });
    w.blank();
}

/// The get-only properties and positional constructor shared by records and
/// rich-enum variants.
fn render_value_members(w: &mut CodeWriter, class: &str, fields: &[FieldBinding]) {
    for f in fields {
        write_doc(w, &f.doc);
        w.line(format!(
            "public {} {} {{ get; }}",
            cs_type(&f.ty),
            f.name.to_upper_camel_case()
        ));
        w.blank();
    }
    let params: Vec<String> = fields
        .iter()
        .map(|f| {
            format!(
                "{} {}",
                cs_type(&f.ty),
                safe_cs_name(&f.name.to_lower_camel_case())
            )
        })
        .collect();
    w.line(format!(
        "/// <summary>Creates a <see cref=\"{class}\"/> from every field.</summary>"
    ));
    w.line(format!("public {class}({})", params.join(", ")));
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

/// Emit the reads of every field into `f{Field}` locals, then return the
/// constructed `class`.
fn render_field_reads(w: &mut CodeWriter, cx: Cx<'_>, class: &str, fields: &[FieldBinding]) {
    let mut args = Vec::new();
    for f in fields {
        let var = format!("f{}", f.name.to_upper_camel_case());
        emit_read(w, cx, &f.ty, &var, "reader", 0);
        args.push(var);
    }
    w.line(format!("return new {class}({});", args.join(", ")));
}

/// Render a record as a sealed data class: get-only properties, a
/// positional constructor, and the internal `WriteTo`/`ReadFrom` pair
/// implementing its value-buffer encoding (fields in declaration order).
pub(crate) fn render_record(w: &mut CodeWriter, cx: Cx<'_>, s: &StructBinding) {
    write_doc(w, &s.doc);
    write_obsolete(w, &s.deprecated);
    w.line(format!("public sealed class {}", s.name));
    w.block("{", "}", |w| {
        render_value_members(w, &s.name, &s.fields);
        w.blank();
        w.line("internal void WriteTo(FfiBufferWriter writer)");
        w.block("{", "}", |w| {
            for f in &s.fields {
                emit_write(w, &f.ty, &f.name.to_upper_camel_case(), "writer", 0);
            }
        });
        w.blank();
        w.line(format!(
            "internal static {} ReadFrom(FfiBufferReader reader)",
            s.name
        ));
        w.block("{", "}", |w| {
            render_field_reads(w, cx, &s.name, &s.fields);
        });
    });
    w.blank();
}

/// Render a rich enum as a closed class hierarchy: an abstract base with a
/// private constructor and one nested sealed class per variant
/// (`Shape.Circle`). The base hosts the codec: an `i32` tag, then the active
/// variant's fields.
pub(crate) fn render_rich_enum(w: &mut CodeWriter, cx: Cx<'_>, e: &EnumBinding) {
    let name = &e.name;
    write_doc(w, &e.doc);
    write_obsolete(w, &e.deprecated);
    w.line(format!("public abstract class {name}"));
    w.block("{", "}", |w| {
        w.line(format!("private {name}()"));
        w.line("{");
        w.line("}");
        w.blank();
        for v in &e.variants {
            render_variant(w, name, v);
        }
        w.line("internal void WriteTo(FfiBufferWriter writer)");
        w.block("{", "}", |w| {
            w.line("switch (this)");
            w.block("{", "}", |w| {
                for v in &e.variants {
                    let binding = if v.fields.is_empty() { "_" } else { "v" };
                    w.line(format!("case {} {binding}:", v.name));
                    w.scope(|w| {
                        w.line(format!("writer.WriteI32({});", v.value));
                        for f in &v.fields {
                            let expr = format!("v.{}", f.name.to_upper_camel_case());
                            emit_write(w, &f.ty, &expr, "writer", 0);
                        }
                        w.line("break;");
                    });
                }
                w.line("default:");
                w.scope(|w| {
                    w.line(format!(
                        "throw new InvalidOperationException(\"unknown {name} variant\");"
                    ));
                });
            });
        });
        w.blank();
        w.line(format!(
            "internal static {name} ReadFrom(FfiBufferReader reader)"
        ));
        w.block("{", "}", |w| {
            w.line("var tag = reader.ReadI32();");
            w.line("switch (tag)");
            w.block("{", "}", |w| {
                for v in &e.variants {
                    w.line(format!("case {}:", v.value));
                    w.block("{", "}", |w| {
                        render_field_reads(w, cx, &v.name, &v.fields);
                    });
                }
                w.line("default:");
                w.scope(|w| {
                    w.line(format!(
                        "throw new InvalidOperationException(\"malformed value buffer: unknown {name} tag \" + tag);"
                    ));
                });
            });
        });
    });
    w.blank();
}

/// One nested sealed variant class of a rich enum.
fn render_variant(w: &mut CodeWriter, enum_name: &str, v: &EnumVariantBinding) {
    write_doc(w, &v.doc);
    w.line(format!("public sealed class {} : {enum_name}", v.name));
    w.block("{", "}", |w| {
        if !v.fields.is_empty() {
            render_value_members(w, &v.name, &v.fields);
        }
    });
    w.blank();
}

/// Render one interface as a sealed wrapper over a `SafeHandle` subclass
/// holding one strong reference. The handle's `ReleaseHandle` calls the
/// interface's `_destroy`; every call passes the handle itself, so the
/// interop stub keeps the object alive for the call even if the wrapper is
/// disposed or collected meanwhile. Two wrappers are equal when they
/// reference the same native object.
pub(crate) fn render_interface(
    w: &mut CodeWriter,
    i: &InterfaceBinding,
    error: Option<&ErrorBinding>,
    cx: Cx<'_>,
) {
    let name = &i.name;
    write_doc(w, &i.doc);
    write_obsolete(w, &i.deprecated);
    w.line(format!(
        "public sealed unsafe class {name} : IDisposable, IEquatable<{name}>"
    ));
    w.block("{", "}", |w| {
        w.line("/// <summary>The native object; borrowed by every call.</summary>");
        w.line("internal NativeHandle Handle { get; }");
        w.blank();
        w.line(format!("private {name}(NativeHandle handle)"));
        w.block("{", "}", |w| {
            w.line("Handle = handle;");
        });
        w.blank();
        w.line("/// <summary>Wraps one strong reference the caller transfers.</summary>");
        w.line(format!("internal static {name} Adopt(IntPtr ptr)"));
        w.block("{", "}", |w| {
            w.line(format!("return new {name}(new NativeHandle(ptr));"));
        });
        w.blank();
        w.line("/// <summary>Mints a second strong reference the caller owns, as an");
        w.line("/// object token in a value buffer requires.</summary>");
        w.line("internal IntPtr CloneHandle()");
        w.block("{", "}", |w| {
            w.line(format!("return NativeMethods.{}(Handle);", i.clone_symbol));
        });
        w.blank();

        for c in &i.constructors {
            let err = ErrCtx::for_fn(c, error, cx);
            let method = c.name.to_upper_camel_case();
            let receiver = if c.name == "new" && matches!(c.shape, CallShape::Sync(_)) {
                Receiver::Constructor(name)
            } else {
                Receiver::Static
            };
            render_callable(w, c, &method, receiver, &err);
        }
        for m in &i.methods {
            let err = ErrCtx::for_fn(m, error, cx);
            render_callable(w, m, &m.name.to_upper_camel_case(), Receiver::Instance, &err);
        }
        for s in &i.statics {
            let err = ErrCtx::for_fn(s, error, cx);
            render_callable(w, s, &s.name.to_upper_camel_case(), Receiver::Static, &err);
        }

        w.line("/// <summary>Releases this wrapper's reference. The native object is");
        w.line("/// dropped once its last reference is released, and an in-flight");
        w.line("/// call finishes first.</summary>");
        w.line("public void Dispose()");
        w.block("{", "}", |w| {
            w.line("Handle.Dispose();");
        });
        w.blank();
        w.line("/// <summary>True when both wrappers reference the same native object.</summary>");
        w.line(format!("public bool Equals({name}? other)"));
        w.block("{", "}", |w| {
            w.line("return other is not null && other.Handle.DangerousGetHandle() == Handle.DangerousGetHandle();");
        });
        w.blank();
        w.line("/// <inheritdoc/>");
        w.line("public override bool Equals(object? obj)");
        w.block("{", "}", |w| {
            w.line(format!("return obj is {name} other && Equals(other);"));
        });
        w.blank();
        w.line("/// <inheritdoc/>");
        w.line("public override int GetHashCode()");
        w.block("{", "}", |w| {
            w.line("return Handle.DangerousGetHandle().GetHashCode();");
        });
        w.blank();
        w.line("/// <summary>One strong reference, released through");
        w.line(format!("/// <c>{}</c>.</summary>", i.destroy_symbol));
        w.line("internal sealed class NativeHandle : SafeHandle");
        w.block("{", "}", |w| {
            w.line("/// <summary>A null handle, passed for an absent optional object.</summary>");
            w.line("internal static readonly NativeHandle Null = new NativeHandle();");
            w.blank();
            w.line("public NativeHandle() : base(IntPtr.Zero, true)");
            w.line("{");
            w.line("}");
            w.blank();
            w.line("internal NativeHandle(IntPtr ptr) : base(IntPtr.Zero, true)");
            w.block("{", "}", |w| {
                w.line("SetHandle(ptr);");
            });
            w.blank();
            w.line("public override bool IsInvalid => handle == IntPtr.Zero;");
            w.blank();
            w.line("protected override bool ReleaseHandle()");
            w.block("{", "}", |w| {
                w.line(format!("NativeMethods.{}(handle);", i.destroy_symbol));
                w.line("return true;");
            });
        });
    });
    w.blank();
}
