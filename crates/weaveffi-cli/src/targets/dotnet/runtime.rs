//! The fixed runtime (`runtime/Runtime.cs`, spliced with the library's
//! names) and the per-domain exception hierarchies.

use crate::codegen::CodeWriter;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::errors;
use weaveffi_model::model::{ErrorBinding, ErrorCodeBinding, Model, ModuleBinding, ABI_VERSION};

use crate::targets::dotnet::codec::{read_expr, write_stmt};
use crate::targets::dotnet::docs::write_doc;
use crate::targets::dotnet::types::{cs_str, cs_type, safe_cs_name, xml_text, Cx};

/// The runtime source shared by every generated library.
const RUNTIME_CS: &str = include_str!("runtime/Runtime.cs");

/// The names a generated library splices into the fixed runtime.
pub(crate) struct RuntimeNames<'a> {
    /// The C# namespace.
    pub namespace: &'a str,
    /// The root exception class (see [`exception_names`]).
    pub exception: &'a str,
    /// The trap exception class (see [`exception_names`]).
    pub bug_exception: &'a str,
    /// The C symbol prefix.
    pub prefix: &'a str,
    /// The native library base name P/Invoke loads.
    pub library: &'a str,
    /// The `{PREFIX}_LIBRARY` override variable.
    pub library_env: &'a str,
}

/// Render `Runtime.cs`: the fixed runtime with every placeholder replaced.
pub(crate) fn render_runtime(names: &RuntimeNames<'_>, filename: &str) -> String {
    let body = RUNTIME_CS
        .replace("{{NAMESPACE}}", names.namespace)
        .replace("{{BUG_EXCEPTION}}", names.bug_exception)
        .replace("{{EXCEPTION}}", names.exception)
        .replace("{{PREFIX}}", names.prefix)
        .replace("{{LIBRARY}}", names.library)
        .replace("{{LIBRARY_ENV}}", names.library_env)
        .replace("{{ABI_VERSION}}", &ABI_VERSION.to_string());
    debug_assert!(!body.contains("{{"), "unfilled placeholder in Runtime.cs");
    format!(
        "{}{body}\n{}",
        render_prelude(CommentStyle::DoubleSlash),
        render_trailer(CommentStyle::DoubleSlash, filename)
    )
}

/// The runtime's two exception classes: the root of every error a throwing
/// call reports (`NativeException`) and the trap a failed non-throwing call
/// raises (`NativeBugException`). A name the API already declares is
/// qualified by the namespace (`KvstoreNativeException`).
pub(crate) fn exception_names(model: &Model, namespace: &str) -> (String, String) {
    let taken = |name: &str| {
        model.modules.iter().any(|m| {
            m.structs.iter().any(|s| s.name == name)
                || m.enums.iter().any(|e| e.name == name)
                || m.interfaces.iter().any(|i| i.name == name)
                || m.callback_interfaces.iter().any(|c| c.name == name)
                || m.errors
                    .as_ref()
                    .is_some_and(|e| dotnet_exception_name(e) == name)
        })
    };
    let pick = |name: &str| {
        if taken(name) {
            format!("{namespace}{name}")
        } else {
            name.to_string()
        }
    };
    (pick("NativeException"), pick("NativeBugException"))
}

/// The C# exception class for one error domain: the domain stem with one
/// `Exception` suffix, so `KvError` becomes `KvException`.
pub(crate) fn dotnet_exception_name(eb: &ErrorBinding) -> String {
    errors::exception_type_name(&eb.type_name)
}

/// The nested exception class for one code (`KvException.KeyNotFound`).
fn code_class(c: &ErrorCodeBinding) -> String {
    errors::pascal(&c.name)
}

/// The exception hierarchy of the error domain `eb`, declared by `module`:
/// an abstract class deriving from the root exception with one nested
/// sealed class per code. Each code class exposes its fields as typed
/// properties and can be constructed by a callback-interface implementation,
/// which reports it (code, message, and fields) to the native caller.
/// `FromError` maps a raw error to the typed exception, decoding the
/// payload; an undeclared code falls back to the root exception.
pub(crate) fn render_domain_exception(
    w: &mut CodeWriter,
    module: &ModuleBinding,
    eb: &ErrorBinding,
    cx: Cx<'_>,
) {
    let exc = dotnet_exception_name(eb);
    let base = cx.base;
    w.line(format!(
        "/// <summary>The errors of the <c>{}</c> domain, declared by module <c>{}</c>.",
        eb.name, module.dot_path
    ));
    w.line("/// Catch a nested class for one code, or this class for any of them.</summary>");
    w.line(format!("public abstract class {exc} : {base}"));
    w.block("{", "}", |w| {
        w.line(format!(
            "private protected {exc}(int code, string message) : base(code, message)"
        ));
        w.line("{");
        w.line("}");
        w.blank();
        w.line("/// <summary>Writes this code's fields, the error payload a callback");
        w.line("/// reports with it.</summary>");
        w.line("internal virtual void WritePayload(FfiBufferWriter writer)");
        w.line("{");
        w.line("}");
        w.blank();
        for c in &eb.codes {
            render_code_class(w, &exc, c);
        }
        w.line("/// <summary>The exception for a raw code and its serialized payload.</summary>");
        w.line(
            "internal static new Exception FromError(int code, string message, byte[]? payload)",
        );
        w.block("{", "}", |w| {
            w.line("var given = message.Length == 0 ? null : message;");
            w.line("switch (code)");
            w.block("{", "}", |w| {
                for c in &eb.codes {
                    let class = code_class(c);
                    w.line(format!("case {class}.ErrorCode:"));
                    if c.fields.is_empty() {
                        w.indent();
                        w.line(format!("return new {class}(given);"));
                        w.dedent();
                        continue;
                    }
                    w.block("{", "}", |w| {
                        w.line("var reader = new FfiBufferReader(payload ?? Array.Empty<byte>());");
                        w.line(format!("var e = new {class}("));
                        w.indent();
                        for f in &c.fields {
                            w.line(format!("{},", read_expr(cx, &f.ty, "reader")));
                        }
                        w.line("given);");
                        w.dedent();
                        w.line("reader.ExpectEnd();");
                        w.line("return e;");
                    });
                }
                w.line("default:");
                w.indent();
                w.line(format!(
                    "return {}.FromError(code, message, payload);",
                    cx.ty(base)
                ));
                w.dedent();
            });
        });
    });
    w.blank();
}

/// One code's nested exception class.
fn render_code_class(w: &mut CodeWriter, exc: &str, c: &ErrorCodeBinding) {
    let class = code_class(c);
    if c.doc.is_some() {
        write_doc(w, &c.doc);
    } else {
        w.line(format!("/// <summary>{}</summary>", xml_text(&c.message)));
    }
    w.line(format!("public sealed class {class} : {exc}"));
    w.block("{", "}", |w| {
        w.line(format!(
            "/// <summary>The domain code, <c>{}</c>.</summary>",
            c.value
        ));
        w.line(format!("public const int ErrorCode = {};", c.value));
        w.blank();
        for f in &c.fields {
            write_doc(w, &f.doc);
            w.line(format!(
                "public {} {} {{ get; }}",
                cs_type(&f.ty),
                f.name.to_upper_camel_case()
            ));
            w.blank();
        }
        let mut params: Vec<String> = c
            .fields
            .iter()
            .map(|f| {
                format!(
                    "{} {}",
                    cs_type(&f.ty),
                    safe_cs_name(&f.name.to_lower_camel_case())
                )
            })
            .collect();
        params.push("string? message = null".into());
        w.line("/// <summary>Creates the error with <paramref name=\"message\"/>, or the");
        w.line(format!(
            "/// default message \"{}\" when it's null.</summary>",
            xml_text(&c.message)
        ));
        w.line(format!(
            "public {class}({}) : base(ErrorCode, message ?? \"{}\")",
            params.join(", "),
            cs_str(&c.message)
        ));
        w.block("{", "}", |w| {
            for f in &c.fields {
                w.line(format!(
                    "{} = {};",
                    f.name.to_upper_camel_case(),
                    safe_cs_name(&f.name.to_lower_camel_case())
                ));
            }
        });
        if !c.fields.is_empty() {
            w.blank();
            w.line("internal override void WritePayload(FfiBufferWriter writer)");
            w.block("{", "}", |w| {
                for f in &c.fields {
                    w.line(write_stmt(&f.ty, "writer", &f.name.to_upper_camel_case()));
                }
            });
        }
    });
    w.blank();
}
