//! Entity rendering: enums, rich enums, records, interfaces, typed error
//! domains, and the recursive module walk emitting them (callback interfaces
//! render through [`crate::targets::swift::callbacks`], value-buffer codecs
//! through [`crate::targets::swift::codec`]).

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use heck::ToUpperCamelCase;
use weaveffi_model::model::{
    CallShape, EnumBinding, ErrorBinding, FieldBinding, FnBinding, InterfaceBinding, Model,
    ModuleBinding, StructBinding,
};

use crate::targets::swift::callbacks::render_swift_callback_interface;
use crate::targets::swift::calls::{
    camel_params, render_callable, render_swift_iterator_class, ErrCtx, Receiver,
};
use crate::targets::swift::codec::{
    render_c_enum_codec, render_interface_codec, render_record_codec, render_rich_enum_codec,
};
use crate::targets::swift::types::{
    deprecated_attr, swift_ident, swift_member, swift_str, SwiftCtx,
};

/// The PascalCase helper stem of an error domain, naming its
/// `wvCheck{Stem}`/`wvMap{Stem}`/`wvReport{Stem}` helpers (derived from the
/// *declaring* module's path, so inheriting submodules reference the
/// ancestor's helpers).
fn stem(eb: &ErrorBinding) -> String {
    eb.owner_path.to_upper_camel_case()
}

/// The `(type name, stem)` of the error domain in scope for `module`.
fn domain_of(model: &Model, module: &ModuleBinding) -> Option<(String, String)> {
    model
        .error_domain(module)
        .map(|e| (e.type_name.clone(), stem(e)))
}

/// Borrow a `(type name, stem)` pair.
fn as_refs(d: &Option<(String, String)>) -> Option<(&str, &str)> {
    d.as_ref().map(|(t, s)| (t.as_str(), s.as_str()))
}

/// The conformance list of a value type: `Hashable` (which implies
/// `Equatable`) when every field is, and always `Sendable`.
fn value_conformances<'t>(
    ctx: &SwiftCtx,
    fields: impl IntoIterator<Item = &'t FieldBinding>,
) -> &'static str {
    if fields.into_iter().all(|f| ctx.is_hashable(&f.ty)) {
        "Hashable, Sendable"
    } else {
        "Sendable"
    }
}

/// Render a C-style enum as an `Int32` raw-value Swift enum, one case per
/// variant, plus its codec. Wrappers convert to and from the imported C
/// type at the call.
fn render_swift_enum(w: &mut CodeWriter, e: &EnumBinding, ctx: &SwiftCtx) {
    w.doc(&e.doc, DocCommentStyle::TripleSlash);
    if let Some(msg) = &e.deprecated {
        w.line(deprecated_attr(msg));
    }
    w.line(format!("public enum {}: Int32, Sendable {{", e.name));
    w.scope(|w| {
        for v in &e.variants {
            w.doc(&v.doc, DocCommentStyle::TripleSlash);
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
    let fields = e.variants.iter().flat_map(|v| v.fields.iter());
    w.doc(&e.doc, DocCommentStyle::TripleSlash);
    if let Some(msg) = &e.deprecated {
        w.line(deprecated_attr(msg));
    }
    w.line(format!(
        "public enum {}: {} {{",
        e.name,
        value_conformances(ctx, fields)
    ));
    w.scope(|w| {
        for v in &e.variants {
            w.doc(&v.doc, DocCommentStyle::TripleSlash);
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

/// Render a record as a plain Swift struct with one `public var` per field
/// and an explicit public memberwise initializer (the synthesized one is
/// internal), plus its codec.
fn render_swift_struct(w: &mut CodeWriter, s: &StructBinding, ctx: &SwiftCtx) {
    w.doc(&s.doc, DocCommentStyle::TripleSlash);
    if let Some(msg) = &s.deprecated {
        w.line(deprecated_attr(msg));
    }
    w.line(format!(
        "public struct {}: {} {{",
        s.name,
        value_conformances(ctx, &s.fields)
    ));
    w.indent();
    for f in &s.fields {
        w.doc(&f.doc, DocCommentStyle::TripleSlash);
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

/// Render one declaring module's typed error surface: a `public enum
/// {TypeName}: Error` whose lowerCamel cases carry the message plus, for
/// codes that declare payload fields, one labeled associated value per
/// field. Also emits the file-scope helpers: `wvMap{Stem}` and
/// `wvCheck{Stem}` convert a filled error slot into it (decoding the payload
/// for codes with fields), and, when a callback method can throw it,
/// `wvReport{Stem}` reports it back through a callback's error slot.
///
/// Only declared codes get typed cases. Domain codes are validated positive,
/// so the mapper's `default` arm is what every reserved negative runtime
/// code falls through to: the runtime error, or `CancellationError` for `-5`.
fn render_swift_error(
    w: &mut CodeWriter,
    module: &ModuleBinding,
    eb: &ErrorBinding,
    ctx: &SwiftCtx,
) {
    let stem = stem(eb);
    let ty = &eb.type_name;

    w.doc(
        &Some(format!(
            "Typed errors reported by the `{}` module.",
            module.segments.join(".")
        )),
        DocCommentStyle::TripleSlash,
    );
    w.line(format!(
        "public enum {ty}: Error, LocalizedError, Sendable {{"
    ));
    w.indent();
    for c in &eb.codes {
        w.doc(&c.doc, DocCommentStyle::TripleSlash);
        let mut parts = vec!["message: String".to_string()];
        parts.extend(
            c.fields
                .iter()
                .map(|f| format!("{}: {}", swift_ident(&f.name), ctx.swift_type(&f.ty))),
        );
        w.line(format!(
            "case {}({})",
            swift_ident(&c.name),
            parts.join(", ")
        ));
    }
    w.blank();
    w.line("/// The message the library reported, or the code's documented default.");
    w.line("public var errorDescription: String? {");
    w.scope(|w| {
        w.line("switch self {");
        for c in &eb.codes {
            // Bind only the message; wildcard the payload fields.
            let mut binds = vec!["message".to_string()];
            binds.extend(c.fields.iter().map(|_| "_".to_string()));
            w.line(format!(
                "case let .{}({}): return message",
                swift_ident(&c.name),
                binds.join(", ")
            ));
        }
        w.line("}");
    });
    w.line("}");
    w.blank();
    w.line("/// The numeric ABI code carried by this error.");
    w.line("public var errorCode: Int32 {");
    w.scope(|w| {
        w.line("switch self {");
        for c in &eb.codes {
            w.line(format!(
                "case .{}: return {}",
                swift_ident(&c.name),
                c.value
            ));
        }
        w.line("}");
    });
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();

    // `wvMap{Stem}`: code -> typed case (default message when the slot
    // carried none), decoding the payload buffer for codes with fields.
    w.line(format!("func wvMap{stem}(_ err: WvError) -> Error {{"));
    w.indent();
    w.line("let message = wvErrorMessage(err)");
    w.line("switch err.code {");
    for c in &eb.codes {
        let case_name = swift_ident(&c.name);
        let message_arg = format!(
            "message: message.isEmpty ? \"{}\" : message",
            swift_str(&c.message)
        );
        if c.fields.is_empty() {
            w.line(format!(
                "case {}: return {ty}.{case_name}({message_arg})",
                c.value
            ));
        } else {
            let mut args = vec![message_arg];
            args.extend(
                c.fields
                    .iter()
                    .map(|f| format!("{}: r.read()", swift_ident(&f.name))),
            );
            w.line(format!("case {}:", c.value));
            w.scope(|w| {
                w.line("var r = WvReader(err.payload_ptr, err.payload_len)");
                w.line(format!("let error = {ty}.{case_name}({})", args.join(", ")));
                w.line("r.finish()");
                w.line("return error");
            });
        }
    }
    w.line("default: return wvRuntimeError(err)");
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();

    w.line(format!(
        "func wvCheck{stem}(_ err: inout WvError) throws {{"
    ));
    w.scope(|w| {
        w.line("guard err.code != 0 else { return }");
        w.line(format!("let error = wvMap{stem}(err)"));
        w.line(format!("{}_error_clear(&err)", ctx.c_prefix));
        w.line("throw error");
    });
    w.line("}");
    w.blank();

    if !ctx.reports(eb) {
        return;
    }
    w.line(format!(
        "/// Reports a ``{ty}`` thrown by a callback method declared to throw it: its"
    ));
    w.line("/// code, its message, and its fields as the payload. Any other error is left");
    w.line("/// to the caller, which reports a callback failure.");
    w.line(format!(
        "func wvReport{stem}(_ error: Error, _ outErr: UnsafeMutablePointer<WvError>?) -> Bool {{"
    ));
    w.scope(|w| {
        w.line(format!(
            "guard let error = error as? {ty} else {{ return false }}"
        ));
        w.line("switch error {");
        for c in &eb.codes {
            let case_name = swift_ident(&c.name);
            if c.fields.is_empty() {
                w.line(format!("case let .{case_name}(message):"));
                w.scope(|w| {
                    w.line(format!("wvSetError(outErr, {}, message)", c.value));
                });
            } else {
                let binds = (0..c.fields.len())
                    .map(|i| format!("v{i}"))
                    .collect::<Vec<_>>();
                w.line(format!(
                    "case let .{case_name}(message, {}):",
                    binds.join(", ")
                ));
                w.scope(|w| {
                    w.line("var payload = WvWriter()");
                    for b in &binds {
                        w.line(format!("payload.write({b})"));
                    }
                    w.line(format!("wvSetError(outErr, {}, message, payload)", c.value));
                });
            }
        }
        w.line("}");
        w.line("return true");
    });
    w.line("}");
    w.blank();
}

/// Render one interface as a `public final class` owning one strong
/// reference to its object: a stored `ptr`, an internal reference-adopting
/// `init(ptr:)`, a `deinit` that releases the reference exactly once through
/// the destroy symbol, and an internal `clonePtr()` that mints a second
/// reference for positions that take ownership (an object token inside a
/// value buffer, a callback method's return). The native object is
/// thread-safe, so the class is `@unchecked Sendable`.
///
/// The constructor named `new` surfaces as `public init`; every other
/// constructor becomes a `public static func` factory. Methods are instance
/// funcs passing `ptr` as the leading C argument; statics are plain `public
/// static func`s.
fn render_swift_interface(
    w: &mut CodeWriter,
    module: &ModuleBinding,
    iface: &InterfaceBinding,
    ctx: &SwiftCtx,
) {
    let domain = domain_of(ctx.model, module);
    w.doc(&iface.doc, DocCommentStyle::TripleSlash);
    if let Some(msg) = &iface.deprecated {
        w.line(deprecated_attr(msg));
    }
    w.line(format!(
        "public final class {}: @unchecked Sendable {{",
        iface.name
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
    w.line("/// Returns a new strong reference to the same object, for a position that");
    w.line("/// takes ownership of it.");
    w.line("func clonePtr() -> OpaquePointer {");
    w.scope(|w| {
        w.line(format!("wvNonNull({}(ptr))", iface.clone_symbol));
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
        let f = camel_params(f);
        let err = ErrCtx::for_fn(&f, as_refs(&domain));
        w.blank();
        render_callable(w, &f, &swift_member(&f.name), receiver, err, ctx);
    }
    w.dedent();
    w.line("}");
    w.blank();
    render_interface_codec(w, iface, ctx);
}

/// Emit every file-scope type one module contributes: the typed error
/// surface, enums and records with their codecs, callback-interface
/// protocols with their vtables, interface classes, and the sequence classes
/// backing `iter<T>` callables.
pub(crate) fn render_swift_module_types(w: &mut CodeWriter, mb: &ModuleBinding, ctx: &SwiftCtx) {
    if let Some(eb) = mb.errors.as_ref() {
        render_swift_error(w, mb, eb, ctx);
    }
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
    let domain = domain_of(ctx.model, mb);
    for cb in &mb.callback_interfaces {
        render_swift_callback_interface(w, cb, as_refs(&domain), ctx);
    }
    for i in &mb.interfaces {
        render_swift_interface(w, mb, i, ctx);
    }
    for f in mb.callables() {
        if let CallShape::Iterator(it) = &f.shape {
            render_swift_iterator_class(w, f, it, ErrCtx::for_fn(f, as_refs(&domain)), ctx);
        }
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
    let domain = domain_of(model, mb);
    w.doc(&mb.doc, DocCommentStyle::TripleSlash);
    w.line(format!("public enum {} {{", mb.name.to_upper_camel_case()));
    w.indent();
    let mut first = true;
    for f in &mb.functions {
        if !first {
            w.blank();
        }
        first = false;
        let f: FnBinding = camel_params(f);
        let err = ErrCtx::for_fn(&f, as_refs(&domain));
        render_callable(w, &f, &swift_ident(&f.name), Receiver::Static, err, ctx);
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
