//! Entity renderers: plain enums, records, rich enums, interfaces, and the
//! typed error surface of a module's declared error domain.

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use heck::{ToShoutySnakeCase, ToSnakeCase};
use weaveffi_model::errors::pascal;
use weaveffi_model::model::{
    EnumBinding, ErrorBinding, FieldBinding, FnBinding, InterfaceBinding, ModuleBinding,
    StructBinding,
};
use weaveffi_model::plan::ErrorStrategy;

use crate::targets::ruby::calls::{render_attach_function, render_callable, RbScope, ScopeKind};
use crate::targets::ruby::types::{rb_field_name, rb_str_literal};
use crate::targets::ruby::RbCtx;

/// The snake_case stem of a domain's generated helpers: `KvError` becomes
/// `kv_error`, naming `_wv_kv_error_from`, `_wv_check_kv_error!`, and
/// `_wv_kv_error_payload`. Domain type names are globally unique
/// (validated), so the helpers can't collide.
fn rb_error_stem(eb: &ErrorBinding) -> String {
    eb.type_name.to_snake_case()
}

/// `_wv_{stem}_from`: builds the domain error matching an ABI code.
pub(crate) fn rb_error_factory_name(eb: &ErrorBinding) -> String {
    format!("_wv_{}_from", rb_error_stem(eb))
}

/// `_wv_check_{stem}!`: raises the typed domain error for a non-zero
/// out-err slot.
fn rb_error_checker_name(eb: &ErrorBinding) -> String {
    format!("_wv_check_{}!", rb_error_stem(eb))
}

/// `_wv_{stem}_payload`: the code and encoded payload of a domain error a
/// callback implementation raised.
pub(crate) fn rb_error_payload_name(eb: &ErrorBinding) -> String {
    format!("_wv_{}_payload", rb_error_stem(eb))
}

/// The error-check call a callable's out-err slot goes through, per the
/// function's [`ErrorStrategy`]: the module domain's typed checker for
/// [`ErrorStrategy::Throws`], the runtime's trap (`_wv_check!`, raising
/// `NativeBugError`) for [`ErrorStrategy::Trap`].
pub(crate) fn rb_checker_name(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match (f.error_strategy(), error) {
        (ErrorStrategy::Throws, Some(eb)) => rb_error_checker_name(eb),
        _ => "_wv_check!".to_string(),
    }
}

/// Emit `attr_reader` lines for `fields`, each with its doc comment,
/// separated by blank lines.
fn emit_readers(w: &mut CodeWriter, fields: &[FieldBinding]) {
    for (idx, f) in fields.iter().enumerate() {
        if idx > 0 {
            w.blank();
        }
        w.doc(&f.doc, DocCommentStyle::Hash);
        w.line(format!("attr_reader :{}", rb_field_name(&f.name)));
    }
}

/// Render one module's declared error domain: a domain class subclassing
/// the root `Error`, one nested subclass per code carrying its stable
/// `CODE` constant, default message, and any declared payload fields as
/// attributes, then the private helpers: the factory and checker throwing
/// wrappers route their out-err slots through, and (when a callback method
/// may raise the domain, `payload`) the encoder reporting a raised domain
/// error back to the producer. Nesting the code classes keeps
/// `KvError::KeyNotFound` spellable and unambiguous even across domains.
///
/// Domain codes are validated positive-only; the negative range is reserved
/// for the runtime. The factory therefore maps only declared codes onto
/// typed classes and lets everything else fall through to `_wv_error` (the
/// root `Error`, or `Cancelled`).
pub(crate) fn render_error(
    w: &mut CodeWriter,
    ctx: &RbCtx,
    module: &ModuleBinding,
    eb: &ErrorBinding,
    payload: bool,
) {
    let domain = &eb.type_name;
    w.blank();
    w.line(format!(
        "# Base error for the `{}` module's error domain.",
        module.dot_path
    ));
    w.block(format!("class {domain} < Error"), "end", |w| {
        for (idx, c) in eb.codes.iter().enumerate() {
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
                    if !c.fields.is_empty() {
                        w.blank();
                        emit_readers(w, &c.fields);
                    }
                    w.blank();
                    let kw: String = c
                        .fields
                        .iter()
                        .map(|f| format!(", {}: nil", rb_field_name(&f.name)))
                        .collect();
                    w.block(format!("def initialize(message = nil{kw})"), "end", |w| {
                        for f in &c.fields {
                            let field = rb_field_name(&f.name);
                            w.line(format!("@{field} = {field}"));
                        }
                        w.line(format!(
                            "super(CODE, message || '{}')",
                            rb_str_literal(&c.message)
                        ));
                    });
                },
            );
        }
    });

    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# The {domain} for a domain `code`, with its payload fields decoded;"
    ));
    w.line("# the root Error (or Cancelled) for any other code.");
    w.block(
        format!(
            "def self.{}(code, message, payload = nil)",
            rb_error_factory_name(eb)
        ),
        "end",
        |w| {
            w.line("message = nil if message.empty?");
            w.line("r = WvBufferReader.new(payload)");
            w.line("error =");
            w.scope(|w| {
                w.line("case code");
                for c in &eb.codes {
                    w.line(format!("when {}", c.value));
                    w.scope(|w| {
                        let class = format!("{domain}::{}", pascal(&c.name));
                        ctx.codecs
                            .emit_new(w, "", &class, Some("message"), &c.fields, "");
                    });
                }
                w.line("else");
                w.scope(|w| {
                    w.line("return _wv_error(code, message.to_s)");
                });
                w.line("end");
            });
            w.line("r.expect_end!");
            w.line("error");
        },
    );

    w.blank();
    w.line("# @api private");
    w.line(format!("# Raises the {domain} for a non-zero error slot."));
    w.block(
        format!("def self.{}(err)", rb_error_checker_name(eb)),
        "end",
        |w| {
            w.line("taken = _wv_take_error(err)");
            w.line(format!(
                "raise {}(*taken) unless taken.nil?",
                rb_error_factory_name(eb)
            ));
        },
    );

    if payload {
        w.blank();
        w.line("# @api private");
        w.line(format!(
            "# The code and value-buffer payload of a {domain} a callback"
        ));
        w.line("# implementation raised, or nil for an error of no declared code.");
        w.block(
            format!("def self.{}(error)", rb_error_payload_name(eb)),
            "end",
            |w| {
                w.line("w = WvBufferWriter.new");
                w.line("code =");
                w.scope(|w| {
                    w.line("case error");
                    for c in &eb.codes {
                        let class = format!("{domain}::{}", pascal(&c.name));
                        if c.fields.is_empty() {
                            w.line(format!("when {class} then {}", c.value));
                            continue;
                        }
                        w.line(format!("when {class}"));
                        w.scope(|w| {
                            for f in &c.fields {
                                let field = rb_field_name(&f.name);
                                w.line(ctx.codecs.write(&f.ty, "w", &format!("error.{field}"), ""));
                            }
                            w.line(c.value.to_string());
                        });
                    }
                    w.line("end");
                });
                w.line("code && [code, w.bytes]");
            },
        );
    }
}

/// Render one plain C-style enum as a module of integer constants, one
/// `SHOUTY_SNAKE` constant per variant.
pub(crate) fn render_enum(w: &mut CodeWriter, e: &EnumBinding) {
    w.blank();
    w.doc(&e.doc, DocCommentStyle::Hash);
    if let Some(msg) = &e.deprecated {
        w.line(format!("# @deprecated {msg}"));
    }
    w.block(format!("module {}", e.name), "end", |w| {
        for v in &e.variants {
            w.doc(&v.doc, DocCommentStyle::Hash);
            w.line(format!("{} = {}", v.name.to_shouty_snake_case(), v.value));
        }
    });
}

/// Emit a value class body: documented `attr_reader`s, a keyword-argument
/// `initialize`, and structural `==` over `class`'s fields.
fn emit_value_class(w: &mut CodeWriter, class: &str, fields: &[FieldBinding]) {
    if !fields.is_empty() {
        emit_readers(w, fields);
        w.blank();
        let kw: Vec<String> = fields
            .iter()
            .map(|f| format!("{}:", rb_field_name(&f.name)))
            .collect();
        let one_line = format!("def initialize({})", kw.join(", "));
        if w.indent_str().len() + one_line.len() <= 100 {
            w.line(one_line);
        } else {
            w.line("def initialize(");
            w.scope(|w| {
                for (i, k) in kw.iter().enumerate() {
                    let sep = if i + 1 == kw.len() { "" } else { "," };
                    w.line(format!("{k}{sep}"));
                }
            });
            w.line(")");
        }
        w.scope(|w| {
            for f in fields {
                let field = rb_field_name(&f.name);
                w.line(format!("@{field} = {field}"));
            }
        });
        w.line("end");
        w.blank();
    }
    w.line("# Structural equality over every field.");
    w.block("def ==(other)", "end", |w| {
        let mut terms = vec![format!("other.is_a?({class})")];
        terms.extend(fields.iter().map(|f| {
            let field = rb_field_name(&f.name);
            format!("{field} == other.{field}")
        }));
        match terms.as_slice() {
            [one] => {
                w.line(one.clone());
            }
            [first, rest @ ..] => {
                w.line(format!("{first} &&"));
                w.scope(|w| {
                    for (i, t) in rest.iter().enumerate() {
                        if i + 1 == rest.len() {
                            w.line(t.clone());
                        } else {
                            w.line(format!("{t} &&"));
                        }
                    }
                });
            }
            [] => unreachable!("the class test is always present"),
        }
    });
}

/// Render one record as a plain Ruby value class: one documented
/// `attr_reader` per field, a keyword-argument `initialize`, and structural
/// `==`. Records are value types: they own no C symbols; they cross the ABI
/// packed into value buffers by the module's `_wv_write_*`/`_wv_read_*`
/// codec helpers.
pub(crate) fn render_struct_class(w: &mut CodeWriter, s: &StructBinding) {
    w.blank();
    w.doc(&s.doc, DocCommentStyle::Hash);
    if let Some(msg) = &s.deprecated {
        w.line(format!("# @deprecated {msg}"));
    }
    w.block(format!("class {}", s.name), "end", |w| {
        emit_value_class(w, &s.name, &s.fields);
    });
}

/// Render one rich (algebraic) enum as a tagged class hierarchy: a base
/// class exposing `tag`, plus one nested value class per variant carrying
/// that variant's fields. Rich enums own no C symbols; they cross the ABI
/// packed into value buffers as an `i32` tag followed by the active
/// variant's fields in declaration order.
pub(crate) fn render_rich_enum_class(w: &mut CodeWriter, e: &EnumBinding) {
    w.blank();
    w.doc(&e.doc, DocCommentStyle::Hash);
    if let Some(msg) = &e.deprecated {
        w.line(format!("# @deprecated {msg}"));
    }
    w.block(format!("class {}", e.name), "end", |w| {
        w.line("# The active variant's integer tag.");
        w.block("def tag", "end", |w| {
            w.line("self.class::TAG");
        });
        for v in &e.variants {
            w.blank();
            w.doc(&v.doc, DocCommentStyle::Hash);
            w.block(format!("class {} < {}", v.name, e.name), "end", |w| {
                w.line(format!("TAG = {}", v.value));
                w.blank();
                emit_value_class(w, &v.name, &v.fields);
            });
        }
    });
}

/// Declare the FFI bindings for one interface: the clone and destroy
/// lifecycle symbols (which keep the GVL; see [`render_attach_function`])
/// plus every constructor, method, and static.
pub(crate) fn render_interface_ffi(w: &mut CodeWriter, i: &InterfaceBinding) {
    w.line(format!(
        "attach_function :{}, [:pointer], :pointer",
        i.clone_symbol
    ));
    w.line(format!(
        "attach_function :{}, [:pointer], :void",
        i.destroy_symbol
    ));
    for f in i
        .constructors
        .iter()
        .chain(i.methods.iter())
        .chain(i.statics.iter())
    {
        render_attach_function(w, f);
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
/// return, an async result, an iterator element, a buffer token, a callback
/// argument) without re-running `initialize`.
pub(crate) fn render_interface_class(
    w: &mut CodeWriter,
    ctx: &RbCtx,
    error: Option<&ErrorBinding>,
    i: &InterfaceBinding,
) {
    let ptr_class = format!("{}Ptr", i.name);
    let module = ctx.module;
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
                w.line(format!("{module}.{}(ptr)", i.destroy_symbol));
            });
        },
    );
    w.blank();
    w.doc(&i.doc, DocCommentStyle::Hash);
    if let Some(msg) = &i.deprecated {
        w.line(format!("# @deprecated {msg}"));
    }
    w.block(format!("class {} < WvObject", i.name), "end", |w| {
        w.line(format!("WV_PTR = {ptr_class}"));
        if !i.constructors.iter().any(|c| c.name == "new") {
            w.line("private_class_method :new");
        }
        w.blank();
        w.line("# @api private");
        w.block("def self._wv_clone(ptr)", "end", |w| {
            w.line(format!("{module}.{}(ptr)", i.clone_symbol));
        });
        // Members render at class depth through the shared callable paths,
        // so sync, async, and iterator members reuse the free-function
        // marshalling.
        for c in &i.constructors {
            let kind = if c.name == "new" {
                ScopeKind::Init
            } else {
                ScopeKind::Factory
            };
            render_callable(w, ctx, error, c, &RbScope::member(kind, module, &i.name));
        }
        for f in i.methods.iter().chain(i.statics.iter()) {
            let kind = if f.has_self {
                ScopeKind::Method
            } else {
                ScopeKind::Static
            };
            let scope = RbScope::member(kind, module, &i.name);
            render_callable(w, ctx, error, f, &scope);
        }
    });
}
