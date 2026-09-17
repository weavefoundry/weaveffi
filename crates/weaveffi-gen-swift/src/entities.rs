//! Entity rendering: enums, rich enums, records, interfaces, typed error
//! domains, and the recursive module walk emitting them (callback interfaces
//! render through [`crate::callbacks`]).

use std::collections::HashMap;

use heck::ToUpperCamelCase;
use weaveffi_core::codegen::common::DocCommentStyle;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{
    CallShape, EnumBinding, ErrorBinding, FieldBinding, InterfaceBinding, ModuleBinding,
    StructBinding,
};
use weaveffi_core::utils::{local_type_name, wrapper_name};
use weaveffi_ir::ir::Module;

use crate::callbacks::render_swift_callback_interface;
use crate::calls::{
    camel_params, render_swift_async_function, render_swift_ctor_init, render_swift_function,
    render_swift_iterator_class, ErrCtx,
};
use crate::codec::{fresh, read_value_stmts, write_value_stmts};
use crate::types::{enum_raw_type, swift_ident, swift_str, swift_type_ctx, SwiftCtx};

/// The PascalCase helper stem of the domain in effect for `module`, naming the
/// per-domain `check{Stem}`/`map{Stem}` helpers (derived from the *declaring*
/// module's path, so inheriting submodules reference the ancestor's helper).
pub(crate) fn domain_stem(module: &ModuleBinding) -> Option<String> {
    module
        .error
        .as_ref()
        .map(|e| e.owner_path.to_upper_camel_case())
}

/// Render a C-style enum as a raw-value Swift enum, one case per variant.
fn render_swift_enum(out: &mut String, e: &EnumBinding) {
    let raw = enum_raw_type(e);
    let mut w = CodeWriter::four_space();
    w.doc(&e.doc, DocCommentStyle::TripleSlash);
    w.line(format!("public enum {}: {} {{", e.name, raw));
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
/// variant's field names. The value crosses the ABI as a buffer; its codec
/// pair is emitted by [`render_rich_enum_codec`].
fn render_swift_rich_enum(out: &mut String, e: &EnumBinding, ctx: SwiftCtx) {
    let mut w = CodeWriter::four_space();
    w.doc(&e.doc, DocCommentStyle::TripleSlash);
    w.line(format!("public enum {} {{", e.name));
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
                    .map(|f| format!("{}: {}", swift_ident(&f.name), swift_type_ctx(&f.ty, ctx)))
                    .collect::<Vec<_>>()
                    .join(", ");
                w.line(format!("case {case_name}({assoc})"));
            }
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the `wvWrite{Name}`/`wvRead{Name}` codec pair for a rich enum: the
/// writer switches on the case, writes the `i32` tag, then the active
/// variant's fields in declaration order; the reader inverts it and traps on
/// an unknown tag.
fn render_rich_enum_codec(out: &mut String, e: &EnumBinding, ctx: SwiftCtx) {
    let local = local_type_name(&e.name);
    let ty_name = ctx.ty_name(local);
    let mut w = CodeWriter::four_space();
    let mut counter = 0usize;

    w.line(format!(
        "/// Serializes a `{local}` into the value-buffer wire format."
    ));
    w.line(format!(
        "func wvWrite{local}(_ value: {ty_name}, into w: inout WvWriter) {{"
    ));
    w.indent();
    w.line("switch value {");
    for v in &e.variants {
        let case_name = swift_ident(&v.name);
        if v.fields.is_empty() {
            w.line(format!("case .{case_name}:"));
            w.indent();
            w.line(format!("w.writeInt32({})", v.value));
            w.dedent();
        } else {
            let binds: Vec<String> = v.fields.iter().map(|_| fresh(&mut counter, "v")).collect();
            w.line(format!("case let .{case_name}({}):", binds.join(", ")));
            w.indent();
            w.line(format!("w.writeInt32({})", v.value));
            for (f, bind) in v.fields.iter().zip(&binds) {
                write_value_stmts(&mut w, &f.ty, bind, "w", ctx, &mut counter);
            }
            w.dedent();
        }
    }
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();

    w.line(format!(
        "/// Deserializes a `{local}` from the value-buffer wire format."
    ));
    w.line(format!(
        "func wvRead{local}(_ r: inout WvReader) -> {ty_name} {{"
    ));
    w.indent();
    w.line("let tag = r.readInt32()");
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
    w.indent();
    w.line(format!("wvDecodeFailure(\"unknown {local} tag \\(tag)\")"));
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render a record as a plain Swift struct: one typed `public var` per field
/// and an explicit public memberwise initializer (the compiler-synthesized
/// one is internal). The value crosses the ABI as a buffer; its codec pair is
/// emitted by [`render_struct_codec`].
fn render_swift_struct(out: &mut String, s: &StructBinding, ctx: SwiftCtx) {
    let mut w = CodeWriter::four_space();
    w.doc(&s.doc, DocCommentStyle::TripleSlash);
    w.line(format!("public struct {} {{", s.name));
    w.indent();
    for f in &s.fields {
        w.doc(&f.doc, DocCommentStyle::TripleSlash);
        w.line(format!(
            "public var {}: {}",
            swift_ident(&f.name),
            swift_type_ctx(&f.ty, ctx)
        ));
    }
    w.blank();
    let params = s
        .fields
        .iter()
        .map(|f| format!("{}: {}", swift_ident(&f.name), swift_type_ctx(&f.ty, ctx)))
        .collect::<Vec<_>>()
        .join(", ");
    w.line(format!("/// Creates a `{}` value.", s.name));
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
    out.push_str(&w.finish());
}

/// Emit the `wvWrite{Name}`/`wvRead{Name}` codec pair for a record: fields in
/// declaration order, delegating nested buffered types recursively.
fn render_struct_codec(out: &mut String, s: &StructBinding, ctx: SwiftCtx) {
    let local = local_type_name(&s.name);
    let ty_name = ctx.ty_name(local);
    let mut w = CodeWriter::four_space();
    let mut counter = 0usize;

    w.line(format!(
        "/// Serializes a `{local}` into the value-buffer wire format."
    ));
    w.line(format!(
        "func wvWrite{local}(_ value: {ty_name}, into w: inout WvWriter) {{"
    ));
    w.indent();
    for f in &s.fields {
        let expr = format!("value.{}", swift_ident(&f.name));
        write_value_stmts(&mut w, &f.ty, &expr, "w", ctx, &mut counter);
    }
    w.dedent();
    w.line("}");
    w.blank();

    w.line(format!(
        "/// Deserializes a `{local}` from the value-buffer wire format."
    ));
    w.line(format!(
        "func wvRead{local}(_ r: inout WvReader) -> {ty_name} {{"
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
/// that convert a non-zero `weaveffi_error` slot into it, decoding the
/// payload buffer for codes with fields.
///
/// Only declared codes get typed cases. Domain codes are validated positive,
/// so the mapper's `default` arm is what every reserved negative runtime
/// code (generic `-1`, panic `-2`, marshalling `-3`) falls through to: the
/// generic [`ERROR_BRAND`] error.
fn render_swift_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding, ctx: SwiftCtx) {
    let stem = eb.owner_path.to_upper_camel_case();
    let ty = &eb.type_name;

    let case_decl = |fields: &[FieldBinding]| -> String {
        let mut parts = vec!["message: String".to_string()];
        for f in fields {
            parts.push(format!(
                "{}: {}",
                swift_ident(&f.name),
                swift_type_ctx(&f.ty, ctx)
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
    w.line(format!("public enum {ty}: Error, LocalizedError {{"));
    w.indent();
    for c in &eb.codes {
        w.doc(&c.doc, DocCommentStyle::TripleSlash);
        w.line(format!(
            "case {}({})",
            swift_ident(&c.name),
            case_decl(&c.fields)
        ));
    }
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
    // Unknown code (including every reserved negative runtime code) ->
    // generic brand error.
    w.line("@inline(__always)");
    w.line(format!(
        "func map{stem}(code: Int32, message: String, payload: [UInt8]?) -> Error {{"
    ));
    w.indent();
    w.line("switch code {");
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
            w.line("var payloadReader = WvReader(bytes: payload ?? [])");
            let mut counter = 0usize;
            let mut args = vec![message_arg];
            for f in &c.fields {
                let var = fresh(&mut counter, "v");
                read_value_stmts(&mut w, &f.ty, &var, "payloadReader", ctx, &mut counter);
                args.push(format!("{}: {var}", swift_ident(&f.name)));
            }
            w.line("payloadReader.finish()");
            w.line(format!("return {ty}.{case_name}({})", args.join(", ")));
            w.dedent();
        }
    }
    w.line(format!(
        "default: return {ERROR_BRAND}.error(code: code, message: message)"
    ));
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();

    w.line("@inline(__always)");
    w.line(format!(
        "func check{stem}(_ err: inout weaveffi_error) throws {{"
    ));
    w.indent();
    w.line("if err.code != 0 {");
    w.scope(|w| {
        w.line("let code = err.code");
        w.line("let message = err.message.flatMap { String(cString: $0) } ?? \"\"");
        w.line("let payload: [UInt8]? = err.payload_ptr.map { [UInt8](UnsafeBufferPointer(start: $0, count: err.payload_len)) }");
        w.line("weaveffi_error_clear(&err)");
        w.line(format!(
            "throw map{stem}(code: code, message: message, payload: payload)"
        ));
    });
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render one interface as a `public final class` owning one strong
/// reference to its object: a stored `ptr`, an internal reference-adopting
/// `init(ptr:)`, a `deinit` that releases the reference exactly once through
/// the destroy symbol, and an internal `clonePtr()` that mints a second
/// reference through the clone symbol for positions that take ownership (an
/// object token inside a value buffer).
///
/// The constructor named `new` surfaces as `public init` (throwing when the
/// IDL marks it `throws`); every other constructor becomes a `public static
/// func` factory. Methods are instance funcs that pass `ptr` as the leading C
/// argument; statics are plain `public static func`s. Member bodies reuse the
/// free-function marshalling paths.
fn render_swift_interface(
    out: &mut String,
    c_prefix: &str,
    module: &ModuleBinding,
    iface: &InterfaceBinding,
    ctx: SwiftCtx,
) {
    let stem = domain_stem(module);
    let class_name = local_type_name(&iface.name);

    let mut w = CodeWriter::four_space();
    w.doc(&iface.doc, DocCommentStyle::TripleSlash);
    w.line(format!("public final class {class_name} {{"));
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
        w.line(format!("{}(ptr)!", iface.clone_symbol));
    });
    w.line("}");
    w.dedent();

    let mut members = String::new();
    for c in &iface.constructors {
        let f = camel_params(c);
        let err = ErrCtx::for_fn(&f, stem.as_deref());
        if f.name == "new" {
            render_swift_ctor_init(&mut members, c_prefix, &module.path, &f, err, ctx);
        } else {
            let swift_name = swift_ident(&f.name);
            render_swift_function(
                &mut members,
                c_prefix,
                &module.path,
                &f,
                &swift_name,
                None,
                err,
                ctx,
            );
        }
    }
    for m in &iface.methods {
        let f = camel_params(m);
        let err = ErrCtx::for_fn(&f, stem.as_deref());
        let swift_name = swift_ident(&f.name);
        if matches!(f.shape, CallShape::Async(_)) {
            render_swift_async_function(
                &mut members,
                c_prefix,
                &module.path,
                &f,
                &swift_name,
                Some("ptr"),
                err,
                ctx,
            );
        } else {
            render_swift_function(
                &mut members,
                c_prefix,
                &module.path,
                &f,
                &swift_name,
                Some("ptr"),
                err,
                ctx,
            );
        }
    }
    for s in &iface.statics {
        let f = camel_params(s);
        let err = ErrCtx::for_fn(&f, stem.as_deref());
        let swift_name = swift_ident(&f.name);
        if matches!(f.shape, CallShape::Async(_)) {
            render_swift_async_function(
                &mut members,
                c_prefix,
                &module.path,
                &f,
                &swift_name,
                None,
                err,
                ctx,
            );
        } else {
            render_swift_function(
                &mut members,
                c_prefix,
                &module.path,
                &f,
                &swift_name,
                None,
                err,
                ctx,
            );
        }
    }
    w.raw(members);

    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit every file-scope type a module (and its submodules, recursively)
/// contributes: the typed error surface, enums and their codecs, records and
/// their codecs, callback-interface protocols with their vtables, interface
/// classes, and the sequence classes backing `iter<T>` callables.
pub(crate) fn render_swift_module_types(
    out: &mut String,
    c_prefix: &str,
    by_path: &HashMap<&str, &ModuleBinding>,
    m: &Module,
    module_path: &str,
    ctx: SwiftCtx,
) {
    let mb = by_path[module_path];
    if let Some(eb) = mb.error.as_ref().filter(|e| e.declared_here) {
        render_swift_error(out, mb, eb, ctx);
    }
    for e in &mb.enums {
        if e.is_rich() {
            // A rich (algebraic) enum is a value type: a native Swift enum
            // with associated values plus its buffer codec pair.
            render_swift_rich_enum(out, e, ctx);
            render_rich_enum_codec(out, e, ctx);
        } else {
            render_swift_enum(out, e);
        }
    }
    for s in &mb.structs {
        // A record is a value type: a plain Swift struct plus its buffer
        // codec pair. Records have no C symbols at all.
        render_swift_struct(out, s, ctx);
        render_struct_codec(out, s, ctx);
    }
    for cb in &mb.callback_interfaces {
        render_swift_callback_interface(out, mb, cb, ctx);
    }
    for i in &mb.interfaces {
        render_swift_interface(out, c_prefix, mb, i, ctx);
    }
    // One lazy sequence class per `iter<T>` callable (free functions and
    // interface members alike), emitted at file scope next to the module's
    // other wrapper types.
    for f in mb.callables() {
        if let CallShape::Iterator(it) = &f.shape {
            render_swift_iterator_class(out, mb, f, it, ctx);
        }
    }
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        render_swift_module_types(out, c_prefix, by_path, sub, &sub_path, ctx);
    }
}

/// Emit the body of one module's namespace `enum`: function wrappers at this
/// depth, then one nested namespace `enum` per submodule, re-indented to its
/// depth.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_swift_module_body(
    out: &mut String,
    c_prefix: &str,
    by_path: &HashMap<&str, &ModuleBinding>,
    m: &Module,
    module_path: &str,
    depth: usize,
    strip_module_prefix: bool,
    ctx: SwiftCtx,
) {
    let indent = "    ".repeat(depth);
    let mb = by_path[module_path];
    let stem = domain_stem(mb);
    let mut bodies: Vec<String> = Vec::new();
    for f in &mb.functions {
        let mut buf = String::new();
        let f = camel_params(f);
        let swift_name = swift_ident(&wrapper_name(module_path, &f.name, strip_module_prefix));
        let err = ErrCtx::for_fn(&f, stem.as_deref());
        if matches!(f.shape, CallShape::Async(_)) {
            render_swift_async_function(
                &mut buf,
                c_prefix,
                module_path,
                &f,
                &swift_name,
                None,
                err,
                ctx,
            );
        } else {
            render_swift_function(
                &mut buf,
                c_prefix,
                module_path,
                &f,
                &swift_name,
                None,
                err,
                ctx,
            );
        }
        bodies.push(buf);
    }
    for buf in bodies {
        if depth > 1 {
            let extra = "    ".repeat(depth - 1);
            for line in buf.lines() {
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str(&extra);
                    out.push_str(line);
                    out.push('\n');
                }
            }
        } else {
            out.push_str(&buf);
        }
    }
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        let sub_name = sub.name.to_upper_camel_case();
        out.push_str(&format!("{indent}public enum {sub_name} {{\n"));
        render_swift_module_body(
            out,
            c_prefix,
            by_path,
            sub,
            &sub_path,
            depth + 1,
            strip_module_prefix,
            ctx,
        );
        out.push_str(&format!("{indent}}}\n"));
    }
}
