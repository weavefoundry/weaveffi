//! Declaration renderers: error domains, C-style enums, records (`Data`
//! classes), rich enums (a module of `Data` variants), interface wrapper
//! classes, and the registrations that tell the runtime how each record,
//! variant, and error payload is laid out in a value buffer and which C
//! functions clone and release each interface's objects.

use heck::ToShoutySnakeCase;
use weaveffi_model::errors::pascal;
use weaveffi_model::model::{EnumBinding, FieldBinding, InterfaceBinding, Model, StructBinding};

use crate::codegen::common::{wrap, DocCommentStyle};
use crate::codegen::docs::Doc;
use crate::codegen::errors::ErrorTable;
use crate::codegen::CodeWriter;
use crate::targets::ruby::calls::{render_callable, RbScope, ScopeKind};
use crate::targets::ruby::docs;
use crate::targets::ruby::types::{
    rb_const, rb_doc_type, rb_error_field_name, rb_field_name, rb_str_literal, rb_wire,
};
use crate::targets::ruby::RbCtx;

/// The longest line kept on one line before wrapping a member list.
const MAX_LINE: usize = 100;

/// Render one error domain: a class under the library's `Error` with one
/// nested class per code, each carrying its `CODE`, its documented
/// `MESSAGE`, and readers for its payload fields (which the runtime's
/// `Error#initialize` takes as required keywords). A code these bindings
/// don't know (the library is newer) raises the domain class itself, with
/// the code and message.
pub(crate) fn render_error(w: &mut CodeWriter, table: &ErrorTable<'_>) {
    let domain = rb_const(&table.type_name);
    w.blank();
    let about = format!(
        "The `{}` error domain of the `{}` module. A code these bindings don't \
         know (from a newer library) raises this class itself, with its code \
         and message.",
        table.domain.name, table.module.dot_path
    );
    w.doc(&Some(wrap(&about, 76)), DocCommentStyle::Hash);
    w.block(format!("class {domain} < Error"), "end", |w| {
        for (idx, row) in table.codes.iter().enumerate() {
            let c = row.code;
            if idx > 0 {
                w.blank();
            }
            let doc = c.doc.clone().unwrap_or_else(|| c.message.clone());
            w.doc(&Some(doc), DocCommentStyle::Hash);
            w.block(
                format!("class {} < {domain}", pascal(&c.name)),
                "end",
                |w| {
                    w.line(format!("CODE = {}", c.value));
                    w.line(format!("MESSAGE = '{}'", rb_str_literal(&c.message)));
                    for f in &c.fields {
                        w.blank();
                        w.doc(&f.doc, DocCommentStyle::Hash);
                        w.line(format!("# @return [{}]", rb_doc_type(&f.ty)));
                        w.line(format!("attr_reader :{}", rb_error_field_name(&f.name)));
                    }
                },
            );
        }
    });
}

/// Render one C-style enum as a module of integer constants, one
/// `SHOUTY_SNAKE` constant per variant.
pub(crate) fn render_enum(w: &mut CodeWriter, ctx: &RbCtx, e: &EnumBinding) {
    w.blank();
    docs::emit_plain(w, &ctx.names, &Doc::new(&e.doc, &e.deprecated));
    w.block(format!("module {}", rb_const(&e.name)), "end", |w| {
        for v in &e.variants {
            w.doc(&v.doc, DocCommentStyle::Hash);
            w.line(format!("{} = {}", v.name.to_shouty_snake_case(), v.value));
        }
    });
}

/// The YARD `@!attribute` tags documenting a `Data` class's members.
fn emit_attributes(w: &mut CodeWriter, fields: &[FieldBinding]) {
    for f in fields {
        w.line(format!("# @!attribute [r] {}", rb_field_name(&f.name)));
        let doc = f.doc.as_deref().map(str::trim).filter(|d| !d.is_empty());
        match doc {
            Some(doc) => {
                let mut lines = doc.lines();
                w.line(format!(
                    "#   @return [{}] {}",
                    rb_doc_type(&f.ty),
                    lines.next().unwrap_or_default()
                ));
                for line in lines {
                    w.line(format!("#     {line}").trim_end());
                }
            }
            None => {
                w.line(format!("#   @return [{}]", rb_doc_type(&f.ty)));
            }
        }
    }
}

/// `Data.define(:a, :b)`, wrapped one member per line when long. `lead`
/// precedes it on the first line (`Item = `, `class Circle < `).
fn emit_data_define(w: &mut CodeWriter, lead: &str, trail: &str, fields: &[FieldBinding]) {
    let members: Vec<String> = fields
        .iter()
        .map(|f| format!(":{}", rb_field_name(&f.name)))
        .collect();
    let args = if members.is_empty() {
        String::new()
    } else {
        format!("({})", members.join(", "))
    };
    let one_line = format!("{lead}Data.define{args}{trail}");
    if w.indent_str().len() + one_line.len() <= MAX_LINE {
        w.line(one_line);
        return;
    }
    w.line(format!("{lead}Data.define("));
    w.scope(|w| {
        for m in &members {
            w.line(format!("{m},"));
        }
    });
    w.line(format!("){trail}"));
}

/// Render one record as a `Data` class: immutable, constructed with
/// keywords (or positionally), with value equality and hashing, `with`,
/// `to_h`, and pattern matching.
pub(crate) fn render_struct(w: &mut CodeWriter, ctx: &RbCtx, s: &StructBinding) {
    w.blank();
    let doc = Doc::new(&s.doc, &s.deprecated);
    docs::emit_plain(w, &ctx.names, &doc);
    if !s.fields.is_empty() && (s.doc.is_some() || s.deprecated.is_some()) {
        w.line("#");
    }
    emit_attributes(w, &s.fields);
    emit_data_define(w, &format!("{} = ", rb_const(&s.name)), "", &s.fields);
}

/// Render one rich enum as a module of `Data` variant classes. Each variant
/// includes the module (so `shape.is_a?(Shape)` holds) and carries its wire
/// `TAG`, which `#tag` returns.
pub(crate) fn render_rich_enum(w: &mut CodeWriter, ctx: &RbCtx, e: &EnumBinding) {
    let name = rb_const(&e.name);
    w.blank();
    docs::emit_plain(w, &ctx.names, &Doc::new(&e.doc, &e.deprecated));
    w.block(format!("module {name}"), "end", |w| {
        w.line("# The active variant's wire tag.");
        w.line("# @return [Integer]");
        w.block("def tag", "end", |w| {
            w.line("self.class::TAG");
        });
        for v in &e.variants {
            w.blank();
            w.doc(&v.doc, DocCommentStyle::Hash);
            if !v.fields.is_empty() && v.doc.is_some() {
                w.line("#");
            }
            emit_attributes(w, &v.fields);
            emit_data_define(w, &format!("class {} < ", rb_const(&v.name)), "", &v.fields);
            w.scope(|w| {
                w.line(format!("include {name}"));
                w.blank();
                w.line(format!("TAG = {}", v.value));
            });
            w.line("end");
        }
    });
}

/// Render one interface as a wrapper class on the runtime's `Handle`
/// (which supplies `close`, `closed?`, `==`/`hash` by native object,
/// `dup`/`clone`, `inspect`, and the GC backstop). A constructor named
/// `new` becomes `initialize`; every other constructor is a class-method
/// factory, and an interface without a `new` hides it; methods pin the
/// wrapper's pointer for the call; statics are class methods.
pub(crate) fn render_interface(w: &mut CodeWriter, ctx: &RbCtx, i: &InterfaceBinding) {
    let class = rb_const(&i.name);
    w.blank();
    docs::emit_plain(w, &ctx.names, &Doc::new(&i.doc, &i.deprecated));
    w.block(format!("class {class} < Bridge::Handle"), "end", |w| {
        let mut first = true;
        if !i.constructors.iter().any(|c| c.name == "new") {
            w.line("private_class_method :new");
            first = false;
        }
        let mut gap = |w: &mut CodeWriter| {
            if !std::mem::take(&mut first) {
                w.blank();
            }
        };
        for c in &i.constructors {
            gap(w);
            let kind = if c.name == "new" {
                ScopeKind::Init
            } else {
                ScopeKind::Factory
            };
            let scope = RbScope {
                kind,
                class: Some(&class),
            };
            render_callable(w, ctx, c, &scope);
        }
        for f in i.methods.iter().chain(&i.statics) {
            gap(w);
            let kind = if f.has_self() {
                ScopeKind::Method
            } else {
                ScopeKind::Static
            };
            let scope = RbScope {
                kind,
                class: Some(&class),
            };
            render_callable(w, ctx, f, &scope);
        }
    });
}

/// `Bridge.record(Class, field: type, ...)`, wrapped one field per line
/// when long.
fn emit_layout(w: &mut CodeWriter, class: &str, fields: &[(String, String)]) {
    let parts: Vec<String> = std::iter::once(class.to_string())
        .chain(fields.iter().map(|(n, t)| format!("{n}: {t}")))
        .collect();
    let one_line = format!("Bridge.record({})", parts.join(", "));
    if w.indent_str().len() + one_line.len() <= MAX_LINE {
        w.line(one_line);
        return;
    }
    w.line("Bridge.record(");
    w.scope(|w| {
        for p in &parts {
            w.line(format!("{p},"));
        }
    });
    w.line(")");
}

/// The private registrations closing the bindings: each interface's
/// clone and destroy functions, then the wire layout of every record,
/// rich-enum variant (and the variants of each rich enum), and error code
/// with a payload.
pub(crate) fn render_layouts(w: &mut CodeWriter, model: &Model, tables: &[ErrorTable<'_>]) {
    w.blank();
    w.line("# How the library clones and releases each interface's objects, and how");
    w.line("# each record, variant, and error payload is laid out in a value buffer.");
    for m in &model.modules {
        for i in &m.interfaces {
            w.line(format!(
                "Bridge.interface({}, :{}, :{})",
                rb_const(&i.name),
                i.clone_symbol,
                i.destroy_symbol
            ));
        }
    }
    let record_fields = |fields: &[FieldBinding]| -> Vec<(String, String)> {
        fields
            .iter()
            .map(|f| (rb_field_name(&f.name), rb_wire(&f.ty)))
            .collect()
    };
    for m in &model.modules {
        for s in &m.structs {
            emit_layout(w, &rb_const(&s.name), &record_fields(&s.fields));
        }
        for e in m.enums.iter().filter(|e| e.is_rich()) {
            let name = rb_const(&e.name);
            let variants: Vec<String> = e
                .variants
                .iter()
                .map(|v| format!("{name}::{}", rb_const(&v.name)))
                .collect();
            for (v, class) in e.variants.iter().zip(&variants) {
                emit_layout(w, class, &record_fields(&v.fields));
            }
            let one_line = format!("Bridge.union({name}, {})", variants.join(", "));
            if w.indent_str().len() + one_line.len() <= MAX_LINE {
                w.line(one_line);
            } else {
                w.line("Bridge.union(");
                w.scope(|w| {
                    w.line(format!("{name},"));
                    for v in &variants {
                        w.line(format!("{v},"));
                    }
                });
                w.line(")");
            }
        }
    }
    for table in tables {
        let domain = rb_const(&table.type_name);
        for row in table.codes.iter().filter(|r| !r.code.fields.is_empty()) {
            let fields: Vec<(String, String)> = row
                .code
                .fields
                .iter()
                .map(|f| (rb_error_field_name(&f.name), rb_wire(&f.ty)))
                .collect();
            emit_layout(w, &format!("{domain}::{}", pascal(&row.code.name)), &fields);
        }
    }
}
