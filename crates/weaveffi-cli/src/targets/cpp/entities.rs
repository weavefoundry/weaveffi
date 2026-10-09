//! Declared-entity rendering: plain enums, value types (records and rich
//! enums), error domains, and interface wrapper classes, plus the dependency
//! ordering that keeps by-value members complete before use.

use std::collections::HashMap;

use crate::codegen::common::DocCommentStyle;
use crate::codegen::docs::Doc;
use crate::codegen::errors::ErrorTable;
use crate::codegen::CodeWriter;
use crate::lang;
use weaveffi_model::model::{
    CallShape, EnumBinding, FieldBinding, FnBinding, InterfaceBinding, ModuleBinding, StructBinding,
};
use weaveffi_model::ty::Ty;

use crate::targets::cpp::calls::{render_definition, render_member_decl, FnKind};
use crate::targets::cpp::types::{cpp_ident, cpp_member_name, cpp_type, defaultable, Ctx};

/// A type's doc comment, with a trailing `@deprecated` paragraph when it's
/// deprecated. Types carry the deprecation in their docs rather than as an
/// attribute, so the generated code that marshals them compiles
/// warning-free.
pub(crate) fn type_doc(
    ctx: &Ctx<'_>,
    doc: &Option<String>,
    deprecated: &Option<String>,
) -> Option<String> {
    let doc = Doc::new(doc, deprecated);
    let spell = |s: &str| ctx.spell(s);
    let note = doc.deprecation(spell).map(|m| format!("@deprecated {m}"));
    match (doc.text(spell), note) {
        (Some(d), Some(n)) => Some(format!("{d}\n\n{n}")),
        (d, n) => d.or(n),
    }
}

/// A field's or variant's doc comment, rewritten.
fn field_doc(ctx: &Ctx<'_>, doc: &Option<String>) -> Option<String> {
    Doc::new(doc, &None).text(|s| ctx.spell(s))
}

// ── Enums ──

/// Append one module's C-style enums as `enum class {Name} : int32_t`. Rich
/// enums are value types, rendered with the records.
pub(crate) fn render_cpp_enums(w: &mut CodeWriter, ctx: &Ctx<'_>, module: &ModuleBinding) {
    for e in module.enums.iter().filter(|e| !e.is_rich()) {
        w.doc(
            &type_doc(ctx, &e.doc, &e.deprecated),
            DocCommentStyle::Javadoc,
        );
        w.block(format!("enum class {} : int32_t {{", e.name), "};", |w| {
            for v in &e.variants {
                w.doc(&field_doc(ctx, &v.doc), DocCommentStyle::Javadoc);
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
    pub(crate) fn render(&self, w: &mut CodeWriter, ctx: &Ctx<'_>) {
        match self {
            ValueDef::Record(s) => render_cpp_record(w, ctx, s),
            ValueDef::Rich(e) => render_cpp_rich_enum(w, ctx, e),
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

/// One member declaration of a record or variant payload, with a default
/// member initializer (zero for scalars and enums, empty otherwise) when its
/// type can be value-initialized. A member holding an interface wrapper, or
/// a record that holds one, has none, and an initializer must provide it.
/// Every other member has a default, so a braced initializer may stop after
/// the members it needs without `-Wmissing-field-initializers` warnings.
fn member_decl(ctx: &Ctx<'_>, f: &FieldBinding) -> String {
    let init = if defaultable(ctx.model, &f.ty) {
        "{}"
    } else {
        ""
    };
    format!("{} {}{init};", cpp_type(&f.ty), cpp_ident(&f.name))
}

/// Whether `ty` is a scalar, `bool`, or C-style enum: trivially copied, and
/// zero when value-initialized.
fn is_scalar(ty: &Ty) -> bool {
    match ty {
        Ty::Prim(p) => p.is_scalar(),
        Ty::Enum(_) => true,
        _ => false,
    }
}

/// Append the hidden-friend `operator==` and `operator!=` of a value
/// struct: memberwise, in declaration order. Floating-point members compare
/// as IEEE values, and interface members by identity.
fn render_equality(w: &mut CodeWriter, name: &str, fields: &[FieldBinding]) {
    w.line("/** Memberwise equality; interface members compare by identity. */");
    if fields.is_empty() {
        w.line(format!(
            "friend bool operator==(const {name}&, const {name}&) noexcept {{ return true; }}"
        ));
    } else {
        let cmp: Vec<String> = fields
            .iter()
            .map(|f| {
                let n = cpp_ident(&f.name);
                format!("a.{n} == b.{n}")
            })
            .collect();
        let head = format!("friend bool operator==(const {name}& a, const {name}& b) {{");
        if cmp.len() <= 2 {
            w.line(format!("{head} return {}; }}", cmp.join(" && ")));
        } else {
            w.block(head, "}", |w| {
                w.line(format!("return {}", cmp[0]));
                w.scope(|w| {
                    for (i, c) in cmp.iter().enumerate().skip(1) {
                        let end = if i + 1 == cmp.len() { ";" } else { "" };
                        w.line(format!("&& {c}{end}"));
                    }
                });
            });
        }
    }
    w.line(format!(
        "friend bool operator!=(const {name}& a, const {name}& b) {{ return !(a == b); }}"
    ));
}

/// Render a record as a plain C++ aggregate: typed members in declaration
/// (and wire) order with default member initializers, and memberwise
/// equality. An interface-typed member is the RAII wrapper held by value,
/// so copying the record shares the object and destroying it releases one
/// reference.
fn render_cpp_record(w: &mut CodeWriter, ctx: &Ctx<'_>, s: &StructBinding) {
    w.doc(
        &type_doc(ctx, &s.doc, &s.deprecated),
        DocCommentStyle::Javadoc,
    );
    w.block(format!("struct {} {{", s.name), "};", |w| {
        for f in &s.fields {
            w.doc(&field_doc(ctx, &f.doc), DocCommentStyle::Javadoc);
            w.line(member_decl(ctx, f));
        }
        if !s.fields.is_empty() {
            w.blank();
        }
        render_equality(w, &s.name, &s.fields);
    });
    w.blank();
}

/// Render a rich enum as a `std::variant`-backed sum type: one payload
/// struct per variant, a `value` member holding the active payload, a nested
/// `Tag` enum mirroring the wire discriminants, a `tag()` reader, and
/// equality. Construct one as `Shape{Shape::Circle{2.0}}`.
fn render_cpp_rich_enum(w: &mut CodeWriter, ctx: &Ctx<'_>, e: &EnumBinding) {
    let name = &e.name;
    w.doc(
        &type_doc(ctx, &e.doc, &e.deprecated),
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
            let vn = cpp_ident(&v.name);
            w.doc(&field_doc(ctx, &v.doc), DocCommentStyle::Javadoc);
            w.block(format!("struct {vn} {{"), "};", |w| {
                for f in &v.fields {
                    w.doc(&field_doc(ctx, &f.doc), DocCommentStyle::Javadoc);
                    w.line(member_decl(ctx, f));
                }
                if !v.fields.is_empty() {
                    w.blank();
                }
                render_equality(w, &vn, &v.fields);
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
        w.blank();
        w.line("/** Equal when the same variant is active with equal payloads. */");
        w.line(format!(
            "friend bool operator==(const {name}& a, const {name}& b) {{ return a.value == b.value; }}"
        ));
        w.line(format!(
            "friend bool operator!=(const {name}& a, const {name}& b) {{ return !(a == b); }}"
        ));
    });
    w.blank();
}

// ── Error domains ──

/// The member name of an error code's field: [`cpp_ident`], with a trailing
/// underscore when it would hide a member every exception has (`code()`,
/// `what()`).
pub(crate) fn error_field(name: &str) -> String {
    lang::escape_member(&cpp_ident(name), &["code", "what"])
}

/// Append one error domain: its class derived from `Error`, one subclass
/// per declared code (with typed members for the code's payload fields),
/// and its `detail::Errors` policy.
///
/// The policy's `raise` throws `Cancelled` for -5 and the root `Error` for
/// any other negative (runtime) code, the code's class for a declared code,
/// and the domain class itself for a positive code these bindings don't
/// know (domains are open: a newer producer may add codes). Its `report`
/// turns a domain exception a callback implementation threw into the
/// producer's error slot, payload included; any other exception is a plain
/// failure (-1).
pub(crate) fn render_domain_error(w: &mut CodeWriter, ctx: &Ctx<'_>, table: &ErrorTable<'_>) {
    let domain = &table.type_name;
    let prefix = ctx.prefix;
    w.line(format!(
        "/** The `{}` error domain of module `{}`: catch a code's class or this. */",
        table.domain.name, table.module.dot_path
    ));
    w.line(format!("class {domain} : public Error {{"));
    w.line("public:");
    w.scope(|w| {
        w.line("/** Builds an error carrying one of the domain's `code`s and its `message`. */");
        w.line(format!(
            "{domain}(int32_t code, const std::string& message) : Error(code, message) {{}}"
        ));
    });
    w.line("};");
    w.blank();

    for row in &table.codes {
        let code = row.code;
        let class = &row.type_name;
        let doc = Doc::new(&code.doc, &None)
            .text(|s| ctx.spell(s))
            .unwrap_or_else(|| code.message.clone());
        w.doc(
            &Some(format!("{doc}\n\nCode {}.", code.value)),
            DocCommentStyle::Javadoc,
        );
        w.line(format!("class {class} : public {domain} {{"));
        w.line("public:");
        w.scope(|w| {
            for f in &code.fields {
                w.doc(&field_doc(ctx, &f.doc), DocCommentStyle::Javadoc);
                w.line(format!("{} {};", cpp_type(&f.ty), error_field(&f.name)));
            }
            if !code.fields.is_empty() {
                w.blank();
            }
            // The message parameter steps aside for a field named `message`.
            let mut message = "message".to_string();
            while code.fields.iter().any(|f| error_field(&f.name) == message) {
                message.push('_');
            }
            let mut params = vec![format!("const std::string& {message}")];
            let mut inits = vec![format!("{domain}({}, {message})", code.value)];
            for f in &code.fields {
                let name = error_field(&f.name);
                params.push(format!("{} {name}", cpp_type(&f.ty)));
                if is_scalar(&f.ty) {
                    inits.push(format!("{name}({name})"));
                } else {
                    inits.push(format!("{name}(std::move({name}))"));
                }
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

    w.line("namespace detail {");
    w.blank();
    w.line(format!(
        "/** Raises and reports `{domain}` and its codes' classes. */"
    ));
    w.line("template <>");
    w.block(format!("struct Errors<{domain}> {{"), "};", |w| {
        w.block(
            format!("[[noreturn]] static void raise(const {prefix}_error& err) {{"),
            "}",
            |w| {
                w.line("if (err.code < 0) raise_runtime<Error>(err);");
                w.block("switch (err.code) {", "}", |w| {
                    for row in &table.codes {
                        let code = row.code;
                        let class = &row.type_name;
                        if code.fields.is_empty() {
                            w.line(format!("case {}: throw {class}(message(err));", code.value));
                        } else {
                            let fields: Vec<String> =
                                code.fields.iter().map(|f| cpp_type(&f.ty)).collect();
                            w.line(format!(
                                "case {}: raise_with_fields<{class}, {}>(err);",
                                code.value,
                                fields.join(", ")
                            ));
                        }
                    }
                    w.line(format!("default: throw {domain}(err.code, message(err));"));
                });
            },
        );
        w.blank();
        w.block(
            format!("static void report({prefix}_error* out_err) noexcept {{"),
            "}",
            |w| {
                w.line("try {");
                w.scope(|w| {
                    w.line("throw;");
                });
                for row in table.codes.iter().filter(|r| !r.code.fields.is_empty()) {
                    let fields: Vec<String> = row
                        .code
                        .fields
                        .iter()
                        .map(|f| format!("e.{}", error_field(&f.name)))
                        .collect();
                    w.line(format!("}} catch (const {}& e) {{", row.type_name));
                    w.scope(|w| {
                        w.line(format!(
                            "report_with_fields(out_err, e, {});",
                            fields.join(", ")
                        ));
                    });
                }
                w.line(format!("}} catch (const {domain}& e) {{"));
                w.scope(|w| {
                    w.line("set_error(out_err, e.code(), e.what());");
                });
                w.line("} catch (...) {");
                w.scope(|w| {
                    w.line("report_current(out_err, -1);");
                });
                w.line("}");
            },
        );
    });
    w.blank();
    w.line("} // namespace detail");
    w.blank();
}

// ── Interfaces ──

/// The C++ name and declaration kind of each interface member. The
/// synchronous constructor named `new` becomes the C++ constructor; every
/// other constructor becomes a static factory named after it.
fn member_kinds(i: &InterfaceBinding) -> Vec<(&FnBinding, String, FnKind<'_>)> {
    let class = i.name.as_str();
    let mut members = Vec::new();
    for c in &i.constructors {
        if c.name == "new" && matches!(c.shape, CallShape::Sync) && c.iterator().is_none() {
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

/// Append an interface's class: the reference-counting core (a
/// `detail::Handle` with the class's `traits`, so the class itself follows
/// the Rule of Zero), the `adopt` constructor, the `handle()` and
/// `clone_handle()` readers, identity equality, and the *declarations* of
/// its members. The member bodies follow every value type and codec
/// ([`render_cpp_interface_members`]); the class comes first so records can
/// hold it by value.
pub(crate) fn render_cpp_interface_class(w: &mut CodeWriter, ctx: &Ctx<'_>, i: &InterfaceBinding) {
    let name = &i.name;
    let tag = &i.c_tag;
    w.doc(
        &type_doc(ctx, &i.doc, &i.deprecated),
        DocCommentStyle::Javadoc,
    );
    w.line(format!("class {name} {{"));
    w.line("public:");
    w.scope(|w| {
        w.line("/** The C type this class wraps. */");
        w.line(format!("using raw_type = {tag};"));
        w.blank();
        w.line(format!(
            "/** Adopts one strong reference to a producer object: `{name}(adopt, raw)`. */"
        ));
        w.line(format!(
            "explicit {name}(adopt_t, raw_type* raw) noexcept : raw_(raw) {{}}"
        ));
        w.blank();
        for (f, cpp_name, kind) in member_kinds(i) {
            render_member_decl(w, ctx, f, &cpp_name, kind);
        }
        w.line("/** The wrapped pointer, borrowed (null after a move): this wrapper keeps its reference. */");
        w.line("const raw_type* handle() const noexcept { return raw_.get(); }");
        w.blank();
        w.line("/** A new strong reference to the object, which the caller owns. */");
        w.line("raw_type* clone_handle() const noexcept { return raw_.clone(); }");
        w.blank();
        w.line("/** Whether both wrap the same producer object. */");
        w.line(format!(
            "friend bool operator==(const {name}& a, const {name}& b) noexcept {{ return a.raw_.get() == b.raw_.get(); }}"
        ));
        w.line(format!(
            "friend bool operator!=(const {name}& a, const {name}& b) noexcept {{ return !(a == b); }}"
        ));
    });
    w.blank();
    w.line("private:");
    w.scope(|w| {
        w.block("struct traits {", "};", |w| {
            w.line(format!("using raw_type = {tag};"));
            w.line(format!(
                "static raw_type* clone(const raw_type* raw) noexcept {{ return {}(raw); }}",
                i.clone_symbol
            ));
            w.line(format!(
                "static void destroy(raw_type* raw) noexcept {{ {}(raw); }}",
                i.destroy_symbol
            ));
        });
        w.blank();
        w.line("detail::Handle<traits> raw_;");
    });
    w.line("};");
    w.blank();
}

/// Append the out-of-line `inline` definitions of an interface's members.
pub(crate) fn render_cpp_interface_members(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    i: &InterfaceBinding,
) {
    for (f, cpp_name, kind) in member_kinds(i) {
        render_definition(w, ctx, f, &cpp_name, kind);
    }
}
