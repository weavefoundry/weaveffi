//! Entity rendering: error domains, plain and rich enums, record
//! dataclasses, and interface wrapper classes.

use crate::codegen::errors::ErrorTable;
use crate::codegen::CodeWriter;
use weaveffi_model::model::{EnumBinding, FieldBinding, InterfaceBinding, StructBinding};

use crate::targets::python::calls::{render_bindings, render_callable, FnScope};
use crate::targets::python::codec::{
    read_expr, render_record_codecs, render_rich_enum_codecs, write_stmt,
};
use crate::targets::python::docs::{comment, docstring};
use crate::targets::python::types::{
    py_binding_name, py_error_field, py_field, py_field_hint, py_str_literal, py_variant,
};
use crate::targets::python::Gen;

// ── Errors ──

/// Render one error domain: a base exception named after the domain
/// (subclassing the root exception), one subclass per code carrying its
/// stable `CODE`, its payload fields as constructor arguments and
/// attributes, and their value-buffer encoding (a callback method that
/// throws the domain raises them back to the producer), then the factory
/// that builds the exception for a code, message, and payload. Each code
/// class is also attached to the domain class, so consumers can catch
/// `KvError.KeyNotFound`.
///
/// Domains are open: the factory maps a positive code these bindings don't
/// know (from a newer library) to the domain's base class itself, with the
/// code and message preserved.
pub(crate) fn render_error(w: &mut CodeWriter, g: &Gen<'_>, t: &ErrorTable<'_>) {
    let domain = &t.type_name;
    let root = &g.root_error;
    let codes: Vec<(String, String)> = t
        .codes
        .iter()
        .map(|row| {
            (
                weaveffi_model::errors::pascal(&row.code.name),
                g.code_class(&row.code.name).to_string(),
            )
        })
        .collect();

    w.blank().blank();
    w.line(format!("class {domain}({root}):"));
    w.scope(|w| {
        docstring(
            w,
            Some(&format!(
                "Base exception of the `{}` error domain (module `{}`).\n\n\
                 Each declared code raises its own subclass. A positive code these\n\
                 bindings don't know (from a newer library) raises this class itself,\n\
                 with `code` and `message` preserved.",
                t.domain.name, t.module.dot_path
            )),
        );
        if !codes.is_empty() {
            // The per-code classes, attached below once they exist.
            w.blank();
            for (alias, class) in &codes {
                w.line(format!("{alias}: ClassVar[type[{class}]]"));
            }
        }
    });

    for (row, (_, class)) in t.codes.iter().zip(&codes) {
        let c = row.code;
        w.blank().blank();
        w.line(format!("class {class}({domain}):"));
        w.scope(|w| {
            let doc = g.text(&c.doc);
            docstring(w, Some(doc.as_deref().unwrap_or(&c.message)));
            w.blank();
            w.line(format!("CODE = {}", c.value));
            if !c.fields.is_empty() {
                w.blank();
                render_fields(w, g, &c.fields, py_error_field);
            }
            let mut params: Vec<String> = c
                .fields
                .iter()
                .map(|f| {
                    format!(
                        "{}: {}",
                        py_error_field(&f.name),
                        py_field_hint(&c.fields, &f.ty)
                    )
                })
                .collect();
            params.push(format!("message: str = \"{}\"", py_str_literal(&c.message)));
            w.blank();
            w.line(format!(
                "def __init__(self, {}) -> None:",
                params.join(", ")
            ));
            w.scope(|w| {
                w.line(format!("super().__init__({}, message)", c.value));
                for f in &c.fields {
                    let attr = py_error_field(&f.name);
                    w.line(format!("self.{attr} = {attr}"));
                }
            });
            if !c.fields.is_empty() {
                w.blank();
                w.line("def _payload(self) -> bytes:");
                w.scope(|w| {
                    w.line("_w = _Writer()");
                    for f in &c.fields {
                        w.line(write_stmt(
                            &format!("self.{}", py_error_field(&f.name)),
                            &f.ty,
                        ));
                    }
                    w.line("return _w.finish()");
                });
            }
        });
    }

    // Scoped aliases: `except KvError.KeyNotFound` names the code through its
    // domain.
    if !codes.is_empty() {
        w.blank().blank();
        for (alias, class) in &codes {
            w.line(format!("{domain}.{alias} = {class}"));
        }
    }

    w.blank().blank();
    w.line(format!(
        "def {}(code: int, message: str, payload: bytes = b\"\") -> {root}:",
        g.domain_factory(&t.domain.name)
    ));
    w.scope(|w| {
        docstring(
            w,
            Some(&format!(
                "The exception for a failure of a call that throws {domain}."
            )),
        );
        for (row, (_, class)) in t.codes.iter().zip(&codes) {
            let c = row.code;
            w.line(format!("if code == {}:", c.value));
            w.scope(|w| {
                if c.fields.is_empty() {
                    w.line(format!("return {class}(message)"));
                    return;
                }
                // Keyword arguments evaluate left to right, matching the
                // wire order of the code's fields.
                w.line(format!("return _decode(payload, lambda _r: {class}("));
                w.scope(|w| {
                    for f in &c.fields {
                        w.line(format!("{}={},", py_error_field(&f.name), read_expr(&f.ty)));
                    }
                    w.line("message=message,");
                });
                w.line("))");
            });
        }
        w.line(format!("return _unknown_code({domain}, code, message)"));
    });
}

// ── Enums ──

/// Render one enum: a plain `IntEnum` for a C-style enum, or the dataclass
/// sum-type hierarchy for a rich enum.
pub(crate) fn render_enum(w: &mut CodeWriter, g: &Gen<'_>, e: &EnumBinding) {
    if e.is_rich() {
        render_rich_enum(w, g, e);
        return;
    }
    w.blank().blank();
    w.line(format!("class {}(IntEnum):", e.name));
    w.scope(|w| {
        let doc = g.doc(&e.doc, &e.deprecated);
        if doc.is_some() {
            docstring(w, doc.as_deref());
            w.blank();
        }
        for v in &e.variants {
            comment(w, g.text(&v.doc).as_deref());
            w.line(format!("{} = {}", py_variant(&v.name), v.value));
        }
    });
}

/// Render a rich (algebraic) enum as an idiomatic Python sum type: a base
/// class holding the nested `Tag` discriminant enum and a `tag` property,
/// one module-level frozen dataclass subclass per variant carrying its
/// fields, scoped aliases (`Shape.Circle` is `ShapeCircle`), and the buffer
/// codec. Consumers construct variants directly and discriminate with
/// `isinstance`, `match`, or the `tag` property.
fn render_rich_enum(w: &mut CodeWriter, g: &Gen<'_>, e: &EnumBinding) {
    let name = &e.name;
    w.blank().blank();
    w.line(format!("class {name}:"));
    w.scope(|w| {
        let doc = g.doc(&e.doc, &e.deprecated);
        if doc.is_some() {
            docstring(w, doc.as_deref());
            w.blank();
        }
        w.line("__slots__ = ()");
        w.blank();
        w.line("class Tag(IntEnum):");
        w.scope(|w| {
            for v in &e.variants {
                comment(w, g.text(&v.doc).as_deref());
                w.line(format!("{} = {}", py_variant(&v.name), v.value));
            }
        });
        w.blank();
        // The variant classes, attached below once they exist.
        for v in &e.variants {
            w.line(format!(
                "{0}: ClassVar[type[{name}{0}]]",
                py_variant(&v.name)
            ));
        }
        w.line(format!("TAG: ClassVar[{name}.Tag]"));
        w.blank();
        w.line("@property");
        w.line(format!("def tag(self) -> {name}.Tag:"));
        w.scope(|w| {
            docstring(w, Some("The discriminant of this value's active variant."));
            w.line("return type(self).TAG");
        });
    });

    for v in &e.variants {
        let class = format!("{name}{}", py_variant(&v.name));
        w.blank().blank();
        w.line("@dataclass(frozen=True, slots=True)");
        w.line(format!("class {class}({name}):"));
        w.scope(|w| {
            let doc = g.text(&v.doc);
            if doc.is_some() {
                docstring(w, doc.as_deref());
                w.blank();
            }
            w.line(format!("TAG = {name}.Tag.{}", py_variant(&v.name)));
            if !v.fields.is_empty() {
                w.blank();
                render_fields(w, g, &v.fields, py_field);
            }
        });
    }

    // Scoped aliases (`Shape.Circle`), assigned once every variant class
    // exists.
    w.blank().blank();
    for v in &e.variants {
        w.line(format!("{name}.{0} = {name}{0}", py_variant(&v.name)));
    }

    render_rich_enum_codecs(w, e);
}

// ── Records ──

/// Render a record as a frozen, slotted dataclass plus its buffer codec.
/// Records have no C symbols: construction, equality, hashing, and repr all
/// come from the dataclass, and instances cross the ABI serialized in value
/// buffers. Derive a changed copy with `dataclasses.replace`.
pub(crate) fn render_struct(w: &mut CodeWriter, g: &Gen<'_>, s: &StructBinding) {
    w.blank().blank();
    w.line("@dataclass(frozen=True, slots=True)");
    w.line(format!("class {}:", s.name));
    w.scope(|w| {
        let doc = g.doc(&s.doc, &s.deprecated);
        docstring(w, doc.as_deref());
        if s.fields.is_empty() {
            if doc.is_none() {
                w.line("pass");
            }
            return;
        }
        if doc.is_some() {
            w.blank();
        }
        render_fields(w, g, &s.fields, py_field);
    });
    render_record_codecs(w, s);
}

/// Emit one annotated attribute line (`name: hint`) per field, with field
/// docs as leading comments; `spell` names each field.
fn render_fields(
    w: &mut CodeWriter,
    g: &Gen<'_>,
    fields: &[FieldBinding],
    spell: fn(&str) -> String,
) {
    for f in fields {
        comment(w, g.text(&f.doc).as_deref());
        w.line(format!(
            "{}: {}",
            spell(&f.name),
            py_field_hint(fields, &f.ty)
        ));
    }
}

// ── Interfaces ──

/// Render one interface as a reference-counted object wrapper class
/// (subclassing the runtime's `_Object`), preceded by the module-level
/// bindings of its members, clone, and destroy. The class holds one strong
/// reference and releases it exactly once, from `close()`, the
/// context-manager exit, or the `__del__` backstop; calls lend the pointer so
/// a concurrent `close()` defers the release until they return. A
/// constructor named `new` becomes `__init__`; every other constructor
/// becomes a `@classmethod` factory; methods lend `self` as the leading C
/// argument; statics are `@staticmethod`s.
pub(crate) fn render_interface(w: &mut CodeWriter, g: &Gen<'_>, i: &InterfaceBinding) {
    let name = &i.name;
    // Consecutive plain sync member bindings are packed into one block.
    let mut packed = false;
    for m in i.members() {
        render_bindings(w, g, m, packed);
        packed = !m.is_async() && m.iterator().is_none();
    }
    let clone = py_binding_name(&i.clone_symbol, g.prefix);
    let destroy = py_binding_name(&i.destroy_symbol, g.prefix);
    if !packed {
        w.blank().blank();
    }
    w.line(format!(
        "{clone} = _bind(\"{}\", ctypes.c_void_p, ctypes.c_void_p)",
        i.clone_symbol
    ));
    w.line(format!(
        "{destroy} = _bind(\"{}\", None, ctypes.c_void_p)",
        i.destroy_symbol
    ));

    w.blank().blank();
    w.line(format!("class {name}(_Object):"));
    w.scope(|w| {
        let doc = g.doc(&i.doc, &i.deprecated);
        if doc.is_some() {
            docstring(w, doc.as_deref());
            w.blank();
        }
        w.line(format!("_destroy = staticmethod({destroy})"));
        w.line(format!("_clone = staticmethod({clone})"));

        match i.constructors.iter().find(|c| c.name == "new") {
            Some(c) => render_callable(w, g, c, FnScope::Init),
            None => {
                // No canonical constructor: instances only come from
                // factories and producer returns.
                w.blank();
                w.line("def __init__(self) -> None:");
                w.scope(|w| {
                    w.line(format!(
                        "raise TypeError(\"{name} cannot be instantiated directly\")"
                    ));
                });
            }
        }
        for c in i.constructors.iter().filter(|c| c.name != "new") {
            render_callable(w, g, c, FnScope::Factory);
        }
        for m in &i.methods {
            render_callable(w, g, m, FnScope::Method);
        }
        for s in &i.statics {
            render_callable(w, g, s, FnScope::Static);
        }
    });
}
