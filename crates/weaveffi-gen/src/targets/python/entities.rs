//! Entity rendering: error domains, plain and rich enums, record
//! dataclasses, and interface wrapper classes.

use crate::codegen::CodeWriter;
use heck::{ToShoutySnakeCase, ToSnakeCase};
use weaveffi_model::model::{
    BindingModel, CallShape, EnumBinding, ErrorBinding, ErrorCodeBinding, FieldBinding, FnBinding,
    InterfaceBinding, ModuleBinding, StructBinding,
};

use crate::targets::python::calls::{render_bindings, render_callable, FnScope};
use crate::targets::python::codec::{py_read_expr, render_record_codecs, render_rich_enum_codecs};
use crate::targets::python::docs::emit_docstring;
use crate::targets::python::types::{
    py_binding_name, py_field, py_field_hint, py_str_literal, py_variant,
};
use crate::targets::python::Gen;

// ── Errors ──

/// The root exception class every error the bindings raise derives from:
/// `Error` (so consumers write `except kvstore.Error`), unless the API
/// declares a type or error code with that name, in which case
/// `{PascalName}Error`, then `{PascalName}BaseError`.
pub(crate) fn root_error_name(model: &BindingModel, pascal_name: &str) -> String {
    let taken = |name: &str| {
        model.modules.iter().any(|m| {
            m.enums.iter().any(|e| e.name == name)
                || m.structs.iter().any(|s| s.name == name)
                || m.interfaces.iter().any(|i| i.name == name)
                || m.callback_interfaces.iter().any(|c| c.name == name)
                || m.error.as_ref().is_some_and(|e| {
                    e.type_name == name
                        || e.codes.iter().any(|c| py_code_class_name(&c.name) == name)
                })
        })
    };
    [
        "Error".to_string(),
        format!("{pascal_name}Error"),
        format!("{pascal_name}BaseError"),
    ]
    .into_iter()
    .find(|n| !taken(n))
    .unwrap_or_else(|| format!("{pascal_name}RootError"))
}

/// The snake_case stem shared by a domain's private helper names.
fn py_error_stem(eb: &ErrorBinding) -> String {
    eb.type_name.to_snake_case()
}

/// `_{stem}_from`: builds the domain exception matching an ABI code.
pub(crate) fn py_factory_name(eb: &ErrorBinding) -> String {
    format!("_{}_from", py_error_stem(eb))
}

/// `_check_{stem}`: raises the domain exception for a non-zero out-err slot.
fn py_domain_checker_name(eb: &ErrorBinding) -> String {
    format!("_check_{}", py_error_stem(eb))
}

/// The error-check call a callable's out-err slot goes through: the module
/// domain's typed checker when the callable throws, the generic
/// `_check_error` (the root exception: panics, marshalling failures, and
/// failed callbacks only) otherwise.
pub(crate) fn py_checker_name(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match error {
        Some(eb) if f.throws => py_domain_checker_name(eb),
        _ => "_check_error".to_string(),
    }
}

/// The Python class name for one error code: plain PascalCase with no forced
/// suffix (`KeyNotFound`, not `KeyNotFoundError`). Each class is also
/// attached to its domain class (`KvError.KeyNotFound`), which stays
/// unambiguous even if two domains declare codes with the same name.
pub(crate) fn py_code_class_name(name: &str) -> String {
    weaveffi_model::errors::pascal(name)
}

/// `_{stem}_payload_{code}`: decodes one code's payload fields onto an
/// exception instance.
fn py_payload_decoder_name(eb: &ErrorBinding, code: &ErrorCodeBinding) -> String {
    format!(
        "_{}_payload_{}",
        py_error_stem(eb),
        code.name.to_snake_case()
    )
}

/// Render one module's declared error domain: a base exception named after
/// the domain (subclassing the root exception), one exception subclass per
/// code carrying its stable `CODE` and default message, the code-to-class
/// table, per-code payload decoders, and the factory/checker helpers
/// throwing wrappers route their out-err slots through. Each code class is
/// also attached to the domain class, so consumers can catch
/// `KvError.KeyNotFound`.
pub(crate) fn render_error(
    out: &mut String,
    g: &Gen<'_>,
    module: &ModuleBinding,
    eb: &ErrorBinding,
) {
    let domain = &eb.type_name;
    let root = g.root_error;
    let factory = py_factory_name(eb);
    let checker = py_domain_checker_name(eb);
    let table = format!("_{}_CODES", eb.type_name.to_shouty_snake_case());
    let payloads = format!("_{}_PAYLOADS", eb.type_name.to_shouty_snake_case());
    let has_payloads = eb.codes.iter().any(|c| !c.fields.is_empty());

    let mut w = CodeWriter::four_space();
    w.blank().blank();
    w.line(format!("class {domain}({root}):"));
    w.scope(|w| {
        w.line(format!(
            "\"\"\"Base exception for the `{}` module's error domain.\"\"\"",
            module.dot_path
        ));
        // The per-code classes, attached below once they exist.
        w.blank();
        for c in &eb.codes {
            let class = py_code_class_name(&c.name);
            w.line(format!("{class}: \"Type[{class}]\""));
        }
    });

    for c in &eb.codes {
        let class = py_code_class_name(&c.name);
        let message = py_str_literal(&c.message);
        w.blank().blank();
        w.line(format!("class {class}({domain}):"));
        w.indent();
        let mut doc = String::new();
        emit_docstring(&mut doc, &c.doc, &w.indent_str());
        if doc.is_empty() {
            emit_docstring(&mut doc, &Some(c.message.clone()), &w.indent_str());
        }
        w.raw(doc);
        w.blank();
        w.line(format!("CODE = {}", c.value));
        w.blank();
        w.line(format!(
            "def __init__(self, message: str = \"{message}\") -> None:"
        ));
        w.scope(|w| {
            w.line(format!("super().__init__({}, message)", c.value));
        });
        w.dedent();
    }

    // Scoped aliases: `except KvError.KeyNotFound` stays unambiguous even if
    // another domain declares a code with the same name.
    w.blank().blank();
    for c in &eb.codes {
        let class = py_code_class_name(&c.name);
        w.line(format!("{domain}.{class} = {class}"));
    }

    w.blank().blank();
    w.line(format!("{table}: Dict[int, Any] = {{"));
    w.scope(|w| {
        for c in &eb.codes {
            let class = py_code_class_name(&c.name);
            w.line(format!("{}: {class},", c.value));
        }
    });
    w.line("}");

    // Payload decoders: one per code that declares structured fields. Each
    // reads the code's fields (in declaration order) from the payload buffer
    // and attaches them as attributes on the exception instance.
    if has_payloads {
        for c in eb.codes.iter().filter(|c| !c.fields.is_empty()) {
            let decoder = py_payload_decoder_name(eb, c);
            let class = py_code_class_name(&c.name);
            w.blank().blank();
            w.line(format!("def {decoder}(_exc: {root}, _r: _Reader) -> None:"));
            w.scope(|w| {
                w.line(format!(
                    "\"\"\"Decode the {class} payload fields onto `_exc`.\"\"\""
                ));
                for f in &c.fields {
                    w.line(format!(
                        "_exc.{} = {}",
                        py_field(&f.name),
                        py_read_expr(&f.ty, 0)
                    ));
                }
            });
        }
        w.blank().blank();
        w.line(format!(
            "{payloads}: Dict[int, Callable[[{root}, _Reader], None]] = {{"
        ));
        w.scope(|w| {
            for c in eb.codes.iter().filter(|c| !c.fields.is_empty()) {
                w.line(format!("{}: {},", c.value, py_payload_decoder_name(eb, c)));
            }
        });
        w.line("}");
    }

    w.blank().blank();
    w.line(format!(
        "def {factory}(code: int, message: str, payload: bytes = b\"\") -> {root}:"
    ));
    w.scope(|w| {
        w.line(format!(
            "\"\"\"Build the {domain} subclass matching `code`, or the plain"
        ));
        w.line(format!(
            "{root} for codes outside the domain (panics, marshalling).\"\"\""
        ));
        w.line(format!("cls = {table}.get(code)"));
        w.line("if cls is None:");
        w.scope(|w| {
            w.line(format!("return {root}(code, message)"));
        });
        w.line("exc: Any = cls(message) if message else cls()");
        if has_payloads {
            w.line(format!("decoder = {payloads}.get(code)"));
            w.line("if decoder is not None and payload:");
            w.scope(|w| {
                w.line("r = _Reader(payload)");
                w.line("decoder(exc, r)");
                w.line("r.expect_end()");
            });
        }
        w.line("return exc");
    });

    w.blank().blank();
    w.line(format!("def {checker}(err: _ErrorStruct) -> None:"));
    w.scope(|w| {
        w.line("if err.code:");
        w.scope(|w| {
            w.line(format!("raise {factory}(*_read_error(err))"));
        });
    });

    out.push_str(&w.finish());
}

// ── Enums ──

/// Render one enum: a plain `IntEnum` for a C-style enum, or the dataclass
/// sum-type hierarchy for a rich (algebraic) enum.
pub(crate) fn render_enum(out: &mut String, e: &EnumBinding) {
    if e.is_rich() {
        render_rich_enum(out, e);
        return;
    }
    let mut w = CodeWriter::four_space();
    w.blank().blank();
    w.line(format!("class {}(IntEnum):", e.name));
    w.indent();
    let mut doc = String::new();
    emit_docstring(&mut doc, &e.doc, "    ");
    w.raw(doc);
    for v in &e.variants {
        emit_comment(&mut w, &v.doc);
        w.line(format!("{} = {}", py_variant(&v.name), v.value));
    }
    out.push_str(&w.finish());
}

/// Emit a doc string as `#` comment lines at the writer's indent.
fn emit_comment(w: &mut CodeWriter, doc: &Option<String>) {
    if let Some(d) = doc {
        for line in d.trim().lines() {
            w.line(format!("# {line}").trim_end());
        }
    }
}

/// Render a rich (algebraic) enum as an idiomatic Python sum type: a base
/// class holding the nested `Tag` discriminant enum and a `tag` property,
/// one module-level `@dataclass` subclass per variant carrying its fields,
/// scoped aliases (`Shape.Circle` is `ShapeCircle`), and the buffer codec
/// functions implementing the wire shape `i32 tag + active variant's
/// fields`. Consumers construct variants directly and discriminate with
/// `isinstance` (or the `tag` property).
fn render_rich_enum(out: &mut String, e: &EnumBinding) {
    let name = &e.name;
    let mut w = CodeWriter::four_space();
    w.blank().blank();
    w.line(format!("class {name}:"));
    w.indent();
    let mut doc = String::new();
    emit_docstring(&mut doc, &e.doc, &w.indent_str());
    if !doc.is_empty() {
        w.raw(doc);
        w.blank();
    }
    w.line("class Tag(IntEnum):");
    w.scope(|w| {
        for v in &e.variants {
            emit_comment(w, &v.doc);
            w.line(format!("{} = {}", py_variant(&v.name), v.value));
        }
    });
    w.blank();
    // The variant classes, attached below once they exist.
    for v in &e.variants {
        w.line(format!("{0}: \"Type[{name}{0}]\"", py_variant(&v.name)));
    }
    w.line(format!("TAG: \"{name}.Tag\""));
    w.blank();
    w.line("@property");
    w.line(format!("def tag(self) -> \"{name}.Tag\":"));
    w.scope(|w| {
        w.line("\"\"\"The discriminant of this value's active variant.\"\"\"");
        w.line("return type(self).TAG");
    });
    w.dedent();

    for v in &e.variants {
        let class = format!("{name}{}", py_variant(&v.name));
        w.blank().blank();
        w.line("@dataclass");
        w.line(format!("class {class}({name}):"));
        w.indent();
        let mut doc = String::new();
        emit_docstring(&mut doc, &v.doc, &w.indent_str());
        if !doc.is_empty() {
            w.raw(doc);
            w.blank();
        }
        w.line(format!("TAG = {name}.Tag.{}", py_variant(&v.name)));
        if !v.fields.is_empty() {
            w.blank();
            render_dataclass_fields(&mut w, &v.fields);
        }
        w.dedent();
    }

    // Scoped aliases (`Shape.Circle`), assigned once every variant class
    // exists.
    w.blank().blank();
    for v in &e.variants {
        w.line(format!("{name}.{0} = {name}{0}", py_variant(&v.name)));
    }

    render_rich_enum_codecs(&mut w, e);
    out.push_str(&w.finish());
}

// ── Records ──

/// Render a record as a plain `@dataclass` value class plus its buffer codec
/// functions. Records have no C symbols: construction, equality, and repr
/// all come from the dataclass, and instances cross the ABI serialized in
/// value buffers.
pub(crate) fn render_struct(out: &mut String, s: &StructBinding) {
    let mut w = CodeWriter::four_space();
    w.blank().blank();
    w.line("@dataclass");
    w.line(format!("class {}:", s.name));
    w.indent();
    let mut doc = String::new();
    emit_docstring(&mut doc, &s.doc, &w.indent_str());
    let has_doc = !doc.is_empty();
    w.raw(doc);
    if s.fields.is_empty() {
        if !has_doc {
            w.line("pass");
        }
    } else {
        if has_doc {
            w.blank();
        }
        render_dataclass_fields(&mut w, &s.fields);
    }
    w.dedent();
    render_record_codecs(&mut w, s);
    out.push_str(&w.finish());
}

/// Emit dataclass field lines (`name: hint`), with field docs as leading
/// comments.
fn render_dataclass_fields(w: &mut CodeWriter, fields: &[FieldBinding]) {
    for f in fields {
        emit_comment(w, &f.doc);
        w.line(format!(
            "{}: {}",
            py_field(&f.name),
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
/// a concurrent `close()` defers the release until they return. `_adopt`
/// wraps a pointer the producer handed over (a return, an async result, an
/// iterator element, or a buffer token); `_clone_ref` mints a second strong
/// reference when the object is written into a value buffer. A constructor
/// named `new` becomes `__init__`; every other constructor becomes a
/// `@classmethod` factory; methods lend `self` as the leading C argument;
/// statics are `@staticmethod`s.
pub(crate) fn render_interface(
    out: &mut String,
    g: &Gen<'_>,
    module: &ModuleBinding,
    i: &InterfaceBinding,
) {
    let error = module.error.as_ref();
    let name = &i.name;
    // Consecutive sync member bindings are packed into one block.
    let mut packed = false;
    for m in i.constructors.iter().chain(&i.methods).chain(&i.statics) {
        render_bindings(out, g, m, error, &format!("{name}.{}", m.name), packed);
        packed = matches!(m.shape, CallShape::Sync(_));
    }
    let clone = py_binding_name(&i.clone_symbol, g.prefix);
    let destroy = py_binding_name(&i.destroy_symbol, g.prefix);
    if !packed {
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "{clone} = _bind(\"{}\", ctypes.c_void_p, ctypes.c_void_p)\n\
         {destroy} = _bind(\"{}\", None, ctypes.c_void_p)\n",
        i.clone_symbol, i.destroy_symbol
    ));

    out.push_str(&format!("\n\nclass {name}(_Object):\n"));
    emit_docstring(out, &i.doc, "    ");
    out.push_str(&format!(
        "\n    _destroy = staticmethod({destroy})\n    _clone = staticmethod({clone})\n"
    ));

    let is_new = |f: &&FnBinding| f.name == "new";
    match i.constructors.iter().find(is_new) {
        Some(c) => render_callable(out, g, module, c, FnScope::Init),
        None => {
            // No canonical constructor: instances only come from factories
            // and producer returns.
            out.push_str("\n    def __init__(self) -> None:");
            out.push_str(&format!(
                "\n        raise TypeError(\"{name} cannot be instantiated directly\")\n"
            ));
        }
    }
    for c in i.constructors.iter().filter(|c| c.name != "new") {
        render_callable(out, g, module, c, FnScope::Factory);
    }
    for m in &i.methods {
        render_callable(out, g, module, m, FnScope::Method);
    }
    for s in &i.statics {
        render_callable(out, g, module, s, FnScope::Static);
    }
}
