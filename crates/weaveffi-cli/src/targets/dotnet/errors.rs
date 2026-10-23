//! Error domains as exception hierarchies, and the error mapping of every
//! call ([`ErrorStrategy`]).

use heck::ToLowerCamelCase;
use weaveffi_model::errors::pascal;
use weaveffi_model::model::{FieldBinding, Model};
use weaveffi_model::plan::ErrorStrategy;

use crate::codegen::errors::{tables, ErrorTable};
use crate::codegen::CodeWriter;
use crate::targets::dotnet::codec::{read_expr, write_call};
use crate::targets::dotnet::docs::Docs;
use crate::targets::dotnet::types::{
    cs_str, cs_type, exception_field_cs, safe_cs_name, xml_text, Cx,
};

/// How a call maps a failure to an exception, rendering its
/// [`ErrorStrategy`]: a domain's `FromError` (typed codes, open to codes
/// added later), the root exception's for `throws: any`, or the bug
/// exception's for a call that can't fail.
#[derive(Clone)]
pub(crate) struct ErrCtx {
    /// The `delegate*` expression of the mapping function.
    pub map: String,
    /// The exception a throwing call documents, with its description.
    doc: Option<(String, String)>,
    /// The domain exception a callback method reports with its code and
    /// fields.
    pub domain: Option<String>,
}

impl ErrCtx {
    /// The error context of a callable (or callback method) with `error`.
    pub(crate) fn new(model: &Model, error: &ErrorStrategy, cx: Cx<'_>) -> Self {
        match error {
            ErrorStrategy::Domain(name) => {
                let exc = domain_exception(model, name);
                ErrCtx {
                    map: format!("&{}.FromError", cx.ty(&exc)),
                    doc: Some((
                        exc.clone(),
                        format!("The call failed with a <c>{name}</c> code."),
                    )),
                    domain: Some(exc),
                }
            }
            ErrorStrategy::Untyped => ErrCtx {
                map: format!("&{}.FromError", cx.ty(cx.base)),
                doc: Some((cx.base.to_string(), "The call failed.".into())),
                domain: None,
            },
            ErrorStrategy::Trap => ErrCtx {
                map: format!("&{}.FromError", cx.ty(cx.bug)),
                doc: None,
                domain: None,
            },
        }
    }

    /// The statement throwing when the local `FfiError` named `var` holds a
    /// failure.
    pub(crate) fn check(&self, var: &str) -> String {
        format!(
            "if ({var}.Code != 0) throw Ffi.TakeError(&{var}, {});",
            self.map
        )
    }

    /// Emit the `<exception>` doc line of a throwing wrapper.
    pub(crate) fn write_doc(&self, w: &mut CodeWriter) {
        if let Some((exc, text)) = &self.doc {
            w.line(format!("/// <exception cref=\"{exc}\">{text}</exception>"));
        }
    }
}

/// The exception class of the error domain `name`: its stem with one
/// `Exception` suffix (`KvError` is `KvException`).
pub(crate) fn domain_exception(model: &Model, name: &str) -> String {
    weaveffi_model::errors::exception_type_name(&model.error_domain(name).name)
}

/// Render every error domain's exception hierarchy.
pub(crate) fn render_domains(w: &mut CodeWriter, model: &Model, docs: &Docs, cx: Cx<'_>) {
    for table in tables(model, "Exception") {
        render_domain(w, &table, docs, cx);
    }
}

/// One domain: a class deriving from the root exception, with one nested
/// sealed class per code. Each code class exposes its fields as typed
/// properties and can be constructed by a callback-interface
/// implementation, which reports it (code, message, and fields) to the
/// native caller. The domain class itself stands for a code these bindings
/// don't know: domains are open, so a library may add codes.
fn render_domain(w: &mut CodeWriter, table: &ErrorTable<'_>, docs: &Docs, cx: Cx<'_>) {
    let exc = &table.type_name;
    let base = cx.base;
    w.line(format!(
        "/// <summary>The errors of the <c>{}</c> domain, declared by module <c>{}</c>.",
        table.domain.name, table.module.dot_path
    ));
    w.line("/// Catch a nested class for one code, or this class for any of them. A code");
    w.line("/// these bindings don't know (one the library added later) surfaces as this");
    w.line(format!(
        "/// class itself, with its <see cref=\"{base}.Code\"/> and message.</summary>"
    ));
    w.line(format!("public class {exc} : {base}"));
    w.block("{", "}", |w| {
        w.line(format!(
            "internal {exc}(int code, string message) : base(code, message)"
        ));
        w.line("{");
        w.line("}");
        w.blank();
        for row in &table.codes {
            let class = pascal(&row.code.name);
            if row.code.doc.is_some() {
                docs.summary(w, &row.code.doc);
            } else {
                w.line(format!("/// <summary>{}</summary>", xml_text(&row.code.message)));
            }
            render_code_class(w, exc, &class, row.code.value, &row.code.message, &row.code.fields, docs);
        }
        w.line("/// <summary>The exception for a raw code and its payload: a typed code");
        w.line("/// class, this class for a code it doesn't know (or a payload that doesn't");
        w.line("/// decode), or the root mapping for a runtime code.</summary>");
        w.line(
            "internal static new Exception FromError(int code, string message, FfiBufferReader payload)",
        );
        w.block("{", "}", |w| {
            w.line("if (code <= 0)");
            w.block("{", "}", |w| {
                w.line(format!(
                    "return {}.FromError(code, message, payload);",
                    cx.ty(base)
                ));
            });
            w.line("var given = message.Length == 0 ? null : message;");
            w.line("try");
            w.block("{", "}", |w| {
                w.line("switch (code)");
                w.block("{", "}", |w| {
                    for row in &table.codes {
                        let class = pascal(&row.code.name);
                        w.line(format!("case {class}.ErrorCode:"));
                        w.indent();
                        if row.code.fields.is_empty() {
                            w.line(format!("return new {class}(given);"));
                        } else {
                            let mut args: Vec<String> = row
                                .code
                                .fields
                                .iter()
                                .map(|f| read_expr(cx, &f.ty, "payload", 0))
                                .collect();
                            args.push("given".into());
                            w.line(format!(
                                "return payload.End(new {class}({}));",
                                args.join(", ")
                            ));
                        }
                        w.dedent();
                    }
                });
            });
            w.line(format!("catch ({} e) when (e.Code == {base}.MarshalErrorCode)", cx.bug));
            w.block("{", "}", |w| {
                w.line("// A payload that doesn't decode keeps the code and message.");
            });
            w.line(format!("return new {exc}(code, message);"));
        });
    });
    w.blank();
}

/// One code's nested exception class.
fn render_code_class(
    w: &mut CodeWriter,
    exc: &str,
    class: &str,
    value: i32,
    message: &str,
    fields: &[FieldBinding],
    docs: &Docs,
) {
    w.line(format!("public sealed class {class} : {exc}"));
    w.block("{", "}", |w| {
        w.line(format!(
            "/// <summary>The domain code, <c>{value}</c>.</summary>"
        ));
        w.line(format!("public const int ErrorCode = {value};"));
        w.blank();
        for f in fields {
            docs.summary(w, &f.doc);
            w.line(format!(
                "public {} {} {{ get; }}",
                cs_type(&f.ty),
                exception_field_cs(&f.name, class)
            ));
            w.blank();
        }
        let mut params: Vec<String> = fields
            .iter()
            .map(|f| format!("{} {}", cs_type(&f.ty), param_name(f)))
            .collect();
        params.push("string? message = null".into());
        w.line("/// <summary>Creates the error, as a callback-interface implementation");
        w.line("/// throws it to report this code.</summary>");
        for f in fields {
            docs.param(w, &param_name(f), &f.doc);
        }
        w.line(format!(
            "/// <param name=\"message\">The message, or null for the default \"{}\".</param>",
            xml_text(message)
        ));
        w.line(format!(
            "public {class}({}) : base(ErrorCode, message ?? \"{}\")",
            params.join(", "),
            cs_str(message)
        ));
        w.block("{", "}", |w| {
            for f in fields {
                w.line(format!(
                    "{} = {};",
                    exception_field_cs(&f.name, class),
                    param_name(f)
                ));
            }
        });
        if !fields.is_empty() {
            w.blank();
            w.line("internal override void WritePayload(FfiBufferWriter writer)");
            w.block("{", "}", |w| {
                for f in fields {
                    let value = exception_field_cs(&f.name, class);
                    w.line(format!("{};", write_call(&f.ty, "writer", &value, 0)));
                }
            });
        }
    });
    w.blank();
}

/// A code field's constructor parameter.
fn param_name(f: &FieldBinding) -> String {
    let name = f.name.to_lower_camel_case();
    if name == "message" {
        "message_".into()
    } else {
        safe_cs_name(&name)
    }
}
