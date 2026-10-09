//! Entity rendering: error domains, enums, rich enums, records, interfaces,
//! and the recursive module walk emitting them (callback interfaces render
//! through [`crate::targets::swift::callbacks`], value-buffer codecs through
//! [`crate::targets::swift::codec`]).

use crate::codegen::common::DocCommentStyle;
use crate::codegen::errors::ErrorTable;
use crate::codegen::CodeWriter;
use crate::lang;
use heck::ToUpperCamelCase;
use weaveffi_model::model::{EnumBinding, InterfaceBinding, Model, ModuleBinding, StructBinding};

use crate::targets::swift::callbacks::render_swift_callback_interface;
use crate::targets::swift::calls::{render_callable, Receiver};
use crate::targets::swift::codec::{
    render_c_enum_codec, render_record_codec, render_rich_enum_codec,
};
use crate::targets::swift::docs::emit_decl_doc;
use crate::targets::swift::types::{swift_ident, swift_member, swift_str, SwiftCtx};

/// Members every domain error enum declares, which a code's case must not
/// redeclare.
const DOMAIN_MEMBERS: &[&str] = &["errorCode", "errorDescription", "message"];

/// The leading associated value of every declared code's case.
const MESSAGE_LABEL: &str = "message";

/// Render one error domain as a `public enum {TypeName}: Error`: one
/// lowerCamel case per code carrying the message plus, for codes that
/// declare fields, one labeled associated value per field; and an `unknown`
/// case for codes a newer library added (domains are open). Its internal
/// `WvDomain` conformance decodes a filled error slot (the payload for
/// codes with fields) and reports the error back from a callback method.
///
/// Domain codes are validated positive; a failed call maps every negative
/// (runtime) code to the runtime error before it gets here.
fn render_swift_error(w: &mut CodeWriter, table: &ErrorTable, ctx: &SwiftCtx) {
    let ty = ctx.ty_name(&table.type_name);
    let cases: Vec<String> = table
        .codes
        .iter()
        .map(|row| lang::escape_member(&swift_ident(&row.code.name), DOMAIN_MEMBERS))
        .collect();
    let unknown = lang::escape_member(
        "unknown",
        &cases.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    // A field named like the leading message gets a `_`.
    let field_label = |name: &str| lang::escape_member(&swift_ident(name), &[MESSAGE_LABEL]);

    w.line(format!(
        "/// The `{}` error domain of the `{}` module.",
        table.domain.name, table.module.dot_path
    ));
    w.line("///");
    w.line("/// Every case carries the message the library reported (the code's");
    w.line("/// documented message when it sent none) and the code's fields.");
    w.line(format!(
        "public enum {}: Error, LocalizedError, Hashable, Sendable {{",
        table.type_name
    ));
    w.indent();
    for (row, case) in table.codes.iter().zip(&cases) {
        emit_decl_doc(w, ctx, &row.code.doc, &None);
        let mut parts = vec![format!("{MESSAGE_LABEL}: String")];
        parts.extend(
            row.code
                .fields
                .iter()
                .map(|f| format!("{}: {}", field_label(&f.name), ctx.swift_type(&f.ty))),
        );
        w.line(format!("case {case}({})", parts.join(", ")));
    }
    w.line("/// A code these bindings don't declare, from a newer library.");
    w.line(format!("case {unknown}(code: Int32, message: String)"));
    w.blank();
    w.line("/// The numeric code the library reported.");
    w.line("public var errorCode: Int32 {");
    w.scope(|w| {
        w.line("switch self {");
        for (row, case) in table.codes.iter().zip(&cases) {
            w.line(format!("case .{case}: return {}", row.code.value));
        }
        w.line(format!("case let .{unknown}(code, _): return code"));
        w.line("}");
    });
    w.line("}");
    w.blank();
    w.line("/// The message the library reported, or the code's documented message.");
    w.line("public var message: String {");
    w.scope(|w| {
        w.line("switch self {");
        for (row, case) in table.codes.iter().zip(&cases) {
            let mut binds = vec![MESSAGE_LABEL.to_string()];
            binds.extend(row.code.fields.iter().map(|_| "_".to_string()));
            w.line(format!(
                "case let .{case}({}): return {MESSAGE_LABEL}",
                binds.join(", ")
            ));
        }
        w.line(format!(
            "case let .{unknown}(_, {MESSAGE_LABEL}): return {MESSAGE_LABEL}"
        ));
        w.line("}");
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "public var errorDescription: String? {{ {MESSAGE_LABEL} }}"
    ));
    w.dedent();
    w.line("}");
    w.blank();

    w.line(format!("extension {ty}: WvDomain {{"));
    w.indent();
    w.line("init(wvError err: WvError) {");
    w.scope(|w| {
        w.line("let message = wvErrorMessage(err)");
        w.line("switch err.code {");
        for (row, case) in table.codes.iter().zip(&cases) {
            let c = row.code;
            let message = format!(
                "message: message.isEmpty ? \"{}\" : message",
                swift_str(&c.message)
            );
            w.line(format!("case {}:", c.value));
            w.scope(|w| {
                if c.fields.is_empty() {
                    w.line(format!("self = .{case}({message})"));
                } else {
                    let mut args = vec![message];
                    args.extend(
                        c.fields
                            .iter()
                            .map(|f| format!("{}: r.read()", field_label(&f.name))),
                    );
                    w.line("var r = wvPayload(err)");
                    w.line(format!("self = .{case}({})", args.join(", ")));
                    w.line("r.finish()");
                }
            });
        }
        w.line("default:");
        w.scope(|w| {
            w.line(format!(
                "self = .{unknown}(code: err.code, message: message)"
            ));
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
    w.line("func wvReport(_ outErr: UnsafeMutablePointer<WvError>?) {");
    w.scope(|w| {
        w.line("switch self {");
        for (row, case) in table.codes.iter().zip(&cases) {
            let c = row.code;
            if c.fields.is_empty() {
                w.line(format!("case let .{case}(message):"));
                w.scope(|w| {
                    w.line(format!("wvSetError(outErr, {}, message)", c.value));
                });
            } else {
                let binds = (0..c.fields.len())
                    .map(|i| format!("v{i}"))
                    .collect::<Vec<_>>();
                w.line(format!("case let .{case}(message, {}):", binds.join(", ")));
                w.scope(|w| {
                    w.line("var payload = WvWriter()");
                    for b in &binds {
                        w.line(format!("payload.write({b})"));
                    }
                    w.line(format!("wvSetError(outErr, {}, message, payload)", c.value));
                });
            }
        }
        w.line(format!("case let .{unknown}(code, message):"));
        w.scope(|w| {
            w.line("wvSetError(outErr, code, message)");
        });
        w.line("}");
    });
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
}

/// Render a C-style enum as an `Int32` raw-value Swift enum, one case per
/// variant, plus its codec. Wrappers pass the raw value, which is the C
/// type's `int32_t`.
fn render_swift_enum(w: &mut CodeWriter, e: &EnumBinding, ctx: &SwiftCtx) {
    emit_decl_doc(w, ctx, &e.doc, &e.deprecated);
    w.line(format!(
        "public enum {}: Int32, CaseIterable, Sendable {{",
        e.name
    ));
    w.scope(|w| {
        for v in &e.variants {
            w.doc(&ctx.doc(&v.doc), DocCommentStyle::TripleSlash);
            w.line(format!("case {} = {}", swift_ident(&v.name), v.value));
        }
    });
    w.line("}");
    w.blank();
    render_c_enum_codec(w, e, ctx);
}

/// Render a rich (algebraic) enum as a native Swift enum with associated
/// values: one case per variant, with labeled associated values matching the
/// variant's field names, plus its codec.
fn render_swift_rich_enum(w: &mut CodeWriter, e: &EnumBinding, ctx: &SwiftCtx) {
    emit_decl_doc(w, ctx, &e.doc, &e.deprecated);
    w.line(format!("public enum {}: Hashable, Sendable {{", e.name));
    w.scope(|w| {
        for v in &e.variants {
            w.doc(&ctx.doc(&v.doc), DocCommentStyle::TripleSlash);
            let case_name = swift_ident(&v.name);
            if v.fields.is_empty() {
                w.line(format!("case {case_name}"));
            } else {
                let assoc = v
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", swift_ident(&f.name), ctx.swift_type(&f.ty)))
                    .collect::<Vec<_>>()
                    .join(", ");
                w.line(format!("case {case_name}({assoc})"));
            }
        }
    });
    w.line("}");
    w.blank();
    render_rich_enum_codec(w, e, ctx);
}

/// Render a record as a plain `Hashable` Swift struct with one `public var`
/// per field and an explicit public memberwise initializer (the synthesized
/// one is internal), plus its codec. Every field type is `Hashable`
/// (interface wrappers by identity), so every record is.
fn render_swift_struct(w: &mut CodeWriter, s: &StructBinding, ctx: &SwiftCtx) {
    emit_decl_doc(w, ctx, &s.doc, &s.deprecated);
    w.line(format!("public struct {}: Hashable, Sendable {{", s.name));
    w.indent();
    for f in &s.fields {
        w.doc(&ctx.doc(&f.doc), DocCommentStyle::TripleSlash);
        w.line(format!(
            "public var {}: {}",
            swift_ident(&f.name),
            ctx.swift_type(&f.ty)
        ));
    }
    if !s.fields.is_empty() {
        w.blank();
    }
    let params = s
        .fields
        .iter()
        .map(|f| format!("{}: {}", swift_ident(&f.name), ctx.swift_type(&f.ty)))
        .collect::<Vec<_>>()
        .join(", ");
    w.line("/// Creates a value from its fields.");
    w.line(format!("public init({params}) {{"));
    w.scope(|w| {
        for f in &s.fields {
            let prop = swift_ident(&f.name);
            w.line(format!("self.{prop} = {prop}"));
        }
    });
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    render_record_codec(w, s, ctx);
}

/// Render one interface as a `public final class` owning one strong
/// reference to its object: a stored `ptr`, an internal reference-adopting
/// `init(ptr:)`, a `deinit` that releases the reference exactly once through
/// the destroy symbol, and an internal `clonePtr()` that mints a second
/// reference for positions that take ownership (an object token inside a
/// value buffer, a callback method's return). It's `Hashable` by identity:
/// two wrappers are equal when they hold the same native object (`_clone`
/// keeps the pointer value). The native object is thread-safe, so the class
/// is `@unchecked Sendable`.
///
/// The constructor named `new` surfaces as `public init`; every other
/// constructor becomes a `public static func` factory. Methods are instance
/// funcs passing `ptr` as the leading C argument; statics are plain `public
/// static func`s.
fn render_swift_interface(w: &mut CodeWriter, iface: &InterfaceBinding, ctx: &SwiftCtx) {
    let name = &iface.name;
    emit_decl_doc(w, ctx, &iface.doc, &iface.deprecated);
    w.line(format!(
        "public final class {name}: WvObject, Hashable, @unchecked Sendable {{"
    ));
    w.indent();
    w.line("let ptr: OpaquePointer");
    w.blank();
    w.line("init(ptr: OpaquePointer) {");
    w.scope(|w| {
        w.line("self.ptr = ptr");
    });
    w.line("}");
    w.blank();
    w.line("deinit {");
    w.scope(|w| {
        w.line(format!("{}(ptr)", iface.destroy_symbol));
    });
    w.line("}");
    w.blank();
    w.line("func clonePtr() -> OpaquePointer {");
    w.scope(|w| {
        w.line(format!("wvNonNull({}(ptr))", iface.clone_symbol));
    });
    w.line("}");
    w.blank();
    w.line("/// Whether two wrappers hold the same native object.");
    w.line(format!(
        "public static func == (lhs: {name}, rhs: {name}) -> Bool {{"
    ));
    w.scope(|w| {
        w.line("lhs.ptr == rhs.ptr");
    });
    w.line("}");
    w.blank();
    w.line("public func hash(into hasher: inout Hasher) {");
    w.scope(|w| {
        w.line("hasher.combine(ptr)");
    });
    w.line("}");

    let constructors = iface.constructors.iter().map(|f| {
        let receiver = if f.name == "new" {
            Receiver::Init
        } else {
            Receiver::Static
        };
        (f, receiver)
    });
    let members = constructors
        .chain(iface.methods.iter().map(|f| (f, Receiver::Instance)))
        .chain(iface.statics.iter().map(|f| (f, Receiver::Static)));
    for (f, receiver) in members {
        w.blank();
        render_callable(w, f, &swift_member(&f.name), receiver, ctx);
    }
    w.dedent();
    w.line("}");
    w.blank();
}

/// Emit every error domain of the API.
pub(crate) fn render_swift_errors(w: &mut CodeWriter, tables: &[ErrorTable], ctx: &SwiftCtx) {
    for table in tables {
        render_swift_error(w, table, ctx);
    }
}

/// Emit every file-scope type one module contributes: enums and records
/// with their codecs, callback-interface protocols with their vtables, and
/// interface classes.
pub(crate) fn render_swift_module_types(w: &mut CodeWriter, mb: &ModuleBinding, ctx: &SwiftCtx) {
    for e in &mb.enums {
        if e.is_rich() {
            render_swift_rich_enum(w, e, ctx);
        } else {
            render_swift_enum(w, e, ctx);
        }
    }
    for s in &mb.structs {
        render_swift_struct(w, s, ctx);
    }
    for cb in &mb.callback_interfaces {
        render_swift_callback_interface(w, cb, ctx);
    }
    for i in &mb.interfaces {
        render_swift_interface(w, i, ctx);
    }
}

/// Emit one module's namespace `enum`: its function wrappers, then one
/// nested namespace per submodule.
pub(crate) fn render_swift_namespace(
    w: &mut CodeWriter,
    model: &Model,
    mb: &ModuleBinding,
    ctx: &SwiftCtx,
) {
    w.doc(&ctx.doc(&mb.doc), DocCommentStyle::TripleSlash);
    w.line(format!("public enum {} {{", mb.name.to_upper_camel_case()));
    w.indent();
    let mut first = true;
    for f in &mb.functions {
        if !first {
            w.blank();
        }
        first = false;
        render_callable(w, f, &swift_ident(&f.name), Receiver::Static, ctx);
    }
    for sub in model.children(mb) {
        if !first {
            w.blank();
        }
        first = false;
        render_swift_namespace(w, model, sub, ctx);
    }
    w.dedent();
    w.line("}");
}
