//! `.pyi` type-stub rendering: a typed mirror of the public surface the
//! generated `weaveffi.py` module exposes.

use weaveffi_core::model::{
    BindingModel, CallbackInterfaceBinding, EnumBinding, ErrorBinding, FnBinding, InterfaceBinding,
    StructBinding,
};
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::docs::emit_doc;
use crate::entities::py_code_class_name;
use crate::types::{
    py_field, py_member_name, py_name, py_type_hint, py_variant, py_wrapper_fn_name,
};

/// Render the full `weaveffi.pyi` stub for the model.
pub(crate) fn render_pyi_module(
    model: &BindingModel,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::Hash, input_basename);
    out.push_str(
        "from abc import ABC, abstractmethod\nfrom enum import IntEnum\n\
         from typing import Callable, Dict, Iterator, List, Optional, Type\n",
    );
    out.push_str("\nclass WeaveFFIError(Exception):\n");
    out.push_str("    GENERIC_ERROR_CODE: int\n");
    out.push_str("    PANIC_ERROR_CODE: int\n");
    out.push_str("    MARSHAL_ERROR_CODE: int\n");
    out.push_str("    FOREIGN_ERROR_CODE: int\n");
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
        for cb in &m.callback_interfaces {
            render_pyi_callback_interface(&mut out, cb);
        }
        for i in &m.interfaces {
            render_pyi_interface(&mut out, i);
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

/// `.pyi` stub for one callback interface: the abstract base class with one
/// abstract method per IDL method, mirroring
/// [`crate::calls::render_callback_interface`].
fn render_pyi_callback_interface(out: &mut String, cb: &CallbackInterfaceBinding) {
    out.push('\n');
    emit_doc(out, &cb.doc, "");
    out.push_str(&format!("class {}(ABC):\n", cb.name));
    for m in &cb.methods {
        let mut params = vec!["self".to_string()];
        params.extend(
            m.params
                .iter()
                .map(|p| format!("{}: {}", py_name(&p.name), py_type_hint(&p.ty))),
        );
        let ret = m
            .ret
            .as_ref()
            .map(py_type_hint)
            .unwrap_or_else(|| "None".into());
        emit_doc(out, &m.doc, "    ");
        out.push_str(&format!(
            "    @abstractmethod\n    def {}({}) -> {}: ...\n",
            py_member_name(&m.name),
            params.join(", "),
            ret
        ));
    }
}

/// `.pyi` stub for one interface wrapper class: `__init__` for the canonical
/// `new` constructor, a classmethod per remaining constructor, the
/// reference-releasing `close()` and context-manager pair, then methods and
/// statics, mirroring [`crate::entities::render_interface`].
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
    out.push_str("    def close(self) -> None: ...\n");
    out.push_str(&format!("    def __enter__(self) -> \"{}\": ...\n", i.name));
    out.push_str("    def __exit__(self, *exc: object) -> bool: ...\n");
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
}

/// `.pyi` stub for a plain C-style enum: an `IntEnum` with typed members.
fn render_pyi_enum(out: &mut String, e: &EnumBinding) {
    out.push('\n');
    emit_doc(out, &e.doc, "");
    out.push_str(&format!("class {}(IntEnum):\n", e.name));
    for v in &e.variants {
        emit_doc(out, &v.doc, "    ");
        out.push_str(&format!("    {}: int\n", py_variant(&v.name)));
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
        out.push_str(&format!("        {}: int\n", py_variant(&v.name)));
    }
    for v in &e.variants {
        out.push_str(&format!(
            "    {0}: Type[\"{name}{0}\"]\n",
            py_variant(&v.name)
        ));
    }
    out.push_str(&format!(
        "    @property\n    def tag(self) -> \"{name}.Tag\": ...\n"
    ));
    for v in &e.variants {
        let class = format!("{name}{}", py_variant(&v.name));
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
