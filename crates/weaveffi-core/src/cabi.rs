//! Shared rendering of the **C ABI declarations** from a
//! [`BindingModel`](crate::model::BindingModel).
//!
//! Both the C generator (which emits the canonical `{prefix}.h`) and the C++
//! generator (whose idiomatic wrapper opens an `extern "C"` block re-declaring
//! the same symbols) render their C declarations through this module. Before it
//! existed the two re-derived the ABI independently and drifted — most visibly,
//! the C++ `extern "C"` block lowered `iter<T>` as a list and omitted callbacks
//! and listeners entirely. Routing both through one model-driven renderer makes
//! that class of drift impossible.

use std::fmt::Write;

use crate::abi::AbiParam;
use crate::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use crate::model::{AbiFn, CallShape, EnumBinding, ModuleBinding, StructBinding};

/// Emit a `/** ... */` doc comment at `indent`.
pub fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Javadoc);
}

/// Join lowered ABI slots into a `"<c-type> <name>, ..."` declaration string.
pub fn params_str(params: &[AbiParam], prefix: &str) -> String {
    params
        .iter()
        .map(|p| format!("{} {}", p.ty.render_c(prefix), p.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render a full `{ret} {symbol}({params});` declaration for a lowered symbol.
pub fn fn_decl(out: &mut String, f: &AbiFn, prefix: &str) {
    let _ = writeln!(
        out,
        "{} {}({});",
        f.ret.render_c(prefix),
        f.symbol,
        params_str(&f.params, prefix)
    );
}

/// Render the runtime typedefs and helper prototypes (`handle_t`, `error`,
/// `free_*`, `cancel_token`) that every WeaveFFI C surface depends on.
pub fn render_runtime_decls(out: &mut String, prefix: &str) {
    let _ = write!(
        out,
        "typedef uint64_t {prefix}_handle_t;\n\n\
         typedef struct {prefix}_error {{ int32_t code; const char* message; }} {prefix}_error;\n\n\
         void {prefix}_error_clear({prefix}_error* err);\n\
         void {prefix}_free_string(const char* ptr);\n\
         void {prefix}_free_bytes(uint8_t* ptr, size_t len);\n\n\
         typedef struct {prefix}_cancel_token {prefix}_cancel_token;\n\
         {prefix}_cancel_token* {prefix}_cancel_token_create(void);\n\
         void {prefix}_cancel_token_cancel({prefix}_cancel_token* token);\n\
         bool {prefix}_cancel_token_is_cancelled(const {prefix}_cancel_token* token);\n\
         void {prefix}_cancel_token_destroy({prefix}_cancel_token* token);\n\n",
    );
}

/// Render an enum's discriminant constants as a C `typedef enum` named
/// `type_name`. Multi-line when any variant is documented.
fn render_enum_constants(out: &mut String, e: &EnumBinding, type_name: &str) {
    emit_doc(out, &e.doc, "");
    if e.variants.iter().any(|v| v.doc.is_some()) {
        out.push_str("typedef enum {\n");
        for (i, v) in e.variants.iter().enumerate() {
            emit_doc(out, &v.doc, "    ");
            let comma = if i + 1 == e.variants.len() { "" } else { "," };
            let _ = writeln!(out, "    {} = {}{comma}", v.c_const, v.value);
        }
        let _ = writeln!(out, "}} {type_name};");
    } else {
        let variants: Vec<String> = e
            .variants
            .iter()
            .map(|v| format!("{} = {}", v.c_const, v.value))
            .collect();
        let _ = writeln!(
            out,
            "typedef enum {{ {} }} {type_name};",
            variants.join(", ")
        );
    }
}

/// Render a C-style enum typedef. Multi-line when any variant is documented.
pub fn render_enum_decl(out: &mut String, e: &EnumBinding) {
    render_enum_constants(out, e, &e.c_tag);
}

/// Render the *discriminant* enum of a rich (algebraic) enum, named
/// `{c_tag}_Tag`. The payload-carrying value itself is an opaque struct
/// `{c_tag}` (forward-declared via [`render_module_type_tags`]); the tag getter
/// returns one of these discriminant constants as `int32_t`.
fn render_rich_enum_tag_decl(out: &mut String, e: &EnumBinding) {
    let tag_enum = format!("{}_Tag", e.c_tag);
    render_enum_constants(out, e, &tag_enum);
}

/// Render the function surface of a rich (algebraic) enum: the tag getter, each
/// variant's constructor and field getters, then the destructor. Assumes the
/// opaque object tag and every referenced type tag are already forward-declared.
fn render_rich_enum_fn_decls(out: &mut String, e: &EnumBinding, prefix: &str) {
    let Some(rich) = &e.rich else {
        return;
    };
    let tag = &e.c_tag;
    emit_doc(out, &e.doc, "");
    let _ = writeln!(out, "int32_t {}(const {tag}* self);", rich.tag_symbol);
    for v in &rich.variants {
        emit_doc(out, &v.doc, "");
        fn_decl(out, &v.create, prefix);
        for field in &v.fields {
            emit_doc(out, &field.doc, "");
            let mut parts = vec![format!("const {tag}* self")];
            parts.extend(
                field
                    .getter_out_params
                    .iter()
                    .map(|p| format!("{} {}", p.ty.render_c(prefix), p.name)),
            );
            let _ = writeln!(
                out,
                "{} {}({});",
                field.getter_ret.render_c(prefix),
                field.getter_symbol,
                parts.join(", ")
            );
        }
    }
    let _ = writeln!(out, "void {}({tag}* self);", rich.destroy_symbol);
    out.push('\n');
}

/// Render the opaque struct/builder *tags* (forward typedefs) for one struct.
///
/// These reference no other types, so emitting every struct's tags before any
/// function declaration lets a function in one module accept or return a struct
/// declared in *another* module (a parent module referencing a child's type).
fn render_struct_tags(out: &mut String, s: &StructBinding) {
    let tag = &s.c_tag;
    let _ = writeln!(out, "typedef struct {tag} {tag};");
    if let Some(b) = &s.builder {
        let bt = &b.builder_tag;
        let _ = writeln!(out, "typedef struct {bt} {bt};");
    }
}

/// Render the function declarations for one struct: create/destroy/getters and,
/// if present, the fluent builder's new/setters/build/destroy. Assumes the
/// struct (and every other struct it may reference) already has a forward
/// typedef emitted via [`render_struct_tags`].
fn render_struct_fn_decls(out: &mut String, s: &StructBinding, prefix: &str) {
    let tag = &s.c_tag;
    emit_doc(out, &s.doc, "");
    fn_decl(out, &s.create, prefix);
    let _ = writeln!(out, "void {}({tag}* ptr);", s.destroy_symbol);
    for field in &s.fields {
        emit_doc(out, &field.doc, "");
        let mut parts = vec![format!("const {tag}* ptr")];
        parts.extend(
            field
                .getter_out_params
                .iter()
                .map(|p| format!("{} {}", p.ty.render_c(prefix), p.name)),
        );
        let _ = writeln!(
            out,
            "{} {}({});",
            field.getter_ret.render_c(prefix),
            field.getter_symbol,
            parts.join(", ")
        );
    }
    out.push('\n');

    if let Some(b) = &s.builder {
        let bt = &b.builder_tag;
        let _ = writeln!(out, "{bt}* {}(void);", b.new_symbol);
        for (field, (_, setter)) in s.fields.iter().zip(&b.setters) {
            emit_doc(out, &field.doc, "");
            let _ = writeln!(
                out,
                "void {setter}({bt}* builder, {});",
                params_str(&field.value_params, prefix)
            );
        }
        let _ = writeln!(
            out,
            "{tag}* {}({bt}* builder, {prefix}_error* out_err);",
            b.build_symbol
        );
        let _ = writeln!(out, "void {}({bt}* builder);", b.destroy_symbol);
        out.push('\n');
    }
}

/// Phase 1a — enum definitions for one module. Enums reference no other types,
/// so they are emitted first across all modules.
pub fn render_module_enum_defs(out: &mut String, module: &ModuleBinding) {
    for e in &module.enums {
        if e.is_rich() {
            render_rich_enum_tag_decl(out, e);
        } else {
            render_enum_decl(out, e);
        }
    }
}

/// Phase 1b — opaque struct/builder/iterator forward typedefs for one module.
/// Pointers to these are all the C ABI ever uses, so a forward typedef is
/// sufficient and lets declarations in any module reference any struct.
pub fn render_module_type_tags(out: &mut String, module: &ModuleBinding) {
    // A rich (algebraic) enum is an opaque object, declared like a struct tag.
    for e in &module.enums {
        if e.is_rich() {
            let t = &e.c_tag;
            let _ = writeln!(out, "typedef struct {t} {t};");
        }
    }
    for s in &module.structs {
        render_struct_tags(out, s);
    }
    for f in &module.functions {
        if let CallShape::Iterator(it) = &f.shape {
            let t = &it.iter_tag;
            let _ = writeln!(out, "typedef struct {t} {t};");
        }
    }
}

/// Phase 1c — callback / async-callback function-pointer typedefs for one
/// module. These may reference enums (by value) and structs (by pointer), so
/// they are emitted after every module's enums and type tags.
pub fn render_module_callback_types(out: &mut String, module: &ModuleBinding, prefix: &str) {
    for cb in &module.callbacks {
        emit_doc(out, &cb.doc, "");
        let _ = writeln!(
            out,
            "typedef void (*{})({});",
            cb.c_fn_type,
            params_str(&cb.abi_params, prefix)
        );
    }
    for f in &module.functions {
        if let CallShape::Async(a) = &f.shape {
            let _ = writeln!(
                out,
                "typedef void (*{})({});",
                a.callback_type,
                params_str(&a.callback_params, prefix)
            );
        }
    }
}

/// Phase 2 — every function prototype for one module: struct create/destroy/
/// getters and builders, listeners, then sync/async/iterator functions. All
/// type tags and callback typedefs are assumed already emitted (phases 1a–1c).
/// Caller controls the leading `// Module:` comment and any framing.
pub fn render_module_fn_decls(out: &mut String, module: &ModuleBinding, prefix: &str) {
    for e in &module.enums {
        render_rich_enum_fn_decls(out, e, prefix);
    }
    for s in &module.structs {
        render_struct_fn_decls(out, s, prefix);
    }
    for l in &module.listeners {
        emit_doc(out, &l.doc, "");
        let _ = writeln!(
            out,
            "uint64_t {}({} callback, void* context);",
            l.register_symbol, l.callback_c_fn_type
        );
        emit_doc(out, &l.doc, "");
        let _ = writeln!(out, "void {}(uint64_t id);", l.unregister_symbol);
    }
    for f in &module.functions {
        emit_doc(out, &f.doc, "");
        if let Some(msg) = &f.deprecated {
            let _ = writeln!(
                out,
                "__attribute__((deprecated(\"{}\")))",
                msg.replace('"', "\\\"")
            );
        }
        match &f.shape {
            CallShape::Iterator(it) => {
                let t = &it.iter_tag;
                fn_decl(out, &it.launch, prefix);
                fn_decl(out, &it.next, prefix);
                let _ = writeln!(out, "void {}({t}* iter);", it.destroy_symbol);
            }
            CallShape::Async(a) => {
                fn_decl(out, &a.launch, prefix);
            }
            CallShape::Sync(abi) => {
                fn_decl(out, abi, prefix);
            }
        }
    }
}

/// Render the complete C ABI declaration surface for `modules` in
/// dependency-safe order: all enum definitions, then all opaque type tags, then
/// all callback typedefs, then per-module function prototypes. Emitting every
/// type tag before any function lets a parent module's function reference a
/// child module's struct — cross-module forward references the previous
/// per-module interleaving could not express.
///
/// The runtime decls (`handle_t`, `error`, `free_*`, cancel token) are *not*
/// emitted here; callers render those first (the C generator inserts its map
/// convention comment in between).
pub fn render_decls(
    out: &mut String,
    modules: &[ModuleBinding],
    prefix: &str,
    module_comments: bool,
) {
    for m in modules {
        render_module_enum_defs(out, m);
    }
    for m in modules {
        render_module_type_tags(out, m);
    }
    for m in modules {
        render_module_callback_types(out, m, prefix);
    }
    out.push('\n');
    for m in modules {
        if module_comments {
            let _ = writeln!(out, "// Module: {}", m.path);
        }
        render_module_fn_decls(out, m, prefix);
        out.push('\n');
    }
}
