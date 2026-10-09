//! Entity renderers: C-style enums, records, rich (algebraic) enums, and the
//! interface wrapper classes.

use weaveffi_model::model::{
    CallShape, EnumBinding, FieldBinding, InterfaceBinding, Model, StructBinding,
};

use crate::codegen::CodeWriter;
use crate::targets::dotnet::calls::{render_callable, Receiver};
use crate::targets::dotnet::codec::{
    equal_expr, hash_expr, needs_deep_equality, read_expr, write_call,
};
use crate::targets::dotnet::docs::Docs;
use crate::targets::dotnet::errors::ErrCtx;
use crate::targets::dotnet::types::{cs_member, cs_type, field_cs, Cx};

/// Render a C-style enum as a C# `enum` with its ABI discriminants (an
/// `int` underneath, like the C typedef).
pub(crate) fn render_enum(w: &mut CodeWriter, docs: &Docs, e: &EnumBinding) {
    docs.summary(w, &e.doc);
    docs.obsolete(w, &e.deprecated);
    w.line(format!("public enum {}", e.name));
    w.block("{", "}", |w| {
        for v in &e.variants {
            docs.summary(w, &v.doc);
            w.line(format!("{} = {},", v.name, v.value));
        }
    });
    w.blank();
}

/// The positional parameter list of a record or variant: one PascalCase
/// property per field.
fn positional(class: &str, fields: &[FieldBinding]) -> String {
    fields
        .iter()
        .map(|f| format!("{} {}", cs_type(&f.ty), field_cs(&f.name, class)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `<param>` docs of a positional record's properties.
fn field_docs(w: &mut CodeWriter, docs: &Docs, class: &str, fields: &[FieldBinding]) {
    for f in fields {
        docs.param(w, &field_cs(&f.name, class), &f.doc);
    }
}

/// The value equality of a record whose fields include a byte array, list,
/// or map, which a record would otherwise compare by reference: `Equals`
/// compares their contents, and `GetHashCode` agrees with it.
fn render_equality(w: &mut CodeWriter, class: &str, fields: &[FieldBinding]) {
    if !fields.iter().any(|f| needs_deep_equality(&f.ty)) {
        return;
    }
    w.line("/// <summary>True when every field of <paramref name=\"other\"/> is equal to this");
    w.line("/// one's, comparing byte arrays, lists, and maps by their contents.</summary>");
    w.line(format!("public bool Equals({class}? other)"));
    w.block("{", "}", |w| {
        w.line("return ReferenceEquals(this, other) || other is not null");
        w.indent();
        for (i, f) in fields.iter().enumerate() {
            let p = field_cs(&f.name, class);
            let end = if i + 1 == fields.len() { ";" } else { "" };
            w.line(format!(
                "&& {}{end}",
                equal_expr(&f.ty, &p, &format!("other.{p}"), 0)
            ));
        }
        w.dedent();
    });
    w.blank();
    w.line("/// <inheritdoc/>");
    w.line("public override int GetHashCode()");
    w.block("{", "}", |w| {
        w.line("var hash = new global::System.HashCode();");
        for f in fields {
            w.line(format!(
                "hash.Add({});",
                hash_expr(&f.ty, &field_cs(&f.name, class))
            ));
        }
        w.line("return hash.ToHashCode();");
    });
    w.blank();
}

/// `new {class}(...)` reading every field in declaration (and wire) order,
/// one argument per line after the first line when there are several. C#
/// evaluates arguments left to right, so the reads happen in order.
fn construct(cx: Cx<'_>, class: &str, fields: &[FieldBinding], indent: &str) -> String {
    let reads: Vec<String> = fields
        .iter()
        .map(|f| read_expr(cx, &f.ty, "reader", 0))
        .collect();
    if reads.len() < 2 {
        return format!("new {class}({})", reads.join(""));
    }
    let sep = format!(",\n{indent}    ");
    format!("new {class}(\n{indent}    {})", reads.join(&sep))
}

/// Render a record as a positional `sealed record` (value equality, `with`,
/// deconstruction), plus the internal `WriteTo`/`ReadFrom` pair
/// implementing its value-buffer encoding (fields in declaration order).
pub(crate) fn render_record(w: &mut CodeWriter, cx: Cx<'_>, docs: &Docs, s: &StructBinding) {
    let name = &s.name;
    docs.summary(w, &s.doc);
    field_docs(w, docs, name, &s.fields);
    docs.obsolete(w, &s.deprecated);
    w.line(format!(
        "public sealed record {name}({})",
        positional(name, &s.fields)
    ));
    w.block("{", "}", |w| {
        render_equality(w, name, &s.fields);
        w.line("internal void WriteTo(FfiBufferWriter writer)");
        w.block("{", "}", |w| {
            for f in &s.fields {
                let value = field_cs(&f.name, name);
                w.line(format!("{};", write_call(&f.ty, "writer", &value, 0)));
            }
        });
        w.blank();
        w.line(format!(
            "internal static {name} ReadFrom(FfiBufferReader reader)"
        ));
        w.block("{", "}", |w| {
            let indent = w.indent_str();
            w.line(format!(
                "return {};",
                construct(cx, name, &s.fields, &indent)
            ));
        });
    });
    w.blank();
}

/// Render a rich enum as a closed record hierarchy: an abstract record with
/// a private constructor and one nested sealed record per variant
/// (`Shape.Circle`). The base hosts the codec: an `i32` tag, then the
/// active variant's fields.
pub(crate) fn render_rich_enum(w: &mut CodeWriter, cx: Cx<'_>, docs: &Docs, e: &EnumBinding) {
    let name = &e.name;
    docs.summary(w, &e.doc);
    docs.obsolete(w, &e.deprecated);
    w.line(format!("public abstract record {name}"));
    w.block("{", "}", |w| {
        w.line(format!("private {name}()"));
        w.line("{");
        w.line("}");
        w.blank();
        for v in &e.variants {
            docs.summary(w, &v.doc);
            field_docs(w, docs, &v.name, &v.fields);
            if v.fields.is_empty() {
                w.line(format!("public sealed record {} : {name};", v.name));
            } else {
                w.line(format!(
                    "public sealed record {}({}) : {name}",
                    v.name,
                    positional(&v.name, &v.fields)
                ));
                if v.fields.iter().any(|f| needs_deep_equality(&f.ty)) {
                    w.block("{", "}", |w| render_equality(w, &v.name, &v.fields));
                } else {
                    w.line("{");
                    w.line("}");
                }
            }
            w.blank();
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
                            let value = format!("v.{}", field_cs(&f.name, &v.name));
                            w.line(format!("{};", write_call(&f.ty, "writer", &value, 0)));
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
            w.line("return tag switch");
            w.line("{");
            w.indent();
            let indent = w.indent_str();
            for v in &e.variants {
                w.line(format!(
                    "{} => {},",
                    v.value,
                    construct(cx, &v.name, &v.fields, &indent)
                ));
            }
            w.line(format!(
                "_ => throw FfiBufferReader.Malformed(\"unknown {name} tag \" + tag),"
            ));
            w.dedent();
            w.line("};");
        });
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
    model: &Model,
    docs: &Docs,
    i: &InterfaceBinding,
    cx: Cx<'_>,
) {
    let name = &i.name;
    docs.summary(w, &i.doc);
    docs.obsolete(w, &i.deprecated);
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
            let err = ErrCtx::new(model, &c.error, cx);
            let method = cs_member(&c.name, name);
            let receiver = if c.name == "new" && matches!(c.shape, CallShape::Sync) && c.iterator().is_none() {
                Receiver::Constructor(name)
            } else {
                Receiver::Static
            };
            render_callable(w, docs, c, &method, receiver, &err, cx);
        }
        for m in &i.methods {
            let err = ErrCtx::new(model, &m.error, cx);
            render_callable(w, docs, m, &cs_member(&m.name, name), Receiver::Instance, &err, cx);
        }
        for s in &i.statics {
            let err = ErrCtx::new(model, &s.error, cx);
            render_callable(w, docs, s, &cs_member(&s.name, name), Receiver::Static, &err, cx);
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
