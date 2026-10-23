//! Entity rendering: C-style and rich enums, records, error domains, and
//! interface (object) wrappers, each with its value-buffer codec pair.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{
    EnumBinding, ErrorBinding, FieldBinding, InterfaceBinding, StructBinding,
};
use weaveffi_model::plan::CallbackRetPass;

use crate::targets::go::calls::{render_function, Receiver};
use crate::targets::go::codec::{func, read_expr, write_stmt};
use crate::targets::go::docs::GoDoc;
use crate::targets::go::names::{self, constructor, pascal};
use crate::targets::go::types::{go_str, go_type};
use crate::targets::go::Ctx;

/// One `name rest` line inside a struct or const block, with its doc
/// comment, for [`emit_aligned`].
struct AlignedEntry {
    doc: Option<String>,
    name: String,
    rest: String,
}

/// Emit `entries` as the body of a struct or const block the way gofmt lays
/// it out: gofmt aligns the second column of consecutive lines and starts a
/// fresh column after a comment line, so each run of entries following a
/// documented one (or the start) is padded to the widest name in that run.
fn emit_aligned(w: &mut CodeWriter, entries: Vec<AlignedEntry>) {
    let mut i = 0;
    while i < entries.len() {
        let mut j = i + 1;
        while j < entries.len() && entries[j].doc.is_none() {
            j += 1;
        }
        let width = entries[i..j]
            .iter()
            .map(|e| e.name.len())
            .max()
            .unwrap_or(0);
        for e in &entries[i..j] {
            GoDoc::plain(e.doc.clone()).emit(w);
            w.line(format!("{:<width$} {}", e.name, e.rest));
        }
        i = j;
    }
}

/// The struct-body entries of `fields`, named by `name`.
fn field_entries(
    ctx: &Ctx,
    fields: &[FieldBinding],
    name: fn(&str) -> String,
) -> Vec<AlignedEntry> {
    fields
        .iter()
        .map(|f| AlignedEntry {
            doc: ctx.docs.of(&f.doc, &None).0,
            name: name(&f.name),
            rest: go_type(&f.ty),
        })
        .collect()
}

/// Render a C-style enum: an `int32` type with one constant per variant,
/// plus its codec pair when it appears inside a buffer.
pub(crate) fn render_enum(w: &mut CodeWriter, ctx: &Ctx, e: &EnumBinding) {
    let name = pascal(&e.name);
    let (doc, deprecated) = ctx.docs.of(&e.doc, &e.deprecated);
    GoDoc::decl(&name, doc).deprecated(deprecated).emit(w);
    w.line(format!("type {name} int32"));
    w.blank();
    w.block("const (", ")", |w| {
        let entries = e
            .variants
            .iter()
            .map(|v| AlignedEntry {
                doc: ctx.docs.of(&v.doc, &None).0,
                name: format!("{name}{}", pascal(&v.name)),
                rest: format!("{name} = {}", v.value),
            })
            .collect();
        emit_aligned(w, entries);
    });
    w.blank();
    if ctx.codecs.has_enum(&e.name) {
        func(
            w,
            &format!("func wvWrite{name}(w *wvWriter, v {name})"),
            "w.writeI32(int32(v))",
        );
        func(
            w,
            &format!("func wvRead{name}(r *wvReader) {name}"),
            &format!("return {name}(r.readI32())"),
        );
    }
}

/// Render a rich enum as a sealed interface (`type Shape interface {
/// isShape() }`) with one struct per variant (`ShapeCircle`), plus its codec
/// pair: the `i32` tag, then the variant's fields in order.
pub(crate) fn render_rich_enum(w: &mut CodeWriter, ctx: &Ctx, e: &EnumBinding) {
    let package = ctx.package;
    let name = pascal(&e.name);
    let (doc, deprecated) = ctx.docs.of(&e.doc, &e.deprecated);
    GoDoc::decl(&name, doc)
        .para(&format!(
            "{name} is a sealed sum type: exactly one of its variant structs is the \
             value at a time."
        ))
        .deprecated(deprecated)
        .emit(w);
    w.block(format!("type {name} interface {{"), "}", |w| {
        w.line(format!("is{name}()"));
    });
    w.blank();

    for v in &e.variants {
        let vn = format!("{name}{}", pascal(&v.name));
        let text = ctx.docs.of(&v.doc, &None).0;
        let doc = match text {
            Some(_) => GoDoc::decl(&vn, text),
            None => GoDoc::plain(None).para(&format!("{vn} is the {} variant of {name}.", v.name)),
        };
        doc.emit(w);
        if v.fields.is_empty() {
            w.line(format!("type {vn} struct{{}}"));
        } else {
            w.block(format!("type {vn} struct {{"), "}", |w| {
                emit_aligned(w, field_entries(ctx, &v.fields, names::field));
            });
        }
        w.blank();
        w.line(format!("func ({vn}) is{name}() {{}}"));
        w.blank();
    }

    let any_fields = e.variants.iter().any(|v| !v.fields.is_empty());
    let switch = if any_fields {
        "switch x := v.(type) {"
    } else {
        "switch v.(type) {"
    };
    w.block(
        format!("func wvWrite{name}(w *wvWriter, v {name}) {{"),
        "}",
        |w| {
            w.line(switch);
            for v in &e.variants {
                w.line(format!("case {name}{}:", pascal(&v.name)));
                w.scope(|w| {
                    w.line(format!("w.writeI32({})", v.value));
                    for f in &v.fields {
                        let expr = format!("x.{}", names::field(&f.name));
                        w.line(write_stmt("w", &expr, &f.ty));
                    }
                });
            }
            w.line("default:");
            w.scope(|w| {
                w.line(format!(
                    "panic(\"{package}: {name} value is not one of its variants\")"
                ));
            });
            w.line("}");
        },
    );
    w.blank();
    w.block(
        format!("func wvRead{name}(r *wvReader) {name} {{"),
        "}",
        |w| {
            w.line("switch r.readI32() {");
            for v in &e.variants {
                let vn = format!("{name}{}", pascal(&v.name));
                w.line(format!("case {}:", v.value));
                w.scope(|w| {
                    if v.fields.is_empty() {
                        w.line(format!("return {vn}{{}}"));
                    } else {
                        w.line(format!("var x {vn}"));
                        for f in &v.fields {
                            w.line(format!(
                                "x.{} = {}",
                                names::field(&f.name),
                                read_expr("r", &f.ty)
                            ));
                        }
                        w.line("return x");
                    }
                });
            }
            w.line("}");
            w.line(format!("wvMalformed(\"{name} tag out of range\")"));
            w.line("return nil");
        },
    );
    w.blank();
}

/// Render a record as a value struct with exported fields, plus its codec
/// pair: the fields in declaration order.
pub(crate) fn render_struct(w: &mut CodeWriter, ctx: &Ctx, s: &StructBinding) {
    let name = pascal(&s.name);
    let (doc, deprecated) = ctx.docs.of(&s.doc, &s.deprecated);
    GoDoc::decl(&name, doc).deprecated(deprecated).emit(w);
    w.block(format!("type {name} struct {{"), "}", |w| {
        emit_aligned(w, field_entries(ctx, &s.fields, names::field));
    });
    w.blank();
    w.block(
        format!("func wvWrite{name}(w *wvWriter, v {name}) {{"),
        "}",
        |w| {
            for f in &s.fields {
                let expr = format!("v.{}", names::field(&f.name));
                w.line(write_stmt("w", &expr, &f.ty));
            }
        },
    );
    w.blank();
    w.block(
        format!("func wvRead{name}(r *wvReader) {name} {{"),
        "}",
        |w| {
            w.line(format!("var v {name}"));
            for f in &s.fields {
                w.line(format!(
                    "v.{} = {}",
                    names::field(&f.name),
                    read_expr("r", &f.ty)
                ));
            }
            w.line("return v");
        },
    );
    w.blank();
}

/// Render an error domain: the sealed interface every code type implements
/// (`KitchenError`), one `*{Code}Error` struct per code (its payload
/// fields, a `Message`, `Error`, and `Code`), the `*Unknown{Domain}` type of
/// a code these bindings don't declare, and the helper mapping a failure
/// onto the domain (an `*Error` for a runtime code).
pub(crate) fn render_error(w: &mut CodeWriter, ctx: &Ctx, e: &ErrorBinding) {
    let gn = ctx.names;
    let d = gn.domain_of(e);
    let domain = &d.iface;
    let marker = format!("is{domain}");
    let codes: Vec<String> = e.codes.iter().map(|c| format!("*{}", gn.code(c))).collect();
    GoDoc::plain(None)
        .para(&format!(
            "{domain} is an error of the {} domain: {}, or *{} for a code these \
             bindings don't declare.",
            e.name,
            codes.join(", "),
            d.unknown
        ))
        .para(&format!(
            "Match one code with errors.As and its type, or any code of the domain \
             with a {domain}. A callback method that declares the domain returns \
             one to report that code."
        ))
        .emit(w);
    w.block(format!("type {domain} interface {{"), "}", |w| {
        w.line("error");
        w.line("// Code returns the error's numeric code.");
        w.line("Code() int32");
        w.line(format!("{marker}()"));
    });
    w.blank();

    for c in &e.codes {
        let ty = gn.code(c);
        let text = ctx
            .docs
            .of(&c.doc, &None)
            .0
            .filter(|d| d.trim() != c.message.trim());
        let doc = match text {
            Some(_) => GoDoc::decl(ty, text),
            None => GoDoc::plain(None).para(&format!(
                "{ty} is the {domain} code {} ({}): {}.",
                c.name,
                c.value,
                c.message.trim_end_matches('.')
            )),
        };
        doc.emit(w);
        w.block(format!("type {ty} struct {{"), "}", |w| {
            emit_aligned(w, field_entries(ctx, &c.fields, names::error_field));
            w.line("// Message describes the failure; when it's empty, Error returns the");
            w.line(format!(
                "// code's default message, {}.",
                go_str(&c.message)
            ));
            w.line("Message string");
        });
        w.blank();
        w.line("// Error returns the error's message.");
        w.block(format!("func (e *{ty}) Error() string {{"), "}", |w| {
            w.block("if e.Message == \"\" {", "}", |w| {
                w.line(format!("return {}", go_str(&c.message)));
            });
            w.line("return e.Message");
        });
        w.blank();
        w.line(format!("// Code returns {}.", c.value));
        func(
            w,
            &format!("func (*{ty}) Code() int32"),
            &format!("return {}", c.value),
        );
        w.line(format!("func (*{ty}) {marker}() {{}}"));
        w.blank();
        if c.fields.is_empty() {
            func(w, &format!("func (*{ty}) wvPayload() []byte"), "return nil");
        } else {
            w.block(format!("func (e *{ty}) wvPayload() []byte {{"), "}", |w| {
                w.line("w := &wvWriter{}");
                for f in &c.fields {
                    let expr = format!("e.{}", names::error_field(&f.name));
                    w.line(write_stmt("w", &expr, &f.ty));
                }
                w.line("return w.buf");
            });
            w.blank();
        }
    }

    let unknown = &d.unknown;
    GoDoc::plain(None)
        .para(&format!(
            "{unknown} is a {domain} code these bindings don't declare, which a \
             newer library may report."
        ))
        .emit(w);
    w.block(format!("type {unknown} struct {{"), "}", |w| {
        w.line("code int32");
        w.line("// Message is the library's message.");
        w.line("Message string");
    });
    w.blank();
    w.line("// Error returns the error's message.");
    w.block(format!("func (e *{unknown}) Error() string {{"), "}", |w| {
        w.block("if e.Message == \"\" {", "}", |w| {
            w.line(format!("return wvCodeMessage({}, e.code)", go_str(&e.name)));
        });
        w.line("return e.Message");
    });
    w.blank();
    w.line("// Code returns the code the library reported.");
    func(
        w,
        &format!("func (e *{unknown}) Code() int32"),
        "return e.code",
    );
    w.line(format!("func (*{unknown}) {marker}() {{}}"));
    w.blank();
    func(
        w,
        &format!("func (*{unknown}) wvPayload() []byte"),
        "return nil",
    );

    w.block(
        format!("func {}(f wvFailure) error {{", d.mapper),
        "}",
        |w| {
            w.line("switch f.code {");
            for c in &e.codes {
                let ty = gn.code(c);
                w.line(format!("case {}:", c.value));
                w.scope(|w| {
                    if c.fields.is_empty() {
                        w.line(format!("return &{ty}{{Message: f.message}}"));
                        return;
                    }
                    w.line(format!("e := &{ty}{{Message: f.message}}"));
                    w.block("if f.payload != nil {", "}", |w| {
                        w.line("r := &wvReader{buf: f.payload}");
                        for f in &c.fields {
                            w.line(format!(
                                "e.{} = {}",
                                names::error_field(&f.name),
                                read_expr("r", &f.ty)
                            ));
                        }
                        w.line("r.expectEnd()");
                    });
                    w.line("return e");
                });
            }
            w.line("}");
            w.block("if f.code > 0 {", "}", |w| {
                w.line(format!(
                    "return &{unknown}{{code: f.code, Message: f.message}}"
                ));
            });
            w.line("return f.err()");
        },
    );
    w.blank();
}

/// Render an interface as a reference-counted object wrapper embedding a
/// `wvObject`, which owns one strong reference released by `Close`
/// (idempotent, and safe to race with in-flight calls) or by a cleanup once
/// the wrapper is unreachable; its codec pair when it appears inside a
/// buffer (an object token carrying a fresh reference); and its members:
/// constructors as factories (`OpenStore`), methods on the wrapper, and
/// statics prefixed with the type (`StoreDefaultCapacity`).
pub(crate) fn render_interface(w: &mut CodeWriter, ctx: &Ctx, iface: &InterfaceBinding) {
    let package = ctx.package;
    let name = pascal(&iface.name);
    let c_tag = &iface.c_tag;
    let (doc, deprecated) = ctx.docs.of(&iface.doc, &iface.deprecated);
    let doc = match doc {
        Some(_) => GoDoc::decl(&name, doc),
        None => GoDoc::plain(None).para(&format!(
            "{name} is a reference-counted object owned by the native library."
        )),
    };
    doc.para(
        "Each wrapper holds one strong reference to the object. Close releases \
         it; a wrapper that's never closed releases it some time after it becomes \
         unreachable.",
    )
    .deprecated(deprecated)
    .emit(w);
    w.block(format!("type {name} struct {{"), "}", |w| {
        w.line("wvObject");
    });
    w.blank();

    func(
        w,
        &format!("func wvDestroy{name}(ptr unsafe.Pointer)"),
        &format!("C.{}((*C.{c_tag})(ptr))", iface.destroy_symbol),
    );

    w.line(format!(
        "// wvAdopt{name} wraps one owned strong reference. A null pointer adopts to nil."
    ));
    w.block(
        format!("func wvAdopt{name}(ptr *C.{c_tag}) *{name} {{"),
        "}",
        |w| {
            w.block("if ptr == nil {", "}", |w| {
                w.line("return nil");
            });
            w.line(format!("s := &{name}{{}}"));
            w.line(format!("s.adopt(unsafe.Pointer(ptr), wvDestroy{name})"));
            w.line("return s");
        },
    );
    w.blank();

    w.line("// native borrows the wrapper's pointer for one call; pair it with a");
    w.line("// deferred s.release().");
    w.block(
        format!("func (s *{name}) native() *C.{c_tag} {{"),
        "}",
        |w| {
            w.block("if s == nil {", "}", |w| {
                w.line(format!("panic(\"{package}: nil *{name}\")"));
            });
            w.line(format!("return (*C.{c_tag})(s.acquire(\"{name}\"))"));
        },
    );
    w.blank();

    let in_buffers = ctx.codecs.has_interface(&iface.name);
    let returned = ctx.model.callback_interfaces().any(|(_, cb)| {
        cb.methods.iter().any(|m| {
            matches!(&m.ret_pass, CallbackRetPass::Object { interface, .. } if *interface == iface.name)
        })
    });
    if in_buffers || returned {
        w.line("// share returns a new strong reference to the wrapper's object for the");
        w.line("// native library to adopt (the wrapper keeps its own), or null for nil.");
        w.block(
            format!("func (s *{name}) share() *C.{c_tag} {{"),
            "}",
            |w| {
                w.block("if s == nil {", "}", |w| {
                    w.line("return nil");
                });
                w.line("ptr := s.native()");
                w.line("defer s.release()");
                w.line(format!("return C.{}(ptr)", iface.clone_symbol));
            },
        );
        w.blank();
    }

    if in_buffers {
        func(
            w,
            &format!("func wvWrite{name}(w *wvWriter, v *{name})"),
            "w.writeU64(uint64(uintptr(unsafe.Pointer(v.share()))))",
        );
        w.block(
            format!("func wvRead{name}(r *wvReader) *{name} {{"),
            "}",
            |w| {
                w.line("token := r.readU64()");
                w.block("if token == 0 {", "}", |w| {
                    w.line("wvMalformed(\"null object token\")");
                });
                w.line(format!(
                    "return wvAdopt{name}((*C.{c_tag})(C.wvHandlePtr(C.uintptr_t(token))))"
                ));
            },
        );
        w.blank();
    }

    for c in &iface.constructors {
        render_function(w, ctx, c, &constructor(&name, c), None);
    }
    for f in &iface.methods {
        let go_name = names::method(&f.name);
        render_function(w, ctx, f, &go_name, Some(Receiver { ty: &name }));
    }
    for f in &iface.statics {
        render_function(w, ctx, f, ctx.names.function(f), None);
    }
}
