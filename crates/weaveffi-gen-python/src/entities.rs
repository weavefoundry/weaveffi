//! Entity rendering: error domains, plain and rich enums, record
//! dataclasses, and interface wrapper classes.

use heck::{ToShoutySnakeCase, ToSnakeCase};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    CallShape, EnumBinding, ErrorBinding, ErrorCodeBinding, FieldBinding, FnBinding,
    InterfaceBinding, ModuleBinding, StructBinding,
};

use crate::calls::{render_callable, render_iterator_class, FnScope};
use crate::codec::{py_read_expr, render_record_codecs, render_rich_enum_codecs};
use crate::docs::emit_docstring;
use crate::types::{py_field, py_str_literal, py_type_hint, py_variant};

// ── Errors ──

/// The snake_case stem shared by a domain's private helper names.
fn py_error_stem(eb: &ErrorBinding) -> String {
    eb.type_name.to_snake_case()
}

/// `_{stem}_from`: builds the domain exception matching an ABI code.
pub(crate) fn py_error_factory_name(eb: &ErrorBinding) -> String {
    format!("_{}_from", py_error_stem(eb))
}

/// `_check_{stem}`: raises the domain exception for a non-zero out-err slot.
fn py_error_checker_name(eb: &ErrorBinding) -> String {
    format!("_check_{}", py_error_stem(eb))
}

/// The error-check call a callable's out-err slot goes through: the module
/// domain's typed checker when the callable throws, the generic
/// `_check_error` (plain `WeaveFFIError`, panics and marshalling failures
/// only) otherwise.
pub(crate) fn py_checker_name(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match error {
        Some(eb) if f.throws => py_error_checker_name(eb),
        _ => "_check_error".to_string(),
    }
}

/// The Python class name for one error code: plain PascalCase with no forced
/// suffix (`KeyNotFound`, not `KeyNotFoundError`), matching the new samples'
/// already-Pascal code names. Each class is also attached to its domain class
/// (`KvError.KeyNotFound`), which stays unambiguous even if two domains
/// declare codes with the same name.
pub(crate) fn py_code_class_name(name: &str) -> String {
    weaveffi_core::errors::pascal(name)
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
/// the domain (subclassing the generic `WeaveFFIError`), one exception
/// subclass per code carrying its stable `CODE` and default message, the
/// code-to-class table, per-code payload decoders, and the factory/checker
/// helpers throwing wrappers route their out-err slots through. Each code
/// class is also attached to the domain class, so consumers can catch
/// `KvError.KeyNotFound`.
pub(crate) fn render_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding) {
    let domain = &eb.type_name;
    let factory = py_error_factory_name(eb);
    let checker = py_error_checker_name(eb);
    let table = format!("_{}_CODES", eb.type_name.to_shouty_snake_case());
    let payloads = format!("_{}_PAYLOADS", eb.type_name.to_shouty_snake_case());
    let has_payloads = eb.codes.iter().any(|c| !c.fields.is_empty());

    let mut w = CodeWriter::four_space();
    w.blank().blank();
    w.line(format!("class {domain}(WeaveFFIError):"));
    w.scope(|w| {
        w.line(format!(
            "\"\"\"Base exception for the `{}` module's error domain.\"\"\"",
            module.path
        ));
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
    w.line(format!("{table}: Dict[int, type] = {{"));
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
        for c in &eb.codes {
            if c.fields.is_empty() {
                continue;
            }
            let decoder = py_payload_decoder_name(eb, c);
            let class = py_code_class_name(&c.name);
            w.blank().blank();
            w.line(format!(
                "def {decoder}(_exc: WeaveFFIError, _r: _BufferReader) -> None:"
            ));
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
        w.line(format!("{payloads}: Dict[int, Callable] = {{"));
        w.scope(|w| {
            for c in &eb.codes {
                if c.fields.is_empty() {
                    continue;
                }
                w.line(format!("{}: {},", c.value, py_payload_decoder_name(eb, c)));
            }
        });
        w.line("}");
    }

    w.blank().blank();
    w.line(format!(
        "def {factory}(code: int, message: str, payload: bytes = b\"\") -> WeaveFFIError:"
    ));
    w.scope(|w| {
        w.line(format!(
            "\"\"\"Build the {domain} subclass matching `code`, or a generic"
        ));
        w.line("WeaveFFIError for codes outside the domain (panics, marshalling).\"\"\"");
        w.line(format!("_cls = {table}.get(code)"));
        w.line("if _cls is None:");
        w.scope(|w| {
            w.line("return WeaveFFIError(code, message)");
        });
        w.line("_exc = _cls(message) if message else _cls()");
        if has_payloads {
            w.line(format!("_decoder = {payloads}.get(code)"));
            w.line("if _decoder is not None and payload:");
            w.scope(|w| {
                w.line("_r = _BufferReader(payload)");
                w.line("_decoder(_exc, _r)");
                w.line("_r.expect_end()");
            });
        }
        w.line("return _exc");
    });

    w.blank().blank();
    w.line(format!("def {checker}(err: _WeaveFFIErrorStruct) -> None:"));
    w.scope(|w| {
        w.line("if err.code != 0:");
        w.scope(|w| {
            w.line("code = err.code");
            w.line("message = err.message.decode(\"utf-8\") if err.message else \"\"");
            // The payload is copied before `weaveffi_error_clear` frees it.
            w.line(
                "payload = ctypes.string_at(err.payload_ptr, err.payload_len) \
if err.payload_ptr else b\"\"",
            );
            w.line("_lib.weaveffi_error_clear(ctypes.byref(err))");
            w.line(format!("raise {factory}(code, message, payload)"));
        });
    });

    out.push_str(&w.finish());
}

// ── Enums ──

/// Render one enum: a plain `IntEnum` for a C-style enum, or the dataclass
/// sum-type hierarchy for a rich (algebraic) enum.
pub(crate) fn render_enum(out: &mut String, e: &EnumBinding) {
    // Rich (algebraic) enums are value sum types serialized into value
    // buffers; they are emitted as dataclass hierarchies, not `IntEnum`s.
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
        if let Some(d) = &v.doc {
            let trimmed = d.trim();
            if !trimmed.is_empty() {
                for line in trimmed.lines() {
                    w.line(format!("# {}", line));
                }
            }
        }
        w.line(format!("{} = {}", py_variant(&v.name), v.value));
    }
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as an idiomatic Python sum type: a base
/// class holding the nested `Tag` discriminant enum and a `tag` property,
/// one module-level `@dataclass` subclass per variant carrying its fields,
/// scoped aliases (`Shape.Circle` is `ShapeCircle`), and the buffer codec
/// functions implementing the wire shape `i32 tag + active variant's
/// fields`. Consumers construct variants directly and discriminate with
/// `isinstance` (or the `tag` property); no FFI symbols are involved.
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
    // Nested discriminant enum (`Shape.Tag.Circle == 1`, ...).
    w.line("class Tag(IntEnum):");
    w.scope(|w| {
        for v in &e.variants {
            if let Some(d) = &v.doc {
                let trimmed = d.trim();
                if !trimmed.is_empty() {
                    for line in trimmed.lines() {
                        w.line(format!("# {}", line));
                    }
                }
            }
            w.line(format!("{} = {}", py_variant(&v.name), v.value));
        }
    });
    w.blank();
    w.line("@property");
    w.line(format!("def tag(self) -> \"{name}.Tag\":"));
    w.scope(|w| {
        w.line("\"\"\"The discriminant of this value's active variant.\"\"\"");
        w.line("return type(self).TAG");
    });
    w.dedent();

    // One module-level dataclass per variant, subclassing the base.
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
        if let Some(d) = &f.doc {
            let trimmed = d.trim();
            if !trimmed.is_empty() {
                for line in trimmed.lines() {
                    w.line(format!("# {}", line));
                }
            }
        }
        w.line(format!("{}: {}", py_field(&f.name), py_type_hint(&f.ty)));
    }
}

// ── Interfaces ──

/// Render one interface as an opaque-object wrapper class, following the
/// struct wrapper's ownership pattern: the class owns the raw C pointer and
/// releases it exactly once, calling the interface's destroy symbol from
/// `__del__`. A constructor named `new` becomes `__init__`; every other
/// constructor becomes a `@classmethod` factory; methods pass `self._ptr` as
/// the leading C argument; statics are `@staticmethod`s. `_from_ptr` wraps a
/// pointer the producer already handed over (a C return value) without
/// re-running the FFI constructor.
pub(crate) fn render_interface(out: &mut String, module: &ModuleBinding, i: &InterfaceBinding) {
    let error = module.error.as_ref();

    // `_...Iterator` helpers are module-level classes; emit them ahead of the
    // wrapper so nothing nests inside the class body. The interface name
    // qualifies the helper so two interfaces can share a method name.
    for m in i.methods.iter().chain(i.statics.iter()) {
        if let (Some(Ty::Iterator(inner)), CallShape::Iterator(it)) = (&m.ret, &m.shape) {
            let checker = py_checker_name(m, error);
            render_iterator_class(
                out,
                &it.iter_tag,
                &format!("{}_{}", i.name, m.name),
                inner,
                &checker,
            );
        }
    }

    out.push_str(&format!("\n\nclass {}:\n", i.name));
    emit_docstring(out, &i.doc, "    ");

    out.push_str(&format!(
        "\n    @classmethod\n    def _from_ptr(cls, ptr) -> \"{}\":",
        i.name
    ));
    out.push_str("\n        _obj = cls.__new__(cls)");
    out.push_str("\n        _obj._ptr = ptr");
    out.push_str("\n        return _obj");

    let new_ctor = i.constructors.iter().find(|c| c.name == "new");
    if let Some(c) = new_ctor {
        render_callable(out, c, error, &FnScope::Init);
    } else {
        // No canonical constructor: expose the same raw-pointer `__init__`
        // the struct wrappers use, so factories stay the only public path.
        out.push_str("\n\n    def __init__(self, _ptr: int) -> None:");
        out.push_str("\n        self._ptr = _ptr\n");
    }

    let destroy = &i.destroy_symbol;
    out.push_str("\n\n    def __del__(self) -> None:");
    out.push_str("\n        if self._ptr is not None:");
    out.push_str(&format!(
        "\n            _lib.{destroy}.argtypes = [ctypes.c_void_p]"
    ));
    out.push_str(&format!("\n            _lib.{destroy}.restype = None"));
    out.push_str(&format!("\n            _lib.{destroy}(self._ptr)"));
    out.push_str("\n            self._ptr = None");

    for c in &i.constructors {
        if c.name != "new" {
            render_callable(out, c, error, &FnScope::Factory);
        }
    }
    for m in &i.methods {
        render_callable(out, m, error, &FnScope::Method { class: &i.name });
    }
    for s in &i.statics {
        render_callable(out, s, error, &FnScope::Static { class: &i.name });
    }
    out.push('\n');
}
