//! Entity renderers: plain enums, records, rich enums, interfaces, and the
//! typed error surface of a module's declared error domain.

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use heck::{ToShoutySnakeCase, ToSnakeCase};
use weaveffi_model::model::{
    EnumBinding, ErrorBinding, FnBinding, InterfaceBinding, ModuleBinding, StructBinding,
};
use weaveffi_model::plan::ErrorStrategy;

use crate::targets::ruby::calls::{render_attach_function, render_callable, RbScope};
use crate::targets::ruby::codec::render_wv_read;
use crate::targets::ruby::docs::emit_doc;
use crate::targets::ruby::types::{rb_field_name, rb_str_literal};

/// The snake_case stem of a domain's generated helpers: `KvError` becomes
/// `kv_error`, naming `kv_error_from` and `check_kv_error!`. Domain type
/// names are globally unique (validated), so the helpers can't collide.
fn rb_error_stem(eb: &ErrorBinding) -> String {
    eb.type_name.to_snake_case()
}

/// `{stem}_from`: builds the domain error matching an ABI code.
pub(crate) fn rb_error_factory_name(eb: &ErrorBinding) -> String {
    format!("{}_from", rb_error_stem(eb))
}

/// `check_{stem}!`: raises the typed domain error for a non-zero out-err slot.
pub(crate) fn rb_error_checker_name(eb: &ErrorBinding) -> String {
    format!("check_{}!", rb_error_stem(eb))
}

/// The error-check call a callable's out-err slot goes through, per the
/// function's [`ErrorStrategy`]: the module domain's typed checker for
/// [`ErrorStrategy::Throws`], the generic `check_error!` (plain `Error`;
/// producer panics and marshalling failures only) for
/// [`ErrorStrategy::Trap`].
pub(crate) fn rb_checker_name(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match (f.error_strategy(), error) {
        (ErrorStrategy::Throws, Some(eb)) => rb_error_checker_name(eb),
        _ => "check_error!".to_string(),
    }
}

/// Render one module's declared error domain: a domain class subclassing the
/// generic `Error`, one nested subclass per code carrying its stable `CODE`
/// constant, default message, and any declared payload fields as attributes,
/// the code-to-class table, and the factory/checker helpers throwing wrappers
/// route their out-err slots through. Nesting the code classes keeps
/// `KvError::KeyNotFound` spellable and unambiguous even across domains.
///
/// Domain codes are validated positive-only; the negative range is reserved
/// for the runtime (`-1` generic, `-2` producer panic, `-3` marshalling
/// failure, `-4` a callback-interface implementation that raised, `-5`
/// cancelled). The factory therefore maps only declared codes onto typed
/// classes and lets everything else fall through to `_wv_error` (the
/// generic `Error`, or `Cancelled`).
pub(crate) fn render_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding) {
    let domain = &eb.type_name;
    let factory = rb_error_factory_name(eb);
    let checker = rb_error_checker_name(eb);
    let table = format!("{}_CODES", eb.type_name.to_shouty_snake_case());

    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.line(format!(
        "# Base error for the `{}` module's error domain.",
        module.path
    ));
    w.line(format!("class {domain} < Error"));
    w.scope(|w| {
        for (idx, c) in eb.codes.iter().enumerate() {
            if idx > 0 {
                w.blank();
            }
            let class = weaveffi_model::errors::pascal(&c.name);
            let doc = c.doc.clone().unwrap_or_else(|| c.message.clone());
            let mut d = String::new();
            emit_doc(&mut d, &Some(doc), "    ");
            w.raw(d);
            w.block(format!("class {class} < {domain}"), "end", |w| {
                w.line(format!("CODE = {}", c.value));
                if !c.fields.is_empty() {
                    w.blank();
                    for f in &c.fields {
                        let mut fd = String::new();
                        emit_doc(&mut fd, &f.doc, "      ");
                        w.raw(fd);
                        w.line(format!("attr_reader :{}", rb_field_name(&f.name)));
                    }
                }
                w.blank();
                let kw: String = c
                    .fields
                    .iter()
                    .map(|f| format!(", {}: nil", rb_field_name(&f.name)))
                    .collect();
                w.block(
                    format!(
                        "def initialize(message = '{}'{kw})",
                        rb_str_literal(&c.message)
                    ),
                    "end",
                    |w| {
                        for f in &c.fields {
                            let field = rb_field_name(&f.name);
                            w.line(format!("@{field} = {field}"));
                        }
                        w.line(format!("super({}, message)", c.value));
                    },
                );
            });
        }
    });
    w.line("end");

    w.blank();
    w.line(format!(
        "# Maps each ABI code of the {domain} domain to its error class."
    ));
    w.line(format!("{table} = {{"));
    w.scope(|w| {
        for c in &eb.codes {
            w.line(format!(
                "{} => {domain}::{},",
                c.value,
                weaveffi_model::errors::pascal(&c.name)
            ));
        }
    });
    w.line("}.freeze");

    w.blank();
    w.line(format!(
        "# Builds the {domain} subclass matching `code`, decoding any payload"
    ));
    w.line("# fields the code declares, or a generic Error for codes outside");
    w.line("# the domain (panics, marshalling, callback failures).");
    w.block(
        format!("def self.{factory}(code, message, payload = nil)"),
        "end",
        |w| {
            if eb.codes.iter().any(|c| !c.fields.is_empty()) {
                w.line("case code");
                for c in eb.codes.iter().filter(|c| !c.fields.is_empty()) {
                    let class = weaveffi_model::errors::pascal(&c.name);
                    w.line(format!("when {}", c.value));
                    w.scope(|w| {
                        w.line("r = WvBufferReader.new(payload || ''.b)");
                        for f in &c.fields {
                            let field = rb_field_name(&f.name);
                            render_wv_read(w, "r", &format!("_wv_{field}"), &f.ty, 0, "");
                        }
                        w.line("r.expect_end!");
                        let kwargs = c
                            .fields
                            .iter()
                            .map(|f| {
                                let field = rb_field_name(&f.name);
                                format!("{field}: _wv_{field}")
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        w.line(format!(
                            "return {domain}::{class}.new({kwargs}) if message.empty?"
                        ));
                        w.line(format!("return {domain}::{class}.new(message, {kwargs})"));
                    });
                }
                w.line("end");
            }
            w.line(format!("cls = {table}[code]"));
            w.line("return _wv_error(code, message) if cls.nil?");
            w.line("message.empty? ? cls.new : cls.new(message)");
        },
    );

    w.blank();
    w.line(format!(
        "# Raises the typed {domain} for a non-zero error slot."
    ));
    w.block(format!("def self.{checker}(err)"), "end", |w| {
        w.line("taken = _wv_take_error(err)");
        w.line(format!("raise {factory}(*taken) if taken"));
    });
    out.push_str(&w.finish());
}

/// Render one plain C-style enum as a module of integer constants, one
/// `SHOUTY_SNAKE` constant per variant.
pub(crate) fn render_enum(out: &mut String, e: &EnumBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &e.doc, "  ");
    w.raw(d);
    w.line(format!("module {}", e.name));
    w.scope(|w| {
        for v in &e.variants {
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "    ");
            w.raw(vd);
            w.line(format!("{} = {}", v.name.to_shouty_snake_case(), v.value));
        }
    });
    w.line("end");
    out.push_str(&w.finish());
}

/// Render one record as a plain Ruby value class: one documented
/// `attr_reader` per field, a keyword-argument `initialize`, and structural
/// `==`. Records are value types: they own no C symbols, no destroy, and no
/// builders; they cross the ABI packed into value buffers by the module's
/// `_wv_write_*`/`_wv_read_*` codec helpers.
pub(crate) fn render_struct_class(out: &mut String, s: &StructBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &s.doc, "  ");
    w.raw(d);
    w.line(format!("class {}", s.name));
    w.scope(|w| {
        for (idx, f) in s.fields.iter().enumerate() {
            if idx > 0 {
                w.blank();
            }
            let mut fd = String::new();
            emit_doc(&mut fd, &f.doc, "    ");
            w.raw(fd);
            w.line(format!("attr_reader :{}", rb_field_name(&f.name)));
        }
        w.blank();
        let kw = s
            .fields
            .iter()
            .map(|f| format!("{}:", rb_field_name(&f.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let open = if kw.is_empty() {
            "def initialize".to_string()
        } else {
            format!("def initialize({kw})")
        };
        w.block(open, "end", |w| {
            for f in &s.fields {
                let field = rb_field_name(&f.name);
                w.line(format!("@{field} = {field}"));
            }
        });
        w.blank();
        w.line("# Structural equality over every field.");
        w.block("def ==(other)", "end", |w| {
            w.line(format!("return false unless other.is_a?({})", s.name));
            for f in &s.fields {
                let field = rb_field_name(&f.name);
                w.line(format!("return false unless {field} == other.{field}"));
            }
            w.line("true");
        });
    });
    w.line("end");
    out.push_str(&w.finish());
}

/// Render one rich (algebraic) enum as an idiomatic tagged class hierarchy:
/// a base class exposing `tag`, plus one nested value-class subclass per
/// variant carrying that variant's fields (documented `attr_reader`s, a
/// keyword-argument `initialize`, structural `==`). Rich enums own no C
/// symbols; they cross the ABI packed into value buffers as an `i32` tag
/// followed by the active variant's fields in declaration order.
pub(crate) fn render_rich_enum_class(out: &mut String, e: &EnumBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &e.doc, "  ");
    w.raw(d);
    w.line(format!("class {}", e.name));
    w.scope(|w| {
        w.line("# The active variant's integer tag.");
        w.block("def tag", "end", |w| {
            w.line("self.class::TAG");
        });
        for v in &e.variants {
            w.blank();
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "    ");
            w.raw(vd);
            w.line(format!("class {} < {}", v.name, e.name));
            w.scope(|w| {
                w.line(format!("TAG = {}", v.value));
                if !v.fields.is_empty() {
                    for f in &v.fields {
                        w.blank();
                        let mut fd = String::new();
                        emit_doc(&mut fd, &f.doc, "      ");
                        w.raw(fd);
                        w.line(format!("attr_reader :{}", rb_field_name(&f.name)));
                    }
                    w.blank();
                    let kw = v
                        .fields
                        .iter()
                        .map(|f| format!("{}:", rb_field_name(&f.name)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    w.block(format!("def initialize({kw})"), "end", |w| {
                        for f in &v.fields {
                            let field = rb_field_name(&f.name);
                            w.line(format!("@{field} = {field}"));
                        }
                    });
                }
                w.blank();
                w.line("# Structural equality over the variant and its fields.");
                w.block("def ==(other)", "end", |w| {
                    w.line(format!("return false unless other.is_a?({})", v.name));
                    for f in &v.fields {
                        let field = rb_field_name(&f.name);
                        w.line(format!("return false unless {field} == other.{field}"));
                    }
                    w.line("true");
                });
            });
            w.line("end");
        }
    });
    w.line("end");
    out.push_str(&w.finish());
}

/// Declare the FFI bindings for one interface: the clone and destroy
/// lifecycle symbols plus every constructor, method, and static through the
/// shared attach path. Methods carry their implicit leading `self` pointer
/// slot in the precomputed ABI signatures, so no special casing is needed
/// here. `blocking` is the API-wide GVL policy (see
/// [`render_attach_function`]); destroying an object can release callback
/// implementations it holds, so it follows the same policy.
pub(crate) fn render_interface_ffi(
    out: &mut String,
    module: &ModuleBinding,
    i: &InterfaceBinding,
    blocking: bool,
) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.line(format!(
        "attach_function :{}, [:pointer], :pointer",
        i.clone_symbol
    ));
    w.line(format!(
        "attach_function :{}, [:pointer], :void{}",
        i.destroy_symbol,
        if blocking { ", blocking: true" } else { "" }
    ));
    out.push_str(&w.finish());
    for f in i
        .constructors
        .iter()
        .chain(i.methods.iter())
        .chain(i.statics.iter())
    {
        render_attach_function(out, module, f, blocking);
    }
}

/// Render one interface as a reference-counted wrapper class on the
/// runtime's `WvObject` base (which supplies `handle`, `close`, `closed?`,
/// `dup`/`clone`, and the call pinning). A `{Name}Ptr < FFI::AutoPointer`
/// subclass owns exactly one strong reference and releases it through the
/// interface's `_destroy` symbol, either from `close` or, as a backstop,
/// from the GC finalizer. A constructor named `new` becomes `initialize`;
/// every other constructor becomes a class-method factory; methods borrow
/// the wrapper's pointer as the leading C argument; statics are class
/// methods. `_from_ptr` adopts a reference the producer handed over (a
/// return, an async result, an iterator element, a buffer token) without
/// re-running `initialize`.
pub(crate) fn render_interface_class(
    out: &mut String,
    module: &ModuleBinding,
    i: &InterfaceBinding,
    rb_module_name: &str,
) {
    let ptr_class = format!("{}Ptr", i.name);
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# Owns one strong reference to a {}; releases it exactly once.",
        i.name
    ));
    w.block(
        format!("class {ptr_class} < FFI::AutoPointer"),
        "end",
        |w| {
            w.block("def self.release(ptr)", "end", |w| {
                w.line(format!("{rb_module_name}.{}(ptr)", i.destroy_symbol));
            });
        },
    );
    w.blank();
    w.doc(&i.doc, DocCommentStyle::Hash);
    if let Some(msg) = &i.deprecated {
        w.line(format!("# @deprecated {msg}"));
    }
    w.line(format!("class {} < WvObject", i.name));
    w.scope(|w| {
        w.line(format!("WV_PTR = {ptr_class}"));
        if !i.constructors.iter().any(|c| c.name == "new") {
            w.line("private_class_method :new");
        }
        w.blank();
        w.line("# @api private");
        w.block("def self._wv_clone(ptr)", "end", |w| {
            w.line(format!("{rb_module_name}.{}(ptr)", i.clone_symbol));
        });
    });
    out.push_str(&w.finish());

    // Members render at class depth through the shared callable paths, so
    // sync, async, and iterator members reuse the free-function marshalling.
    let scope_of = |f: &FnBinding, ctor: bool| -> RbScope {
        let module_name = rb_module_name;
        if ctor && f.name == "new" {
            RbScope::Init { module_name }
        } else if ctor {
            RbScope::Factory { module_name }
        } else if f.has_self {
            RbScope::Method { module_name }
        } else {
            RbScope::Static { module_name }
        }
    };
    for c in &i.constructors {
        render_callable(out, module, c, &scope_of(c, true));
    }
    for f in i.methods.iter().chain(i.statics.iter()) {
        render_callable(out, module, f, &scope_of(f, false));
    }

    let mut close = CodeWriter::two_space().with_depth(1);
    close.line("end");
    out.push_str(&close.finish());
}
