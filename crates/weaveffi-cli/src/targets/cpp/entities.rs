//! Declared-entity rendering: plain enums, value types (records and rich
//! enums), typed error domains, and interface RAII classes, plus the
//! dependency ordering that keeps by-value members complete before use.

use std::collections::HashMap;

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use weaveffi_model::model::{
    CallShape, EnumBinding, ErrorBinding, FnBinding, InterfaceBinding, ModuleBinding, StructBinding,
};
use weaveffi_model::ty::{Family, Ty};

use crate::targets::cpp::calls::{
    iterator_class_name, render_definition, render_iterator_range, render_member_decl, FnKind,
};
use crate::targets::cpp::codec::{read_expr, write_stmt};
use crate::targets::cpp::types::{cpp_error_class, cpp_ident, cpp_member_name, cpp_type};

/// A doc comment with a trailing `@deprecated` line when the declaration is
/// deprecated. Types carry the deprecation in their docs rather than as an
/// attribute, so the generated code that marshals them compiles warning-free.
pub(crate) fn doc_text(doc: Option<&str>, deprecated: Option<&str>) -> Option<String> {
    match (doc, deprecated) {
        (Some(d), Some(msg)) => Some(format!("{d}\n\n@deprecated {msg}")),
        (None, Some(msg)) => Some(format!("@deprecated {msg}")),
        (d, None) => d.map(str::to_string),
    }
}

/// `expr` handed on to a constructor: moved, unless it's a direct value
/// (a scalar or enum), which copies.
fn pass_on(ty: &Ty, expr: &str) -> String {
    if ty.family() == Family::Direct {
        expr.to_string()
    } else {
        format!("std::move({expr})")
    }
}

// ── Enums ──

/// Append one module's C-style enums as `enum class {Name} : int32_t`. Rich
/// enums are value types, rendered with the records.
pub(crate) fn render_cpp_enums(w: &mut CodeWriter, module: &ModuleBinding) {
    for e in module.enums.iter().filter(|e| !e.is_rich()) {
        w.doc(
            &doc_text(e.doc.as_deref(), e.deprecated.as_deref()),
            DocCommentStyle::Javadoc,
        );
        w.block(format!("enum class {} : int32_t {{", e.name), "};", |w| {
            for v in &e.variants {
                w.doc(&v.doc, DocCommentStyle::Javadoc);
                w.line(format!("{} = {},", cpp_ident(&v.name), v.value));
            }
        });
        w.blank();
    }
}

// ── Value types: records and rich enums ──

/// A value type emitted as a plain C++ struct: a record or a rich enum. Both
/// cross the ABI in value buffers, may nest one another, and are ordered
/// together so a by-value member's type is complete before its holder.
pub(crate) enum ValueDef<'a> {
    /// A record: a plain struct with typed members.
    Record(&'a StructBinding),
    /// A rich enum: a `std::variant`-backed sum type.
    Rich(&'a EnumBinding),
}

impl ValueDef<'_> {
    /// The value type's C++ name.
    pub(crate) fn name(&self) -> &str {
        match self {
            ValueDef::Record(s) => &s.name,
            ValueDef::Rich(e) => &e.name,
        }
    }

    /// Names of other value types this one holds by value.
    fn deps(&self) -> Vec<String> {
        let mut deps = Vec::new();
        let fields: Vec<&Ty> = match self {
            ValueDef::Record(s) => s.fields.iter().map(|f| &f.ty).collect(),
            ValueDef::Rich(e) => e
                .variants
                .iter()
                .flat_map(|v| &v.fields)
                .map(|f| &f.ty)
                .collect(),
        };
        for ty in fields {
            collect_value_deps(ty, &mut deps);
        }
        deps
    }

    /// Append the type's definition.
    pub(crate) fn render(&self, w: &mut CodeWriter) {
        match self {
            ValueDef::Record(s) => render_cpp_record(w, s),
            ValueDef::Rich(e) => render_cpp_rich_enum(w, e),
        }
    }
}

/// Collect the names of value types reachable from `ty` through optionals,
/// lists, and maps. Interfaces aren't collected: every interface class is
/// complete before any value type.
fn collect_value_deps(ty: &Ty, deps: &mut Vec<String>) {
    match ty {
        Ty::Record(n) | Ty::RichEnum(n) => deps.push(n.clone()),
        Ty::Optional(inner) | Ty::List(inner) => collect_value_deps(inner, deps),
        Ty::Map(k, v) => {
            collect_value_deps(k, deps);
            collect_value_deps(v, deps);
        }
        _ => {}
    }
}

/// Every record and rich enum, ordered so anything a type holds is defined
/// before it (depth-first post-order, declaration order breaking ties).
pub(crate) fn value_types_in_order(modules: &[ModuleBinding]) -> Vec<ValueDef<'_>> {
    let mut defs: Vec<Option<ValueDef<'_>>> = modules
        .iter()
        .flat_map(|m| {
            let records = m.structs.iter().map(ValueDef::Record);
            let rich = m.enums.iter().filter(|e| e.is_rich()).map(ValueDef::Rich);
            records.chain(rich)
        })
        .map(Some)
        .collect();
    let deps: Vec<Vec<String>> = defs.iter().flatten().map(ValueDef::deps).collect();
    let index: HashMap<String, usize> = defs
        .iter()
        .flatten()
        .enumerate()
        .map(|(i, d)| (d.name().to_string(), i))
        .collect();

    fn visit(
        i: usize,
        deps: &[Vec<String>],
        index: &HashMap<String, usize>,
        state: &mut [u8],
        order: &mut Vec<usize>,
    ) {
        // 0 unvisited, 1 on the stack (a cycle stops here), 2 emitted.
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        for d in &deps[i] {
            if let Some(&j) = index.get(d) {
                visit(j, deps, index, state, order);
            }
        }
        state[i] = 2;
        order.push(i);
    }

    let mut state = vec![0u8; defs.len()];
    let mut order = Vec::with_capacity(defs.len());
    for i in 0..defs.len() {
        visit(i, &deps, &index, &mut state, &mut order);
    }
    order
        .into_iter()
        .map(|i| defs[i].take().expect("each type is emitted once"))
        .collect()
}

/// Render a record as a plain C++ value struct: typed members in declaration
/// (and wire) order. An interface-typed member is the RAII wrapper held by
/// value, so copying the record clones the reference and destroying it
/// releases one.
fn render_cpp_record(w: &mut CodeWriter, s: &StructBinding) {
    w.doc(
        &doc_text(s.doc.as_deref(), s.deprecated.as_deref()),
        DocCommentStyle::Javadoc,
    );
    w.block(format!("struct {} {{", s.name), "};", |w| {
        for f in &s.fields {
            w.doc(&f.doc, DocCommentStyle::Javadoc);
            w.line(format!("{} {};", cpp_type(&f.ty), cpp_ident(&f.name)));
        }
    });
    w.blank();
}

/// Render a rich enum as a `std::variant`-backed sum type: one payload struct
/// per variant, a `value` member holding the active payload, a nested `Tag`
/// enum mirroring the wire discriminants, and a `tag()` reader. Construct one
/// as `Shape{Shape::Circle{2.0}}`.
fn render_cpp_rich_enum(w: &mut CodeWriter, e: &EnumBinding) {
    let name = &e.name;
    w.doc(
        &doc_text(e.doc.as_deref(), e.deprecated.as_deref()),
        DocCommentStyle::Javadoc,
    );
    w.block(format!("struct {name} {{"), "};", |w| {
        w.line(format!(
            "/** Discriminant identifying the active variant of `{name}`. */"
        ));
        w.block("enum class Tag : int32_t {", "};", |w| {
            for v in &e.variants {
                w.line(format!("{} = {},", cpp_ident(&v.name), v.value));
            }
        });
        w.blank();
        for v in &e.variants {
            w.doc(&v.doc, DocCommentStyle::Javadoc);
            w.block(format!("struct {} {{", cpp_ident(&v.name)), "};", |w| {
                for f in &v.fields {
                    w.doc(&f.doc, DocCommentStyle::Javadoc);
                    w.line(format!("{} {};", cpp_type(&f.ty), cpp_ident(&f.name)));
                }
            });
            w.blank();
        }
        let alts: Vec<String> = e.variants.iter().map(|v| cpp_ident(&v.name)).collect();
        w.line("/** The active variant's payload. */");
        w.line(format!("std::variant<{}> value;", alts.join(", ")));
        w.blank();
        w.line("/** The tag of the active variant. */");
        w.block("Tag tag() const noexcept {", "}", |w| {
            let tags: Vec<String> = alts.iter().map(|v| format!("Tag::{v}")).collect();
            w.line(format!(
                "static constexpr Tag tags[] = {{{}}};",
                tags.join(", ")
            ));
            w.line("return tags[value.index()];");
        });
    });
    w.blank();
}

// ── Typed error domains ──

/// `detail::make_{path}_error`: builds the exception for a code, message,
/// and payload of the domain.
pub(crate) fn make_error_fn(eb: &ErrorBinding) -> String {
    format!("detail::make_{}_error", eb.owner_path)
}

/// `detail::check_{path}`: throws the domain's exception for a failed call.
pub(crate) fn check_fn(eb: &ErrorBinding) -> String {
    format!("detail::check_{}", eb.owner_path)
}

/// `detail::report_{path}_error`: reports a domain exception a callback
/// implementation threw through the vtable entry's `out_err`.
pub(crate) fn report_fn(eb: &ErrorBinding) -> String {
    format!("detail::report_{}_error", eb.owner_path)
}

/// Append one module's typed error domain: a domain exception derived from
/// `Error`, one subclass per declared code (with typed members for the
/// code's payload fields), and the `detail` helpers that map a failed call's
/// `out_err` to the typed exception. With `reports`, also the helper that
/// reports a thrown domain exception from a callback trampoline.
///
/// Domain codes are positive and the runtime owns every negative code, so
/// the mapping sends -5 to `Cancelled` and any other negative code to the
/// root `Error` before consulting the domain's codes; an undeclared positive
/// code falls back to the domain class itself.
pub(crate) fn render_domain_error(
    w: &mut CodeWriter,
    module: &ModuleBinding,
    eb: &ErrorBinding,
    prefix: &str,
    reports: bool,
) {
    let domain = &eb.type_name;
    w.line(format!(
        "/** The errors the `{}` module's throwing calls report; catch a code's subclass or this. */",
        module.dot_path
    ));
    w.line(format!("class {domain} : public Error {{"));
    w.line("public:");
    w.scope(|w| {
        w.line("/** Builds an error carrying a domain `code` and `message`. */");
        w.line(format!(
            "{domain}(int32_t code, const std::string& message) : Error(code, message) {{}}"
        ));
    });
    w.line("};");
    w.blank();

    for code in &eb.codes {
        let class = cpp_error_class(&code.name);
        let doc = code.doc.clone().unwrap_or_else(|| code.message.clone());
        w.doc(
            &Some(format!("{doc}\n\nCode {}.", code.value)),
            DocCommentStyle::Javadoc,
        );
        w.line(format!("class {class} : public {domain} {{"));
        w.line("public:");
        w.scope(|w| {
            for f in &code.fields {
                w.doc(&f.doc, DocCommentStyle::Javadoc);
                w.line(format!("{} {};", cpp_type(&f.ty), cpp_ident(&f.name)));
            }
            if !code.fields.is_empty() {
                w.blank();
            }
            let mut params = vec!["const std::string& message".to_string()];
            let mut inits = vec![format!("{domain}({}, message)", code.value)];
            for f in &code.fields {
                let name = cpp_ident(&f.name);
                params.push(format!("{} {name}", cpp_type(&f.ty)));
                inits.push(format!("{name}({})", pass_on(&f.ty, &name)));
            }
            w.line("/** Builds the error with the producer's `message` and the code's fields. */");
            let explicit = if code.fields.is_empty() {
                "explicit "
            } else {
                ""
            };
            w.line(format!(
                "{explicit}{class}({}) : {} {{}}",
                params.join(", "),
                inits.join(", ")
            ));
        });
        w.line("};");
        w.blank();
    }

    let path = &eb.owner_path;
    w.line("namespace detail {");
    w.blank();
    w.line(format!(
        "/** The exception for a failed `{domain}` call: its code's subclass, Cancelled, or the root Error. */"
    ));
    w.block(
        format!(
            "inline std::exception_ptr make_{path}_error(int32_t code, const std::string& message, const uint8_t* payload_ptr, size_t payload_len) {{"
        ),
        "}",
        |w| {
            w.line("if (code == -5) return std::make_exception_ptr(Cancelled(message));");
            w.line("if (code < 0) return std::make_exception_ptr(Error(code, message));");
            if eb.codes.iter().all(|c| c.fields.is_empty()) {
                w.line("(void)payload_ptr;");
                w.line("(void)payload_len;");
            }
            w.block("switch (code) {", "}", |w| {
                for code in &eb.codes {
                    let class = cpp_error_class(&code.name);
                    if code.fields.is_empty() {
                        w.line(format!(
                            "case {}: return std::make_exception_ptr({class}(message));",
                            code.value
                        ));
                        continue;
                    }
                    w.block(format!("case {}: {{", code.value), "}", |w| {
                        // Locals, because constructor arguments evaluate in
                        // an unspecified order and the fields are read in
                        // wire order.
                        w.line("BufferReader r(payload_ptr, payload_len);");
                        let mut args = vec!["message".to_string()];
                        for f in &code.fields {
                            let var = format!("f_{}", f.name);
                            w.line(format!("{} {var} = {};", cpp_type(&f.ty), read_expr(&f.ty, "r")));
                            args.push(pass_on(&f.ty, &var));
                        }
                        w.line("r.expect_end();");
                        w.line(format!(
                            "return std::make_exception_ptr({class}({}));",
                            args.join(", ")
                        ));
                    });
                }
                w.line(format!(
                    "default: return std::make_exception_ptr({domain}(code, message));"
                ));
            });
        },
    );
    w.blank();
    w.line(format!(
        "/** Throws the typed `{domain}` exception if `err` carries a nonzero code. */"
    ));
    w.block(
        format!("inline void check_{path}({prefix}_error& err) {{"),
        "}",
        |w| {
            w.line("if (err.code == 0) return;");
            // The error owns the payload, so the exception (which decodes
            // it) is built before error_clear releases it.
            w.line(format!(
                "std::exception_ptr ex = make_{path}_error(err.code, error_message(err), err.payload_ptr, err.payload_len);"
            ));
            w.line(format!("{prefix}_error_clear(&err);"));
            w.line("std::rethrow_exception(ex);");
        },
    );
    w.blank();
    if reports {
        render_report_fn(w, eb, prefix);
    }
    w.line("} // namespace detail");
    w.blank();
}

/// Append the helper a throwing callback method's trampoline uses to report a
/// `{domain}` exception: a declared code goes back with its fields encoded as
/// the payload; an exception whose code the domain doesn't declare (or a
/// code with fields thrown as the bare domain class, which has no fields to
/// send) is a callback failure, -4.
fn render_report_fn(w: &mut CodeWriter, eb: &ErrorBinding, prefix: &str) {
    let domain = &eb.type_name;
    w.line(format!(
        "/** Reports a `{domain}` a callback implementation threw through the vtable entry's `out_err`. */"
    ));
    w.block(
        format!(
            "inline void report_{}_error(const {domain}& e, {prefix}_error* out_err) {{",
            eb.owner_path
        ),
        "}",
        |w| {
            w.block("switch (e.code()) {", "}", |w| {
                for code in &eb.codes {
                    let class = cpp_error_class(&code.name);
                    if code.fields.is_empty() {
                        w.line(format!(
                            "case {}: {prefix}_error_set(out_err, {}, e.what()); return;",
                            code.value, code.value
                        ));
                        continue;
                    }
                    w.block(format!("case {}: {{", code.value), "}", |w| {
                        w.line(format!(
                            "const auto* typed = dynamic_cast<const {class}*>(&e);"
                        ));
                        w.line("if (typed == nullptr) break;");
                        w.line("BufferWriter payload;");
                        for f in &code.fields {
                            w.line(write_stmt(
                                &f.ty,
                                &format!("typed->{}", cpp_ident(&f.name)),
                                "payload",
                            ));
                        }
                        w.line(format!(
                            "{prefix}_error_set(out_err, {}, e.what());",
                            code.value
                        ));
                        w.line(format!(
                            "{prefix}_error_set_payload(out_err, payload.data(), payload.size());"
                        ));
                        w.line("return;");
                    });
                }
                w.line("default: break;");
            });
            w.line(format!("{prefix}_error_set(out_err, -4, e.what());"));
        },
    );
    w.blank();
}

// ── Interfaces ──

/// Append the reference-counting RAII skeleton of an interface class: the
/// `raw_type` alias of the C tag, the tagged constructor adopting one strong
/// reference, a destructor releasing it, copies that take a new reference
/// through `_clone`, moves that transfer the pointer, and the
/// `handle()`/`clone_handle()` readers.
fn render_raii_skeleton(w: &mut CodeWriter, i: &InterfaceBinding) {
    let name = &i.name;
    let tag = &i.c_tag;
    let clone = &i.clone_symbol;
    let destroy = &i.destroy_symbol;

    w.line("/** The C type this class wraps. */");
    w.line(format!("using raw_type = {tag};"));
    w.blank();
    w.line(format!(
        "/** Adopts one strong reference to a producer object: `{name}(adopt, raw)`. */"
    ));
    w.line(format!(
        "explicit {name}(adopt_t, {tag}* h) noexcept : raw_(h) {{}}"
    ));
    w.blank();
    w.line("/** Releases this wrapper's reference; the object is dropped with its last one. */");
    w.block(format!("~{name}() {{"), "}", |w| {
        w.line(format!("if (raw_ != nullptr) {destroy}(raw_);"));
    });
    w.blank();
    w.line("/** Copies share the object: the copy takes a new strong reference. */");
    w.line(format!(
        "{name}(const {name}& other) : raw_({clone}(other.raw_)) {{}}"
    ));
    w.blank();
    w.line("/** Takes a new reference to `other`'s object and releases the current one. */");
    w.block(
        format!("{name}& operator=(const {name}& other) {{"),
        "}",
        |w| {
            w.block("if (this != &other) {", "}", |w| {
                w.line(format!("{tag}* h = {clone}(other.raw_);"));
                w.line(format!("if (raw_ != nullptr) {destroy}(raw_);"));
                w.line("raw_ = h;");
            });
            w.line("return *this;");
        },
    );
    w.blank();
    w.line("/** Transfers `other`'s reference; `other` becomes empty. */");
    w.line(format!(
        "{name}({name}&& other) noexcept : raw_(other.raw_) {{ other.raw_ = nullptr; }}"
    ));
    w.blank();
    w.line("/** Releases the current reference and takes over `other`'s. */");
    w.block(
        format!("{name}& operator=({name}&& other) noexcept {{"),
        "}",
        |w| {
            w.block("if (this != &other) {", "}", |w| {
                w.line(format!("if (raw_ != nullptr) {destroy}(raw_);"));
                w.line("raw_ = other.raw_;");
                w.line("other.raw_ = nullptr;");
            });
            w.line("return *this;");
        },
    );
    w.blank();
    w.line("/** The wrapped pointer, borrowed (null after a move): this wrapper keeps its reference. */");
    w.line(format!(
        "const {tag}* handle() const noexcept {{ return raw_; }}"
    ));
    w.blank();
    w.line("/** A new strong reference to the object, which the caller owns. */");
    w.line(format!(
        "{tag}* clone_handle() const {{ return {clone}(raw_); }}"
    ));
    w.blank();
}

/// The C++ name and declaration kind of each interface member. The
/// synchronous constructor named `new` becomes the C++ constructor; every
/// other constructor becomes a static factory named after it.
fn member_kinds(i: &InterfaceBinding) -> Vec<(&FnBinding, String, FnKind<'_>)> {
    let class = i.name.as_str();
    let mut members = Vec::new();
    for c in &i.constructors {
        if c.name == "new" && matches!(c.shape, CallShape::Sync(_)) {
            members.push((c, class.to_string(), FnKind::Ctor { class }));
        } else {
            members.push((c, cpp_member_name(&c.name), FnKind::Static { class }));
        }
    }
    for m in &i.methods {
        members.push((m, cpp_member_name(&m.name), FnKind::Method { class }));
    }
    for s in &i.statics {
        members.push((s, cpp_member_name(&s.name), FnKind::Static { class }));
    }
    members
}

/// Append the forward declarations an interface needs before any class body:
/// the wrapper class and the range class of every iterator-returning member.
pub(crate) fn render_cpp_interface_forward_decls(w: &mut CodeWriter, i: &InterfaceBinding) {
    w.line(format!("class {};", i.name));
    for (f, _, kind) in member_kinds(i) {
        if matches!(f.shape, CallShape::Iterator(_)) {
            w.line(format!("class {};", iterator_class_name(f, kind)));
        }
    }
}

/// Append an interface's class definition: the RAII skeleton plus the
/// *declarations* of its members. The member bodies follow every value type
/// and codec ([`render_cpp_interface_members`]); the class itself comes
/// first so records can hold it by value.
pub(crate) fn render_cpp_interface_class(
    w: &mut CodeWriter,
    i: &InterfaceBinding,
    error: Option<&ErrorBinding>,
) {
    w.doc(
        &doc_text(i.doc.as_deref(), i.deprecated.as_deref()),
        DocCommentStyle::Javadoc,
    );
    w.line(format!("class {} {{", i.name));
    w.scope(|w| {
        w.line(format!("{}* raw_;", i.c_tag));
        w.blank();
    });
    w.line("public:");
    w.scope(|w| {
        render_raii_skeleton(w, i);
        for (f, cpp_name, kind) in member_kinds(i) {
            render_member_decl(w, f, &cpp_name, kind, error);
        }
    });
    w.line("};");
    w.blank();
}

/// Append the range classes of an interface's iterator-returning members.
pub(crate) fn render_cpp_interface_iterators(
    w: &mut CodeWriter,
    i: &InterfaceBinding,
    error: Option<&ErrorBinding>,
    prefix: &str,
) {
    for (f, cpp_name, kind) in member_kinds(i) {
        if let CallShape::Iterator(it) = &f.shape {
            render_iterator_range(w, f, it, &cpp_name, kind, error, prefix);
        }
    }
}

/// Append the out-of-line `inline` definitions of an interface's members.
pub(crate) fn render_cpp_interface_members(
    w: &mut CodeWriter,
    i: &InterfaceBinding,
    error: Option<&ErrorBinding>,
    prefix: &str,
) {
    for (f, cpp_name, kind) in member_kinds(i) {
        render_definition(w, f, &cpp_name, kind, error, prefix);
    }
}
