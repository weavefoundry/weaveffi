//! The fixed runtime (`runtime/Runtime.cs`, spliced with the library's
//! names) and the per-domain typed exceptions.

use crate::codegen::CodeWriter;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use heck::ToUpperCamelCase;
use weaveffi_model::errors;
use weaveffi_model::model::{BindingModel, ErrorBinding, ABI_VERSION};

use crate::targets::dotnet::codec::emit_read;
use crate::targets::dotnet::docs::write_doc;
use crate::targets::dotnet::types::{cs_str, xml_text, Cx};

/// The runtime source shared by every generated library.
const RUNTIME_CS: &str = include_str!("runtime/Runtime.cs");

/// The names a generated library splices into the fixed runtime.
pub(crate) struct RuntimeNames<'a> {
    /// The C# namespace.
    pub namespace: &'a str,
    /// The base exception class (see [`base_exception_name`]).
    pub exception: &'a str,
    /// The C symbol prefix.
    pub prefix: &'a str,
    /// The native library base name P/Invoke loads.
    pub library: &'a str,
    /// The `{PREFIX}_LIBRARY` override variable.
    pub library_env: &'a str,
}

/// Render `Runtime.cs`: the fixed runtime with every placeholder replaced.
pub(crate) fn render_runtime(
    names: &RuntimeNames<'_>,
    input_basename: &str,
    filename: &str,
) -> String {
    let body = RUNTIME_CS
        .replace("{{NAMESPACE}}", names.namespace)
        .replace("{{EXCEPTION}}", names.exception)
        .replace("{{PREFIX}}", names.prefix)
        .replace("{{LIBRARY}}", names.library)
        .replace("{{LIBRARY_ENV}}", names.library_env)
        .replace("{{ABI_VERSION}}", &ABI_VERSION.to_string());
    format!(
        "{}{body}\n{}",
        render_prelude(CommentStyle::DoubleSlash, input_basename),
        render_trailer(CommentStyle::DoubleSlash, filename)
    )
}

/// The base exception every failure derives from: `NativeException`, unless
/// the API declares a type of that name, in which case the namespace
/// qualifies it (`KvstoreNativeException`).
pub(crate) fn base_exception_name(model: &BindingModel, namespace: &str) -> String {
    const NAME: &str = "NativeException";
    let taken = model.modules.iter().any(|m| {
        m.structs.iter().any(|s| s.name == NAME)
            || m.enums.iter().any(|e| e.name == NAME)
            || m.interfaces.iter().any(|i| i.name == NAME)
            || m.error
                .as_ref()
                .is_some_and(|e| dotnet_exception_name(e) == NAME)
    });
    if taken {
        format!("{namespace}{NAME}")
    } else {
        NAME.to_string()
    }
}

/// The C# exception class for one error domain: the domain stem with one
/// `Exception` suffix, so `KvError` becomes `KvException`.
pub(crate) fn dotnet_exception_name(eb: &ErrorBinding) -> String {
    errors::exception_type_name(&eb.type_name)
}

/// One typed exception class per declared error domain, deriving from the
/// base exception. Each code is a `public const int`, and `FromError` maps a
/// raw error to the typed exception, decoding any payload fields into
/// `Data` (keyed by the IDL field name). Undeclared codes (the negative
/// runtime codes) fall back to the base exception's mapping.
pub(crate) fn render_domain_exception(w: &mut CodeWriter, eb: &ErrorBinding, cx: Cx<'_>) {
    let exc = dotnet_exception_name(eb);
    let base = cx.base;
    w.line(format!(
        "/// <summary>Typed exception for the {} error domain (module {}).</summary>",
        eb.type_name,
        eb.owner_path.replace('_', ".")
    ));
    w.line(format!("public class {exc} : {base}"));
    w.block("{", "}", |w| {
        for c in &eb.codes {
            if c.doc.is_some() {
                write_doc(w, &c.doc);
            } else {
                w.line(format!("/// <summary>{}</summary>", xml_text(&c.message)));
            }
            w.line(format!(
                "public const int {} = {};",
                errors::pascal(&c.name),
                c.value
            ));
            w.blank();
        }
        w.line("/// <summary>Creates an exception carrying a domain error code.</summary>");
        w.line(format!(
            "public {exc}(int code, string message) : base(code, message)"
        ));
        w.line("{");
        w.line("}");
        w.blank();
        w.line(
            "internal static new Exception FromError(int code, string message, byte[]? payload)",
        );
        w.block("{", "}", |w| {
            w.line("switch (code)");
            w.block("{", "}", |w| {
                for c in &eb.codes {
                    let default = cs_str(&c.message);
                    let ctor = format!(
                        "new {exc}(code, string.IsNullOrEmpty(message) ? \"{default}\" : message)"
                    );
                    w.line(format!("case {}:", errors::pascal(&c.name)));
                    if c.fields.is_empty() {
                        w.indent();
                        w.line(format!("return {ctor};"));
                        w.dedent();
                        continue;
                    }
                    w.block("{", "}", |w| {
                        w.line(format!("var e = {ctor};"));
                        w.line("if (payload != null)");
                        w.block("{", "}", |w| {
                            w.line("var reader = new FfiBufferReader(payload);");
                            for f in &c.fields {
                                let var = format!("f{}", f.name.to_upper_camel_case());
                                emit_read(w, cx, &f.ty, &var, "reader", 0);
                                w.line(format!("e.Data[\"{}\"] = {var};", cs_str(&f.name)));
                            }
                            w.line("reader.ExpectEnd();");
                        });
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
