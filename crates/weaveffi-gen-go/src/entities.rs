//! Entity rendering: plain and rich enums, records, typed error domains,
//! typed-handle wrappers, and interface wrapper types.

use std::collections::HashSet;

use heck::ToUpperCamelCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    BindingModel, CallShape, EnumBinding, ErrorBinding, InterfaceBinding, ModuleBinding,
    StructBinding,
};
use weaveffi_core::utils::{c_abi_struct_name, local_type_name};

use crate::calls::{render_async_function, render_function, ErrCtx};
use crate::codec::{emit_buffer_read, emit_buffer_write};
use crate::docs::emit_doc;
use crate::types::{go_str, go_type, handle_wrapper};

/// The PascalCase helper stem of the domain in effect for `module`, naming
/// the per-domain `wvMap{Stem}` helper (derived from the *declaring* module's
/// path, so inheriting submodules reference the ancestor's helper).
pub(crate) fn domain_stem(module: &ModuleBinding) -> Option<String> {
    module
        .error
        .as_ref()
        .map(|e| e.owner_path.to_upper_camel_case())
}

/// Render one declaring module's typed error surface: a
/// `type {TypeName} struct` implementing `error` (so `errors.As` selects on
/// the domain), exported `int32` code constants in the plain-enum const style
/// (`{TypeName}{CodePascal}`), one payload struct per code that declares
/// fields, and the `wvMap{Stem}` helper converting a non-zero slot's
/// `(code, message, payload)` into the typed error (default message when the
/// slot carried none, decoded payload attached when the code declares fields,
/// generic [`ERROR_BRAND`] fallback for unknown codes).
///
/// Domain codes are validated positive-only, so the runtime's reserved
/// negative codes (generic error, producer panic, marshalling failure) can
/// never match a `case` arm here: they fall through to the generic
/// [`ERROR_BRAND`] fallback rather than a typed domain case.
pub(crate) fn render_error(
    out: &mut String,
    module: &ModuleBinding,
    eb: &ErrorBinding,
    prefix: &str,
) {
    let stem = eb.owner_path.to_upper_camel_case();
    let ty = &eb.type_name;
    let dotted = module.segments.join(".");
    let has_payloads = eb.codes.iter().any(|c| !c.fields.is_empty());

    let mut w = CodeWriter::tabs();
    w.line(format!(
        "// {ty} is a typed error reported by the `{dotted}` module."
    ));
    w.block(format!("type {ty} struct {{"), "}", |w| {
        w.line(format!(
            "// Code is the numeric ABI error code (one of the {ty} constants)."
        ));
        w.line("Code int32");
        w.line("// Message is the human-readable error message.");
        w.line("Message string");
        if has_payloads {
            w.line("// Payload holds the matched code's structured fields when that code");
            w.line("// declares any (a pointer to the per-code payload struct), else nil.");
            w.line("Payload any");
        }
    });
    w.blank();
    w.block(format!("func (e *{ty}) Error() string {{"), "}", |w| {
        w.line(format!(
            "return fmt.Sprintf(\"{dotted}: %s (code %d)\", e.Message, e.Code)"
        ));
    });
    w.blank();

    w.line(format!("// {ty} codes."));
    w.block("const (", ")", |w| {
        for c in &eb.codes {
            let cname = format!("{ty}{}", c.name.to_upper_camel_case());
            let doc = c.doc.clone().unwrap_or_else(|| c.message.clone());
            let mut cd = String::new();
            emit_doc(&mut cd, &Some(doc), "\t", Some(&cname));
            w.raw(cd);
            w.line(format!("{cname} int32 = {}", c.value));
        }
    });
    w.blank();

    // One payload struct per code that declares structured fields.
    for c in &eb.codes {
        if c.fields.is_empty() {
            continue;
        }
        let cname = format!("{ty}{}", c.name.to_upper_camel_case());
        let pname = format!("{cname}Payload");
        w.line(format!(
            "// {pname} carries the structured fields of {cname}."
        ));
        w.block(format!("type {pname} struct {{"), "}", |w| {
            for f in &c.fields {
                let fname = f.name.to_upper_camel_case();
                let mut fd = String::new();
                emit_doc(&mut fd, &f.doc, "\t", Some(&fname));
                w.raw(fd);
                w.line(format!("{fname} {}", go_type(&f.ty)));
            }
        });
        w.blank();
    }

    w.line(format!(
        "// wvMap{stem} converts a non-zero code from the `{dotted}` domain into a"
    ));
    w.line(format!(
        "// *{ty}, falling back to the generic *{ERROR_BRAND} for unknown codes."
    ));
    w.block(
        format!("func wvMap{stem}(code int32, message string, payload []byte) error {{"),
        "}",
        |w| {
            w.line("switch code {");
            for c in &eb.codes {
                let cname = format!("{ty}{}", c.name.to_upper_camel_case());
                w.line(format!("case {cname}:"));
                w.indent();
                w.block("if message == \"\" {", "}", |w| {
                    w.line(format!("message = {}", go_str(&c.message)));
                });
                if c.fields.is_empty() {
                    w.line(format!("return &{ty}{{Code: code, Message: message}}"));
                } else {
                    let pname = format!("{cname}Payload");
                    w.line(format!("e := &{ty}{{Code: code, Message: message}}"));
                    w.block("if payload != nil {", "}", |w| {
                        w.line("r := &wvReader{buf: payload}");
                        w.line(format!("p := &{pname}{{}}"));
                        for f in &c.fields {
                            let fname = f.name.to_upper_camel_case();
                            emit_buffer_read(
                                w,
                                "r",
                                &format!("p.{fname}"),
                                &f.ty,
                                &fname,
                                0,
                                prefix,
                                &eb.owner_path,
                            );
                        }
                        w.line("r.expectEnd()");
                        w.line("e.Payload = p");
                    });
                    w.line("return e");
                }
                w.dedent();
            }
            w.line("default:");
            w.indent();
            w.line("return wvBrandError(code, message, payload)");
            w.dedent();
            w.line("}");
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// Render one plain C-style enum as an `int32` newtype with exported
/// constants. Rich (algebraic) enums are value sum types rendered by
/// [`render_rich_enum`]; each renderer skips the other kind.
pub(crate) fn render_enum(out: &mut String, e: &EnumBinding) {
    if e.is_rich() {
        return;
    }
    let name = e.name.to_upper_camel_case();
    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &e.doc, "", Some(&name));
    w.raw(d);
    w.line(format!("type {name} int32"));
    w.blank();
    w.block("const (", ")", |w| {
        for v in &e.variants {
            let vname = format!("{name}{}", v.name.to_upper_camel_case());
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "\t", Some(&vname));
            w.raw(vd);
            w.line(format!("{vname} {name} = {}", v.value));
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as an idiomatic Go sum type: a sealed
/// interface (`type Shape interface { isShape() }`) with one struct per
/// variant (`ShapeCircle`, holding that variant's fields as exported struct
/// fields), plus the pack/unpack pair serializing the `i32` tag followed by
/// the active variant's fields in wire order. Rich enums have no C symbols;
/// values only cross the ABI inside value buffers.
///
/// A plain C-style enum is skipped here (it is handled by [`render_enum`]).
pub(crate) fn render_rich_enum(out: &mut String, prefix: &str, module: &str, e: &EnumBinding) {
    if !e.is_rich() {
        return;
    }
    let name = e.name.to_upper_camel_case();

    let mut w = CodeWriter::tabs();
    if e.doc.is_some() {
        let mut d = String::new();
        emit_doc(&mut d, &e.doc, "", Some(&name));
        w.raw(d);
        w.line("//");
    }
    w.line(format!(
        "// {name} is a sealed sum type: exactly one of its variant structs is the"
    ));
    w.line("// value at a time.");
    w.block(format!("type {name} interface {{"), "}", |w| {
        w.line(format!("is{name}()"));
    });
    w.blank();

    for v in &e.variants {
        let vn = format!("{name}{}", v.name.to_upper_camel_case());
        let mut vd = String::new();
        emit_doc(&mut vd, &v.doc, "", Some(&vn));
        if vd.is_empty() {
            w.line(format!("// {vn} is the `{}` variant of {name}.", v.name));
        } else {
            w.raw(vd);
        }
        if v.fields.is_empty() {
            w.line(format!("type {vn} struct{{}}"));
        } else {
            w.block(format!("type {vn} struct {{"), "}", |w| {
                for f in &v.fields {
                    let fname = f.name.to_upper_camel_case();
                    let mut fd = String::new();
                    emit_doc(&mut fd, &f.doc, "\t", Some(&fname));
                    w.raw(fd);
                    w.line(format!("{fname} {}", go_type(&f.ty)));
                }
            });
        }
        w.blank();
        w.line(format!("func ({vn}) is{name}() {{}}"));
        w.blank();
    }

    w.line(format!(
        "// wvPack{name} appends v to w in the value-buffer wire format."
    ));
    w.block(
        format!("func wvPack{name}(w *wvWriter, v {name}) {{"),
        "}",
        |w| {
            w.line("switch x := v.(type) {");
            for v in &e.variants {
                let vn = format!("{name}{}", v.name.to_upper_camel_case());
                w.line(format!("case {vn}:"));
                w.indent();
                w.line(format!("w.writeI32({})", v.value));
                for f in &v.fields {
                    let fname = f.name.to_upper_camel_case();
                    let site = format!("{}{fname}", v.name.to_upper_camel_case());
                    emit_buffer_write(w, "w", &format!("x.{fname}"), &f.ty, &site, 0);
                }
                w.dedent();
            }
            w.line("default:");
            w.indent();
            w.line(format!(
                "panic(\"weaveffi: {name} value is not one of its variants\")"
            ));
            w.dedent();
            w.line("}");
        },
    );
    w.blank();

    w.line(format!("// wvUnpack{name} decodes one {name} from r."));
    w.block(
        format!("func wvUnpack{name}(r *wvReader) {name} {{"),
        "}",
        |w| {
            w.line("switch r.readI32() {");
            for v in &e.variants {
                let vn = format!("{name}{}", v.name.to_upper_camel_case());
                w.line(format!("case {}:", v.value));
                w.indent();
                if v.fields.is_empty() {
                    w.line(format!("return {vn}{{}}"));
                } else {
                    w.line(format!("var x {vn}"));
                    for f in &v.fields {
                        let fname = f.name.to_upper_camel_case();
                        let site = format!("{}{fname}", v.name.to_upper_camel_case());
                        emit_buffer_read(
                            w,
                            "r",
                            &format!("x.{fname}"),
                            &f.ty,
                            &site,
                            0,
                            prefix,
                            module,
                        );
                    }
                    w.line("return x");
                }
                w.dedent();
            }
            w.line("default:");
            w.indent();
            w.line(format!(
                "panic(\"weaveffi: malformed value buffer: {name} tag out of range\")"
            ));
            w.dedent();
            w.line("}");
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// Render one record as a plain Go value struct with exported, typed fields,
/// plus its pack/unpack pair serializing the fields in declaration (wire)
/// order. Records have no C symbols: no create, no destroy, no getters, no
/// builders; instances only cross the ABI inside value buffers.
pub(crate) fn render_struct(out: &mut String, prefix: &str, module: &str, s: &StructBinding) {
    let name = s.name.to_upper_camel_case();

    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &s.doc, "", Some(&name));
    w.raw(d);
    w.block(format!("type {name} struct {{"), "}", |w| {
        for f in &s.fields {
            let fname = f.name.to_upper_camel_case();
            let mut fd = String::new();
            emit_doc(&mut fd, &f.doc, "\t", Some(&fname));
            w.raw(fd);
            w.line(format!("{fname} {}", go_type(&f.ty)));
        }
    });
    w.blank();

    w.line(format!(
        "// wvPack{name} appends v to w in the value-buffer wire format."
    ));
    w.block(
        format!("func wvPack{name}(w *wvWriter, v {name}) {{"),
        "}",
        |w| {
            for f in &s.fields {
                let fname = f.name.to_upper_camel_case();
                emit_buffer_write(w, "w", &format!("v.{fname}"), &f.ty, &fname, 0);
            }
        },
    );
    w.blank();

    w.line(format!("// wvUnpack{name} decodes one {name} from r."));
    w.block(
        format!("func wvUnpack{name}(r *wvReader) {name} {{"),
        "}",
        |w| {
            w.line(format!("var v {name}"));
            for f in &s.fields {
                let fname = f.name.to_upper_camel_case();
                emit_buffer_read(
                    w,
                    "r",
                    &format!("v.{fname}"),
                    &f.ty,
                    &fname,
                    0,
                    prefix,
                    module,
                );
            }
            w.line("return v");
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// Render one interface as an opaque-object wrapper: a struct owning the
/// `*C.{c_tag}` handle, freed by an explicit `Close` (idempotent, nils the
/// pointer).
///
/// Constructors become package-level factory functions named
/// `{PascalCtor}{Type}` (`new` gives `NewStore`, `open` gives `OpenStore`);
/// methods are methods on the wrapper passing `s.ptr` as the leading C
/// argument; statics are package-level functions namespaced by the type
/// (`StoreDefaultCapacity`). Members reuse the free-function marshalling
/// paths, including the sync/async/iterator shapes and the throws split.
pub(crate) fn render_interface(
    out: &mut String,
    prefix: &str,
    m: &ModuleBinding,
    iface: &InterfaceBinding,
    stem: Option<&str>,
) {
    let name = local_type_name(&iface.name).to_upper_camel_case();
    let c_tag = &iface.c_tag;

    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &iface.doc, "", Some(&name));
    w.raw(d);
    w.block(format!("type {name} struct {{"), "}", |w| {
        w.line(format!("ptr *C.{c_tag}"));
    });
    w.blank();
    out.push_str(&w.finish());

    for c in &iface.constructors {
        let go_name = format!("{}{name}", c.name.to_upper_camel_case());
        let err = ErrCtx::of(c, stem);
        render_function(out, prefix, &m.path, c, &go_name, None, err);
    }

    for f in &iface.methods {
        let go_name = f.name.to_upper_camel_case();
        let err = ErrCtx::of(f, stem);
        if let CallShape::Async(ab) = &f.shape {
            render_async_function(out, prefix, &m.path, f, ab, &go_name, Some(&name), err);
        } else {
            render_function(out, prefix, &m.path, f, &go_name, Some(&name), err);
        }
    }

    for f in &iface.statics {
        let go_name = format!("{name}{}", f.name.to_upper_camel_case());
        let err = ErrCtx::of(f, stem);
        if let CallShape::Async(ab) = &f.shape {
            render_async_function(out, prefix, &m.path, f, ab, &go_name, None, err);
        } else {
            render_function(out, prefix, &m.path, f, &go_name, None, err);
        }
    }

    let mut w = CodeWriter::tabs();
    w.block(format!("func (s *{name}) Close() {{"), "}", |w| {
        w.block("if s.ptr != nil {", "}", |w| {
            w.line(format!("C.{}(s.ptr)", iface.destroy_symbol));
            w.line("s.ptr = nil");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Collect every typed-handle referent reachable from the model's type
/// positions, deduplicated by wrapper name in first-occurrence order (which
/// keeps the emitted set deterministic).
pub(crate) fn collect_typed_handles(model: &BindingModel, prefix: &str) -> Vec<(String, String)> {
    fn visit(
        ty: &Ty,
        module: &str,
        prefix: &str,
        seen: &mut HashSet<String>,
        out: &mut Vec<(String, String)>,
    ) {
        match ty {
            Ty::TypedHandle(n) => {
                let name = handle_wrapper(n);
                if seen.insert(name.clone()) {
                    out.push((name, c_abi_struct_name(n, module, prefix)));
                }
            }
            Ty::Optional(i) | Ty::List(i) | Ty::Iterator(i) => {
                visit(i, module, prefix, seen, out);
            }
            Ty::Map(k, v) => {
                visit(k, module, prefix, seen, out);
                visit(v, module, prefix, seen, out);
            }
            _ => {}
        }
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in &model.modules {
        for s in &m.structs {
            for f in &s.fields {
                visit(&f.ty, &m.path, prefix, &mut seen, &mut out);
            }
        }
        for e in &m.enums {
            for v in &e.variants {
                for f in &v.fields {
                    visit(&f.ty, &m.path, prefix, &mut seen, &mut out);
                }
            }
        }
        if let Some(eb) = &m.error {
            if eb.declared_here {
                for c in &eb.codes {
                    for f in &c.fields {
                        visit(&f.ty, &m.path, prefix, &mut seen, &mut out);
                    }
                }
            }
        }
        for cb in &m.callbacks {
            for p in &cb.params {
                visit(&p.ty, &m.path, prefix, &mut seen, &mut out);
            }
        }
        for f in m.callables() {
            for p in &f.params {
                visit(&p.ty, &m.path, prefix, &mut seen, &mut out);
            }
            if let Some(ret) = &f.ret {
                visit(ret, &m.path, prefix, &mut seen, &mut out);
            }
        }
    }
    out
}

/// Render one wrapper struct per typed-handle referent. A typed handle is a
/// borrowed opaque id with no destroy symbol, so the wrapper carries no
/// `Close`.
pub(crate) fn render_typed_handles(out: &mut String, handles: &[(String, String)]) {
    let mut w = CodeWriter::tabs();
    for (name, tag) in handles {
        w.line(format!(
            "// {name} is a typed handle naming a producer-owned resource. It wraps"
        ));
        w.line("// the opaque C pointer and owes no release call.");
        w.block(format!("type {name} struct {{"), "}", |w| {
            w.line(format!("ptr *C.{tag}"));
        });
        w.blank();
    }
    out.push_str(&w.finish());
}
