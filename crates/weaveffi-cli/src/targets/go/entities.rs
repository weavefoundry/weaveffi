//! Entity rendering: C-style and rich enums, records, error domains, and
//! interface (object) wrappers, each with its value-buffer codec pair.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{
    CallShape, EnumBinding, ErrorBinding, FieldBinding, InterfaceBinding, ModuleBinding,
    StructBinding,
};

use crate::targets::go::calls::{render_async, render_sync, Receiver};
use crate::targets::go::codec::{func, read_expr, write_stmt, BufferTypes};
use crate::targets::go::docs::{GoDoc, Kind};
use crate::targets::go::names::{self, constructor, domain_type, pascal};
use crate::targets::go::types::{go_str, go_type};
use crate::targets::go::Ctx;

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
fn emit_aligned(w: &mut CodeWriter, entries: Vec<AlignedEntry>, kind: Kind) {
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
            GoDoc::new(&e.name, e.doc.as_deref(), kind, None).emit(w);
            w.line(format!("{:<width$} {}", e.name, e.rest));
        }
        i = j;
    }
}

/// The struct-body entries of `fields`, named by `name`.
fn field_entries(fields: &[FieldBinding], name: fn(&str) -> String) -> Vec<AlignedEntry> {
    fields
        .iter()
        .map(|f| AlignedEntry {
            doc: f.doc.clone().filter(|d| !d.trim().is_empty()),
            name: name(&f.name),
            rest: go_type(&f.ty),
        })
        .collect()
}

/// Render a C-style enum: an `int32` type with one constant per variant,
/// plus its codec pair when it appears inside a buffer.
pub(crate) fn render_enum(w: &mut CodeWriter, e: &EnumBinding, codecs: &BufferTypes) {
    let name = pascal(&e.name);
    GoDoc::new(&name, e.doc.as_deref(), Kind::Value, None)
        .deprecated(e.deprecated.as_deref())
        .emit(w);
    w.line(format!("type {name} int32"));
    w.blank();
    w.block("const (", ")", |w| {
        let entries = e
            .variants
            .iter()
            .map(|v| AlignedEntry {
                doc: v.doc.clone().filter(|d| !d.trim().is_empty()),
                name: format!("{name}{}", pascal(&v.name)),
                rest: format!("{name} = {}", v.value),
            })
            .collect();
        emit_aligned(w, entries, Kind::Clause);
    });
    w.blank();
    if codecs.enums.contains(&e.name) {
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
pub(crate) fn render_rich_enum(w: &mut CodeWriter, package: &str, e: &EnumBinding) {
    let name = pascal(&e.name);
    GoDoc::new(&name, e.doc.as_deref(), Kind::Value, None)
        .para(&format!(
            "{name} is a sealed sum type: exactly one of its variant structs is the \
             value at a time."
        ))
        .deprecated(e.deprecated.as_deref())
        .emit(w);
    w.block(format!("type {name} interface {{"), "}", |w| {
        w.line(format!("is{name}()"));
    });
    w.blank();

    for v in &e.variants {
        let vn = format!("{name}{}", pascal(&v.name));
        GoDoc::new(
            &vn,
            v.doc.as_deref(),
            Kind::Clause,
            Some(format!("{vn} is the {} variant of {name}.", v.name)),
        )
        .emit(w);
        if v.fields.is_empty() {
            w.line(format!("type {vn} struct{{}}"));
        } else {
            w.block(format!("type {vn} struct {{"), "}", |w| {
                emit_aligned(w, field_entries(&v.fields, names::field), Kind::Value);
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
pub(crate) fn render_struct(w: &mut CodeWriter, s: &StructBinding) {
    let name = pascal(&s.name);
    GoDoc::new(&name, s.doc.as_deref(), Kind::Value, None)
        .deprecated(s.deprecated.as_deref())
        .emit(w);
    w.block(format!("type {name} struct {{"), "}", |w| {
        emit_aligned(w, field_entries(&s.fields, names::field), Kind::Value);
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

/// The name of the helper converting a failure in the domain `e` into its
/// error-code type (`wvKvError`).
pub(crate) fn domain_mapper(e: &ErrorBinding) -> String {
    format!("wv{}", domain_type(e))
}

/// Render an error domain: the sealed `{Domain}` interface every code type
/// implements, one `*{Code}Error` struct per code (its payload fields, a
/// `Message`, `Error`, and `Code`), and the helper mapping a failure to the
/// code's type, with an *Error for any code outside the domain.
pub(crate) fn render_error(w: &mut CodeWriter, ctx: &Ctx, m: &ModuleBinding, e: &ErrorBinding) {
    let gn = ctx.names;
    let domain = domain_type(e);
    let marker = format!("is{domain}");
    let codes: Vec<&str> = e.codes.iter().map(|c| gn.code(c)).collect();
    let list = match codes.as_slice() {
        [one] => format!("*{one}"),
        [init @ .., last] => format!(
            "{}, or *{last}",
            init.iter()
                .map(|c| format!("*{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        [] => unreachable!("an error domain declares at least one code"),
    };
    GoDoc::new(
        &domain,
        None,
        Kind::Value,
        Some(format!(
            "{domain} is an error the {} module reports: {list}.",
            m.dot_path
        )),
    )
    .para(&if ctx.throwing_callbacks {
        format!(
            "Match one code with errors.As and its type, or any of them with a \
             {domain}. A throwing callback method returns one to report that code."
        )
    } else {
        format!("Match one code with errors.As and its type, or any of them with a {domain}.")
    })
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
        let doc = c
            .doc
            .as_deref()
            .filter(|d| d.trim() != c.message.trim())
            .map(str::to_string);
        GoDoc::new(
            ty,
            doc.as_deref(),
            Kind::Clause,
            Some(format!(
                "{ty} is the {domain} code {} ({}): {}.",
                c.name,
                c.value,
                c.message.trim_end_matches('.')
            )),
        )
        .emit(w);
        w.block(format!("type {ty} struct {{"), "}", |w| {
            emit_aligned(w, field_entries(&c.fields, names::error_field), Kind::Value);
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
            w.block(format!("func (*{ty}) wvPayload() []byte {{"), "}", |w| {
                w.line("return nil");
            });
        } else {
            w.block(format!("func (e *{ty}) wvPayload() []byte {{"), "}", |w| {
                w.line("w := &wvWriter{}");
                for f in &c.fields {
                    let expr = format!("e.{}", names::error_field(&f.name));
                    w.line(write_stmt("w", &expr, &f.ty));
                }
                w.line("return w.buf");
            });
        }
        w.blank();
    }

    w.block(
        format!("func {}(f wvFailure) error {{", domain_mapper(e)),
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
            w.line("return f.err()");
        },
    );
    w.blank();
}

/// Render an interface as a reference-counted object wrapper whose `wvRef`
/// owns one strong reference, released by `Close` (idempotent, and safe to
/// race with in-flight calls) or, as a backstop, by a finalizer; its codec
/// pair when it appears inside a buffer (an object token carrying a fresh
/// reference); and its members: constructors as factories (`OpenStore`),
/// methods on the wrapper, and statics prefixed with the type
/// (`StoreDefaultCapacity`).
pub(crate) fn render_interface(w: &mut CodeWriter, ctx: &Ctx, iface: &InterfaceBinding) {
    let package = ctx.package;
    let name = pascal(&iface.name);
    let c_tag = &iface.c_tag;
    GoDoc::new(
        &name,
        iface.doc.as_deref(),
        Kind::Value,
        Some(format!(
            "{name} is a reference-counted object owned by the native library."
        )),
    )
    .para(
        "Each wrapper holds one strong reference; Close releases it, and a \
         finalizer releases it if the wrapper is garbage collected first.",
    )
    .deprecated(iface.deprecated.as_deref())
    .emit(w);
    w.block(format!("type {name} struct {{"), "}", |w| {
        w.line("ref wvRef");
    });
    w.blank();

    w.block(
        format!("func wvDestroy{name}(ptr unsafe.Pointer) {{"),
        "}",
        |w| {
            w.line(format!("C.{}((*C.{c_tag})(ptr))", iface.destroy_symbol));
        },
    );
    w.blank();

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
            w.line(format!("s.ref.init(unsafe.Pointer(ptr), wvDestroy{name})"));
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
            w.line("defer s.ref.release()");
            w.line(format!("return C.{}(ptr)", iface.clone_symbol));
        },
    );
    w.blank();

    if ctx.codecs.interfaces.contains(&iface.name) {
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

    w.line("// Close releases the wrapper's strong reference and always returns nil.");
    w.line("// It's idempotent and safe to call from any goroutine, even while a call");
    w.line("// on the wrapper is in flight: the reference is then released when that");
    w.line("// call returns. The object itself is dropped once its last reference, from");
    w.line("// any wrapper, record, or the native library, is gone.");
    w.block(format!("func (s *{name}) Close() error {{"), "}", |w| {
        w.line("runtime.SetFinalizer(s, nil)");
        w.line("s.ref.close()");
        w.line("return nil");
    });
    w.blank();

    for c in &iface.constructors {
        render_sync(w, ctx, c, &constructor(&name, c), None);
    }
    for f in &iface.methods {
        let go_name = names::method(&f.name);
        let recv = Some(Receiver { ty: &name });
        if let CallShape::Async(ab) = &f.shape {
            render_async(w, ctx, f, ab, &go_name, recv);
        } else {
            render_sync(w, ctx, f, &go_name, recv);
        }
    }
    for f in &iface.statics {
        let go_name = format!("{name}{}", pascal(&f.name));
        if let CallShape::Async(ab) = &f.shape {
            render_async(w, ctx, f, ab, &go_name, None);
        } else {
            render_sync(w, ctx, f, &go_name, None);
        }
    }
}
