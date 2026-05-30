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

/// Render an enum typedef. Multi-line when any variant is documented.
pub fn render_enum_decl(out: &mut String, e: &EnumBinding) {
    emit_doc(out, &e.doc, "");
    if e.variants.iter().any(|v| v.doc.is_some()) {
        out.push_str("typedef enum {\n");
        for (i, v) in e.variants.iter().enumerate() {
            emit_doc(out, &v.doc, "    ");
            let comma = if i + 1 == e.variants.len() { "" } else { "," };
            let _ = writeln!(out, "    {} = {}{comma}", v.c_const, v.value);
        }
        let _ = writeln!(out, "}} {};", e.c_tag);
    } else {
        let variants: Vec<String> = e
            .variants
            .iter()
            .map(|v| format!("{} = {}", v.c_const, v.value))
            .collect();
        let _ = writeln!(
            out,
            "typedef enum {{ {} }} {};",
            variants.join(", "),
            e.c_tag
        );
    }
}

/// Render the opaque struct typedef plus create/destroy/getters, then any
/// fluent builder.
pub fn render_struct_decls(out: &mut String, s: &StructBinding, prefix: &str) {
    let tag = &s.c_tag;
    emit_doc(out, &s.doc, "");
    let _ = writeln!(out, "typedef struct {tag} {tag};");
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
        let _ = writeln!(out, "typedef struct {bt} {bt};");
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

/// Render every declaration for one module: enums, structs, callbacks,
/// listeners, then functions (sync/async/iterator). Caller controls the
/// leading `// Module:` comment and any framing.
pub fn render_module_decls(out: &mut String, module: &ModuleBinding, prefix: &str) {
    for e in &module.enums {
        render_enum_decl(out, e);
    }
    for s in &module.structs {
        render_struct_decls(out, s, prefix);
    }
    for cb in &module.callbacks {
        emit_doc(out, &cb.doc, "");
        let _ = writeln!(
            out,
            "typedef void (*{})({});",
            cb.c_fn_type,
            params_str(&cb.abi_params, prefix)
        );
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
                let _ = writeln!(out, "typedef struct {t} {t};");
                fn_decl(out, &it.launch, prefix);
                fn_decl(out, &it.next, prefix);
                let _ = writeln!(out, "void {}({t}* iter);", it.destroy_symbol);
            }
            CallShape::Async(a) => {
                let _ = writeln!(
                    out,
                    "typedef void (*{})({});",
                    a.callback_type,
                    params_str(&a.callback_params, prefix)
                );
                fn_decl(out, &a.launch, prefix);
            }
            CallShape::Sync(abi) => {
                fn_decl(out, abi, prefix);
            }
        }
    }
}
