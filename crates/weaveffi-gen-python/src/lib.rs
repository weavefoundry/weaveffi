//! Python (`ctypes`) binding generator for WeaveFFI.
//!
//! Emits a pip-installable package containing `ctypes`-based bindings and
//! `.pyi` type stubs over the C ABI. Async functions surface as
//! `async def` wrappers. Implements the [`Generator`] trait.

use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::{c_symbol_name, local_type_name, wrapper_name};
use weaveffi_ir::ir::{Api, EnumDef, Function, Module, StructDef, StructField, TypeRef};

pub struct PythonGenerator;

impl PythonGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        package_name: &str,
        strip_module_prefix: bool,
    ) -> Result<()> {
        let dir = out_dir.join("python");
        let pkg_dir = dir.join(package_name);
        std::fs::create_dir_all(&pkg_dir)?;
        std::fs::write(
            pkg_dir.join("__init__.py"),
            "from .weaveffi import *  # noqa: F401,F403\n",
        )?;
        std::fs::write(
            pkg_dir.join("weaveffi.py"),
            render_python_module(api, strip_module_prefix),
        )?;
        std::fs::write(
            pkg_dir.join("weaveffi.pyi"),
            render_pyi_module(api, strip_module_prefix),
        )?;
        std::fs::write(
            dir.join("pyproject.toml"),
            render_pyproject_toml(package_name),
        )?;
        std::fs::write(dir.join("setup.py"), render_setup_py(package_name))?;
        std::fs::write(dir.join("README.md"), render_readme())?;
        Ok(())
    }
}

impl Generator for PythonGenerator {
    fn name(&self) -> &'static str {
        "python"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "weaveffi", true)
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(
            api,
            out_dir,
            config.python_package_name(),
            config.strip_module_prefix,
        )
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        let pkg = "weaveffi";
        vec![
            out_dir
                .join(format!("python/{pkg}/__init__.py"))
                .to_string(),
            out_dir
                .join(format!("python/{pkg}/weaveffi.py"))
                .to_string(),
            out_dir
                .join(format!("python/{pkg}/weaveffi.pyi"))
                .to_string(),
            out_dir.join("python/pyproject.toml").to_string(),
            out_dir.join("python/setup.py").to_string(),
            out_dir.join("python/README.md").to_string(),
        ]
    }
}

// ── Type helpers ──

fn is_c_pointer_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::StringUtf8
            | TypeRef::BorrowedStr
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes
            | TypeRef::Struct(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
            | TypeRef::Iterator(_)
    )
}

fn snake_to_pascal(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(c).collect(),
            }
        })
        .collect()
}

fn iter_type_name(func_name: &str, module: &str) -> String {
    let pascal = snake_to_pascal(func_name);
    format!("weaveffi_{module}_{pascal}Iterator")
}

fn py_ctypes_scalar(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 => "ctypes.c_int32",
        TypeRef::U32 => "ctypes.c_uint32",
        TypeRef::I64 => "ctypes.c_int64",
        TypeRef::F64 => "ctypes.c_double",
        TypeRef::Bool => "ctypes.c_int32",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "ctypes.c_char_p",
        TypeRef::Handle => "ctypes.c_uint64",
        TypeRef::TypedHandle(_) => "ctypes.c_void_p",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "ctypes.c_uint8",
        TypeRef::Struct(_) => "ctypes.c_void_p",
        TypeRef::Enum(_) => "ctypes.c_int32",
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) | TypeRef::Iterator(_) => {
            "ctypes.c_void_p"
        }
    }
}

fn py_type_hint(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::Handle => "int".into(),
        TypeRef::TypedHandle(name) => format!("\"{}\"", name),
        TypeRef::F64 => "float".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "str".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "bytes".into(),
        TypeRef::Enum(name) => format!("\"{}\"", name),
        TypeRef::Struct(name) => format!("\"{}\"", local_type_name(name)),
        TypeRef::Optional(inner) => format!("Optional[{}]", py_type_hint(inner)),
        TypeRef::List(inner) => format!("List[{}]", py_type_hint(inner)),
        TypeRef::Map(k, v) => format!("Dict[{}, {}]", py_type_hint(k), py_type_hint(v)),
        TypeRef::Iterator(inner) => format!("Iterator[{}]", py_type_hint(inner)),
    }
}

fn py_param_argtypes(ty: &TypeRef) -> Vec<String> {
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => vec![
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
        TypeRef::Bytes | TypeRef::BorrowedBytes => (
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

/// `(param_name, ctypes_type)` pairs for async C callback parameters after `(context, err)`.
fn py_async_cb_trailing_fields(ret: &Option<TypeRef>) -> Vec<(String, String)> {
    match ret {
        None => vec![],
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => vec![
            ("result".into(), "ctypes.POINTER(ctypes.c_uint8)".into()),
            ("result_len".into(), "ctypes.c_size_t".into()),
        ],
        Some(TypeRef::List(inner)) => vec![
            (
                "result".into(),
                format!("ctypes.POINTER({})", py_ctypes_scalar(inner)),
            ),
            ("result_len".into(), "ctypes.c_size_t".into()),
        ],
        Some(TypeRef::Map(k, v)) => vec![
            (
                "result_keys".into(),
                format!("ctypes.POINTER({})", py_ctypes_scalar(k)),
            ),
            (
                "result_values".into(),
                format!("ctypes.POINTER({})", py_ctypes_scalar(v)),
            ),
            ("result_len".into(), "ctypes.c_size_t".into()),
        ],
        Some(TypeRef::Optional(inner)) => {
            if is_c_pointer_type(inner) {
                py_async_cb_trailing_fields(&Some(*inner.clone()))
            } else {
                vec![(
                    "result".into(),
                    format!("ctypes.POINTER({})", py_ctypes_scalar(inner)),
                )]
            }
        }
        Some(ty) => vec![("result".into(), py_ctypes_scalar(ty).to_string())],
    }
}

fn append_async_success_handler(out: &mut String, ret: &Option<TypeRef>, ind: &str) {
    match ret {
        None => {
            out.push_str(&format!("{ind}_state[\"val\"] = None\n"));
        }
        Some(TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Handle) => {
            out.push_str(&format!("{ind}_state[\"val\"] = result\n"));
        }
        Some(TypeRef::Bool) => {
            out.push_str(&format!("{ind}_state[\"val\"] = bool(result)\n"));
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            out.push_str(&format!(
                "{ind}_s = _bytes_to_string(result) or \"\" if result else \"\"\n"
            ));
            out.push_str(&format!("{ind}if result:\n"));
            out.push_str(&format!("{ind}    _lib.weaveffi_free_string(result)\n"));
            out.push_str(&format!("{ind}_state[\"val\"] = _s\n"));
        }
        Some(TypeRef::Enum(name)) => {
            out.push_str(&format!("{ind}_state[\"val\"] = {name}(result)\n"));
        }
        Some(TypeRef::Struct(name)) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}if result is None:\n"));
            out.push_str(&format!(
                "{ind}    _state[\"err\"] = WeaveffiError(-1, \"null pointer\")\n"
            ));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = {name}(result)\n"));
        }
        Some(TypeRef::TypedHandle(name)) => {
            out.push_str(&format!("{ind}if result is None:\n"));
            out.push_str(&format!(
                "{ind}    _state[\"err\"] = WeaveffiError(-1, \"null pointer\")\n"
            ));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = {name}(result)\n"));
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            out.push_str(&format!("{ind}if not result:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = b\"\"\n"));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _n = int(result_len)\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = bytes(result[:_n])\n"));
            out.push_str(&format!(
                "{ind}    _lib.weaveffi_free_bytes(result, ctypes.c_size_t(_n))\n"
            ));
        }
        Some(TypeRef::List(inner)) => {
            let elem = py_read_element("result[_i]", inner);
            out.push_str(&format!("{ind}if not result:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = []\n"));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _rl = int(result_len)\n"));
            out.push_str(&format!(
                "{ind}    _state[\"val\"] = [{elem} for _i in range(_rl)]\n"
            ));
        }
        Some(TypeRef::Map(k, v)) => {
            let kread = py_read_element("result_keys[_i]", k);
            let vread = py_read_element("result_values[_i]", v);
            out.push_str(&format!("{ind}if not result_keys or not result_values:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = {{}}\n"));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _ml = int(result_len)\n"));
            out.push_str(&format!(
                "{ind}    _state[\"val\"] = {{{kread}: {vread} for _i in range(_ml)}}\n"
            ));
        }
        Some(TypeRef::Optional(inner)) => {
            if is_c_pointer_type(inner) {
                match inner.as_ref() {
                    TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _s = _bytes_to_string(result)\n"));
                        out.push_str(&format!("{ind}    _lib.weaveffi_free_string(result)\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = _s\n"));
                    }
                    TypeRef::Struct(name) => {
                        let name = local_type_name(name);
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = {name}(result)\n"));
                    }
                    TypeRef::TypedHandle(name) => {
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = {name}(result)\n"));
                    }
                    TypeRef::Bytes | TypeRef::BorrowedBytes => {
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _n = int(result_len)\n"));
                        out.push_str(&format!("{ind}    _b = bytes(result[:_n])\n"));
                        out.push_str(&format!(
                            "{ind}    _lib.weaveffi_free_bytes(result, ctypes.c_size_t(_n))\n"
                        ));
                        out.push_str(&format!("{ind}    _state[\"val\"] = _b\n"));
                    }
                    TypeRef::List(elem) => {
                        let read = py_read_element("result[_i]", elem);
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _rl = int(result_len)\n"));
                        out.push_str(&format!(
                            "{ind}    _state[\"val\"] = [{read} for _i in range(_rl)]\n"
                        ));
                    }
                    TypeRef::Map(k, v) => {
                        let kread = py_read_element("result_keys[_i]", k);
                        let vread = py_read_element("result_values[_i]", v);
                        out.push_str(&format!("{ind}if not result_keys or not result_values:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _ml = int(result_len)\n"));
                        out.push_str(&format!(
                            "{ind}    _state[\"val\"] = {{{kread}: {vread} for _i in range(_ml)}}\n"
                        ));
                    }
                    _ => append_async_success_handler(out, &Some(*inner.clone()), ind),
                }
            } else {
                match inner.as_ref() {
                    TypeRef::Bool => {
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = bool(result[0])\n"));
                    }
                    TypeRef::Enum(name) => {
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = {name}(result[0])\n"));
                    }
                    _ => {
                        out.push_str(&format!("{ind}if not result:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = result[0]\n"));
                    }
                }
            }
        }
        Some(TypeRef::Iterator(_)) => todo!("async iterator return"),
    }
}

fn render_async_ffi_call_body(out: &mut String, module_name: &str, f: &Function) {
    let c_sym = c_symbol_name(module_name, &f.name);
    let c_async = format!("{c_sym}_async");
    let ind = "    ";

    out.push_str(&format!("{ind}_fn = _lib.{c_async}\n"));
    out.push_str(&format!("{ind}_ev = threading.Event()\n"));
    out.push_str(&format!("{ind}_state = {{\"err\": None, \"val\": None}}\n"));

    let trailing = py_async_cb_trailing_fields(&f.returns);
    let mut cb_param_list: Vec<String> = vec!["context".into(), "err".into()];
    cb_param_list.extend(trailing.iter().map(|(n, _)| n.clone()));
    let cb_params_joined = cb_param_list.join(", ");

    out.push_str(&format!("{ind}def _cb_impl({cb_params_joined}):\n"));
    out.push_str(&format!("{ind}    try:\n"));
    out.push_str(&format!(
        "{ind}        if err and err.contents.code != 0:\n"
    ));
    out.push_str(&format!("{ind}            _code = err.contents.code\n"));
    out.push_str(&format!(
        "{ind}            _msg = err.contents.message.decode(\"utf-8\") if err.contents.message else \"\"\n"
    ));
    out.push_str(&format!(
        "{ind}            _lib.weaveffi_error_clear(ctypes.byref(err.contents))\n"
    ));
    out.push_str(&format!(
        "{ind}            _state[\"err\"] = WeaveffiError(_code, _msg)\n"
    ));
    out.push_str(&format!("{ind}        else:\n"));
    append_async_success_handler(out, &f.returns, "                ");
    out.push_str(&format!("{ind}    finally:\n"));
    out.push_str(&format!("{ind}        _ev.set()\n"));

    let mut cf_parts: Vec<String> = vec![
        "ctypes.c_void_p".into(),
        "ctypes.POINTER(_WeaveffiErrorStruct)".into(),
    ];
    cf_parts.extend(trailing.iter().map(|(_, t)| t.clone()));
    out.push_str(&format!(
        "{ind}_cb_type = ctypes.CFUNCTYPE(None, {})\n",
        cf_parts.join(", ")
    ));
    out.push_str(&format!("{ind}_cb = _cb_type(_cb_impl)\n"));

    let mut argtypes: Vec<String> = Vec::new();
    for p in &f.params {
        argtypes.extend(py_param_argtypes(&p.ty));
    }
    if f.cancellable {
        argtypes.push("ctypes.c_void_p".into());
    }
    argtypes.push("_cb_type".into());
    argtypes.push("ctypes.c_void_p".into());

    out.push_str(&format!("{ind}_fn.argtypes = [{}]\n", argtypes.join(", ")));
    out.push_str(&format!("{ind}_fn.restype = None\n"));

    for p in &f.params {
        for line in py_param_conversion(&p.name, &p.ty, ind) {
            out.push_str(&line);
            out.push('\n');
        }
    }

    let mut call_args: Vec<String> = Vec::new();
    for p in &f.params {
        call_args.extend(py_param_call_args(&p.name, &p.ty));
    }
    if f.cancellable {
        call_args.push("None".into());
    }
    call_args.push("_cb".into());
    call_args.push("None".into());

    out.push_str(&format!("{ind}_fn({})\n", call_args.join(", ")));
    out.push_str(&format!("{ind}_ev.wait()\n"));
    out.push_str(&format!("{ind}if _state[\"err\"] is not None:\n"));
    out.push_str(&format!("{ind}    raise _state[\"err\"]\n"));
    if f.returns.is_some() {
        out.push_str(&format!("{ind}return _state[\"val\"]\n"));
    }
}

// ── Rendering ──

fn render_python_module(api: &Api, strip_module_prefix: bool) -> String {
    let mut out = String::new();
    render_preamble(&mut out);
    let has_async = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async));
    if has_async {
        out.push_str("\nimport asyncio\nimport threading\n");
    }
    for m in &api.modules {
        render_python_module_content(&mut out, m, &m.name, strip_module_prefix);
    }
    out.push('\n');
    out
}

fn collect_all_modules(modules: &[Module]) -> Vec<&Module> {
    let mut all = Vec::new();
    for m in modules {
        all.push(m);
        all.extend(collect_all_modules(&m.modules));
    }
    all
}

fn collect_modules_with_path(modules: &[Module]) -> Vec<(&Module, String)> {
    let mut result = Vec::new();
    for m in modules {
        collect_module_with_path(m, &m.name, &mut result);
    }
    result
}

fn collect_module_with_path<'a>(m: &'a Module, path: &str, out: &mut Vec<(&'a Module, String)>) {
    out.push((m, path.to_string()));
    for sub in &m.modules {
        collect_module_with_path(sub, &format!("{path}_{}", sub.name), out);
    }
}

fn render_python_module_content(
    out: &mut String,
    m: &Module,
    module_path: &str,
    strip_module_prefix: bool,
) {
    out.push_str(&format!("\n\n# === Module: {} ===", module_path));
    for e in &m.enums {
        render_enum(out, e);
    }
    for s in &m.structs {
        render_struct(out, module_path, s);
        if s.builder {
            render_builder(out, s);
        }
    }
    for f in &m.functions {
        render_function(out, module_path, f, strip_module_prefix);
    }
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        render_python_module_content(out, sub, &sub_path, strip_module_prefix);
    }
}

/// Emits a Python `# ...` line comment at `indent`. Used above C ABI binding
/// declarations (`attach_function`-style binds) where docstrings can't live.
fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    let Some(doc) = doc else {
        return;
    };
    let doc = doc.trim();
    if doc.is_empty() {
        return;
    }
    for line in doc.lines() {
        out.push_str(indent);
        if line.is_empty() {
            out.push_str("#\n");
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
}

/// Emits a Python triple-quoted `"""..."""` docstring as the first statement
/// of a class or function body, at the given `indent`.
fn emit_docstring(out: &mut String, doc: &Option<String>, indent: &str) {
    let Some(doc) = doc else {
        return;
    };
    let doc = doc.trim();
    if doc.is_empty() {
        return;
    }
    if doc.contains('\n') {
        out.push_str(indent);
        out.push_str("\"\"\"\n");
        for line in doc.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str(indent);
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(indent);
        out.push_str("\"\"\"\n");
    } else {
        out.push_str(indent);
        out.push_str("\"\"\"");
        out.push_str(doc);
        out.push_str("\"\"\"\n");
    }
}

/// Emits a NumPy/Google-style docstring with a `Parameters` section listing
/// each parameter that has a `doc:` value. Skips entirely when there is
/// nothing to document.
fn emit_fn_docstring(
    out: &mut String,
    doc: &Option<String>,
    params: &[weaveffi_ir::ir::Param],
    indent: &str,
) {
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    let documented_params: Vec<&weaveffi_ir::ir::Param> = params
        .iter()
        .filter(|p| {
            p.doc
                .as_ref()
                .map(|d| !d.trim().is_empty())
                .unwrap_or(false)
        })
        .collect();
    if trimmed_doc.is_none() && documented_params.is_empty() {
        return;
    }
    out.push_str(indent);
    out.push_str("\"\"\"");
    if let Some(d) = trimmed_doc {
        if d.contains('\n') {
            out.push('\n');
            for line in d.lines() {
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str(indent);
                    out.push_str(line);
                    out.push('\n');
                }
            }
        } else {
            out.push_str(d);
            out.push('\n');
        }
    } else {
        out.push('\n');
    }
    if !documented_params.is_empty() {
        out.push('\n');
        out.push_str(indent);
        out.push_str("Parameters\n");
        out.push_str(indent);
        out.push_str("----------\n");
        for p in documented_params {
            let pdoc = p.doc.as_ref().unwrap().trim();
            let mut lines = pdoc.lines();
            let first = lines.next().unwrap_or("");
            out.push_str(indent);
            out.push_str(&format!("{} : {}\n", p.name, first));
            for line in lines {
                out.push_str(indent);
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str("    ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out.push_str(indent);
    out.push_str("\"\"\"\n");
}

fn render_preamble(out: &mut String) {
    out.push_str(
        r#""""WeaveFFI Python ctypes bindings (auto-generated)"""
import contextlib
import ctypes
import platform
from enum import IntEnum
from typing import Dict, Iterator, List, Optional


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


class _PointerGuard(contextlib.AbstractContextManager):
    def __init__(self, ptr, free_fn) -> None:
        self.ptr = ptr
        self._free_fn = free_fn

    def __exit__(self, *exc) -> bool:
        if self.ptr is not None:
            self._free_fn(self.ptr)
            self.ptr = None
        return False


def _string_to_bytes(s: Optional[str]) -> Optional[bytes]:
    if s is None:
        return None
    return s.encode("utf-8")


def _bytes_to_string(ptr) -> Optional[str]:
    if ptr is None:
        return None
    return ptr.decode("utf-8")
"#,
    );
}

fn render_enum(out: &mut String, e: &EnumDef) {
    out.push_str(&format!("\n\nclass {}(IntEnum):\n", e.name));
    emit_docstring(out, &e.doc, "    ");
    for v in &e.variants {
        if let Some(d) = &v.doc {
            let trimmed = d.trim();
            if !trimmed.is_empty() {
                for line in trimmed.lines() {
                    out.push_str(&format!("    # {}\n", line));
                }
            }
        }
        out.push_str(&format!("    {} = {}\n", v.name, v.value));
    }
}

fn render_struct(out: &mut String, module_name: &str, s: &StructDef) {
    let prefix = format!("weaveffi_{}_{}", module_name, s.name);

    out.push_str(&format!("\n\nclass {}:\n", s.name));
    emit_docstring(out, &s.doc, "    ");

    out.push_str("\n    def __init__(self, _ptr: int) -> None:");
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

fn render_builder(out: &mut String, s: &StructDef) {
    let builder_name = format!("{}Builder", s.name);
    out.push_str(&format!("\n\nclass {}:\n", builder_name));
    emit_docstring(out, &s.doc, "    ");
    out.push_str("    def __init__(self) -> None:");
    for field in &s.fields {
        let py_ty = py_type_hint(&field.ty);
        out.push_str(&format!(
            "\n        self._{}: Optional[{}] = None",
            field.name, py_ty
        ));
    }
    for field in &s.fields {
        let py_ty = py_type_hint(&field.ty);
        out.push_str(&format!(
            "\n\n    def with_{}(self, value: {}) -> \"{}\":",
            field.name, py_ty, builder_name
        ));
        if let Some(d) = &field.doc {
            let trimmed = d.trim();
            if !trimmed.is_empty() {
                if trimmed.contains('\n') {
                    out.push_str("\n        \"\"\"\n");
                    for line in trimmed.lines() {
                        if line.is_empty() {
                            out.push('\n');
                        } else {
                            out.push_str("        ");
                            out.push_str(line);
                            out.push('\n');
                        }
                    }
                    out.push_str("        \"\"\"");
                } else {
                    out.push_str(&format!("\n        \"\"\"{}\"\"\"", trimmed));
                }
            }
        }
        out.push_str(&format!("\n        self._{} = value", field.name));
        out.push_str("\n        return self");
    }
    let ret_ty = py_type_hint(&TypeRef::Struct(s.name.clone()));
    out.push_str(&format!("\n\n    def build(self) -> {}:", ret_ty));
    for field in &s.fields {
        out.push_str(&format!(
            "\n        if self._{} is None:\n            raise ValueError(\"missing field: {}\")",
            field.name, field.name
        ));
    }
    out.push_str(&format!(
        "\n        raise NotImplementedError(\"{}Builder.build requires FFI backing\")",
        s.name
    ));
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
    emit_docstring(out, &field.doc, ind);
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

fn render_function(out: &mut String, module_name: &str, f: &Function, strip_module_prefix: bool) {
    let func_name = wrapper_name(module_name, &f.name, strip_module_prefix);
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

    let def_name = if f.r#async {
        format!("_{func_name}_sync")
    } else {
        func_name.clone()
    };

    if let Some(TypeRef::Iterator(inner)) = &f.returns {
        render_iterator_class(out, module_name, &f.name, inner);
    }

    out.push_str(&format!(
        "\n\ndef {}({}) -> {}:\n",
        def_name,
        params_sig.join(", "),
        ret_hint
    ));

    let ind = "    ";

    emit_fn_docstring(out, &f.doc, &f.params, ind);

    if let Some(msg) = &f.deprecated {
        out.push_str(&format!(
            "{ind}import warnings\n{ind}warnings.warn(\"{}\", DeprecationWarning, stacklevel=2)\n",
            msg.replace('"', "\\\"")
        ));
    }

    if f.r#async {
        render_async_ffi_call_body(out, module_name, f);
    } else {
        let c_sym = c_symbol_name(module_name, &f.name);
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
            if let TypeRef::Iterator(inner) = ret_ty {
                render_iterator_return(out, module_name, &f.name, inner, ind);
            } else {
                render_return_value(out, ret_ty, ind);
            }
        }
    }

    if f.r#async {
        let params_joined = params_sig.join(", ");
        out.push_str(&format!(
            "\n\nasync def {}({}) -> {}:\n",
            func_name, params_joined, ret_hint
        ));
        emit_fn_docstring(out, &f.doc, &f.params, ind);
        out.push_str("    _loop = asyncio.get_event_loop()\n");
        let arg_names: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
        let executor_args = if arg_names.is_empty() {
            def_name
        } else {
            format!("{def_name}, {}", arg_names.join(", "))
        };
        if f.returns.is_some() {
            out.push_str(&format!(
                "    return await _loop.run_in_executor(None, {executor_args})\n"
            ));
        } else {
            out.push_str(&format!(
                "    await _loop.run_in_executor(None, {executor_args})\n"
            ));
        }
    }
}

// ── Param helpers ──

fn py_list_convert_expr(name: &str, elem: &TypeRef) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("*[_string_to_bytes(v) for v in {name}]")
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => format!("*[v._ptr for v in {name}]"),
        TypeRef::Enum(_) => format!("*[v.value for v in {name}]"),
        TypeRef::Bool => format!("*[1 if v else 0 for v in {name}]"),
        _ => format!("*{name}"),
    }
}

fn py_map_elem_convert(list_name: &str, ty: &TypeRef, var: &str) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("*[_string_to_bytes({var}) for {var} in {list_name}]")
        }
        TypeRef::Enum(_) => format!("*[{var}.value for {var} in {list_name}]"),
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
            format!("*[{var}._ptr for {var} in {list_name}]")
        }
        TypeRef::Bool => format!("*[1 if {var} else 0 for {var} in {list_name}]"),
        _ => format!("*{list_name}"),
    }
}

fn py_param_conversion(name: &str, ty: &TypeRef, ind: &str) -> Vec<String> {
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
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
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                vec![format!("{ind}_{name}_c = _string_to_bytes({name})")]
            }
            TypeRef::Enum(_) => {
                vec![format!(
                    "{ind}_{name}_c = ctypes.byref(ctypes.c_int32({name}.value)) if {name} is not None else None"
                )]
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
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
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![format!("_string_to_bytes({name})")],
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![format!("_{name}_arr"), format!("len({name})")]
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => vec![format!("{name}._ptr")],
        TypeRef::Enum(_) => vec![format!("{name}.value")],
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![format!("_{name}_c")],
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                vec![format!("{name}._ptr if {name} is not None else None")]
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => {
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
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
    }
}

// ── Return helpers ──

fn py_read_element(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("_bytes_to_string({expr})"),
        TypeRef::Struct(name) => {
            let name = local_type_name(name);
            format!("{name}({expr})")
        }
        TypeRef::TypedHandle(name) => format!("{name}({expr})"),
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
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{ind}return _bytes_to_string(_result) or \"\"\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return b\"\"\n"));
            out.push_str(&format!("{ind}return bytes(_result[:_out_len.value])\n"));
        }
        TypeRef::Struct(name) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}if _result is None:\n"));
            out.push_str(&format!(
                "{ind}    raise WeaveffiError(-1, \"null pointer\")\n"
            ));
            out.push_str(&format!("{ind}return {name}(_result)\n"));
        }
        TypeRef::TypedHandle(name) => {
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
        TypeRef::Iterator(_) => unreachable!("iterator return handled in render_function"),
    }
}

fn render_optional_return(out: &mut String, inner: &TypeRef, ind: &str) {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{ind}return _bytes_to_string(_result)\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return bytes(_result[:_out_len.value])\n"));
        }
        TypeRef::Struct(name) => {
            let name = local_type_name(name);
            out.push_str(&format!("{ind}if _result is None:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}return {name}(_result)\n"));
        }
        TypeRef::TypedHandle(name) => {
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

// ── Iterator helpers ──

fn py_read_iter_item(inner: &TypeRef) -> String {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "_bytes_to_string(_out_item.value)".into(),
        TypeRef::Struct(name) => {
            let name = local_type_name(name);
            format!("{name}(_out_item.value)")
        }
        TypeRef::TypedHandle(name) => format!("{name}(_out_item.value)"),
        TypeRef::Enum(name) => format!("{name}(_out_item.value)"),
        TypeRef::Bool => "bool(_out_item.value)".into(),
        _ => "_out_item.value".into(),
    }
}

fn render_iterator_class(out: &mut String, module_name: &str, func_name: &str, inner: &TypeRef) {
    let iter_tag = iter_type_name(func_name, module_name);
    let pascal = snake_to_pascal(func_name);
    let class_name = format!("_{pascal}Iterator");
    let item_scalar = py_ctypes_scalar(inner);
    let read_expr = py_read_iter_item(inner);

    out.push_str(&format!("\n\nclass {class_name}:"));
    out.push_str("\n    def __init__(self, ptr):");
    out.push_str("\n        self._ptr = ptr");
    out.push_str("\n        self._done = False");

    out.push_str("\n\n    def __iter__(self):");
    out.push_str("\n        return self");

    out.push_str("\n\n    def __next__(self):");
    out.push_str("\n        if self._done:");
    out.push_str("\n            raise StopIteration");
    out.push_str(&format!("\n        _next_fn = _lib.{iter_tag}_next"));
    out.push_str(&format!(
        "\n        _next_fn.argtypes = [ctypes.c_void_p, ctypes.POINTER({item_scalar}), ctypes.POINTER(_WeaveffiErrorStruct)]"
    ));
    out.push_str("\n        _next_fn.restype = ctypes.c_int32");
    out.push_str(&format!("\n        _out_item = {item_scalar}()"));
    out.push_str("\n        _err = _WeaveffiErrorStruct()");
    out.push_str(
        "\n        _has = _next_fn(self._ptr, ctypes.byref(_out_item), ctypes.byref(_err))",
    );
    out.push_str("\n        _check_error(_err)");
    out.push_str("\n        if not _has:");
    out.push_str("\n            self._done = True");
    out.push_str("\n            self._destroy()");
    out.push_str("\n            raise StopIteration");
    out.push_str(&format!("\n        return {read_expr}"));

    out.push_str("\n\n    def _destroy(self):");
    out.push_str("\n        if self._ptr is not None:");
    out.push_str(&format!(
        "\n            _destroy_fn = _lib.{iter_tag}_destroy"
    ));
    out.push_str("\n            _destroy_fn.argtypes = [ctypes.c_void_p]");
    out.push_str("\n            _destroy_fn.restype = None");
    out.push_str("\n            _destroy_fn(self._ptr)");
    out.push_str("\n            self._ptr = None");

    out.push_str("\n\n    def __del__(self):");
    out.push_str("\n        self._destroy()");
    out.push('\n');
}

fn render_iterator_return(
    out: &mut String,
    module_name: &str,
    func_name: &str,
    inner: &TypeRef,
    ind: &str,
) {
    let iter_tag = iter_type_name(func_name, module_name);
    let item_scalar = py_ctypes_scalar(inner);
    let read_expr = py_read_iter_item(inner);

    out.push_str(&format!("{ind}_next_fn = _lib.{iter_tag}_next\n"));
    out.push_str(&format!(
        "{ind}_next_fn.argtypes = [ctypes.c_void_p, ctypes.POINTER({item_scalar}), ctypes.POINTER(_WeaveffiErrorStruct)]\n"
    ));
    out.push_str(&format!("{ind}_next_fn.restype = ctypes.c_int32\n"));

    out.push_str(&format!("{ind}_destroy_fn = _lib.{iter_tag}_destroy\n"));
    out.push_str(&format!("{ind}_destroy_fn.argtypes = [ctypes.c_void_p]\n"));
    out.push_str(&format!("{ind}_destroy_fn.restype = None\n"));

    out.push_str(&format!("{ind}_items = []\n"));
    out.push_str(&format!("{ind}while True:\n"));
    out.push_str(&format!("{ind}    _out_item = {item_scalar}()\n"));
    out.push_str(&format!("{ind}    _item_err = _WeaveffiErrorStruct()\n"));
    out.push_str(&format!(
        "{ind}    _has = _next_fn(_result, ctypes.byref(_out_item), ctypes.byref(_item_err))\n"
    ));
    out.push_str(&format!("{ind}    _check_error(_item_err)\n"));
    out.push_str(&format!("{ind}    if not _has:\n"));
    out.push_str(&format!("{ind}        break\n"));
    out.push_str(&format!("{ind}    _items.append({read_expr})\n"));

    out.push_str(&format!("{ind}_destroy_fn(_result)\n"));
    out.push_str(&format!("{ind}return _items\n"));
}

// ── Packaging ──

fn render_pyproject_toml(package_name: &str) -> String {
    format!(
        r#"[build-system]
requires = ["setuptools>=61.0"]
build-backend = "setuptools.build_meta"

[project]
name = "{package_name}"
version = "0.1.0"
description = "Python bindings for WeaveFFI (auto-generated)"
requires-python = ">=3.8"

[tool.setuptools]
packages = ["{package_name}"]
"#,
    )
}

fn render_setup_py(package_name: &str) -> String {
    format!(
        r#"from setuptools import setup

setup(
    name="{package_name}",
    version="0.1.0",
    packages=["{package_name}"],
)
"#,
    )
}

fn render_readme() -> &'static str {
    r#"# WeaveFFI Python Bindings

Auto-generated Python bindings using ctypes.

## Prerequisites

- Python >= 3.8
- The compiled shared library (`libweaveffi.so`, `libweaveffi.dylib`, or `weaveffi.dll`) available on your library search path.

## Install

```bash
pip install .
```

## Development install

```bash
pip install -e .
```

## Usage

```python
from weaveffi import *
```
"#
}

// ── Type stub (.pyi) rendering ──

fn render_pyi_module(api: &Api, strip_module_prefix: bool) -> String {
    let mut out = String::from(
        "from enum import IntEnum\nfrom typing import Dict, Iterator, List, Optional\n",
    );
    for (m, path) in collect_modules_with_path(&api.modules) {
        for e in &m.enums {
            render_pyi_enum(&mut out, e);
        }
        for s in &m.structs {
            render_pyi_struct(&mut out, s);
        }
        for f in &m.functions {
            render_pyi_function(&mut out, &path, f, strip_module_prefix);
        }
    }
    out
}

fn render_pyi_enum(out: &mut String, e: &EnumDef) {
    out.push('\n');
    emit_doc(out, &e.doc, "");
    out.push_str(&format!("class {}(IntEnum):\n", e.name));
    for v in &e.variants {
        emit_doc(out, &v.doc, "    ");
        out.push_str(&format!("    {}: int\n", v.name));
    }
}

fn render_pyi_struct(out: &mut String, s: &StructDef) {
    out.push('\n');
    emit_doc(out, &s.doc, "");
    out.push_str(&format!("class {}:\n", s.name));
    for field in &s.fields {
        let py_ty = py_type_hint(&field.ty);
        emit_doc(out, &field.doc, "    ");
        out.push_str(&format!(
            "    @property\n    def {}(self) -> {}: ...\n",
            field.name, py_ty
        ));
    }
}

fn render_pyi_function(
    out: &mut String,
    module_name: &str,
    f: &Function,
    strip_module_prefix: bool,
) {
    let func_name = wrapper_name(module_name, &f.name, strip_module_prefix);
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, py_type_hint(&p.ty)))
        .collect();
    let ret = f
        .returns
        .as_ref()
        .map(py_type_hint)
        .unwrap_or_else(|| "None".into());
    let async_kw = if f.r#async { "async " } else { "" };
    out.push('\n');
    emit_doc(out, &f.doc, "");
    out.push_str(&format!(
        "{async_kw}def {}({}) -> {}: ...\n",
        func_name,
        params.join(", "),
        ret
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::config::GeneratorConfig;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".into(),
            modules,
            generators: None,
        }
    }

    fn simple_module(functions: Vec<Function>) -> Module {
        Module {
            name: "math".into(),
            functions,
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
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
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_python_gen_output");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir).unwrap();

        let init = std::fs::read_to_string(tmp.join("python/weaveffi/__init__.py")).unwrap();
        assert!(init.contains("from .weaveffi import *"));

        let weaveffi = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.py")).unwrap();
        assert!(weaveffi.contains("WeaveFFI"));
        assert!(weaveffi.contains("def add("));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn output_files_lists_all() {
        let api = make_api(vec![]);
        let out = Utf8Path::new("/tmp/out");
        let files = PythonGenerator.output_files(&api, out);
        assert_eq!(
            files,
            vec![
                out.join("python/weaveffi/__init__.py").to_string(),
                out.join("python/weaveffi/weaveffi.py").to_string(),
                out.join("python/weaveffi/weaveffi.pyi").to_string(),
                out.join("python/pyproject.toml").to_string(),
                out.join("python/setup.py").to_string(),
                out.join("python/README.md").to_string(),
            ]
        );
    }

    #[test]
    fn preamble_has_load_library() {
        let api = make_api(vec![]);
        let py = render_python_module(&api, true);
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
        let py = render_python_module(&api, true);
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
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let py = render_python_module(&api, true);
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
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
        assert!(
            py.contains("def echo(msg: str) -> str:"),
            "missing signature: {py}"
        );
        assert!(py.contains("ctypes.c_char_p"), "missing c_char_p: {py}");
        assert!(
            py.contains("_string_to_bytes(msg)"),
            "missing _string_to_bytes call: {py}"
        );
        assert!(
            py.contains("_bytes_to_string(_result)"),
            "missing _bytes_to_string call: {py}"
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
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let py = render_python_module(&api, true);
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
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
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
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Enum("Color".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
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
                        default: None,
                    },
                    StructField {
                        name: "age".into(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
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
            py.contains("_bytes_to_string(_result)"),
            "missing _bytes_to_string in getter: {py}"
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
    fn python_builder_generated() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
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
                            default: None,
                        },
                        StructField {
                            name: "age".into(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: true,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        PythonGenerator.generate(&api, out).unwrap();
        let py = std::fs::read_to_string(out.join("python/weaveffi/weaveffi.py")).unwrap();
        assert!(
            py.contains("class ContactBuilder"),
            "missing builder class: {py}"
        );
        assert!(py.contains("def with_name("), "missing with_name: {py}");
        assert!(py.contains("def with_age("), "missing with_age: {py}");
        assert!(py.contains("def build("), "missing build: {py}");
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
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
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
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Bool),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let py = render_python_module(&api, true);
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
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let py = render_python_module(&api, true);
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
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Bytes),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
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
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
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
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
        assert!(
            py.contains("-> Optional[str]:"),
            "missing optional str return: {py}"
        );
        assert!(
            py.contains("return _bytes_to_string(_result)"),
            "missing _bytes_to_string for optional string: {py}"
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
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_ids".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
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
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
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
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
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
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
        assert!(
            py.contains("def email(self) -> Optional[str]:"),
            "missing optional getter: {py}"
        );
        assert!(
            py.contains("_bytes_to_string(_result)"),
            "missing _bytes_to_string in optional getter: {py}"
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
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
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
            callbacks: vec![],
            listeners: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            functions: vec![
                Function {
                    name: "create_contact".into(),
                    params: vec![
                        Param {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Handle),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "count_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_python_gen_contacts");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir).unwrap();

        let py = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.py")).unwrap();

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
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
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
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);
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

    #[test]
    fn python_generates_type_stubs() {
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
            callbacks: vec![],
            listeners: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "tags".into(),
                        ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "metadata".into(),
                        ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            functions: vec![
                Function {
                    name: "create_contact".into(),
                    params: vec![
                        Param {
                            name: "name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Handle),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "delete_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_python_pyi");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir).unwrap();

        let pyi_path = tmp.join("python/weaveffi/weaveffi.pyi");
        assert!(pyi_path.exists(), ".pyi file must exist");

        let pyi = std::fs::read_to_string(&pyi_path).unwrap();

        assert!(
            pyi.contains("from enum import IntEnum"),
            "missing IntEnum import"
        );
        assert!(
            pyi.contains("from typing import Dict, Iterator, List, Optional"),
            "missing typing imports"
        );

        assert!(
            pyi.contains("class ContactType(IntEnum):"),
            "missing enum stub"
        );
        assert!(
            pyi.contains("    Personal: int"),
            "missing enum variant Personal"
        );
        assert!(pyi.contains("    Work: int"), "missing enum variant Work");

        assert!(pyi.contains("class Contact:"), "missing struct stub");
        assert!(
            pyi.contains("    def id(self) -> int: ..."),
            "missing id property: {pyi}"
        );
        assert!(
            pyi.contains("    def name(self) -> str: ..."),
            "missing name property: {pyi}"
        );
        assert!(
            pyi.contains("    def email(self) -> Optional[str]: ..."),
            "missing email property: {pyi}"
        );
        assert!(
            pyi.contains("    def tags(self) -> List[str]: ..."),
            "missing tags property: {pyi}"
        );
        assert!(
            pyi.contains("    def metadata(self) -> Dict[str, int]: ..."),
            "missing metadata property: {pyi}"
        );

        assert!(
            pyi.contains("def create_contact(name: str, email: Optional[str]) -> int: ..."),
            "missing create_contact stub: {pyi}"
        );
        assert!(
            pyi.contains("def get_contact(id: int) -> \"Contact\": ..."),
            "missing get_contact stub: {pyi}"
        );
        assert!(
            pyi.contains("def delete_contact(id: int) -> None: ..."),
            "missing delete_contact stub: {pyi}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_python_basic() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_py_basic");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir).unwrap();

        let py = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.py")).unwrap();

        assert!(py.contains("def add(a: int, b: int) -> int:"));
        assert!(py.contains("_fn = _lib.weaveffi_math_add"));
        assert!(py.contains("ctypes.c_int32, ctypes.c_int32"));
        assert!(py.contains("_fn.restype = ctypes.c_int32"));
        assert!(py.contains("_err = _WeaveffiErrorStruct()"));
        assert!(py.contains("_check_error(_err)"));
        assert!(py.contains("return _result"));

        assert!(py.contains("import ctypes"));
        assert!(py.contains("from enum import IntEnum"));
        assert!(py.contains("from typing import Dict, Iterator, List, Optional"));
        assert!(py.contains("class WeaveffiError(Exception):"));
        assert!(py.contains("def _load_library()"));
        assert!(py.contains("_lib = _load_library()"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_python_with_structs() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: Some("A contact record".into()),
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "last_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);

        assert!(py.contains("class Contact:"), "missing class decl");
        assert!(
            py.contains("\"\"\"A contact record\"\"\""),
            "missing doc: {py}"
        );
        assert!(py.contains("def __init__(self, _ptr: int) -> None:"));
        assert!(py.contains("self._ptr = _ptr"));
        assert!(py.contains("def __del__(self) -> None:"));
        assert!(py.contains("weaveffi_contacts_Contact_destroy"));

        assert!(py.contains("@property\n    def id(self) -> int:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_id"));
        assert!(py.contains("_fn.restype = ctypes.c_int64"));

        assert!(py.contains("@property\n    def first_name(self) -> str:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_first_name"));

        assert!(py.contains("@property\n    def last_name(self) -> str:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_last_name"));

        assert!(py.contains("@property\n    def email(self) -> Optional[str]:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_email"));
    }

    #[test]
    fn generate_python_with_enums() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "get_type".into(),
                params: vec![Param {
                    name: "ct".into(),
                    ty: TypeRef::Enum("ContactType".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Enum("ContactType".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![EnumDef {
                name: "ContactType".into(),
                doc: Some("Type of contact".into()),
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
                    EnumVariant {
                        name: "Other".into(),
                        value: 2,
                        doc: None,
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);

        assert!(py.contains("class ContactType(IntEnum):"));
        assert!(py.contains("\"\"\"Type of contact\"\"\""));
        assert!(py.contains("Personal = 0"));
        assert!(py.contains("Work = 1"));
        assert!(py.contains("Other = 2"));

        assert!(
            py.contains("ct: \"ContactType\""),
            "missing enum param hint"
        );
        assert!(
            py.contains("-> \"ContactType\":"),
            "missing enum return hint"
        );
        assert!(py.contains("ct.value"), "missing .value for enum param");
        assert!(
            py.contains("return ContactType(_result)"),
            "missing enum return wrap"
        );
        assert!(py.contains("ctypes.c_int32"), "enum should use c_int32 ABI");
    }

    #[test]
    fn generate_python_with_optionals() {
        let api = make_api(vec![Module {
            name: "store".into(),
            functions: vec![
                Function {
                    name: "find_int".into(),
                    params: vec![Param {
                        name: "key".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "find_name".into(),
                    params: vec![Param {
                        name: "prefix".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "find_contact".into(),
                    params: vec![],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                        "Contact".into(),
                    )))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "find_flag".into(),
                    params: vec![],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Bool))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);

        assert!(
            py.contains("key: Optional[int]"),
            "missing Optional[int] param"
        );
        assert!(
            py.contains("-> Optional[int]:"),
            "missing Optional[int] return"
        );
        assert!(
            py.contains("ctypes.byref(ctypes.c_int32(key)) if key is not None else None"),
            "missing optional i32 conversion"
        );
        assert!(
            py.contains("ctypes.POINTER(ctypes.c_int32)"),
            "missing POINTER for optional i32"
        );

        assert!(
            py.contains("prefix: Optional[str]"),
            "missing Optional[str] param"
        );
        assert!(
            py.contains("-> Optional[str]:"),
            "missing Optional[str] return"
        );
        assert!(
            py.contains("_string_to_bytes(prefix)"),
            "missing optional _string_to_bytes"
        );

        assert!(
            py.contains("-> Optional[\"Contact\"]:"),
            "missing Optional struct return"
        );
        assert!(
            py.contains("if _result is None:\n        return None\n    return Contact(_result)"),
            "missing optional struct None check"
        );

        assert!(
            py.contains("-> Optional[bool]:"),
            "missing Optional[bool] return"
        );
        assert!(
            py.contains("return bool(_result[0])"),
            "missing optional bool deref"
        );
    }

    #[test]
    fn generate_python_with_lists() {
        let api = make_api(vec![Module {
            name: "batch".into(),
            functions: vec![
                Function {
                    name: "process_ids".into(),
                    params: vec![Param {
                        name: "ids".into(),
                        ty: TypeRef::List(Box::new(TypeRef::I32)),
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_names".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_items".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Item".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);

        assert!(py.contains("ids: List[int]"), "missing List[int] param");
        assert!(
            py.contains("(ctypes.c_int32 * len(ids))(*ids)"),
            "missing list-to-array conversion"
        );
        assert!(
            py.contains("ctypes.POINTER(ctypes.c_int32)"),
            "missing POINTER for list param"
        );
        assert!(py.contains("ctypes.c_size_t"), "missing size_t for length");

        assert!(
            py.contains("-> List[str]:"),
            "missing List[str] return: {py}"
        );
        assert!(
            py.contains("_bytes_to_string(_result[_i]) for _i in range(_out_len.value)"),
            "missing string list _bytes_to_string: {py}"
        );

        assert!(
            py.contains("-> List[\"Item\"]:"),
            "missing List struct return"
        );
        assert!(
            py.contains("Item(_result[_i]) for _i in range(_out_len.value)"),
            "missing struct wrapping in list"
        );
    }

    #[test]
    fn generate_python_with_maps() {
        let api = make_api(vec![Module {
            name: "config".into(),
            functions: vec![
                Function {
                    name: "set_config".into(),
                    params: vec![Param {
                        name: "settings".into(),
                        ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_config".into(),
                    params: vec![],
                    returns: Some(TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::I32),
                    )),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);

        assert!(
            py.contains("settings: Dict[str, int]"),
            "missing Dict param hint"
        );
        assert!(
            py.contains("list(settings.keys())"),
            "missing keys extraction"
        );
        assert!(
            py.contains("_settings_vals = [settings[_k] for _k in _settings_keys]"),
            "missing values extraction"
        );
        assert!(
            py.contains("ctypes.c_char_p * len(_settings_keys)"),
            "missing key array creation"
        );
        assert!(
            py.contains("ctypes.c_int32 * len(_settings_vals)"),
            "missing value array creation"
        );

        assert!(
            py.contains("-> Dict[str, int]:"),
            "missing Dict return hint"
        );
        assert!(
            py.contains("_out_keys = ctypes.POINTER(ctypes.c_char_p)()"),
            "missing out_keys init"
        );
        assert!(
            py.contains("_out_values = ctypes.POINTER(ctypes.c_int32)()"),
            "missing out_values init"
        );
        assert!(
            py.contains("_out_len = ctypes.c_size_t(0)"),
            "missing out_len init"
        );
        assert!(
            py.contains("if not _out_keys or not _out_values:"),
            "missing empty map check"
        );
        assert!(
            py.contains("_bytes_to_string(_out_keys[_i]): _out_values[_i]"),
            "missing map comprehension"
        );
    }

    #[test]
    fn generate_python_pyi_types() {
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
                    EnumVariant {
                        name: "Other".into(),
                        value: 2,
                        doc: None,
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "tags".into(),
                        ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "scores".into(),
                        ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            functions: vec![
                Function {
                    name: "create_contact".into(),
                    params: vec![
                        Param {
                            name: "name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Handle),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "delete_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            errors: None,
            modules: vec![],
        }]);

        let pyi = render_pyi_module(&api, true);

        assert!(pyi.contains("from enum import IntEnum"));
        assert!(pyi.contains("from typing import Dict, Iterator, List, Optional"));

        assert!(pyi.contains("class ContactType(IntEnum):"));
        assert!(pyi.contains("    Personal: int"));
        assert!(pyi.contains("    Work: int"));
        assert!(pyi.contains("    Other: int"));

        assert!(pyi.contains("class Contact:"));
        assert!(pyi.contains("    def id(self) -> int: ..."));
        assert!(pyi.contains("    def first_name(self) -> str: ..."));
        assert!(pyi.contains("    def email(self) -> Optional[str]: ..."));
        assert!(pyi.contains("    def tags(self) -> List[str]: ..."));
        assert!(pyi.contains("    def scores(self) -> Dict[str, int]: ..."));

        assert!(pyi.contains("def create_contact(name: str, email: Optional[str]) -> int: ..."));
        assert!(pyi.contains("def get_contact(id: int) -> \"Contact\": ..."));
        assert!(pyi.contains("def list_contacts() -> List[\"Contact\"]: ..."));
        assert!(pyi.contains("def delete_contact(id: int) -> None: ..."));
    }

    #[test]
    fn generate_python_full_contacts() {
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
                    EnumVariant {
                        name: "Other".into(),
                        value: 2,
                        doc: None,
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "last_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            functions: vec![
                Function {
                    name: "create_contact".into(),
                    params: vec![
                        Param {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "last_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Handle),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "delete_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Bool),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "count_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_py_full_contacts");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir).unwrap();

        let py = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.py")).unwrap();
        let pyi = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.pyi")).unwrap();

        assert!(py.contains("class ContactType(IntEnum):"));
        assert!(py.contains("Personal = 0"));
        assert!(py.contains("Work = 1"));
        assert!(py.contains("Other = 2"));

        assert!(py.contains("class Contact:"));
        assert!(py.contains("weaveffi_contacts_Contact_destroy"));
        assert!(py.contains("@property\n    def id(self) -> int:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_id"));
        assert!(py.contains("@property\n    def first_name(self) -> str:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_first_name"));
        assert!(py.contains("@property\n    def last_name(self) -> str:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_last_name"));
        assert!(py.contains("@property\n    def email(self) -> Optional[str]:"));
        assert!(py.contains("weaveffi_contacts_Contact_get_email"));
        assert!(py.contains("@property\n    def contact_type(self) -> \"ContactType\":"));
        assert!(py.contains("weaveffi_contacts_Contact_get_contact_type"));
        assert!(py.contains("return ContactType(_result)"));

        assert!(py.contains("def create_contact("));
        assert!(py.contains("first_name: str"));
        assert!(py.contains("last_name: str"));
        assert!(py.contains("email: Optional[str]"));
        assert!(py.contains("contact_type: \"ContactType\""));
        assert!(py.contains("-> int:"));
        assert!(py.contains("weaveffi_contacts_create_contact"));
        assert!(py.contains("_string_to_bytes(first_name)"));
        assert!(py.contains("contact_type.value"));

        assert!(py.contains("def get_contact(id: int) -> \"Contact\":"));
        assert!(py.contains("weaveffi_contacts_get_contact"));
        assert!(py.contains("return Contact(_result)"));

        assert!(py.contains("def list_contacts() -> List[\"Contact\"]:"));
        assert!(py.contains("weaveffi_contacts_list_contacts"));
        assert!(py.contains("Contact(_result[_i]) for _i in range(_out_len.value)"));

        assert!(py.contains("def delete_contact(id: int) -> bool:"));
        assert!(py.contains("weaveffi_contacts_delete_contact"));
        assert!(py.contains("return bool(_result)"));

        assert!(py.contains("def count_contacts() -> int:"));
        assert!(py.contains("weaveffi_contacts_count_contacts"));

        assert!(pyi.contains("class ContactType(IntEnum):"));
        assert!(pyi.contains("    Personal: int"));
        assert!(pyi.contains("    Work: int"));
        assert!(pyi.contains("    Other: int"));
        assert!(pyi.contains("class Contact:"));
        assert!(pyi.contains("def create_contact("));
        assert!(pyi.contains("def get_contact(id: int) -> \"Contact\": ..."));
        assert!(pyi.contains("def list_contacts() -> List[\"Contact\"]: ..."));
        assert!(pyi.contains("def delete_contact(id: int) -> bool: ..."));
        assert!(pyi.contains("def count_contacts() -> int: ..."));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn python_generates_packaging() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_python_packaging");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir).unwrap();

        let pyproject = std::fs::read_to_string(tmp.join("python/pyproject.toml")).unwrap();
        assert!(
            pyproject.contains("[build-system]"),
            "missing build-system: {pyproject}"
        );
        assert!(
            pyproject.contains("setuptools"),
            "missing setuptools: {pyproject}"
        );
        assert!(
            pyproject.contains("[project]"),
            "missing project section: {pyproject}"
        );
        assert!(
            pyproject.contains("name = \"weaveffi\""),
            "missing project name: {pyproject}"
        );
        assert!(
            pyproject.contains("version = \"0.1.0\""),
            "missing version: {pyproject}"
        );
        assert!(
            pyproject.contains("[tool.setuptools]"),
            "missing tool.setuptools: {pyproject}"
        );
        assert!(
            pyproject.contains("packages = [\"weaveffi\"]"),
            "missing packages list: {pyproject}"
        );

        let setup = std::fs::read_to_string(tmp.join("python/setup.py")).unwrap();
        assert!(
            setup.contains("from setuptools import setup"),
            "missing setuptools import: {setup}"
        );
        assert!(
            setup.contains("name=\"weaveffi\""),
            "missing package name: {setup}"
        );

        let readme = std::fs::read_to_string(tmp.join("python/README.md")).unwrap();
        assert!(
            readme.contains("pip install"),
            "missing install instructions: {readme}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn python_has_memory_helpers() {
        let api = make_api(vec![]);
        let py = render_python_module(&api, true);
        assert!(
            py.contains("import contextlib"),
            "missing contextlib import"
        );
        assert!(
            py.contains("class _PointerGuard(contextlib.AbstractContextManager):"),
            "missing _PointerGuard class"
        );
        assert!(
            py.contains("def __exit__(self, *exc)"),
            "missing _PointerGuard.__exit__"
        );
        assert!(
            py.contains("def _string_to_bytes("),
            "missing _string_to_bytes helper"
        );
        assert!(
            py.contains("def _bytes_to_string("),
            "missing _bytes_to_string helper"
        );
    }

    #[test]
    fn python_custom_package_name() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let config = GeneratorConfig {
            python_package_name: Some("my_bindings".into()),
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_py_custom_pkg");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        assert!(
            tmp.join("python/my_bindings/__init__.py").exists(),
            "package dir should use custom name"
        );
        assert!(
            tmp.join("python/my_bindings/weaveffi.py").exists(),
            "module file should be inside custom package dir"
        );

        let pyproject = std::fs::read_to_string(tmp.join("python/pyproject.toml")).unwrap();
        assert!(
            pyproject.contains("name = \"my_bindings\""),
            "pyproject.toml should use custom name: {pyproject}"
        );
        assert!(
            pyproject.contains("packages = [\"my_bindings\"]"),
            "pyproject.toml packages should use custom name: {pyproject}"
        );

        let setup = std::fs::read_to_string(tmp.join("python/setup.py")).unwrap();
        assert!(
            setup.contains("name=\"my_bindings\""),
            "setup.py should use custom name: {setup}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn python_strip_module_prefix() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "create_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let config = GeneratorConfig {
            strip_module_prefix: true,
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_python_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let py = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.py")).unwrap();
        assert!(
            py.contains("def create_contact("),
            "stripped name should be create_contact: {py}"
        );
        assert!(
            !py.contains("def contacts_create_contact("),
            "should not contain module-prefixed name: {py}"
        );
        assert!(
            py.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {py}"
        );

        let pyi = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.pyi")).unwrap();
        assert!(
            pyi.contains("def create_contact("),
            "pyi stripped name should be create_contact: {pyi}"
        );

        let no_strip = GeneratorConfig::default();
        let tmp2 = std::env::temp_dir().join("weaveffi_test_python_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        PythonGenerator
            .generate_with_config(&api, out_dir2, &no_strip)
            .unwrap();

        let py2 = std::fs::read_to_string(tmp2.join("python/weaveffi/weaveffi.py")).unwrap();
        assert!(
            py2.contains("def contacts_create_contact("),
            "default should use module-prefixed name: {py2}"
        );

        let pyi2 = std::fs::read_to_string(tmp2.join("python/weaveffi/weaveffi.pyi")).unwrap();
        assert!(
            pyi2.contains("def contacts_create_contact("),
            "pyi default should use module-prefixed name: {pyi2}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
    }

    #[test]
    fn python_deeply_nested_optional() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "data".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                        Box::new(TypeRef::Struct("Contact".into())),
                    ))))),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let pyi = render_pyi_module(&api, true);
        assert!(
            pyi.contains("Optional[List[Optional["),
            "should contain deeply nested optional type: {pyi}"
        );
    }

    #[test]
    fn python_map_of_lists() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "scores".into(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::List(Box::new(TypeRef::I32))),
                    ),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let pyi = render_pyi_module(&api, true);
        assert!(
            pyi.contains("Dict[str, List[int]]"),
            "should contain map of lists type: {pyi}"
        );
    }

    #[test]
    fn python_enum_keyed_map() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "contacts".into(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::Enum("Color".into())),
                        Box::new(TypeRef::Struct("Contact".into())),
                    ),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: None,
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
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let pyi = render_pyi_module(&api, true);
        assert!(
            pyi.contains("Dict[\"Color\", \"Contact\"]"),
            "should contain enum-keyed map type: {pyi}"
        );
    }

    #[test]
    fn python_typed_handle_type() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "get_info".into(),
                    params: vec![Param {
                        name: "contact".into(),
                        ty: TypeRef::TypedHandle("Contact".into()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let py = render_python_module(&api, true);
        assert!(
            py.contains("contact: \"Contact\""),
            "TypedHandle should use class type not int: {py}"
        );
        assert!(
            py.contains("contact._ptr"),
            "TypedHandle call arg should extract ._ptr: {py}"
        );
        assert!(
            py.contains("ctypes.c_void_p"),
            "TypedHandle ctypes type should be c_void_p: {py}"
        );
    }

    #[test]
    fn python_no_double_free_on_error() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);

        assert!(
            py.contains("_string_to_bytes(name)"),
            "string param should use _string_to_bytes(name): {py}"
        );
        assert!(
            !py.contains("weaveffi_free_string(name"),
            "input string param must not be freed with weaveffi_free_string(name): {py}"
        );
        assert!(
            !py.contains("free(name"),
            "input string param must not be passed to free(name: {py}"
        );

        let fn_sig = "def find_contact(name: str) -> \"Contact\":";
        let start = py
            .find(fn_sig)
            .unwrap_or_else(|| panic!("missing find_contact signature: {py}"));
        let rest = &py[start..];
        let end_offset = rest[1..]
            .find("\n\ndef ")
            .or_else(|| rest[1..].find("\n\nclass "))
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        let body = &rest[..end_offset];
        let err_pos = body
            .find("_check_error(_err)")
            .expect("_check_error should appear in find_contact");
        let contact_pos = body
            .find("return Contact(_result)")
            .expect("return Contact(_result) should appear in find_contact");
        assert!(
            err_pos < contact_pos,
            "_check_error(_err) should precede return Contact(_result): {body}"
        );

        let class_start = py
            .find("class Contact:")
            .expect("Contact class should be defined");
        let after_class = &py[class_start..];
        let class_end = after_class[1..]
            .find("\n\nclass ")
            .or_else(|| after_class[1..].find("\n\ndef "))
            .map(|i| i + 1)
            .unwrap_or(after_class.len());
        let contact_class = &after_class[..class_end];
        assert!(
            contact_class.contains("def __del__(self)"),
            "Contact should define __del__: {contact_class}"
        );
        assert!(
            contact_class.contains("_destroy"),
            "Contact.__del__ should call _destroy: {contact_class}"
        );
    }

    #[test]
    fn python_null_check_on_optional_return() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true);

        assert!(
            py.contains("if _result is None:\n        return None"),
            "optional struct return should null-check before wrap: {py}"
        );
        let none_check = py
            .find("if _result is None:\n        return None")
            .expect("null-check block");
        let wrap = py
            .find("return Contact(_result)")
            .expect("Contact(_result) wrap");
        assert!(
            wrap > none_check,
            "Contact(_result) should appear after null check: {py}"
        );
    }

    #[test]
    fn python_async_function_is_async_def() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "fetch_data".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);
        let code = render_python_module(&api, true);
        assert!(
            code.contains("import asyncio"),
            "should import asyncio: {code}"
        );
        assert!(
            code.contains("def _fetch_data_sync(id: int) -> str:"),
            "should have sync helper: {code}"
        );
        assert!(
            code.contains("async def fetch_data(id: int) -> str:"),
            "should have async wrapper: {code}"
        );
        assert!(
            code.contains("asyncio.get_event_loop()"),
            "should use get_event_loop: {code}"
        );
        assert!(
            code.contains("run_in_executor(None, _fetch_data_sync, id)"),
            "should use run_in_executor with sync fn and args: {code}"
        );
    }

    /// `ctypes.CFUNCTYPE` instances pin the C trampoline; `_cb` is held alive
    /// in the local frame for the lifetime of the synchronous helper, which
    /// blocks on `_ev.wait()` until the C callback fires. The `try/finally`
    /// around `_state` mutation ensures `_ev.set()` always runs, releasing
    /// the wait and letting `_cb` drop together with the helper frame.
    #[test]
    fn python_async_pins_callback_for_lifetime() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "fetch_data".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);
        let code = render_python_module(&api, true);
        let pin_count = code.matches("_cb = _cb_type(_cb_impl)").count();
        let wait_count = code.matches("_ev.wait()").count();
        let set_count = code.matches("_ev.set()").count();
        assert_eq!(
            pin_count, 1,
            "expected one `_cb = _cb_type(_cb_impl)` per async fn, got {pin_count}: {code}"
        );
        assert_eq!(
            wait_count, set_count,
            "every `_ev.wait()` must be matched by an `_ev.set()` in finally: wait={wait_count} set={set_count}: {code}"
        );
        assert!(
            code.contains("finally:\n            _ev.set()"),
            "_ev.set() must be in a finally block: {code}"
        );
    }

    #[test]
    fn python_pyi_async_function() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "fetch_data".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);
        let stubs = render_pyi_module(&api, true);
        assert!(
            stubs.contains("async def fetch_data(id: int) -> str: ..."),
            "pyi should declare async def: {stubs}"
        );
    }

    #[test]
    fn python_cross_module_struct() {
        let api = make_api(vec![
            Module {
                name: "types".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Name".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "value".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
            Module {
                name: "ops".into(),
                functions: vec![Function {
                    name: "get_name".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Struct("types.Name".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
        ]);

        let code = render_python_module(&api, true);
        let stubs = render_pyi_module(&api, true);

        assert!(
            code.contains("Name(_result)"),
            "cross-module return should construct Name, not types.Name: {code}"
        );
        assert!(
            !code.contains("types.Name"),
            "dot-qualified name should not appear in generated Python code: {code}"
        );
        assert!(
            stubs.contains("\"Name\""),
            "pyi should use local type name: {stubs}"
        );
        assert!(
            !stubs.contains("types.Name"),
            "dot-qualified name should not appear in pyi stubs: {stubs}"
        );
    }

    #[test]
    fn python_nested_module_output() {
        let api = make_api(vec![Module {
            name: "parent".to_string(),
            functions: vec![Function {
                name: "outer_fn".to_string(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![Module {
                name: "child".to_string(),
                functions: vec![Function {
                    name: "inner_fn".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
        }]);
        let py = render_python_module(&api, true);
        assert!(
            py.contains("# === Module: parent ==="),
            "parent module section missing: {py}"
        );
        assert!(
            py.contains("# === Module: parent_child ==="),
            "nested child module section missing: {py}"
        );
        assert!(
            py.contains("weaveffi_parent_outer_fn"),
            "parent C function missing: {py}"
        );
        assert!(
            py.contains("weaveffi_parent_child_inner_fn"),
            "nested child C function missing: {py}"
        );
        let pyi = render_pyi_module(&api, true);
        assert!(
            pyi.contains("def inner_fn"),
            "nested child function missing from pyi: {pyi}"
        );
    }

    #[test]
    fn python_type_hint_iterator() {
        assert_eq!(
            py_type_hint(&TypeRef::Iterator(Box::new(TypeRef::I32))),
            "Iterator[int]"
        );
        assert_eq!(
            py_type_hint(&TypeRef::Iterator(Box::new(TypeRef::Struct(
                "Contact".into()
            )))),
            "Iterator[\"Contact\"]"
        );
    }

    #[test]
    fn python_iterator_return() {
        let api = make_api(vec![Module {
            name: "data".to_string(),
            functions: vec![Function {
                name: "list_items".to_string(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let py = render_python_module(&api, true);
        assert!(
            py.contains("ListItemsIterator"),
            "should reference iterator type name: {py}"
        );
        assert!(
            py.contains("_next"),
            "should call _next for iteration: {py}"
        );
        assert!(
            py.contains("_destroy"),
            "should call _destroy for cleanup: {py}"
        );
    }

    #[test]
    fn deprecated_function_generates_annotation() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add_old".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: Some("Use add_v2 instead".into()),
            since: Some("0.1.0".into()),
        }])]);
        let py = render_python_module(&api, true);
        assert!(
            py.contains("warnings.warn(\"Use add_v2 instead\", DeprecationWarning, stacklevel=2)"),
            "missing deprecation warning: {py}"
        );
    }

    fn doc_api() -> Api {
        make_api(vec![Module {
            name: "docs".into(),
            functions: vec![Function {
                name: "do_thing".into(),
                params: vec![Param {
                    name: "x".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: Some("the input value".into()),
                }],
                returns: Some(TypeRef::I32),
                doc: Some("Performs a thing.".into()),
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Item".into(),
                doc: Some("An item we track.".into()),
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    doc: Some("Stable id".into()),
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Kind".into(),
                doc: Some("Kind of item.".into()),
                variants: vec![EnumVariant {
                    name: "Small".into(),
                    value: 0,
                    doc: Some("A small one".into()),
                }],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    #[test]
    fn python_emits_doc_on_function() {
        let py = render_python_module(&doc_api(), true);
        assert!(py.contains("\"\"\"Performs a thing."), "{py}");
    }

    #[test]
    fn python_emits_doc_on_struct() {
        let py = render_python_module(&doc_api(), true);
        assert!(py.contains("\"\"\"An item we track.\"\"\""), "{py}");
    }

    #[test]
    fn python_emits_doc_on_enum_variant() {
        let py = render_python_module(&doc_api(), true);
        assert!(py.contains("\"\"\"Kind of item.\"\"\""), "{py}");
        assert!(py.contains("# A small one"), "{py}");
    }

    #[test]
    fn python_emits_doc_on_field() {
        let py = render_python_module(&doc_api(), true);
        assert!(py.contains("\"\"\"Stable id\"\"\""), "{py}");
    }

    #[test]
    fn python_emits_doc_on_param() {
        let py = render_python_module(&doc_api(), true);
        assert!(py.contains("Parameters"), "{py}");
        assert!(py.contains("x : the input value"), "{py}");
    }
}
