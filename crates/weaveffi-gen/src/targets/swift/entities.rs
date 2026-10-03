//! Entity rendering: enums, rich enums, records, interfaces, typed error
//! domains, and the recursive module walk emitting them (callback interfaces
//! render through [`crate::targets::swift::callbacks`]).

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use crate::utils::wrapper_name;
use heck::ToUpperCamelCase;
use weaveffi_model::model::{
    BindingModel, CallShape, EnumBinding, ErrorBinding, FieldBinding, InterfaceBinding,
    ModuleBinding, StructBinding,
};

use crate::targets::swift::callbacks::render_swift_callback_interface;
use crate::targets::swift::calls::{
    camel_params, render_callable, render_swift_iterator_class, ErrCtx, Receiver,
};
use crate::targets::swift::codec::{fresh, read_value_stmts, write_value_stmts};
use crate::targets::swift::types::{deprecated_attr, swift_ident, swift_str, SwiftCtx};

/// The PascalCase helper stem of the domain in effect for `module`, naming the
/// per-domain `check{Stem}`/`map{Stem}` helpers (derived from the *declaring*
/// module's path, so inheriting submodules reference the ancestor's helper).
fn domain_stem(module: &ModuleBinding) -> Option<String> {
    module
        .error
        .as_ref()
        .map(|e| e.owner_path.to_upper_camel_case())
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
/// variant. Wrappers convert to and from the imported C type at the call.
fn render_swift_enum(out: &mut String, e: &EnumBinding) {
    let mut w = CodeWriter::four_space();
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
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as a native Swift enum with associated
/// values: one case per variant, with labeled associated values matching the
/// variant's field names, plus its `wvWrite`/`wvRead` codec pair. The writer
/// switches on the case, writes the `i32` tag, then the active variant's
/// fields in order; the reader inverts it and stops on an unknown tag.
fn render_swift_rich_enum(out: &mut String, e: &EnumBinding, ctx: &SwiftCtx) {
    let name = &e.name;
    let ty_name = ctx.ty_name(name);
    let fields = e.variants.iter().flat_map(|v| v.fields.iter());
    let mut w = CodeWriter::four_space();
    w.doc(&e.doc, DocCommentStyle::TripleSlash);
    if let Some(msg) = &e.deprecated {
        w.line(deprecated_attr(msg));
    }
    w.line(format!(
        "public enum {name}: {} {{",
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

    let mut counter = 0usize;
    // The codecs of a deprecated type are deprecated too, so they can name it
    // without a warning.
    let codec_attr = e.deprecated.as_deref().map(deprecated_attr);
    if let Some(attr) = &codec_attr {
        w.line(attr);
    }
    w.line(format!(
        "func wvWrite{name}(_ value: {ty_name}, into w: inout WvWriter) {{"
    ));
    w.indent();
    w.line("switch value {");
    for v in &e.variants {
        let case_name = swift_ident(&v.name);
        if v.fields.is_empty() {
            w.line(format!("case .{case_name}:"));
            w.scope(|w| {
                w.line(format!("w.writeI32({})", v.value));
            });
        } else {
            let binds: Vec<String> = v.fields.iter().map(|_| fresh(&mut counter, "v")).collect();
            w.line(format!("case let .{case_name}({}):", binds.join(", ")));
            w.indent();
            w.line(format!("w.writeI32({})", v.value));
            for (f, bind) in v.fields.iter().zip(&binds) {
                write_value_stmts(&mut w, &f.ty, bind, "w", &mut counter);
            }
            w.dedent();
        }
    }
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();

    if let Some(attr) = &codec_attr {
        w.line(attr);
    }
    w.line(format!(
        "func wvRead{name}(_ r: inout WvReader) -> {ty_name} {{"
    ));
    w.indent();
    w.line("let tag = r.readI32()");
    w.line("switch tag {");
    for v in &e.variants {
        let case_name = swift_ident(&v.name);
        w.line(format!("case {}:", v.value));
        w.indent();
        if v.fields.is_empty() {
            w.line(format!("return .{case_name}"));
        } else {
            let mut labeled = Vec::new();
            for f in &v.fields {
                let var = fresh(&mut counter, "v");
                read_value_stmts(&mut w, &f.ty, &var, "r", ctx, &mut counter);
                labeled.push(format!("{}: {var}", swift_ident(&f.name)));
            }
            w.line(format!("return .{case_name}({})", labeled.join(", ")));
        }
        w.dedent();
    }
    w.line("default:");
    w.scope(|w| {
        w.line(format!("wvDecodeFailure(\"unknown {name} tag \\(tag)\")"));
    });
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render a record as a plain Swift struct with one `public var` per field
/// and an explicit public memberwise initializer (the synthesized one is
/// internal), plus its `wvWrite`/`wvRead` codec pair (fields in declaration
/// order).
fn render_swift_struct(out: &mut String, s: &StructBinding, ctx: &SwiftCtx) {
    let name = &s.name;
    let ty_name = ctx.ty_name(name);
    let mut w = CodeWriter::four_space();
    w.doc(&s.doc, DocCommentStyle::TripleSlash);
    if let Some(msg) = &s.deprecated {
        w.line(deprecated_attr(msg));
    }
    w.line(format!(
        "public struct {name}: {} {{",
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

    let mut counter = 0usize;
    let value = if s.fields.is_empty() { "_" } else { "value" };
    let writer = if s.fields.is_empty() { "_" } else { "w" };
    let codec_attr = s.deprecated.as_deref().map(deprecated_attr);
    if let Some(attr) = &codec_attr {
        w.line(attr);
    }
    w.line(format!(
        "func wvWrite{name}(_ {value}: {ty_name}, into {writer}: inout WvWriter) {{"
    ));
    w.indent();
    for f in &s.fields {
        let expr = format!("value.{}", swift_ident(&f.name));
        write_value_stmts(&mut w, &f.ty, &expr, "w", &mut counter);
    }
    w.dedent();
    w.line("}");
    w.blank();

    let reader = if s.fields.is_empty() { "_" } else { "r" };
    if let Some(attr) = &codec_attr {
        w.line(attr);
    }
    w.line(format!(
        "func wvRead{name}(_ {reader}: inout WvReader) -> {ty_name} {{"
    ));
    w.indent();
    let mut labeled = Vec::new();
    for f in &s.fields {
        let var = fresh(&mut counter, "v");
        read_value_stmts(&mut w, &f.ty, &var, "r", ctx, &mut counter);
        labeled.push(format!("{}: {var}", swift_ident(&f.name)));
    }
    w.line(format!("return {ty_name}({})", labeled.join(", ")));
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render one declaring module's typed error surface: a `public enum
/// {TypeName}: Error` whose lowerCamel cases carry the runtime message plus,
/// for codes that declare payload fields, one labeled associated value per
/// field. Also emits the file-scope `map{Stem}` and `check{Stem}` helpers
/// that convert a filled error slot into it, decoding the payload buffer for
/// codes with fields.
///
/// Only declared codes get typed cases. Domain codes are validated positive,
/// so the mapper's `default` arm is what every reserved negative runtime
/// code falls through to: the runtime error, or `CancellationError` for `-5`.
fn render_swift_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding, ctx: &SwiftCtx) {
    let stem = eb.owner_path.to_upper_camel_case();
    let ty = &eb.type_name;

    let case_decl = |fields: &[FieldBinding]| -> String {
        let mut parts = vec!["message: String".to_string()];
        for f in fields {
            parts.push(format!(
                "{}: {}",
                swift_ident(&f.name),
                ctx.swift_type(&f.ty)
            ));
        }
        parts.join(", ")
    };

    let mut w = CodeWriter::four_space();
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
        w.line(format!(
            "case {}({})",
            swift_ident(&c.name),
            case_decl(&c.fields)
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

    // `map{Stem}`: code -> typed case (default message when the slot carried
    // none), decoding the payload buffer for codes that declare fields.
    w.line(format!("func map{stem}(_ err: WvError) -> Error {{"));
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
            w.line(format!("case {}:", c.value));
            w.indent();
            w.line("var r = WvReader(err.payload_ptr, err.payload_len)");
            let mut counter = 0usize;
            let mut args = vec![message_arg];
            for f in &c.fields {
                let var = fresh(&mut counter, "v");
                read_value_stmts(&mut w, &f.ty, &var, "r", ctx, &mut counter);
                args.push(format!("{}: {var}", swift_ident(&f.name)));
            }
            w.line("r.finish()");
            w.line(format!("return {ty}.{case_name}({})", args.join(", ")));
            w.dedent();
        }
    }
    w.line("default: return wvRuntimeError(err)");
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();

    w.line(format!("func check{stem}(_ err: inout WvError) throws {{"));
    w.indent();
    w.line("guard err.code != 0 else { return }");
    w.line(format!("let error = map{stem}(err)"));
    w.line(format!("{}_error_clear(&err)", ctx.c_prefix));
    w.line("throw error");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render one interface as a `public final class` owning one strong
/// reference to its object: a stored `ptr`, an internal reference-adopting
/// `init(ptr:)`, a `deinit` that releases the reference exactly once through
/// the destroy symbol, and an internal `clonePtr()` that mints a second
/// reference for positions that take ownership (an object token inside a
/// value buffer). The native object is thread-safe, so the class is
/// `@unchecked Sendable`.
///
/// The constructor named `new` surfaces as `public init`; every other
/// constructor becomes a `public static func` factory. Methods are instance
/// funcs passing `ptr` as the leading C argument; statics are plain `public
/// static func`s.
fn render_swift_interface(
    out: &mut String,
    module: &ModuleBinding,
    iface: &InterfaceBinding,
    ctx: &SwiftCtx,
) {
    let stem = domain_stem(module);
    let class_name = &iface.name;

    let mut w = CodeWriter::four_space();
    w.doc(&iface.doc, DocCommentStyle::TripleSlash);
    if let Some(msg) = &iface.deprecated {
        w.line(deprecated_attr(msg));
    }
    w.line(format!(
        "public final class {class_name}: @unchecked Sendable {{"
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
    w.line("/// takes ownership of it (an object token inside a value buffer).");
    w.line("func clonePtr() -> OpaquePointer {");
    w.scope(|w| {
        w.line(format!("wvNonNull({}(ptr))", iface.clone_symbol));
    });
    w.line("}");
    w.dedent();

    let mut members = String::new();
    let mut render = |f: &weaveffi_model::model::FnBinding, receiver: Receiver| {
        let f = camel_params(f);
        let err = ErrCtx::for_fn(&f, stem.as_deref());
        members.push('\n');
        let receiver = if receiver == Receiver::Static && f.name == "new" {
            Receiver::Init
        } else {
            receiver
        };
        render_callable(
            &mut members,
            &f,
            &swift_ident(&f.name),
            receiver,
            err,
            ctx,
            1,
        );
    };
    for c in &iface.constructors {
        render(c, Receiver::Static);
    }
    for m in &iface.methods {
        render(m, Receiver::Instance);
    }
    for s in &iface.statics {
        render(s, Receiver::Static);
    }
    w.raw(members);

    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit every file-scope type one module contributes: the typed error
/// surface, enums and records with their codecs, callback-interface
/// protocols with their vtables, interface classes, and the sequence classes
/// backing `iter<T>` callables.
pub(crate) fn render_swift_module_types(out: &mut String, mb: &ModuleBinding, ctx: &SwiftCtx) {
    if let Some(eb) = mb.error.as_ref().filter(|e| e.declared_here) {
        render_swift_error(out, mb, eb, ctx);
    }
    for e in &mb.enums {
        if e.is_rich() {
            render_swift_rich_enum(out, e, ctx);
        } else {
            render_swift_enum(out, e);
        }
    }
    for s in &mb.structs {
        render_swift_struct(out, s, ctx);
    }
    for cb in &mb.callback_interfaces {
        render_swift_callback_interface(out, cb, ctx);
    }
    for i in &mb.interfaces {
        render_swift_interface(out, mb, i, ctx);
    }
    let stem = domain_stem(mb);
    for f in mb.callables() {
        if let CallShape::Iterator(it) = &f.shape {
            render_swift_iterator_class(out, f, it, ErrCtx::for_fn(f, stem.as_deref()), ctx);
        }
    }
}

/// Emit one module's namespace `enum` at `depth`: its function wrappers,
/// then one nested namespace per submodule.
pub(crate) fn render_swift_namespace(
    out: &mut String,
    model: &BindingModel,
    mb: &ModuleBinding,
    depth: usize,
    strip_module_prefix: bool,
    ctx: &SwiftCtx,
) {
    let indent = "    ".repeat(depth);
    let stem = domain_stem(mb);
    out.push_str(&format!(
        "{indent}public enum {} {{\n",
        mb.name.to_upper_camel_case()
    ));
    let mut first = true;
    for f in &mb.functions {
        if !first {
            out.push('\n');
        }
        first = false;
        let f = camel_params(f);
        let swift_name = swift_ident(&wrapper_name(&mb.path, &f.name, strip_module_prefix));
        let err = ErrCtx::for_fn(&f, stem.as_deref());
        render_callable(out, &f, &swift_name, Receiver::Static, err, ctx, depth + 1);
    }
    for sub in model.children(mb) {
        if !first {
            out.push('\n');
        }
        first = false;
        render_swift_namespace(out, model, sub, depth + 1, strip_module_prefix, ctx);
    }
    out.push_str(&format!("{indent}}}\n"));
}
