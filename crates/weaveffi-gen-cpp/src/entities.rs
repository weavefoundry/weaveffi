//! Declared-entity rendering: plain enums, value types (records and rich
//! enums), typed error domains, and interface RAII classes, plus the
//! dependency ordering that keeps by-value members complete before use.

use std::collections::HashMap;

use weaveffi_core::codegen::common::DocCommentStyle;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    CallShape, EnumBinding, ErrorBinding, InterfaceBinding, ModuleBinding, StructBinding,
};
use weaveffi_core::utils::local_type_name;
use weaveffi_ir::ir::TypeRef;

use crate::calls::{render_cpp_callable, FnKind};
use crate::codec::emit_read_decl;
use crate::types::{cpp_error_class, cpp_fn_name, cpp_ident, cpp_type};

// ── Namespace: enums ──

/// Emit one module's plain C-style enums as `enum class {Name} : int32_t`.
/// Rich (algebraic) enums are value types, emitted as variant structs
/// alongside records; this renderer skips them.
pub(crate) fn render_cpp_enums(out: &mut String, module: &ModuleBinding) {
    let mut w = CodeWriter::four_space();
    for e in &module.enums {
        // Rich (algebraic) enums are value types, emitted as variant structs
        // alongside records; only plain C-style enums map to `enum class`.
        if e.is_rich() {
            continue;
        }
        w.doc(&e.doc, DocCommentStyle::Javadoc);
        w.block(format!("enum class {} : int32_t {{", e.name), "};", |w| {
            for (i, v) in e.variants.iter().enumerate() {
                w.doc(&v.doc, DocCommentStyle::Javadoc);
                let comma = if i + 1 < e.variants.len() { "," } else { "" };
                w.line(format!("{} = {}{}", v.name, v.value, comma));
            }
        });
        w.blank();
    }
    out.push_str(&w.finish());
}

// ── Value types: records and rich enums ──

/// A value type emitted as a plain C++ struct: a record or a rich (algebraic)
/// enum. Both cross the ABI serialized in value buffers, may nest one
/// another, and are ordered together so a by-value member's type is complete
/// before its holder.
pub(crate) enum ValueDef<'a> {
    /// A record: a plain struct with typed members.
    Record(&'a StructBinding),
    /// A rich enum: a `std::variant`-backed sum type.
    Rich(&'a EnumBinding),
}

impl ValueDef<'_> {
    /// The value type's local C++ name.
    pub(crate) fn name(&self) -> &str {
        match self {
            ValueDef::Record(s) => &s.name,
            ValueDef::Rich(e) => &e.name,
        }
    }

    /// Local names of other value types this one holds by value.
    pub(crate) fn deps(&self) -> Vec<String> {
        let mut deps = Vec::new();
        match self {
            ValueDef::Record(s) => {
                for f in &s.fields {
                    collect_value_deps(&f.ty, &mut deps);
                }
            }
            ValueDef::Rich(e) => {
                for v in &e.variants {
                    for f in &v.fields {
                        collect_value_deps(&f.ty, &mut deps);
                    }
                }
            }
        }
        deps
    }
}

/// Collect the local names of value types (records and rich enums) reachable
/// from `ty`, recursing through optional/list/map wrappers.
fn collect_value_deps(ty: &TypeRef, deps: &mut Vec<String>) {
    match ty {
        TypeRef::Record(n) | TypeRef::RichEnum(n) => deps.push(local_type_name(n).to_string()),
        TypeRef::Optional(inner) | TypeRef::List(inner) => collect_value_deps(inner, deps),
        TypeRef::Map(k, v) => {
            collect_value_deps(k, deps);
            collect_value_deps(v, deps);
        }
        _ => {}
    }
}

/// Local names of other interfaces referenced by an interface's member
/// signatures (returned or accepted by value, so their classes must be
/// complete first).
pub(crate) fn interface_deps(i: &InterfaceBinding) -> Vec<String> {
    fn collect(ty: &TypeRef, deps: &mut Vec<String>) {
        match ty {
            TypeRef::Interface(n) => deps.push(local_type_name(n).to_string()),
            TypeRef::Optional(inner) | TypeRef::Iterator(inner) => collect(inner, deps),
            _ => {}
        }
    }
    let mut deps = Vec::new();
    for f in i
        .constructors
        .iter()
        .chain(i.methods.iter())
        .chain(i.statics.iter())
    {
        for p in &f.params {
            collect(&p.ty, &mut deps);
        }
        if let Some(ret) = &f.ret {
            collect(ret, &mut deps);
        }
    }
    deps
}

/// Order entries so that anything an entry depends on is emitted before it.
/// Pure DFS post-order; original walk order is the stable tiebreaker, and
/// the first definition wins when two modules share a local name (the
/// flattened C++ type namespace can't hold duplicates anyway).
pub(crate) fn topo_order(names: &[String], deps: &[Vec<String>]) -> Vec<usize> {
    fn visit(
        i: usize,
        deps: &[Vec<String>],
        name_to_idx: &HashMap<&str, usize>,
        state: &mut [u8],
        order: &mut Vec<usize>,
    ) {
        // 0 = unvisited, 1 = on stack (skip to break any cycle), 2 = emitted.
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        for d in &deps[i] {
            if let Some(&j) = name_to_idx.get(d.as_str()) {
                if j != i {
                    visit(j, deps, name_to_idx, state, order);
                }
            }
        }
        state[i] = 2;
        order.push(i);
    }

    let mut name_to_idx: HashMap<&str, usize> = HashMap::new();
    for (i, n) in names.iter().enumerate() {
        name_to_idx.entry(n.as_str()).or_insert(i);
    }
    let mut state = vec![0u8; names.len()];
    let mut order = Vec::with_capacity(names.len());
    for i in 0..names.len() {
        visit(i, deps, &name_to_idx, &mut state, &mut order);
    }
    order
}

/// Render a record as a plain C++ value struct: typed members in declaration
/// (and wire) order, no handles, no destructors, no builders.
pub(crate) fn render_cpp_record(out: &mut String, s: &StructBinding, module: &str, prefix: &str) {
    let mut w = CodeWriter::four_space();
    w.doc(&s.doc, DocCommentStyle::Javadoc);
    w.line(format!("struct {} {{", s.name));
    w.scope(|w| {
        for f in &s.fields {
            w.doc(&f.doc, DocCommentStyle::Javadoc);
            w.line(format!(
                "{} {};",
                cpp_type(&f.ty, module, prefix),
                cpp_ident(&f.name)
            ));
        }
    });
    w.line("};");
    w.blank();
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as a `std::variant`-backed sum type: one
/// payload struct per variant, a `value` member holding the active payload,
/// a nested `Tag` enum mirroring the wire discriminants, and a `tag()` reader.
/// Construct one as `Shape{Shape::Circle{2.0}}`.
pub(crate) fn render_cpp_rich_enum(out: &mut String, e: &EnumBinding, module: &str, prefix: &str) {
    let name = &e.name;
    let mut w = CodeWriter::four_space();
    w.doc(&e.doc, DocCommentStyle::Javadoc);
    w.line(format!("struct {name} {{"));
    w.scope(|w| {
        w.line(format!(
            "/** Discriminant identifying the active variant of `{name}`. */"
        ));
        w.block("enum class Tag : int32_t {", "};", |w| {
            for (i, v) in e.variants.iter().enumerate() {
                let comma = if i + 1 < e.variants.len() { "," } else { "" };
                w.line(format!("{} = {}{}", cpp_ident(&v.name), v.value, comma));
            }
        });
        w.blank();

        for v in &e.variants {
            w.doc(&v.doc, DocCommentStyle::Javadoc);
            w.line(format!("struct {} {{", cpp_ident(&v.name)));
            w.scope(|w| {
                for f in &v.fields {
                    w.doc(&f.doc, DocCommentStyle::Javadoc);
                    w.line(format!(
                        "{} {};",
                        cpp_type(&f.ty, module, prefix),
                        cpp_ident(&f.name)
                    ));
                }
            });
            w.line("};");
            w.blank();
        }

        let alts: Vec<String> = e.variants.iter().map(|v| cpp_ident(&v.name)).collect();
        w.line("/** The active variant's payload. */");
        w.line(format!("std::variant<{}> value;", alts.join(", ")));
        w.blank();
        w.line("/** The tag of the active variant. */");
        w.line("Tag tag() const {");
        w.scope(|w| {
            w.line("static constexpr Tag tags[] = {");
            w.scope(|w| {
                for (i, v) in e.variants.iter().enumerate() {
                    let comma = if i + 1 < e.variants.len() { "," } else { "" };
                    w.line(format!("Tag::{}{}", cpp_ident(&v.name), comma));
                }
            });
            w.line("};");
            w.line("return tags[value.index()];");
        });
        w.line("}");
    });
    w.line("};");
    w.blank();
    out.push_str(&w.finish());
}

// ── Typed error domains ──

/// Emit one module's typed error domain: a domain exception derived from
/// `WeaveFFIError`, one subclass per declared code (with typed members for
/// any payload fields the code declares), and the per-domain
/// `detail::make_{path}_error`/`detail::check_{path}` helpers that throwing
/// wrappers use to map a nonzero `out_err` to the typed exception, decoding
/// the payload buffer along the way.
///
/// Domain codes are validated positive-only, and the runtime reserves every
/// negative code (generic error, producer panic, marshalling failure), so
/// the mapping helper routes negative codes to the generic `WeaveFFIError`
/// before consulting the domain's cases; unknown positive codes fall back to
/// the domain exception itself.
pub(crate) fn render_domain_error(out: &mut String, eb: &ErrorBinding, prefix: &str) {
    let domain = &eb.type_name;
    let path = &eb.owner_path;

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "/** Typed errors reported by the `{}` module's throwing functions. */",
        eb.owner_path
    ));
    w.line(format!("class {domain} : public WeaveFFIError {{"));
    w.line("public:");
    w.scope(|w| {
        w.line(format!(
            "{domain}(int32_t code, const std::string& msg) : WeaveFFIError(code, msg) {{}}"
        ));
    });
    w.line("};");
    w.blank();

    for code in &eb.codes {
        let class = cpp_error_class(&code.name);
        let doc = code.doc.clone().unwrap_or_else(|| code.message.clone());
        w.doc(&Some(doc), DocCommentStyle::Javadoc);
        w.line(format!("class {class} : public {domain} {{"));
        w.line("public:");
        w.scope(|w| {
            for fld in &code.fields {
                w.doc(&fld.doc, DocCommentStyle::Javadoc);
                w.line(format!(
                    "{} {};",
                    cpp_type(&fld.ty, path, prefix),
                    cpp_ident(&fld.name)
                ));
            }
            if code.fields.is_empty() {
                w.line(format!(
                    "{class}(const std::string& msg) : {domain}({}, msg) {{}}",
                    code.value
                ));
            } else {
                w.blank();
                let mut params = vec!["const std::string& msg".to_string()];
                let mut inits = vec![format!("{domain}({}, msg)", code.value)];
                for fld in &code.fields {
                    let name = cpp_ident(&fld.name);
                    params.push(format!("{} {name}", cpp_type(&fld.ty, path, prefix)));
                    inits.push(format!("{name}(std::move({name}))"));
                }
                w.line(format!(
                    "{class}({}) : {} {{}}",
                    params.join(", "),
                    inits.join(", ")
                ));
            }
        });
        w.line("};");
        w.blank();
    }

    w.line("namespace detail {");
    w.blank();
    w.line(format!(
        "/** Map a `{path}` error code and payload to its typed exception (WeaveFFIError for runtime codes, {domain} for unknown codes). */"
    ));
    w.line(format!(
        "inline std::exception_ptr make_{path}_error(int32_t code, const std::string& msg, const uint8_t* payload_ptr, size_t payload_len) {{"
    ));
    w.scope(|w| {
        // The negative range is reserved for the runtime (-1 generic error,
        // -2 producer panic, -3 marshalling failure); those never map to a
        // typed domain error, on the throwing path or anywhere else.
        w.line("if (code < 0) return std::make_exception_ptr(WeaveFFIError(code, msg));");
        w.line("switch (code) {");
        for code in &eb.codes {
            let class = cpp_error_class(&code.name);
            if code.fields.is_empty() {
                w.line(format!(
                    "case {}: return std::make_exception_ptr({class}(msg));",
                    code.value
                ));
            } else {
                w.line(format!("case {}: {{", code.value));
                w.scope(|w| {
                    w.line("BufferReader payload_r(payload_ptr, payload_len);");
                    let mut args = vec!["msg".to_string()];
                    for fld in &code.fields {
                        let var = format!("f_{}", fld.name);
                        emit_read_decl(w, &fld.ty, &var, "payload_r", path, prefix);
                        args.push(format!("std::move({var})"));
                    }
                    w.line("payload_r.expect_end();");
                    w.line(format!(
                        "return std::make_exception_ptr({class}({}));",
                        args.join(", ")
                    ));
                });
                w.line("}");
            }
        }
        w.line(format!(
            "default: return std::make_exception_ptr({domain}(code, msg));"
        ));
        w.line("}");
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "/** Throw the typed `{path}` exception if `err` carries a nonzero code. */"
    ));
    w.line(format!("inline void check_{path}({prefix}_error& err) {{"));
    w.scope(|w| {
        w.line("if (err.code == 0) return;");
        w.line("std::string msg(err.message ? err.message : \"unknown error\");");
        // The payload buffer is owned by the error and released by
        // error_clear, so the exception (which decodes it) is built first.
        w.line(format!(
            "std::exception_ptr ex = make_{path}_error(err.code, msg, err.payload_ptr, err.payload_len);"
        ));
        w.line(format!("{prefix}_error_clear(&err);"));
        w.line("std::rethrow_exception(ex);");
    });
    w.line("}");
    w.blank();
    w.line("} // namespace detail");
    w.blank();
    out.push_str(&w.finish());
}

// ── Namespace: interfaces ──

/// Emit the shared move-only RAII skeleton an interface class uses: adopted
/// `void*` handle, destructor calling `destroy_symbol`, deleted copy, move
/// constructor and move assignment, and the raw `handle()` reader.
fn emit_raii_skeleton(w: &mut CodeWriter, name: &str, c_tag: &str, destroy_symbol: &str) {
    w.line(format!("explicit {name}(void* h) : handle_(h) {{}}"));
    w.blank();

    w.line(format!("~{name}() {{"));
    w.scope(|w| {
        w.line(format!(
            "if (handle_) {destroy_symbol}(static_cast<{c_tag}*>(handle_));"
        ));
    });
    w.line("}");
    w.blank();

    w.line(format!("{name}(const {name}&) = delete;"));
    w.line(format!("{name}& operator=(const {name}&) = delete;"));
    w.blank();

    w.line(format!(
        "{name}({name}&& other) noexcept : handle_(other.handle_) {{"
    ));
    w.scope(|w| {
        w.line("other.handle_ = nullptr;");
    });
    w.line("}");
    w.blank();

    w.line(format!("{name}& operator=({name}&& other) noexcept {{"));
    w.scope(|w| {
        w.line("if (this != &other) {");
        w.scope(|w| {
            w.line(format!(
                "if (handle_) {destroy_symbol}(static_cast<{c_tag}*>(handle_));"
            ));
            w.line("handle_ = other.handle_;");
            w.line("other.handle_ = nullptr;");
        });
        w.line("}");
        w.line("return *this;");
    });
    w.line("}");
    w.blank();

    w.line("void* handle() const { return handle_; }");
    w.blank();
}

/// Render an interface as a move-only RAII class. The constructor named `new`
/// becomes the canonical C++ constructor (adopting the handle the C
/// constructor returns); every other constructor becomes a static factory.
/// Methods pass the wrapped handle as the leading C argument and are declared
/// `const` (the ABI receiver is a const pointer); statics are static member
/// functions. Sync, async, and iterator member shapes reuse the free-function
/// marshalling paths.
pub(crate) fn render_cpp_interface(
    out: &mut String,
    i: &InterfaceBinding,
    module: &ModuleBinding,
    prefix: &str,
) {
    let name = &i.name;

    let mut w = CodeWriter::four_space();
    w.doc(&i.doc, DocCommentStyle::Javadoc);
    w.line(format!("class {name} {{"));
    w.scope(|w| {
        w.line("void* handle_;");
        w.blank();
    });
    w.line("public:");
    w.scope(|w| {
        emit_raii_skeleton(w, name, &i.c_tag, &i.destroy_symbol);

        let mut members = String::new();
        for c in &i.constructors {
            if c.name == "new" && matches!(c.shape, CallShape::Sync(_)) {
                render_cpp_callable(&mut members, c, name, FnKind::Ctor, module, prefix);
            } else {
                render_cpp_callable(
                    &mut members,
                    c,
                    &cpp_fn_name(&c.name),
                    FnKind::Static,
                    module,
                    prefix,
                );
            }
        }
        for m in &i.methods {
            render_cpp_callable(
                &mut members,
                m,
                &cpp_fn_name(&m.name),
                FnKind::Method { c_tag: &i.c_tag },
                module,
                prefix,
            );
        }
        for s in &i.statics {
            render_cpp_callable(
                &mut members,
                s,
                &cpp_fn_name(&s.name),
                FnKind::Static,
                module,
                prefix,
            );
        }
        w.raw(members);
    });
    w.line("};");
    w.blank();
    out.push_str(&w.finish());
}
