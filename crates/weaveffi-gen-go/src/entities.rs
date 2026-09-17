//! Entity rendering: plain and rich enums, records, typed error domains, and
//! interface (object) wrapper types.

use heck::ToUpperCamelCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{
    CallShape, EnumBinding, ErrorBinding, InterfaceBinding, ModuleBinding, StructBinding,
};
use weaveffi_core::utils::local_type_name;

use crate::calls::{render_async_function, render_function, ErrCtx};
use crate::codec::{emit_buffer_read, emit_buffer_write};
use crate::docs::emit_doc;
use crate::types::{adopt_fn, go_str, go_type, token_fn, untoken_fn};

/// One `name rest` line inside a struct or const block, with its optional
/// doc comment, for [`emit_aligned`].
struct AlignedEntry {
    doc: Option<String>,
    name: String,
    rest: String,
}

/// Emit `entries` as the body of a struct or const block the way gofmt lays
/// it out: gofmt aligns the second column of consecutive lines and starts a
/// fresh column after a comment line, so each run of entries following a
/// documented one (or the start) is padded to the widest name in that run.
fn emit_aligned(w: &mut CodeWriter, entries: &[AlignedEntry]) {
    let docs: Vec<String> = entries
        .iter()
        .map(|e| {
            let mut d = String::new();
            emit_doc(&mut d, &e.doc, "\t", Some(&e.name));
            d
        })
        .collect();
    let mut i = 0;
    while i < entries.len() {
        let mut j = i + 1;
        while j < entries.len() && docs[j].is_empty() {
            j += 1;
        }
        let width = entries[i..j]
            .iter()
            .map(|e| e.name.len())
            .max()
            .unwrap_or(0);
        for (e, d) in entries[i..j].iter().zip(&docs[i..j]) {
            w.raw(d.clone());
            w.line(format!("{:<width$} {}", e.name, e.rest));
        }
        i = j;
    }
}

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
pub(crate) fn render_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding) {
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
        let entries: Vec<AlignedEntry> = eb
            .codes
            .iter()
            .map(|c| AlignedEntry {
                doc: Some(c.doc.clone().unwrap_or_else(|| c.message.clone())),
                name: format!("{ty}{}", c.name.to_upper_camel_case()),
                rest: format!("int32 = {}", c.value),
            })
            .collect();
        emit_aligned(w, &entries);
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
            let entries: Vec<AlignedEntry> = c
                .fields
                .iter()
                .map(|f| AlignedEntry {
                    doc: f.doc.clone(),
                    name: f.name.to_upper_camel_case(),
                    rest: go_type(&f.ty),
                })
                .collect();
            emit_aligned(w, &entries);
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
                            emit_buffer_read(w, "r", &format!("p.{fname}"), &f.ty, &fname, 0);
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
        let entries: Vec<AlignedEntry> = e
            .variants
            .iter()
            .map(|v| AlignedEntry {
                doc: v.doc.clone(),
                name: format!("{name}{}", v.name.to_upper_camel_case()),
                rest: format!("{name} = {}", v.value),
            })
            .collect();
        emit_aligned(w, &entries);
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
pub(crate) fn render_rich_enum(out: &mut String, e: &EnumBinding) {
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
                let entries: Vec<AlignedEntry> = v
                    .fields
                    .iter()
                    .map(|f| AlignedEntry {
                        doc: f.doc.clone(),
                        name: f.name.to_upper_camel_case(),
                        rest: go_type(&f.ty),
                    })
                    .collect();
                emit_aligned(w, &entries);
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
                        emit_buffer_read(w, "r", &format!("x.{fname}"), &f.ty, &site, 0);
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
/// builders; instances only cross the ABI inside value buffers. An interface
/// field holds a wrapper pointer and crosses as an object token (see
/// [`crate::codec`]).
pub(crate) fn render_struct(out: &mut String, s: &StructBinding) {
    let name = s.name.to_upper_camel_case();

    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &s.doc, "", Some(&name));
    w.raw(d);
    w.block(format!("type {name} struct {{"), "}", |w| {
        let entries: Vec<AlignedEntry> = s
            .fields
            .iter()
            .map(|f| AlignedEntry {
                doc: f.doc.clone(),
                name: f.name.to_upper_camel_case(),
                rest: go_type(&f.ty),
            })
            .collect();
        emit_aligned(w, &entries);
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
                emit_buffer_read(w, "r", &format!("v.{fname}"), &f.ty, &fname, 0);
            }
            w.line("return v");
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// Render one interface as a reference-counted object wrapper: a struct
/// holding one strong reference (`*C.{c_tag}`) released by an explicit
/// `Close` (idempotent, nils the pointer) or, as a backstop, by a
/// `runtime.SetFinalizer` hook, so `destroy` runs exactly once per wrapper.
///
/// Three private helpers accompany the type: `wvAdopt{Name}` wraps an owned
/// pointer coming back from the producer (a return, an async result, an
/// iterator element, a callback-method argument) and returns nil for a null
/// pointer, so `Interface?` needs no separate path; `wvToken{Name}` clones
/// the wrapper's reference into a value-buffer object token; and
/// `wvUntoken{Name}` adopts the reference a token carries.
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
    let adopt = adopt_fn(&iface.name);
    let token = token_fn(&iface.name);
    let untoken = untoken_fn(&iface.name);

    let mut w = CodeWriter::tabs();
    let mut d = String::new();
    emit_doc(&mut d, &iface.doc, "", Some(&name));
    if d.is_empty() {
        w.line(format!(
            "// {name} is a reference-counted object owned by the native library."
        ));
    } else {
        w.raw(d);
        w.line("//");
    }
    w.line("// Each wrapper holds one strong reference; Close releases it, and a");
    w.line("// finalizer releases it if the wrapper is garbage collected first.");
    if let Some(msg) = &iface.deprecated {
        w.line("//");
        w.line(format!("// Deprecated: {msg}"));
    }
    w.block(format!("type {name} struct {{"), "}", |w| {
        w.line(format!("ptr *C.{c_tag}"));
    });
    w.blank();

    w.line(format!(
        "// {adopt} adopts one owned strong reference into a new wrapper. A null"
    ));
    w.line("// pointer adopts to nil.");
    w.block(
        format!("func {adopt}(ptr *C.{c_tag}) *{name} {{"),
        "}",
        |w| {
            w.block("if ptr == nil {", "}", |w| {
                w.line("return nil");
            });
            w.line(format!("s := &{name}{{ptr: ptr}}"));
            w.line(format!("runtime.SetFinalizer(s, (*{name}).Close)"));
            w.line("return s");
        },
    );
    w.blank();

    w.line(format!(
        "// {token} clones o's reference into a value-buffer object token. The"
    ));
    w.line("// wrapper keeps its own reference; the token carries the new one.");
    w.block(format!("func {token}(o *{name}) uint64 {{"), "}", |w| {
        w.block("if o == nil || o.ptr == nil {", "}", |w| {
            w.line(format!(
                "panic(\"weaveffi: nil or closed {name} cannot be encoded in a non-optional position\")"
            ));
        });
        w.line(format!(
            "return uint64(uintptr(unsafe.Pointer(C.{}(o.ptr))))",
            iface.clone_symbol
        ));
    });
    w.blank();

    w.line(format!(
        "// {untoken} adopts the strong reference carried by a value-buffer object"
    ));
    w.line("// token.");
    w.block(
        format!("func {untoken}(token uint64) *{name} {{"),
        "}",
        |w| {
            w.block("if token == 0 {", "}", |w| {
                w.line("panic(\"weaveffi: malformed value buffer: null object token\")");
            });
            w.line(format!(
                "return {adopt}((*C.{c_tag})(C.wvHandlePtr(C.uintptr_t(token))))"
            ));
        },
    );
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
    w.line("// Close releases the wrapper's strong reference. Calling it more than once");
    w.line("// is harmless; the object itself is dropped by the native library when its");
    w.line("// last reference (from any wrapper, record, or producer) is released.");
    w.block(format!("func (s *{name}) Close() {{"), "}", |w| {
        w.block("if s.ptr != nil {", "}", |w| {
            w.line(format!("C.{}(s.ptr)", iface.destroy_symbol));
            w.line("s.ptr = nil");
            w.line("runtime.SetFinalizer(s, nil)");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}
