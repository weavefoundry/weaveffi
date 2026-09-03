//! JS/TS naming, type mapping, and doc-comment emission.
//!
//! Everything here answers "how is this IDL name or type spelled in
//! JavaScript and TypeScript?": exported function names, escaped parameter
//! identifiers, TS type annotations, and the JSDoc blocks both emitted files
//! share.

use heck::ToLowerCamelCase;
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::lang;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{ErrorBinding, FnBinding, ParamBinding};
use weaveffi_core::utils::{local_type_name, wrapper_name};

/// The exported JS name of a free function or listener endpoint:
/// [`wrapper_name`] (module-prefixed or stripped per config) converted to
/// lowerCamelCase, so module `kv`'s `open_store` exports as `openStore`
/// (stripped, the default) or `kvOpenStore`.
pub(crate) fn js_fn_name(module: &str, func: &str, strip: bool) -> String {
    wrapper_name(module, func, strip).to_lower_camel_case()
}

/// The camelCase JS spelling of an IDL parameter name, escaped when the
/// camel-cased form collides with a JS/TS reserved word (a parameter named
/// `import` becomes `import_`; property and method positions never need the
/// escape, so only parameters route through here).
pub(crate) fn js_param_name(name: &str) -> String {
    lang::escape_ident(&name.to_lower_camel_case(), lang::JS_KEYWORDS)
}

/// The addon-internal JS export base of an interface member
/// (`{Interface}_{member}`). These names are wiring between the addon and the
/// generated classes, not public API, so they keep the raw member spelling.
pub(crate) fn iface_member_base(iface: &str, member: &str) -> String {
    format!("{iface}_{member}")
}

/// Escape a string for embedding in a single-quoted JS literal.
pub(crate) fn js_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
}

/// The TS annotation of one IR type.
pub(crate) fn ts_type_for(ty: &Ty) -> String {
    match ty {
        Ty::I8
        | Ty::I16
        | Ty::U8
        | Ty::U16
        | Ty::I32
        | Ty::U32
        | Ty::I64
        | Ty::U64
        | Ty::F32
        | Ty::F64 => "number".into(),
        Ty::Bool => "boolean".into(),
        Ty::StringUtf8 | Ty::BorrowedStr => "string".into(),
        Ty::Bytes | Ty::BorrowedBytes => "Buffer".into(),
        Ty::Handle => "bigint".into(),
        // Records, rich enums, plain enums, interfaces, and typed handles
        // surface as bare local TS names. A cross-module reference (e.g.
        // `handle<Store>` resolved to `kv.Store`) must annotate the *local*
        // type `Store`; the qualified IR name is not a declared TS type in
        // this module.
        Ty::TypedHandle(name) => local_type_name(name).to_string(),
        Ty::Record(name) | Ty::RichEnum(name) => local_type_name(name).to_string(),
        Ty::Interface(name) => local_type_name(name).to_string(),
        Ty::Enum(name) => local_type_name(name).to_string(),
        Ty::Optional(inner) => format!("{} | null", ts_type_for(inner)),
        Ty::List(inner) => {
            let inner_ts = ts_type_for(inner);
            if matches!(inner.as_ref(), Ty::Optional(_)) {
                format!("({inner_ts})[]")
            } else {
                format!("{inner_ts}[]")
            }
        }
        Ty::Map(k, v) => format!("Record<{}, {}>", ts_type_for(k), ts_type_for(v)),
        // `iter<T>` is a lazy pull stream, not a materialized array.
        Ty::Iterator(inner) => {
            let t = ts_type_for(inner);
            format!("IterableIterator<{t}>")
        }
    }
}

/// Emits a JSDoc comment at `indent`. Single-line docs collapse to
/// `/** text */`; multi-line docs expand to a block with ` * ` prefixed lines.
pub(crate) fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Javadoc);
}

/// Emits a JSDoc block for a function: function doc, `@param name desc` for
/// each documented parameter, and an optional trailing tag list.
pub(crate) fn emit_fn_doc(
    out: &mut String,
    doc: &Option<String>,
    params: &[ParamBinding],
    indent: &str,
    extra_tags: &[String],
) {
    let has_param_docs = params.iter().any(|p| p.doc.is_some());
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    if trimmed_doc.is_none() && !has_param_docs && extra_tags.is_empty() {
        return;
    }
    out.push_str(indent);
    out.push_str("/**\n");
    if let Some(d) = trimmed_doc {
        for line in d.lines() {
            out.push_str(indent);
            if line.is_empty() {
                out.push_str(" *\n");
            } else {
                out.push_str(" * ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    for p in params {
        if let Some(pdoc) = &p.doc {
            let pdoc = pdoc.trim();
            if pdoc.is_empty() {
                continue;
            }
            let mut lines = pdoc.lines();
            if let Some(first) = lines.next() {
                out.push_str(indent);
                out.push_str(&format!(" * @param {} {}\n", js_param_name(&p.name), first));
            }
            for line in lines {
                out.push_str(indent);
                if line.is_empty() {
                    out.push_str(" *\n");
                } else {
                    out.push_str(" *   ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    for tag in extra_tags {
        out.push_str(indent);
        out.push_str(" * ");
        out.push_str(tag);
        out.push('\n');
    }
    out.push_str(indent);
    out.push_str(" */\n");
}

/// The TS parameter list of a callable, camel-cased.
pub(crate) fn ts_params(f: &FnBinding) -> String {
    f.params
        .iter()
        .map(|p| format!("{}: {}", js_param_name(&p.name), ts_type_for(&p.ty)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The TS return annotation of a callable (`Promise`-wrapped when async).
pub(crate) fn ts_ret(f: &FnBinding) -> String {
    let base = match &f.ret {
        Some(ty) => ts_type_for(ty),
        None => "void".into(),
    };
    if f.is_async {
        format!("Promise<{base}>")
    } else {
        base
    }
}

/// The standard JSDoc tag list of a callable: the C mapping, a `@throws` tag
/// naming the module's domain class for throwing callables, and any
/// deprecation notice.
pub(crate) fn ts_fn_tags(f: &FnBinding, error: Option<&ErrorBinding>) -> Vec<String> {
    let mut tags = vec![format!("Maps to C function: {}", f.c_base)];
    if let (true, Some(eb)) = (f.throws, error) {
        tags.push(format!("@throws {{{}}}", eb.type_name));
    }
    if let Some(msg) = &f.deprecated {
        tags.push(format!("@deprecated {}", msg));
    }
    tags
}
