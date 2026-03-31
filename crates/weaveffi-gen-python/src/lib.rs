use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_core::utils::c_symbol_name;
use weaveffi_ir::ir::{Api, EnumDef, Function, StructDef, StructField, TypeRef};

pub struct PythonGenerator;

impl Generator for PythonGenerator {
    fn name(&self) -> &'static str {
        "python"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        let dir = out_dir.join("python");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("__init__.py"),
            "from .weaveffi import *  # noqa: F401,F403\n",
        )?;
        std::fs::write(dir.join("weaveffi.py"), render_python_module(api))?;
        Ok(())
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("python/__init__.py").to_string(),
            out_dir.join("python/weaveffi.py").to_string(),
        ]
    }
}

// ── Type helpers ──

fn is_c_pointer_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::StringUtf8
            | TypeRef::Bytes
            | TypeRef::Struct(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
    )
}

fn py_ctypes_scalar(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 => "ctypes.c_int32",
        TypeRef::U32 => "ctypes.c_uint32",
        TypeRef::I64 => "ctypes.c_int64",
        TypeRef::F64 => "ctypes.c_double",
        TypeRef::Bool => "ctypes.c_int32",
        TypeRef::StringUtf8 => "ctypes.c_char_p",
        TypeRef::Handle => "ctypes.c_uint64",
        TypeRef::Bytes => "ctypes.c_uint8",
        TypeRef::Struct(_) => "ctypes.c_void_p",
        TypeRef::Enum(_) => "ctypes.c_int32",
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) => "ctypes.c_void_p",
    }
}

fn py_type_hint(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::Handle => "int".into(),
        TypeRef::F64 => "float".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 => "str".into(),
        TypeRef::Bytes => "bytes".into(),
        TypeRef::Struct(name) | TypeRef::Enum(name) => format!("\"{}\"", name),
        TypeRef::Optional(inner) => format!("Optional[{}]", py_type_hint(inner)),
        TypeRef::List(inner) => format!("List[{}]", py_type_hint(inner)),
        TypeRef::Map(k, v) => format!("Dict[{}, {}]", py_type_hint(k), py_type_hint(v)),
    }
}

fn py_param_argtypes(ty: &TypeRef) -> Vec<String> {
    match ty {
        TypeRef::Bytes => vec![
            "ctypes.POINTER(ctypes.c_uint8)".into(),
            "ctypes.c_size_t".into(),
        ],
        TypeRef::Optional(inner) if !is_c_pointer_type(inner) => {
            vec![format!("ctypes.POINTER({})", py_ctypes_scalar(inner))]
        }
        TypeRef::Optional(inner) => py_param_argtypes(inner),
        TypeRef::List(inner) => vec![
            format!("ctypes.POINTER({})", py_ctypes_scalar(inner)),
            "ctypes.c_size_t".into(),
        ],
        TypeRef::Map(k, v) => vec![
            format!("ctypes.POINTER({})", py_ctypes_scalar(k)),
            format!("ctypes.POINTER({})", py_ctypes_scalar(v)),
            "ctypes.c_size_t".into(),
        ],
        _ => vec![py_ctypes_scalar(ty).into()],
    }
}

/// Returns `(restype, out_param_argtypes)` for a return type.
fn py_return_info(ty: &TypeRef) -> (String, Vec<String>) {
    match ty {
        TypeRef::Bytes => (
            "ctypes.POINTER(ctypes.c_uint8)".into(),
            vec!["ctypes.POINTER(ctypes.c_size_t)".into()],
        ),
        TypeRef::Optional(inner) if !is_c_pointer_type(inner) => (
            format!("ctypes.POINTER({})", py_ctypes_scalar(inner)),
            vec![],
        ),
        TypeRef::Optional(inner) => py_return_info(inner),
        TypeRef::List(inner) => (
            format!("ctypes.POINTER({})", py_ctypes_scalar(inner)),
            vec!["ctypes.POINTER(ctypes.c_size_t)".into()],
        ),
        TypeRef::Map(k, v) => (
            "None".into(),
            vec![
                format!("ctypes.POINTER(ctypes.POINTER({}))", py_ctypes_scalar(k)),
                format!("ctypes.POINTER(ctypes.POINTER({}))", py_ctypes_scalar(v)),
                "ctypes.POINTER(ctypes.c_size_t)".into(),
            ],
        ),
        _ => (py_ctypes_scalar(ty).into(), vec![]),
    }
}

fn get_map_kv(ty: &TypeRef) -> Option<(&TypeRef, &TypeRef)> {
    match ty {
        TypeRef::Map(k, v) => Some((k, v)),
        TypeRef::Optional(inner) => get_map_kv(inner),
        _ => None,
    }
}

// ── Rendering ──

fn render_python_module(api: &Api) -> String {
    let mut out = String::new();
    render_preamble(&mut out);
    for m in &api.modules {
        out.push_str(&format!("\n\n# === Module: {} ===", m.name));
        for e in &m.enums {
            render_enum(&mut out, e);
        }
        for s in &m.structs {
            render_struct(&mut out, &m.name, s);
        }
        for f in &m.functions {
            render_function(&mut out, &m.name, f);
        }
    }
    out.push('\n');
    out
}

fn render_preamble(out: &mut String) {
    out.push_str(
        r#""""WeaveFFI Python ctypes bindings (auto-generated)"""
import ctypes
import platform
from enum import IntEnum
from typing import Dict, List, Optional


class WeaveffiError(Exception):
    def __init__(self, code: int, message: str) -> None:
        self.code = code
        self.message = message
        super().__init__(f"({code}) {message}")


class _WeaveffiErrorStruct(ctypes.Structure):
    _fields_ = [
        ("code", ctypes.c_int32),
        ("message", ctypes.c_char_p),
    ]


def _load_library() -> ctypes.CDLL:
    system = platform.system()
    if system == "Darwin":
        name = "libweaveffi.dylib"
    elif system == "Windows":
        name = "weaveffi.dll"
    else:
        name = "libweaveffi.so"
    return ctypes.CDLL(name)


_lib = _load_library()
_lib.weaveffi_error_clear.argtypes = [ctypes.POINTER(_WeaveffiErrorStruct)]
_lib.weaveffi_error_clear.restype = None
_lib.weaveffi_free_string.argtypes = [ctypes.c_char_p]
_lib.weaveffi_free_string.restype = None
_lib.weaveffi_free_bytes.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t]
_lib.weaveffi_free_bytes.restype = None


def _check_error(err: _WeaveffiErrorStruct) -> None:
    if err.code != 0:
        code = err.code
        message = err.message.decode("utf-8") if err.message else ""
        _lib.weaveffi_error_clear(ctypes.byref(err))
        raise WeaveffiError(code, message)
"#,
    );
}

fn render_enum(out: &mut String, e: &EnumDef) {
    out.push_str(&format!("\n\nclass {}(IntEnum):", e.name));
    if let Some(doc) = &e.doc {
        out.push_str(&format!("\n    \"\"\"{}\"\"\"", doc));
    }
    for v in &e.variants {
        out.push_str(&format!("\n    {} = {}", v.name, v.value));
    }
    out.push('\n');
}

fn render_struct(out: &mut String, module_name: &str, s: &StructDef) {
    let prefix = format!("weaveffi_{}_{}", module_name, s.name);

    out.push_str(&format!("\n\nclass {}:", s.name));
    if let Some(doc) = &s.doc {
        out.push_str(&format!("\n    \"\"\"{}\"\"\"", doc));
    }

    out.push_str("\n\n    def __init__(self, _ptr: int) -> None:");
    out.push_str("\n        self._ptr = _ptr");

    out.push_str("\n\n    def __del__(self) -> None:");
    out.push_str("\n        if self._ptr is not None:");
    out.push_str(&format!(
        "\n            _lib.{prefix}_destroy.argtypes = [ctypes.c_void_p]"
    ));
    out.push_str(&format!(
        "\n            _lib.{prefix}_destroy.restype = None"
    ));
    out.push_str(&format!("\n            _lib.{prefix}_destroy(self._ptr)"));
    out.push_str("\n            self._ptr = None");

    for field in &s.fields {
        render_getter(out, &prefix, field);
    }
    out.push('\n');
}

fn render_getter(out: &mut String, prefix: &str, field: &StructField) {
    let getter = format!("{prefix}_get_{}", field.name);
    let py_ty = py_type_hint(&field.ty);
    let ind = "        ";

    out.push_str(&format!(
        "\n\n    @property\n    def {}(self) -> {}:\n",
        field.name, py_ty
    ));
    out.push_str(&format!("{ind}_fn = _lib.{getter}\n"));

    let (restype, out_argtypes) = py_return_info(&field.ty);
    let mut argtypes = vec!["ctypes.c_void_p".to_string()];
    argtypes.extend(out_argtypes.iter().cloned());

    out.push_str(&format!("{ind}_fn.argtypes = [{}]\n", argtypes.join(", ")));
    out.push_str(&format!("{ind}_fn.restype = {restype}\n"));

    if out_argtypes.is_empty() {
        out.push_str(&format!("{ind}_result = _fn(self._ptr)\n"));
    } else if let Some((k, v)) = get_map_kv(&field.ty) {
        out.push_str(&format!(
            "{ind}_out_keys = ctypes.POINTER({})()\n",
            py_ctypes_scalar(k)
        ));
        out.push_str(&format!(
            "{ind}_out_values = ctypes.POINTER({})()\n",
            py_ctypes_scalar(v)
        ));
        out.push_str(&format!("{ind}_out_len = ctypes.c_size_t(0)\n"));
        out.push_str(&format!("{ind}_fn(self._ptr, ctypes.byref(_out_keys), ctypes.byref(_out_values), ctypes.byref(_out_len))\n"));
    } else {
        out.push_str(&format!("{ind}_out_len = ctypes.c_size_t(0)\n"));
        out.push_str(&format!(
            "{ind}_result = _fn(self._ptr, ctypes.byref(_out_len))\n"
        ));
    }

    render_return_value(out, &field.ty, ind);
}

fn render_function(out: &mut String, module_name: &str, f: &Function) {
    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, py_type_hint(&p.ty)))
        .collect();
    let ret_hint = f
        .returns
        .as_ref()
        .map(py_type_hint)
        .unwrap_or_else(|| "None".to_string());

    out.push_str(&format!(
        "\n\ndef {}({}) -> {}:\n",
        f.name,
        params_sig.join(", "),
        ret_hint
    ));

    let c_sym = c_symbol_name(module_name, &f.name);
    let ind = "    ";

    out.push_str(&format!("{ind}_fn = _lib.{c_sym}\n"));

    let mut argtypes: Vec<String> = Vec::new();
    for p in &f.params {
        argtypes.extend(py_param_argtypes(&p.ty));
    }
    let mut out_ret_argtypes = Vec::new();
    let restype;
    if let Some(ret_ty) = &f.returns {
        let (rt, oat) = py_return_info(ret_ty);
        argtypes.extend(oat.iter().cloned());
        restype = rt;
        out_ret_argtypes = oat;
    } else {
        restype = "None".to_string();
    }
    argtypes.push("ctypes.POINTER(_WeaveffiErrorStruct)".into());

    out.push_str(&format!("{ind}_fn.argtypes = [{}]\n", argtypes.join(", ")));
    out.push_str(&format!("{ind}_fn.restype = {restype}\n"));

    for p in &f.params {
        for line in py_param_conversion(&p.name, &p.ty, ind) {
            out.push_str(&line);
            out.push('\n');
        }
    }

    out.push_str(&format!("{ind}_err = _WeaveffiErrorStruct()\n"));

    let is_map_ret = f.returns.as_ref().and_then(get_map_kv).is_some();
    let has_out_len = !out_ret_argtypes.is_empty() && !is_map_ret;

    if let Some((k, v)) = f.returns.as_ref().and_then(get_map_kv) {
        out.push_str(&format!(
            "{ind}_out_keys = ctypes.POINTER({})()\n",
            py_ctypes_scalar(k)
        ));
        out.push_str(&format!(
            "{ind}_out_values = ctypes.POINTER({})()\n",
            py_ctypes_scalar(v)
        ));
        out.push_str(&format!("{ind}_out_len = ctypes.c_size_t(0)\n"));
    } else if has_out_len {
        out.push_str(&format!("{ind}_out_len = ctypes.c_size_t(0)\n"));
    }

    let mut call_args: Vec<String> = Vec::new();
    for p in &f.params {
        call_args.extend(py_param_call_args(&p.name, &p.ty));
    }
    if is_map_ret {
        call_args.push("ctypes.byref(_out_keys)".into());
        call_args.push("ctypes.byref(_out_values)".into());
        call_args.push("ctypes.byref(_out_len)".into());
    } else if has_out_len {
        call_args.push("ctypes.byref(_out_len)".into());
    }
    call_args.push("ctypes.byref(_err)".into());

    let call_expr = format!("_fn({})", call_args.join(", "));
    if f.returns.is_some() && !is_map_ret {
        out.push_str(&format!("{ind}_result = {call_expr}\n"));
    } else {
        out.push_str(&format!("{ind}{call_expr}\n"));
    }

    out.push_str(&format!("{ind}_check_error(_err)\n"));

    if let Some(ret_ty) = &f.returns {
        render_return_value(out, ret_ty, ind);
    }
}

// ── Param helpers ──

fn py_list_convert_expr(name: &str, elem: &TypeRef) -> String {
    match elem {
        TypeRef::StringUtf8 => format!("*[v.encode(\"utf-8\") for v in {name}]"),
        TypeRef::Struct(_) => format!("*[v._ptr for v in {name}]"),
        TypeRef::Enum(_) => format!("*[v.value for v in {name}]"),
        TypeRef::Bool => format!("*[1 if v else 0 for v in {name}]"),
        _ => format!("*{name}"),
    }
}

fn py_map_elem_convert(list_name: &str, ty: &TypeRef, var: &str) -> String {
    match ty {
        TypeRef::StringUtf8 => format!("*[{var}.encode(\"utf-8\") for {var} in {list_name}]"),
        TypeRef::Enum(_) => format!("*[{var}.value for {var} in {list_name}]"),
        TypeRef::Struct(_) => format!("*[{var}._ptr for {var} in {list_name}]"),
        TypeRef::Bool => format!("*[1 if {var} else 0 for {var} in {list_name}]"),
        _ => format!("*{list_name}"),
    }
}

fn py_param_conversion(name: &str, ty: &TypeRef, ind: &str) -> Vec<String> {
    match ty {
        TypeRef::Bytes => {
            let s = py_ctypes_scalar(&TypeRef::Bytes);
            vec![format!("{ind}_{name}_arr = ({s} * len({name}))(*{name})")]
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Handle => {
                let s = py_ctypes_scalar(inner);
                vec![format!(
                    "{ind}_{name}_c = ctypes.byref({s}({name})) if {name} is not None else None"
                )]
            }
            TypeRef::Bool => {
                vec![format!(
                    "{ind}_{name}_c = ctypes.byref(ctypes.c_int32(1 if {name} else 0)) if {name} is not None else None"
                )]
            }
            TypeRef::StringUtf8 => {
                vec![format!(
                    "{ind}_{name}_c = {name}.encode(\"utf-8\") if {name} is not None else None"
                )]
            }
            TypeRef::Enum(_) => {
                vec![format!(
                    "{ind}_{name}_c = ctypes.byref(ctypes.c_int32({name}.value)) if {name} is not None else None"
                )]
            }
            TypeRef::Bytes => {
                let s = py_ctypes_scalar(&TypeRef::Bytes);
                vec![
                    format!("{ind}if {name} is not None:"),
                    format!("{ind}    _{name}_arr = ({s} * len({name}))(*{name})"),
                    format!("{ind}    _{name}_len = len({name})"),
                    format!("{ind}else:"),
                    format!("{ind}    _{name}_arr = None"),
                    format!("{ind}    _{name}_len = 0"),
                ]
            }
            TypeRef::List(elem) => {
                let s = py_ctypes_scalar(elem);
                let convert = py_list_convert_expr(name, elem);
                vec![
                    format!("{ind}if {name} is not None:"),
                    format!("{ind}    _{name}_arr = ({s} * len({name}))({convert})"),
                    format!("{ind}    _{name}_len = len({name})"),
                    format!("{ind}else:"),
                    format!("{ind}    _{name}_arr = None"),
                    format!("{ind}    _{name}_len = 0"),
                ]
            }
            _ => vec![],
        },
        TypeRef::List(inner) => {
            let s = py_ctypes_scalar(inner);
            let convert = py_list_convert_expr(name, inner);
            vec![format!("{ind}_{name}_arr = ({s} * len({name}))({convert})")]
        }
        TypeRef::Map(k, v) => {
            let ks = py_ctypes_scalar(k);
            let vs = py_ctypes_scalar(v);
            let kconv = py_map_elem_convert(&format!("_{name}_keys"), k, "_k");
            let vconv = py_map_elem_convert(&format!("_{name}_vals"), v, "_v");
            vec![
                format!("{ind}_{name}_keys = list({name}.keys())"),
                format!("{ind}_{name}_vals = [{name}[_k] for _k in _{name}_keys]"),
                format!("{ind}_{name}_ka = ({ks} * len(_{name}_keys))({kconv})"),
                format!("{ind}_{name}_va = ({vs} * len(_{name}_vals))({vconv})"),
            ]
        }
        _ => vec![],
    }
}

fn py_param_call_args(name: &str, ty: &TypeRef) -> Vec<String> {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Handle => {
            vec![name.to_string()]
        }
        TypeRef::Bool => vec![format!("1 if {name} else 0")],
        TypeRef::StringUtf8 => vec![format!("{name}.encode(\"utf-8\")")],
        TypeRef::Bytes => vec![format!("_{name}_arr"), format!("len({name})")],
        TypeRef::Struct(_) => vec![format!("{name}._ptr")],
        TypeRef::Enum(_) => vec![format!("{name}.value")],
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 => vec![format!("_{name}_c")],
            TypeRef::Struct(_) => {
                vec![format!("{name}._ptr if {name} is not None else None")]
            }
            TypeRef::Bytes | TypeRef::List(_) => {
                vec![format!("_{name}_arr"), format!("_{name}_len")]
            }
            TypeRef::Map(_, _) => vec![
                format!("_{name}_ka"),
                format!("_{name}_va"),
                format!("_{name}_len"),
            ],
            _ if !is_c_pointer_type(inner) => vec![format!("_{name}_c")],
            _ => py_param_call_args(name, inner),
        },
        TypeRef::List(_) => vec![format!("_{name}_arr"), format!("len({name})")],
        TypeRef::Map(_, _) => vec![
            format!("_{name}_ka"),
            format!("_{name}_va"),
            format!("len(_{name}_keys)"),
        ],
    }
}

// ── Return helpers ──

fn py_read_element(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 => format!("{expr}.decode(\"utf-8\")"),
        TypeRef::Struct(name) => format!("{name}({expr})"),
        TypeRef::Enum(name) => format!("{name}({expr})"),
        TypeRef::Bool => format!("bool({expr})"),
        _ => expr.to_string(),
    }
}

fn render_return_value(out: &mut String, ty: &TypeRef, ind: &str) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Handle => {
            out.push_str(&format!("{ind}return _result\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{ind}return bool(_result)\n"));
        }
        TypeRef::StringUtf8 => {
            out.push_str(&format!("{ind}if _result is None:\n"));
            out.push_str(&format!("{ind}    return \"\"\n"));
            out.push_str(&format!("{ind}return _result.decode(\"utf-8\")\n"));
        }
        TypeRef::Bytes => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return b\"\"\n"));
            out.push_str(&format!("{ind}return bytes(_result[:_out_len.value])\n"));
        }
        TypeRef::Struct(name) => {
            out.push_str(&format!("{ind}if _result is None:\n"));
            out.push_str(&format!(
                "{ind}    raise WeaveffiError(-1, \"null pointer\")\n"
            ));
            out.push_str(&format!("{ind}return {name}(_result)\n"));
        }
        TypeRef::Enum(name) => {
            out.push_str(&format!("{ind}return {name}(_result)\n"));
        }
        TypeRef::Optional(inner) => render_optional_return(out, inner, ind),
        TypeRef::List(inner) => render_list_return(out, inner, ind),
        TypeRef::Map(k, v) => render_map_return(out, k, v, ind),
    }
}

fn render_optional_return(out: &mut String, inner: &TypeRef, ind: &str) {
    match inner {
        TypeRef::StringUtf8 => {
            out.push_str(&format!("{ind}if _result is None:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return _result.decode(\"utf-8\")\n"));
        }
        TypeRef::Bytes => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return bytes(_result[:_out_len.value])\n"));
        }
        TypeRef::Struct(name) => {
            out.push_str(&format!("{ind}if _result is None:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return {name}(_result)\n"));
        }
        TypeRef::Enum(name) => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return {name}(_result[0])\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return bool(_result[0])\n"));
        }
        _ if !is_c_pointer_type(inner) => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return _result[0]\n"));
        }
        _ => {
            out.push_str(&format!("{ind}return _result\n"));
        }
    }
}

fn render_list_return(out: &mut String, inner: &TypeRef, ind: &str) {
    out.push_str(&format!("{ind}if not _result:\n"));
    out.push_str(&format!("{ind}    return []\n"));
    let elem = py_read_element("_result[_i]", inner);
    out.push_str(&format!(
        "{ind}return [{elem} for _i in range(_out_len.value)]\n"
    ));
}

fn render_map_return(out: &mut String, k: &TypeRef, v: &TypeRef, ind: &str) {
    out.push_str(&format!("{ind}if not _out_keys or not _out_values:\n"));
    out.push_str(&format!("{ind}    return {{}}\n"));
    let key_read = py_read_element("_out_keys[_i]", k);
    let val_read = py_read_element("_out_values[_i]", v);
    out.push_str(&format!(
        "{ind}return {{{key_read}: {val_read} for _i in range(_out_len.value)}}\n"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".into(),
            modules,
        }
    }

    fn simple_module(functions: Vec<Function>) -> Module {
        Module {
            name: "math".into(),
            functions,
            structs: vec![],
            enums: vec![],
            errors: None,
        }
    }

    #[test]
    fn generator_name_is_python() {
        assert_eq!(PythonGenerator.name(), "python");
    }

    #[test]
    fn generate_creates_output_files() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
        }])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_python_gen_output");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir).unwrap();

        let init = std::fs::read_to_string(tmp.join("python/__init__.py")).unwrap();
        assert!(init.contains("from .weaveffi import *"));

        let weaveffi = std::fs::read_to_string(tmp.join("python/weaveffi.py")).unwrap();
        assert!(weaveffi.contains("WeaveFFI"));
        assert!(weaveffi.contains("def add("));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn output_files_lists_both() {
        let api = make_api(vec![]);
        let out = Utf8Path::new("/tmp/out");
        let files = PythonGenerator.output_files(&api, out);
        assert_eq!(
            files,
            vec!["/tmp/out/python/__init__.py", "/tmp/out/python/weaveffi.py"]
        );
    }

    #[test]
    fn preamble_has_load_library() {
        let api = make_api(vec![]);
        let py = render_python_module(&api);
        assert!(py.contains("def _load_library()"), "missing _load_library");
        assert!(
            py.contains("libweaveffi.dylib"),
            "missing macOS library name"
        );
        assert!(py.contains("libweaveffi.so"), "missing Linux library name");
        assert!(py.contains("weaveffi.dll"), "missing Windows library name");
        assert!(py.contains("ctypes.CDLL(name)"), "missing CDLL call");
    }

    #[test]
    fn preamble_has_error_handling() {
        let api = make_api(vec![]);
        let py = render_python_module(&api);
        assert!(
            py.contains("class WeaveffiError(Exception):"),
            "missing error class"
        );
        assert!(
            py.contains("class _WeaveffiErrorStruct(ctypes.Structure):"),
            "missing error struct"
        );
        assert!(py.contains("def _check_error("), "missing _check_error");
        assert!(
            py.contains("weaveffi_error_clear"),
            "missing error_clear setup"
        );
    }

    #[test]
    fn simple_i32_function() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
        }])]);

        let py = render_python_module(&api);
        assert!(
            py.contains("def add(a: int, b: int) -> int:"),
            "missing function signature: {py}"
        );
        assert!(
            py.contains("_lib.weaveffi_math_add"),
            "missing C symbol: {py}"
        );
        assert!(
            py.contains("ctypes.c_int32, ctypes.c_int32"),
            "missing argtypes: {py}"
        );
        assert!(
            py.contains("_fn.restype = ctypes.c_int32"),
            "missing restype: {py}"
        );
        assert!(
            py.contains("_check_error(_err)"),
            "missing error check: {py}"
        );
        assert!(py.contains("return _result"), "missing return: {py}");
    }

    #[test]
    fn string_function_encode_decode() {
        let api = make_api(vec![Module {
            name: "text".into(),
            functions: vec![Function {
                name: "echo".into(),
                params: vec![Param {
                    name: "msg".into(),
                    ty: TypeRef::StringUtf8,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(
            py.contains("def echo(msg: str) -> str:"),
            "missing signature: {py}"
        );
        assert!(py.contains("ctypes.c_char_p"), "missing c_char_p: {py}");
        assert!(
            py.contains("msg.encode(\"utf-8\")"),
            "missing encode call: {py}"
        );
        assert!(
            py.contains("_result.decode(\"utf-8\")"),
            "missing decode call: {py}"
        );
    }

    #[test]
    fn void_function() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "reset".into(),
            params: vec![],
            returns: None,
            doc: None,
            r#async: false,
        }])]);

        let py = render_python_module(&api);
        assert!(
            py.contains("def reset() -> None:"),
            "missing void signature: {py}"
        );
        assert!(
            py.contains("_fn.restype = None"),
            "missing None restype: {py}"
        );
        assert!(
            !py.contains("_result ="),
            "void function should not assign _result: {py}"
        );
    }

    #[test]
    fn enum_intenum_class() {
        let api = make_api(vec![Module {
            name: "paint".into(),
            functions: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: Some("Primary colors".into()),
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        value: 2,
                        doc: None,
                    },
                ],
            }],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(
            py.contains("class Color(IntEnum):"),
            "missing IntEnum class: {py}"
        );
        assert!(
            py.contains("\"\"\"Primary colors\"\"\""),
            "missing doc: {py}"
        );
        assert!(py.contains("Red = 0"), "missing Red: {py}");
        assert!(py.contains("Green = 1"), "missing Green: {py}");
        assert!(py.contains("Blue = 2"), "missing Blue: {py}");
    }

    #[test]
    fn enum_param_and_return() {
        let api = make_api(vec![Module {
            name: "paint".into(),
            functions: vec![Function {
                name: "mix".into(),
                params: vec![Param {
                    name: "a".into(),
                    ty: TypeRef::Enum("Color".into()),
                }],
                returns: Some(TypeRef::Enum("Color".into())),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(py.contains("a: \"Color\""), "missing enum param hint: {py}");
        assert!(
            py.contains("-> \"Color\":"),
            "missing enum return hint: {py}"
        );
        assert!(py.contains("a.value"), "missing .value conversion: {py}");
        assert!(
            py.contains("return Color(_result)"),
            "missing enum return wrap: {py}"
        );
    }

    #[test]
    fn struct_class_with_getters() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                    StructField {
                        name: "age".into(),
                        ty: TypeRef::I32,
                        doc: None,
                    },
                ],
            }],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(py.contains("class Contact:"), "missing class: {py}");
        assert!(
            py.contains("def __init__(self, _ptr: int)"),
            "missing __init__: {py}"
        );
        assert!(
            py.contains("self._ptr = _ptr"),
            "missing _ptr assignment: {py}"
        );
        assert!(py.contains("def __del__(self)"), "missing __del__: {py}");
        assert!(
            py.contains("weaveffi_contacts_Contact_destroy"),
            "missing destroy call: {py}"
        );
        assert!(
            py.contains("def name(self) -> str:"),
            "missing name getter: {py}"
        );
        assert!(
            py.contains("weaveffi_contacts_Contact_get_name"),
            "missing name getter C call: {py}"
        );
        assert!(
            py.contains("_result.decode(\"utf-8\")"),
            "missing string decode in getter: {py}"
        );
        assert!(
            py.contains("def age(self) -> int:"),
            "missing age getter: {py}"
        );
        assert!(
            py.contains("weaveffi_contacts_Contact_get_age"),
            "missing age getter C call: {py}"
        );
    }

    #[test]
    fn struct_return() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "get_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::Handle,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(
            py.contains("-> \"Contact\":"),
            "missing struct return hint: {py}"
        );
        assert!(
            py.contains("ctypes.c_void_p"),
            "missing void_p for struct: {py}"
        );
        assert!(
            py.contains("return Contact(_result)"),
            "missing struct wrapping: {py}"
        );
    }

    #[test]
    fn bool_uses_c_int32() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "is_valid".into(),
            params: vec![Param {
                name: "flag".into(),
                ty: TypeRef::Bool,
            }],
            returns: Some(TypeRef::Bool),
            doc: None,
            r#async: false,
        }])]);

        let py = render_python_module(&api);
        assert!(py.contains("flag: bool"), "missing bool param: {py}");
        assert!(py.contains("-> bool:"), "missing bool return: {py}");
        assert!(
            py.contains("ctypes.c_int32"),
            "missing c_int32 for Bool: {py}"
        );
        assert!(
            py.contains("1 if flag else 0"),
            "missing bool-to-int conversion: {py}"
        );
        assert!(
            py.contains("return bool(_result)"),
            "missing int-to-bool conversion: {py}"
        );
    }

    #[test]
    fn handle_uses_c_uint64() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "create".into(),
            params: vec![],
            returns: Some(TypeRef::Handle),
            doc: None,
            r#async: false,
        }])]);

        let py = render_python_module(&api);
        assert!(
            py.contains("ctypes.c_uint64"),
            "missing c_uint64 for Handle: {py}"
        );
    }

    #[test]
    fn bytes_param_and_return() {
        let api = make_api(vec![Module {
            name: "store".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "data".into(),
                    ty: TypeRef::Bytes,
                }],
                returns: Some(TypeRef::Bytes),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(py.contains("data: bytes"), "missing bytes param: {py}");
        assert!(py.contains("-> bytes:"), "missing bytes return: {py}");
        assert!(
            py.contains("ctypes.POINTER(ctypes.c_uint8)"),
            "missing uint8 pointer: {py}"
        );
        assert!(py.contains("ctypes.c_size_t"), "missing size_t: {py}");
        assert!(py.contains("_out_len"), "missing out_len: {py}");
    }

    #[test]
    fn optional_value_param_and_return() {
        let api = make_api(vec![Module {
            name: "store".into(),
            functions: vec![Function {
                name: "find".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(
            py.contains("id: Optional[int]"),
            "missing optional param: {py}"
        );
        assert!(
            py.contains("-> Optional[int]:"),
            "missing optional return: {py}"
        );
        assert!(
            py.contains("ctypes.POINTER(ctypes.c_int32)"),
            "missing POINTER for optional: {py}"
        );
        assert!(
            py.contains("ctypes.byref(ctypes.c_int32(id)) if id is not None else None"),
            "missing optional param conversion: {py}"
        );
        assert!(py.contains("return None"), "missing None return path: {py}");
        assert!(
            py.contains("return _result[0]"),
            "missing pointer deref: {py}"
        );
    }

    #[test]
    fn optional_string_return() {
        let api = make_api(vec![Module {
            name: "store".into(),
            functions: vec![Function {
                name: "get_name".into(),
                params: vec![],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(
            py.contains("-> Optional[str]:"),
            "missing optional str return: {py}"
        );
        assert!(
            py.contains("if _result is None:\n        return None"),
            "missing None check for optional string: {py}"
        );
        assert!(
            py.contains("_result.decode(\"utf-8\")"),
            "missing decode for optional string: {py}"
        );
    }

    #[test]
    fn list_param_and_return() {
        let api = make_api(vec![Module {
            name: "batch".into(),
            functions: vec![
                Function {
                    name: "process".into(),
                    params: vec![Param {
                        name: "ids".into(),
                        ty: TypeRef::List(Box::new(TypeRef::I32)),
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                },
                Function {
                    name: "get_ids".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                },
            ],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(py.contains("ids: List[int]"), "missing list param: {py}");
        assert!(py.contains("-> List[int]:"), "missing list return: {py}");
        assert!(
            py.contains("ctypes.c_int32 * len(ids)"),
            "missing ctypes array creation: {py}"
        );
        assert!(
            py.contains("_out_len"),
            "missing out_len for list return: {py}"
        );
        assert!(
            py.contains("for _i in range(_out_len.value)"),
            "missing list iteration: {py}"
        );
    }

    #[test]
    fn map_param_and_return() {
        let api = make_api(vec![Module {
            name: "store".into(),
            functions: vec![
                Function {
                    name: "update".into(),
                    params: vec![Param {
                        name: "scores".into(),
                        ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                },
                Function {
                    name: "get_scores".into(),
                    params: vec![],
                    returns: Some(TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::I32),
                    )),
                    doc: None,
                    r#async: false,
                },
            ],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(
            py.contains("scores: Dict[str, int]"),
            "missing map param: {py}"
        );
        assert!(
            py.contains("-> Dict[str, int]:"),
            "missing map return: {py}"
        );
        assert!(
            py.contains("list(scores.keys())"),
            "missing keys extraction: {py}"
        );
        assert!(py.contains("_out_keys"), "missing out_keys: {py}");
        assert!(py.contains("_out_values"), "missing out_values: {py}");
    }

    #[test]
    fn struct_optional_string_getter() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "email".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    doc: None,
                }],
            }],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(
            py.contains("def email(self) -> Optional[str]:"),
            "missing optional getter: {py}"
        );
        assert!(
            py.contains("return None"),
            "missing None return for optional getter: {py}"
        );
    }

    #[test]
    fn struct_enum_field_getter() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "role".into(),
                    ty: TypeRef::Enum("Role".into()),
                    doc: None,
                }],
            }],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(
            py.contains("def role(self) -> \"Role\":"),
            "missing enum getter: {py}"
        );
        assert!(
            py.contains("return Role(_result)"),
            "missing enum wrapping in getter: {py}"
        );
    }

    #[test]
    fn comprehensive_contacts_api() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            enums: vec![EnumDef {
                name: "ContactType".into(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Personal".into(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Work".into(),
                        value: 1,
                        doc: None,
                    },
                ],
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        doc: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                    },
                    StructField {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        doc: None,
                    },
                ],
            }],
            functions: vec![
                Function {
                    name: "create_contact".into(),
                    params: vec![
                        Param {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        },
                        Param {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                        },
                    ],
                    returns: Some(TypeRef::Handle),
                    doc: None,
                    r#async: false,
                },
                Function {
                    name: "get_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                    }],
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                },
                Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                },
                Function {
                    name: "count_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                },
            ],
            errors: None,
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_python_gen_contacts");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir).unwrap();

        let py = std::fs::read_to_string(tmp.join("python/weaveffi.py")).unwrap();

        assert!(py.contains("class ContactType(IntEnum):"));
        assert!(py.contains("Personal = 0"));
        assert!(py.contains("Work = 1"));

        assert!(py.contains("class Contact:"));
        assert!(py.contains("weaveffi_contacts_Contact_destroy"));
        assert!(py.contains("def id(self) -> int:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_id"));
        assert!(py.contains("def first_name(self) -> str:"));
        assert!(py.contains("def email(self) -> Optional[str]:"));
        assert!(py.contains("def contact_type(self) -> \"ContactType\":"));

        assert!(py.contains("def create_contact("));
        assert!(py.contains("weaveffi_contacts_create_contact"));
        assert!(py.contains("def get_contact(id: int) -> \"Contact\":"));
        assert!(py.contains("def list_contacts() -> List[\"Contact\"]:"));
        assert!(py.contains("def count_contacts() -> int:"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn type_hint_mapping() {
        assert_eq!(py_type_hint(&TypeRef::I32), "int");
        assert_eq!(py_type_hint(&TypeRef::U32), "int");
        assert_eq!(py_type_hint(&TypeRef::I64), "int");
        assert_eq!(py_type_hint(&TypeRef::F64), "float");
        assert_eq!(py_type_hint(&TypeRef::Bool), "bool");
        assert_eq!(py_type_hint(&TypeRef::StringUtf8), "str");
        assert_eq!(py_type_hint(&TypeRef::Bytes), "bytes");
        assert_eq!(py_type_hint(&TypeRef::Handle), "int");
        assert_eq!(py_type_hint(&TypeRef::Struct("Foo".into())), "\"Foo\"");
        assert_eq!(py_type_hint(&TypeRef::Enum("Bar".into())), "\"Bar\"");
        assert_eq!(
            py_type_hint(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "Optional[int]"
        );
        assert_eq!(
            py_type_hint(&TypeRef::List(Box::new(TypeRef::I32))),
            "List[int]"
        );
        assert_eq!(
            py_type_hint(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "Dict[str, int]"
        );
    }

    #[test]
    fn ctypes_scalar_mapping() {
        assert_eq!(py_ctypes_scalar(&TypeRef::I32), "ctypes.c_int32");
        assert_eq!(py_ctypes_scalar(&TypeRef::U32), "ctypes.c_uint32");
        assert_eq!(py_ctypes_scalar(&TypeRef::I64), "ctypes.c_int64");
        assert_eq!(py_ctypes_scalar(&TypeRef::F64), "ctypes.c_double");
        assert_eq!(py_ctypes_scalar(&TypeRef::Bool), "ctypes.c_int32");
        assert_eq!(py_ctypes_scalar(&TypeRef::StringUtf8), "ctypes.c_char_p");
        assert_eq!(py_ctypes_scalar(&TypeRef::Handle), "ctypes.c_uint64");
        assert_eq!(py_ctypes_scalar(&TypeRef::Bytes), "ctypes.c_uint8");
        assert_eq!(
            py_ctypes_scalar(&TypeRef::Struct("X".into())),
            "ctypes.c_void_p"
        );
        assert_eq!(
            py_ctypes_scalar(&TypeRef::Enum("X".into())),
            "ctypes.c_int32"
        );
    }

    #[test]
    fn list_struct_return() {
        let api = make_api(vec![Module {
            name: "store".into(),
            functions: vec![Function {
                name: "list_items".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Item".into())))),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(
            py.contains("-> List[\"Item\"]:"),
            "missing list struct return: {py}"
        );
        assert!(
            py.contains("Item(_result[_i])"),
            "missing struct wrapping in list: {py}"
        );
    }

    #[test]
    fn struct_bytes_field_getter() {
        let api = make_api(vec![Module {
            name: "storage".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Blob".into(),
                doc: None,
                fields: vec![StructField {
                    name: "data".into(),
                    ty: TypeRef::Bytes,
                    doc: None,
                }],
            }],
            enums: vec![],
            errors: None,
        }]);

        let py = render_python_module(&api);
        assert!(
            py.contains("def data(self) -> bytes:"),
            "missing bytes getter: {py}"
        );
        assert!(
            py.contains("_out_len = ctypes.c_size_t(0)"),
            "missing out_len in bytes getter: {py}"
        );
        assert!(
            py.contains("_result[:_out_len.value]"),
            "missing bytes slice: {py}"
        );
    }
}
