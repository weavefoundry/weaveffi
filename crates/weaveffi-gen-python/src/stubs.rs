//! `.pyi` type-stub rendering: a typed mirror of the public surface the
//! generated `weaveffi.py` module exposes.

use weaveffi_core::model::{
    BindingModel, EnumBinding, ErrorBinding, FnBinding, InterfaceBinding, ListenerBinding,
    ModuleBinding, StructBinding,
};
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::docs::emit_doc;
use crate::entities::py_code_class_name;
use crate::types::{
    py_callable_hint, py_field, py_member_name, py_name, py_type_hint, py_wrapper_fn_name,
};

/// Render the full `weaveffi.pyi` stub for the model.
pub(crate) fn render_pyi_module(
    model: &BindingModel,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::Hash, input_basename);
    out.push_str(
        "from enum import IntEnum\nfrom typing import Callable, Dict, Iterator, List, Optional, Type\n",
    );
    out.push_str("\nclass WeaveFFIError(Exception):\n");
    out.push_str("    code: int\n");
    out.push_str("    message: str\n");
    out.push_str("    def __init__(self, code: int, message: str) -> None: ...\n");
    for m in &model.modules {
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_pyi_error(&mut out, eb);
        }
        for e in &m.enums {
            if e.is_rich() {
                render_pyi_rich_enum(&mut out, e);
            } else {
                render_pyi_enum(&mut out, e);
            }
        }
        for s in &m.structs {
            render_pyi_struct(&mut out, s);
        }
        for i in &m.interfaces {
            render_pyi_interface(&mut out, i);
        }
        for l in &m.listeners {
            render_pyi_listener(&mut out, m, l, strip_module_prefix);
        }
        for f in &m.functions {
            render_pyi_function(&mut out, &m.path, f, strip_module_prefix);
        }
    }
    out.push('\n');
    out.push_str(&render_trailer(CommentStyle::Hash, "weaveffi.pyi"));
    out
}

/// `.pyi` stub for one module's error domain: the domain base class (with
/// its scoped per-code aliases) plus a per-code subclass carrying its stable
/// `CODE` and any structured payload fields, mirroring
/// [`crate::entities::render_error`].
fn render_pyi_error(out: &mut String, eb: &ErrorBinding) {
    let domain = &eb.type_name;
    out.push('\n');
    out.push_str(&format!("class {domain}(WeaveFFIError):\n"));
    for c in &eb.codes {
        let class = py_code_class_name(&c.name);
        out.push_str(&format!("    {class}: Type[\"{class}\"]\n"));
    }
    out.push_str("    def __init__(self, code: int, message: str) -> None: ...\n");
    for c in &eb.codes {
        let class = py_code_class_name(&c.name);
        out.push('\n');
        emit_doc(out, &c.doc, "");
        out.push_str(&format!("class {class}({domain}):\n"));
        out.push_str("    CODE: int\n");
        for f in &c.fields {
            out.push_str(&format!(
                "    {}: {}\n",
                py_field(&f.name),
                py_type_hint(&f.ty)
            ));
        }
        out.push_str("    def __init__(self, message: str = ...) -> None: ...\n");
    }
}

/// `.pyi` stub for one interface wrapper class: `__init__` for the canonical
/// `new` constructor, a classmethod per remaining constructor, then methods
/// and statics, mirroring [`crate::entities::render_interface`].
fn render_pyi_interface(out: &mut String, i: &InterfaceBinding) {
    out.push('\n');
    emit_doc(out, &i.doc, "");
    out.push_str(&format!("class {}:\n", i.name));
    let member_sig = |f: &FnBinding, receiver: Option<&str>| -> String {
        let mut params: Vec<String> = receiver.iter().map(|r| r.to_string()).collect();
        params.extend(
            f.params
                .iter()
                .map(|p| format!("{}: {}", py_name(&p.name), py_type_hint(&p.ty))),
        );
        params.join(", ")
    };
    let async_kw = |f: &FnBinding| if f.is_async { "async " } else { "" };
    if let Some(c) = i.constructors.iter().find(|c| c.name == "new") {
        out.push_str(&format!(
            "    def __init__({}) -> None: ...\n",
            member_sig(c, Some("self"))
        ));
    }
    for c in i.constructors.iter().filter(|c| c.name != "new") {
        out.push_str(&format!(
            "    @classmethod\n    def {}({}) -> \"{}\": ...\n",
            py_member_name(&c.name),
            member_sig(c, Some("cls")),
            i.name
        ));
    }
    for m in &i.methods {
        let ret = m
            .ret
            .as_ref()
            .map(py_type_hint)
            .unwrap_or_else(|| "None".into());
        out.push_str(&format!(
            "    {}def {}({}) -> {}: ...\n",
            async_kw(m),
            py_member_name(&m.name),
            member_sig(m, Some("self")),
            ret
        ));
    }
    for s in &i.statics {
        let ret = s
            .ret
            .as_ref()
            .map(py_type_hint)
            .unwrap_or_else(|| "None".into());
        out.push_str(&format!(
            "    @staticmethod\n    {}def {}({}) -> {}: ...\n",
            async_kw(s),
            py_member_name(&s.name),
            member_sig(s, None),
            ret
        ));
    }
    if i.constructors.is_empty() && i.methods.is_empty() && i.statics.is_empty() {
        out.push_str("    ...\n");
    }
}

/// `.pyi` stub for a plain C-style enum: an `IntEnum` with typed members.
fn render_pyi_enum(out: &mut String, e: &EnumBinding) {
    out.push('\n');
    emit_doc(out, &e.doc, "");
    out.push_str(&format!("class {}(IntEnum):\n", e.name));
    for v in &e.variants {
        emit_doc(out, &v.doc, "    ");
        out.push_str(&format!("    {}: int\n", v.name));
    }
}

/// `.pyi` stub for a rich (algebraic) enum: the base class with its nested
/// `Tag` `IntEnum`, scoped variant aliases, and `tag` reader, plus one
/// dataclass-shaped variant subclass with its fields and constructor,
/// mirroring the rich-enum rendering in [`crate::entities`].
fn render_pyi_rich_enum(out: &mut String, e: &EnumBinding) {
    let name = &e.name;
    out.push('\n');
    emit_doc(out, &e.doc, "");
    out.push_str(&format!("class {name}:\n"));
    out.push_str("    class Tag(IntEnum):\n");
    for v in &e.variants {
        emit_doc(out, &v.doc, "        ");
        out.push_str(&format!("        {}: int\n", v.name));
    }
    for v in &e.variants {
        out.push_str(&format!("    {}: Type[\"{name}{}\"]\n", v.name, v.name));
    }
    out.push_str(&format!(
        "    @property\n    def tag(self) -> \"{name}.Tag\": ...\n"
    ));
    for v in &e.variants {
        let class = format!("{name}{}", v.name);
        out.push('\n');
        emit_doc(out, &v.doc, "");
        out.push_str(&format!("class {class}({name}):\n"));
        out.push_str(&format!("    TAG: \"{name}.Tag\"\n"));
        for f in &v.fields {
            out.push_str(&format!(
                "    {}: {}\n",
                py_field(&f.name),
                py_type_hint(&f.ty)
            ));
        }
        let mut params = vec!["self".to_string()];
        params.extend(
            v.fields
                .iter()
                .map(|f| format!("{}: {}", py_field(&f.name), py_type_hint(&f.ty))),
        );
        out.push_str(&format!(
            "    def __init__({}) -> None: ...\n",
            params.join(", ")
        ));
    }
}

/// `.pyi` stub for a record: a dataclass-shaped value class with typed field
/// attributes and the generated constructor, mirroring
/// [`crate::entities::render_struct`].
fn render_pyi_struct(out: &mut String, s: &StructBinding) {
    out.push('\n');
    emit_doc(out, &s.doc, "");
    out.push_str(&format!("class {}:\n", s.name));
    for field in &s.fields {
        let py_ty = py_type_hint(&field.ty);
        emit_doc(out, &field.doc, "    ");
        out.push_str(&format!("    {}: {}\n", py_field(&field.name), py_ty));
    }
    let mut params = vec!["self".to_string()];
    params.extend(
        s.fields
            .iter()
            .map(|f| format!("{}: {}", py_field(&f.name), py_type_hint(&f.ty))),
    );
    out.push_str(&format!(
        "    def __init__({}) -> None: ...\n",
        params.join(", ")
    ));
}

/// `.pyi` stub for one listener's register/unregister wrapper pair.
fn render_pyi_listener(
    out: &mut String,
    module: &ModuleBinding,
    l: &ListenerBinding,
    strip_module_prefix: bool,
) {
    let Some(cb) = module.callbacks.iter().find(|c| c.name == l.event_callback) else {
        unreachable!("listener '{}' references unknown callback", l.name);
    };
    let register_name = py_wrapper_fn_name(
        &module.path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    );
    let unregister_name = py_wrapper_fn_name(
        &module.path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    );
    out.push('\n');
    emit_doc(out, &l.doc, "");
    out.push_str(&format!(
        "def {register_name}(callback: {}) -> int: ...\n",
        py_callable_hint(&cb.params)
    ));
    out.push_str(&format!(
        "def {unregister_name}(listener_id: int) -> None: ...\n"
    ));
}

/// `.pyi` stub for one module-level free function.
fn render_pyi_function(
    out: &mut String,
    module_name: &str,
    f: &FnBinding,
    strip_module_prefix: bool,
) {
    let func_name = py_wrapper_fn_name(module_name, &f.name, strip_module_prefix);
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", py_name(&p.name), py_type_hint(&p.ty)))
        .collect();
    let ret = f
        .ret
        .as_ref()
        .map(py_type_hint)
        .unwrap_or_else(|| "None".into());
    let async_kw = if f.is_async { "async " } else { "" };
    out.push('\n');
    emit_doc(out, &f.doc, "");
    out.push_str(&format!(
        "{async_kw}def {}({}) -> {}: ...\n",
        func_name,
        params.join(", "),
        ret
    ));
}
