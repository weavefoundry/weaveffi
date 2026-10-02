//! Entity rendering: plain and rich enums, records, typed error domains, and
//! interface (object) wrapper types.

use crate::codegen::CodeWriter;
use crate::utils::local_type_name;
use heck::ToUpperCamelCase;
use weaveffi_model::model::{
    CallShape, EnumBinding, ErrorBinding, InterfaceBinding, ModuleBinding, StructBinding,
};

use crate::targets::go::calls::{render_async_function, render_function, ErrCtx};
use crate::targets::go::codec::{emit_buffer_read, emit_buffer_write};
use crate::targets::go::docs::emit_doc;
use crate::targets::go::types::{adopt_fn, go_str, go_type, token_fn, untoken_fn};

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
/// generic `Error` fallback for unknown codes).
///
/// Domain codes are validated positive-only, so the runtime's reserved
/// negative codes (generic error, producer panic, marshalling failure) can
/// never match a `case` arm here: they fall through to the generic
/// `Error` fallback rather than a typed domain case.
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
        "// *{ty}, falling back to the generic *Error for unknown codes."
    ));
    w.block(
        format!("func wvMap{stem}(f wvFailure) error {{"),
        "}",
        |w| {
            w.line("switch f.code {");
            for c in &eb.codes {
                let cname = format!("{ty}{}", c.name.to_upper_camel_case());
                w.line(format!("case {cname}:"));
                w.indent();
                w.block("if f.message == \"\" {", "}", |w| {
                    w.line(format!("f.message = {}", go_str(&c.message)));
                });
                if c.fields.is_empty() {
                    w.line(format!("return &{ty}{{Code: f.code, Message: f.message}}"));
                } else {
                    let pname = format!("{cname}Payload");
                    w.line(format!("e := &{ty}{{Code: f.code, Message: f.message}}"));
                    w.block("if f.payload != nil {", "}", |w| {
                        w.line("r := &wvReader{buf: f.payload}");
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
            w.line("return f.err()");
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
pub(crate) fn render_rich_enum(out: &mut String, package: &str, e: &EnumBinding) {
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
                "panic(\"{package}: {name} value is not one of its variants\")"
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
                "panic(\"{package}: malformed value buffer: {name} tag out of range\")"
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
/// [`crate::targets::go::codec`]).
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
/// whose `wvRef` owns one strong reference, released by an explicit `Close`
/// (idempotent and safe to race with in-flight calls) or, as a backstop, by
/// a finalizer, so `destroy` runs exactly once per wrapper.
///
/// Private helpers accompany the type: `native` borrows the pointer for one
/// call (panicking after `Close`); `wvAdopt{Name}` wraps an owned pointer
/// coming back from the producer (a return, an async result, an iterator
/// element, a callback-method argument) and returns nil for a null pointer,
/// so `Interface?` needs no separate path; `wvToken{Name}` clones the
/// wrapper's reference into a value-buffer object token; and
/// `wvUntoken{Name}` adopts the reference a token carries.
///
/// Constructors become package-level factory functions named
/// `{PascalCtor}{Type}` (`new` gives `NewStore`, `open` gives `OpenStore`);
/// methods are methods on the wrapper passing its pointer as the leading C
/// argument; statics are package-level functions namespaced by the type
/// (`StoreDefaultCapacity`). Members reuse the free-function marshalling
/// paths, including the sync/async/iterator shapes and the throws split.
pub(crate) fn render_interface(
    out: &mut String,
    prefix: &str,
    package: &str,
    iface: &InterfaceBinding,
    stem: Option<&str>,
) {
    let name = local_type_name(&iface.name).to_upper_camel_case();
    let c_tag = &iface.c_tag;
    let adopt = adopt_fn(&iface.name);
    let token = token_fn(&iface.name);
    let untoken = untoken_fn(&iface.name);
    let destroy = format!("wvDestroy{name}");

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
        w.line("ref wvRef");
    });
    w.blank();

    w.block(format!("func {destroy}(ptr unsafe.Pointer) {{"), "}", |w| {
        w.line(format!("C.{}((*C.{c_tag})(ptr))", iface.destroy_symbol));
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
            w.line(format!("s := &{name}{{}}"));
            w.line(format!("s.ref.init(unsafe.Pointer(ptr), {destroy})"));
            w.line(format!("runtime.SetFinalizer(s, (*{name}).Close)"));
            w.line("return s");
        },
    );
    w.blank();

    w.line("// native borrows the wrapper's pointer for one call; pair it with a");
    w.line("// deferred s.ref.release().");
    w.block(
        format!("func (s *{name}) native() *C.{c_tag} {{"),
        "}",
        |w| {
            w.block("if s == nil {", "}", |w| {
                w.line(format!("panic(\"{package}: nil *{name}\")"));
            });
            w.line(format!("return (*C.{c_tag})(s.ref.acquire(\"{name}\"))"));
        },
    );
    w.blank();

    w.line(format!(
        "// {token} clones o's reference into a value-buffer object token. The"
    ));
    w.line("// wrapper keeps its own reference; the token carries the new one.");
    w.block(format!("func {token}(o *{name}) uint64 {{"), "}", |w| {
        w.line("ptr := o.native()");
        w.line("defer o.ref.release()");
        w.line(format!(
            "return uint64(uintptr(unsafe.Pointer(C.{}(ptr))))",
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
                w.line(format!(
                    "panic(\"{package}: malformed value buffer: null object token\")"
                ));
            });
            w.line(format!(
                "return {adopt}((*C.{c_tag})(C.wvHandlePtr(C.uintptr_t(token))))"
            ));
        },
    );
    w.blank();

    w.line("// Close releases the wrapper's strong reference and always returns nil.");
    w.line("// It's idempotent and safe to call from any goroutine, even while a call");
    w.line("// on the wrapper is in flight: the reference is then released when that");
    w.line("// call returns. The object itself is dropped once its last reference, from");
    w.line("// any wrapper, record, or the native side, is gone.");
    w.block(format!("func (s *{name}) Close() error {{"), "}", |w| {
        w.line("runtime.SetFinalizer(s, nil)");
        w.line("s.ref.close()");
        w.line("return nil");
    });
    w.blank();
    out.push_str(&w.finish());

    for c in &iface.constructors {
        let go_name = format!("{}{name}", c.name.to_upper_camel_case());
        render_function(out, prefix, c, &go_name, None, ErrCtx::of(c, stem));
    }

    for f in &iface.methods {
        let go_name = f.name.to_upper_camel_case();
        let err = ErrCtx::of(f, stem);
        if let CallShape::Async(ab) = &f.shape {
            render_async_function(out, prefix, f, ab, &go_name, Some(&name), err);
        } else {
            render_function(out, prefix, f, &go_name, Some(&name), err);
        }
    }

    for f in &iface.statics {
        let go_name = format!("{name}{}", f.name.to_upper_camel_case());
        let err = ErrCtx::of(f, stem);
        if let CallShape::Async(ab) = &f.shape {
            render_async_function(out, prefix, f, ab, &go_name, None, err);
        } else {
            render_function(out, prefix, f, &go_name, None, err);
        }
    }
}
