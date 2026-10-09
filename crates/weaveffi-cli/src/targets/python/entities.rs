//! Entity rendering: error domains, plain and rich enums, record
//! dataclasses, and interface wrapper classes.

use crate::codegen::CodeWriter;
use heck::ToSnakeCase;
use weaveffi_model::model::{
    CallShape, EnumBinding, ErrorBinding, FieldBinding, FnBinding, InterfaceBinding, Model,
    ModuleBinding, StructBinding,
};

use crate::targets::python::calls::{render_bindings, render_callable, FnScope};
use crate::targets::python::codec::{
    read_expr, render_record_codecs, render_rich_enum_codecs, write_stmt,
};
use crate::targets::python::docs::{comment, docstring, with_deprecation};
use crate::targets::python::types::{
    py_binding_name, py_error_field, py_field, py_field_hint, py_str_literal, py_variant,
};
use crate::targets::python::Gen;

// ── Errors ──

/// The first of `candidates` that no declaration of the API (a type, or an
/// error code's class) already names.
fn free_name(model: &Model, candidates: [String; 3]) -> String {
    let taken = |name: &str| {
        model.modules.iter().any(|m| {
            m.enums.iter().any(|e| e.name == name)
                || m.structs.iter().any(|s| s.name == name)
                || m.interfaces.iter().any(|i| i.name == name)
                || m.callback_interfaces.iter().any(|c| c.name == name)
                || m.errors.as_ref().is_some_and(|e| {
                    e.type_name == name
                        || e.codes.iter().any(|c| py_code_class_name(&c.name) == name)
                })
        })
    };
    let fallback = format!("{}Root", candidates[2]);
    candidates
        .into_iter()
        .find(|n| !taken(n))
        .unwrap_or(fallback)
}

/// The root exception class every declared error derives from: `Error` (so
/// consumers write `except kvstore.Error`), unless the API declares a type
/// or error code with that name, in which case `{PascalName}Error`, then
/// `{PascalName}BaseError`.
pub(crate) fn root_error_name(model: &Model, pascal_name: &str) -> String {
    free_name(
        model,
        [
            "Error".to_string(),
            format!("{pascal_name}Error"),
            format!("{pascal_name}BaseError"),
        ],
    )
}

/// The unchecked `RuntimeError` subclass a failed call that declares no
/// errors raises: `InternalError`, unless the API declares that name.
pub(crate) fn trap_error_name(model: &Model, pascal_name: &str) -> String {
    free_name(
        model,
        [
            "InternalError".to_string(),
            format!("{pascal_name}InternalError"),
            format!("{pascal_name}RuntimeError"),
        ],
    )
}

/// `_{stem}_from`: builds the domain exception matching a code, message,
/// and payload.
pub(crate) fn py_factory_name(eb: &ErrorBinding) -> String {
    format!("_{}_from", eb.type_name.to_snake_case())
}

/// The factory an out-err slot of `f` is raised through: the module
/// domain's typed factory when `f` declares errors, the unchecked trap
/// otherwise.
pub(crate) fn py_raise_factory(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match error {
        Some(eb) if f.throws => py_factory_name(eb),
        _ => "_trap_from".to_string(),
    }
}

/// The Python class name for one error code: plain PascalCase with no forced
/// suffix (`KeyNotFound`, not `KeyNotFoundError`). Each class is also
/// attached to its domain class (`KvError.KeyNotFound`).
pub(crate) fn py_code_class_name(name: &str) -> String {
    weaveffi_model::errors::pascal(name)
}

/// Render one module's declared error domain: a base exception named after
/// the domain (subclassing the root exception), one subclass per code
/// carrying its stable `CODE`, its payload fields as constructor arguments
/// and attributes, and their value-buffer encoding (a callback method that
/// declares errors raises them back to the producer), then the factory that
/// builds the exception for a code, message, and payload. Each code class
/// is also attached to the domain class, so consumers can catch
/// `KvError.KeyNotFound`.
pub(crate) fn render_error(
    w: &mut CodeWriter,
    g: &Gen<'_>,
    module: &ModuleBinding,
    eb: &ErrorBinding,
) {
    let domain = &eb.type_name;
    let root = &g.root_error;

    w.blank().blank();
    w.line(format!("class {domain}({root}):"));
    w.scope(|w| {
        docstring(
            w,
            Some(&format!(
                "Base exception for the `{}` module's error domain.",
                module.dot_path
            )),
        );
        // The per-code classes, attached below once they exist.
        w.blank();
        for c in &eb.codes {
            let class = py_code_class_name(&c.name);
            w.line(format!("{class}: Type[{class}]"));
        }
    });

    for c in &eb.codes {
        let class = py_code_class_name(&c.name);
        w.blank().blank();
        w.line(format!("class {class}({domain}):"));
        w.scope(|w| {
            docstring(w, Some(c.doc.as_deref().unwrap_or(&c.message)));
            w.blank();
            w.line(format!("CODE = {}", c.value));
            if !c.fields.is_empty() {
                w.blank();
                render_dataclass_fields(w, &c.fields, py_error_field);
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
    w.blank().blank();
    for c in &eb.codes {
        let class = py_code_class_name(&c.name);
        w.line(format!("{domain}.{class} = {class}"));
    }

    w.blank().blank();
    w.line(format!(
        "def {}(code: int, message: str, payload: bytes = b\"\") -> {root}:",
        py_factory_name(eb)
    ));
    w.scope(|w| {
        docstring(
            w,
            Some(&format!(
                "The {domain} subclass for `code` with its payload fields, or the\n\
                 plain {root} for a runtime code (a panic, a marshalling failure)."
            )),
        );
        for c in &eb.codes {
            let class = py_code_class_name(&c.name);
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
        w.line(format!("return {root}(code, message)"));
    });
}

// ── Enums ──

/// Render one enum: a plain `IntEnum` for a C-style enum, or the dataclass
/// sum-type hierarchy for a rich enum.
pub(crate) fn render_enum(w: &mut CodeWriter, e: &EnumBinding) {
    if e.is_rich() {
        render_rich_enum(w, e);
        return;
    }
    w.blank().blank();
    w.line(format!("class {}(IntEnum):", e.name));
    w.scope(|w| {
        let doc = with_deprecation(e.doc.as_deref(), e.deprecated.as_deref());
        if doc.is_some() {
            docstring(w, doc.as_deref());
            w.blank();
        }
        for v in &e.variants {
            comment(w, v.doc.as_deref());
            w.line(format!("{} = {}", py_variant(&v.name), v.value));
        }
    });
}

/// Render a rich (algebraic) enum as an idiomatic Python sum type: a base
/// class holding the nested `Tag` discriminant enum and a `tag` property,
/// one module-level `@dataclass` subclass per variant carrying its fields,
/// scoped aliases (`Shape.Circle` is `ShapeCircle`), and the buffer codec.
/// Consumers construct variants directly and discriminate with `isinstance`
/// (or the `tag` property).
fn render_rich_enum(w: &mut CodeWriter, e: &EnumBinding) {
    let name = &e.name;
    w.blank().blank();
    w.line(format!("class {name}:"));
    w.scope(|w| {
        let doc = with_deprecation(e.doc.as_deref(), e.deprecated.as_deref());
        if doc.is_some() {
            docstring(w, doc.as_deref());
            w.blank();
        }
        w.line("class Tag(IntEnum):");
        w.scope(|w| {
            for v in &e.variants {
                comment(w, v.doc.as_deref());
                w.line(format!("{} = {}", py_variant(&v.name), v.value));
            }
        });
        w.blank();
        // The variant classes, attached below once they exist.
        for v in &e.variants {
            w.line(format!("{0}: Type[{name}{0}]", py_variant(&v.name)));
        }
        w.line(format!("TAG: {name}.Tag"));
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
        w.line("@dataclass");
        w.line(format!("class {class}({name}):"));
        w.scope(|w| {
            if v.doc.is_some() {
                docstring(w, v.doc.as_deref());
                w.blank();
            }
            w.line(format!("TAG = {name}.Tag.{}", py_variant(&v.name)));
            if !v.fields.is_empty() {
                w.blank();
                render_dataclass_fields(w, &v.fields, py_field);
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

/// Render a record as a plain `@dataclass` value class plus its buffer
/// codec. Records have no C symbols: construction, equality, and repr all
/// come from the dataclass, and instances cross the ABI serialized in value
/// buffers.
pub(crate) fn render_struct(w: &mut CodeWriter, s: &StructBinding) {
    w.blank().blank();
    w.line("@dataclass");
    w.line(format!("class {}:", s.name));
    w.scope(|w| {
        let doc = with_deprecation(s.doc.as_deref(), s.deprecated.as_deref());
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
        render_dataclass_fields(w, &s.fields, py_field);
    });
    render_record_codecs(w, s);
}

/// Emit one annotated attribute line (`name: hint`) per field, with field
/// docs as leading comments; `spell` names each field.
fn render_dataclass_fields(w: &mut CodeWriter, fields: &[FieldBinding], spell: fn(&str) -> String) {
    for f in fields {
        comment(w, f.doc.as_deref());
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
pub(crate) fn render_interface(
    w: &mut CodeWriter,
    g: &Gen<'_>,
    module: &ModuleBinding,
    i: &InterfaceBinding,
) {
    let error = g.model.error_domain(module);
    let name = &i.name;
    // Consecutive sync member bindings are packed into one block.
    let mut packed = false;
    for m in i.constructors.iter().chain(&i.methods).chain(&i.statics) {
        render_bindings(w, g, m, error, &format!("{name}.{}", m.name), packed);
        packed = matches!(m.shape, CallShape::Sync(_));
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
        let doc = with_deprecation(i.doc.as_deref(), i.deprecated.as_deref());
        if doc.is_some() {
            docstring(w, doc.as_deref());
            w.blank();
        }
        w.line(format!("_destroy = staticmethod({destroy})"));
        w.line(format!("_clone = staticmethod({clone})"));

        match i.constructors.iter().find(|c| c.name == "new") {
            Some(c) => render_callable(w, g, module, c, FnScope::Init),
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
            render_callable(w, g, module, c, FnScope::Factory);
        }
        for m in &i.methods {
            render_callable(w, g, module, m, FnScope::Method);
        }
        for s in &i.statics {
            render_callable(w, g, module, s, FnScope::Static);
        }
    });
}
