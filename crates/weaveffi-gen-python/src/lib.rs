use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::{stamp_header, Capability, Generator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::templates::{api_to_context, TemplateEngine};
use weaveffi_core::utils::{local_type_name, wrapper_name};
use weaveffi_ir::ir::{
    Api, CallbackDef, EnumDef, Function, ListenerDef, Module, StructDef, StructField, TypeRef,
};

pub struct PythonGenerator;

/// Name under which the Python module template is registered.
const MODULE_TEMPLATE: &str = "python/module.tera";
/// Name under which the Python stubs template is registered.
const STUBS_TEMPLATE: &str = "python/stubs.tera";

/// Built-in Python module template, compiled into the binary. Exposed so
/// callers (and tests) can seed a [`TemplateEngine`] with the shipped default
/// via [`TemplateEngine::load_builtin`].
pub const BUILTIN_MODULE_TEMPLATE: &str = include_str!("../templates/python/module.tera");
/// Built-in Python stubs template, compiled into the binary. Exposed so
/// callers (and tests) can seed a [`TemplateEngine`] with the shipped default
/// via [`TemplateEngine::load_builtin`].
pub const BUILTIN_STUBS_TEMPLATE: &str = include_str!("../templates/python/stubs.tera");

/// Build a [`TemplateEngine`] pre-loaded with this crate's built-in templates.
/// User templates loaded via [`TemplateEngine::load_dir`] override entries of
/// the same name.
pub fn builtin_template_engine() -> Result<TemplateEngine> {
    let mut engine = TemplateEngine::new();
    engine.load_builtin(MODULE_TEMPLATE, BUILTIN_MODULE_TEMPLATE)?;
    engine.load_builtin(STUBS_TEMPLATE, BUILTIN_STUBS_TEMPLATE)?;
    Ok(engine)
}

fn stamp_hash(body: String) -> String {
    format!("# {}\n{body}", stamp_header("python"))
}

impl PythonGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        package_name: &str,
        strip_module_prefix: bool,
        c_prefix: &str,
    ) -> Result<()> {
        let dir = out_dir.join("python");
        let pkg_dir = dir.join(package_name);
        let tests_dir = dir.join("tests");
        std::fs::create_dir_all(&pkg_dir)?;
        std::fs::create_dir_all(&tests_dir)?;
        std::fs::write(pkg_dir.join("__init__.py"), stamp_hash(render_init_py()))?;
        std::fs::write(
            pkg_dir.join("weaveffi.py"),
            stamp_hash(render_python_module(api, strip_module_prefix, c_prefix)),
        )?;
        std::fs::write(
            pkg_dir.join("weaveffi.pyi"),
            stamp_hash(render_pyi_module(api, strip_module_prefix)),
        )?;
        std::fs::write(
            dir.join("pyproject.toml"),
            stamp_hash(render_pyproject_toml(package_name)),
        )?;
        std::fs::write(
            dir.join("setup.py"),
            stamp_hash(render_setup_py(package_name)),
        )?;
        std::fs::write(
            dir.join("MANIFEST.in"),
            stamp_hash(render_manifest_in(package_name)),
        )?;
        std::fs::write(tests_dir.join("__init__.py"), stamp_hash(String::new()))?;
        std::fs::write(
            tests_dir.join("test_smoke.py"),
            stamp_hash(render_smoke_test(api, package_name, strip_module_prefix)),
        )?;
        // README.md is documentation, not a source file; leave it unstamped.
        std::fs::write(dir.join("README.md"), render_readme())?;
        Ok(())
    }
}

impl Generator for PythonGenerator {
    fn name(&self) -> &'static str {
        "python"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "weaveffi", true, "weaveffi")
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
            config.c_prefix(),
        )
    }

    fn generate_with_templates(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
        templates: Option<&TemplateEngine>,
    ) -> Result<()> {
        if let Some(engine) = templates {
            let has_module = engine.has_template(MODULE_TEMPLATE);
            let has_stubs = engine.has_template(STUBS_TEMPLATE);
            if has_module || has_stubs {
                let package_name = config.python_package_name();
                let strip_module_prefix = config.strip_module_prefix;
                let c_prefix = config.c_prefix();
                let dir = out_dir.join("python");
                let pkg_dir = dir.join(package_name);
                let tests_dir = dir.join("tests");
                std::fs::create_dir_all(&pkg_dir)?;
                std::fs::create_dir_all(&tests_dir)?;

                std::fs::write(pkg_dir.join("__init__.py"), stamp_hash(render_init_py()))?;

                let ctx = api_to_context(api);
                let module_body = if has_module {
                    engine.render(MODULE_TEMPLATE, &ctx)?
                } else {
                    render_python_module(api, strip_module_prefix, c_prefix)
                };
                std::fs::write(pkg_dir.join("weaveffi.py"), stamp_hash(module_body))?;

                let stubs_body = if has_stubs {
                    engine.render(STUBS_TEMPLATE, &ctx)?
                } else {
                    render_pyi_module(api, strip_module_prefix)
                };
                std::fs::write(pkg_dir.join("weaveffi.pyi"), stamp_hash(stubs_body))?;

                std::fs::write(
                    dir.join("pyproject.toml"),
                    stamp_hash(render_pyproject_toml(package_name)),
                )?;
                std::fs::write(
                    dir.join("setup.py"),
                    stamp_hash(render_setup_py(package_name)),
                )?;
                std::fs::write(
                    dir.join("MANIFEST.in"),
                    stamp_hash(render_manifest_in(package_name)),
                )?;
                std::fs::write(tests_dir.join("__init__.py"), stamp_hash(String::new()))?;
                std::fs::write(
                    tests_dir.join("test_smoke.py"),
                    stamp_hash(render_smoke_test(api, package_name, strip_module_prefix)),
                )?;
                std::fs::write(dir.join("README.md"), render_readme())?;
                return Ok(());
            }
        }
        self.generate_with_config(api, out_dir, config)
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
            out_dir.join("python/MANIFEST.in").to_string(),
            out_dir.join("python/tests/__init__.py").to_string(),
            out_dir.join("python/tests/test_smoke.py").to_string(),
            out_dir.join("python/README.md").to_string(),
        ]
    }

    fn output_files_with_config(
        &self,
        _api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Vec<String> {
        let pkg = config.python_package_name();
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
            out_dir.join("python/MANIFEST.in").to_string(),
            out_dir.join("python/tests/__init__.py").to_string(),
            out_dir.join("python/tests/test_smoke.py").to_string(),
            out_dir.join("python/README.md").to_string(),
        ]
    }

    fn capabilities(&self) -> &'static [Capability] {
        &[
            Capability::Callbacks,
            Capability::Listeners,
            Capability::Iterators,
            Capability::Builders,
            Capability::AsyncFunctions,
            Capability::CancellableAsync,
            Capability::TypedHandles,
            Capability::BorrowedTypes,
            Capability::MapTypes,
            Capability::NestedModules,
            Capability::CrossModuleTypes,
            Capability::ErrorDomains,
            Capability::DeprecatedAnnotations,
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

fn iter_type_name(func_name: &str, module: &str, c_prefix: &str) -> String {
    let pascal = snake_to_pascal(func_name);
    format!("{c_prefix}_{module}_{pascal}Iterator")
}

fn c_symbol_name(c_prefix: &str, module: &str, func: &str) -> String {
    format!("{c_prefix}_{module}_{func}")
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
        TypeRef::Callback(_) => unreachable!("validator should have rejected callback Python type"),
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
        TypeRef::Callback(_) => "Callable[..., Any]".into(),
    }
}

fn py_param_argtypes(ty: &TypeRef) -> Vec<String> {
    match ty {
        TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes => vec![
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
        TypeRef::Callback(name) => vec![format!("_{name}"), "ctypes.c_void_p".into()],
        _ => vec![py_ctypes_scalar(ty).into()],
    }
}

/// Returns `(restype, out_param_argtypes)` for a return type.
fn py_return_info(ty: &TypeRef) -> (String, Vec<String>) {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            ("ctypes.POINTER(ctypes.c_char)".into(), vec![])
        }
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
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            vec![("result".into(), "ctypes.POINTER(ctypes.c_char)".into())]
        }
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
        Some(TypeRef::Callback(_)) => {
            unreachable!("validator should have rejected async Python callback return type")
        }
        Some(ty) => vec![("result".into(), py_ctypes_scalar(ty).to_string())],
    }
}

fn append_async_success_handler(
    out: &mut String,
    ret: &Option<TypeRef>,
    ind: &str,
    c_prefix: &str,
) {
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
            out.push_str(&format!("{ind}_ptr = result\n"));
            out.push_str(&format!("{ind}if not _ptr:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = \"\"\n"));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!(
                "{ind}    _s = ctypes.cast(_ptr, ctypes.c_char_p).value.decode(\"utf-8\")\n"
            ));
            out.push_str(&format!("{ind}    _lib.{c_prefix}_free_string(_ptr)\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = _s\n"));
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
                "{ind}    _lib.{c_prefix}_free_bytes(result, ctypes.c_size_t(_n))\n"
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
                        out.push_str(&format!("{ind}_ptr = result\n"));
                        out.push_str(&format!("{ind}if not _ptr:\n"));
                        out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                        out.push_str(&format!("{ind}else:\n"));
                        out.push_str(&format!(
                            "{ind}    _s = ctypes.cast(_ptr, ctypes.c_char_p).value.decode(\"utf-8\")\n"
                        ));
                        out.push_str(&format!("{ind}    _lib.{c_prefix}_free_string(_ptr)\n"));
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
                            "{ind}    _lib.{c_prefix}_free_bytes(result, ctypes.c_size_t(_n))\n"
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
                    _ => append_async_success_handler(out, &Some(*inner.clone()), ind, c_prefix),
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
        Some(TypeRef::Iterator(_)) => {
            unreachable!("validator should have rejected async iterator return")
        }
        Some(TypeRef::Callback(_)) => {
            unreachable!("validator should have rejected async Python callback return type")
        }
    }
}

fn render_async_ffi_call_body(out: &mut String, module_name: &str, f: &Function, c_prefix: &str) {
    let c_sym = c_symbol_name(c_prefix, module_name, &f.name);
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
        "{ind}            _lib.{c_prefix}_error_clear(ctypes.byref(err.contents))\n"
    ));
    out.push_str(&format!(
        "{ind}            _state[\"err\"] = WeaveffiError(_code, _msg)\n"
    ));
    out.push_str(&format!("{ind}        else:\n"));
    append_async_success_handler(out, &f.returns, "            ", c_prefix);
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
        call_args.push("_cancel_token".into());
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

fn render_python_module(api: &Api, strip_module_prefix: bool, c_prefix: &str) -> String {
    let mut out = String::new();
    render_preamble(&mut out, c_prefix);
    let has_async = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async));
    if has_async {
        out.push_str("\nimport asyncio\nimport threading\n");
    }
    let has_cancellable_async = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async && f.cancellable));
    if has_cancellable_async {
        render_cancel_token_bindings(&mut out, c_prefix);
    }
    for m in &api.modules {
        render_python_module_content(&mut out, m, &m.name, strip_module_prefix, c_prefix);
    }
    out.push('\n');
    out
}

/// Emit `ctypes` argtype/restype declarations for the `{c_prefix}_cancel_token_*`
/// C ABI so cancellable `async def` wrappers can forward `asyncio.CancelledError`
/// to `{c_prefix}_cancel_token_cancel`.
fn render_cancel_token_bindings(out: &mut String, c_prefix: &str) {
    out.push_str(&format!(
        "\n_lib.{c_prefix}_cancel_token_create.argtypes = []\n"
    ));
    out.push_str(&format!(
        "_lib.{c_prefix}_cancel_token_create.restype = ctypes.c_void_p\n"
    ));
    out.push_str(&format!(
        "_lib.{c_prefix}_cancel_token_cancel.argtypes = [ctypes.c_void_p]\n"
    ));
    out.push_str(&format!(
        "_lib.{c_prefix}_cancel_token_cancel.restype = None\n"
    ));
    out.push_str(&format!(
        "_lib.{c_prefix}_cancel_token_destroy.argtypes = [ctypes.c_void_p]\n"
    ));
    out.push_str(&format!(
        "_lib.{c_prefix}_cancel_token_destroy.restype = None\n"
    ));
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
    c_prefix: &str,
) {
    out.push_str(&format!("\n\n# === Module: {} ===", module_path));
    for e in &m.enums {
        render_enum(out, e);
    }
    for cb in &m.callbacks {
        render_callback(out, cb);
    }
    for s in &m.structs {
        render_struct(out, module_path, s, c_prefix);
        if s.builder {
            render_builder(out, s);
        }
    }
    for l in &m.listeners {
        render_listener(out, module_path, l, c_prefix);
    }
    for f in &m.functions {
        render_function(out, module_path, f, strip_module_prefix, c_prefix);
    }
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        render_python_module_content(out, sub, &sub_path, strip_module_prefix, c_prefix);
    }
}

fn render_preamble(out: &mut String, c_prefix: &str) {
    out.push_str(&format!(
        r#""""WeaveFFI Python ctypes bindings (auto-generated)"""
import contextlib
import ctypes
import platform
from enum import IntEnum
from typing import Any, Callable, Dict, Iterator, List, Optional


class WeaveffiError(Exception):
    def __init__(self, code: int, message: str) -> None:
        self.code = code
        self.message = message
        super().__init__(f"({{code}}) {{message}}")


class _WeaveffiErrorStruct(ctypes.Structure):
    _fields_ = [
        ("code", ctypes.c_int32),
        ("message", ctypes.c_char_p),
    ]


def _load_library() -> ctypes.CDLL:
    system = platform.system()
    if system == "Darwin":
        name = "lib{c_prefix}.dylib"
    elif system == "Windows":
        name = "{c_prefix}.dll"
    else:
        name = "lib{c_prefix}.so"
    return ctypes.CDLL(name)


_lib = _load_library()
_lib.{c_prefix}_error_clear.argtypes = [ctypes.POINTER(_WeaveffiErrorStruct)]
_lib.{c_prefix}_error_clear.restype = None
_lib.{c_prefix}_free_string.argtypes = [ctypes.c_void_p]
_lib.{c_prefix}_free_string.restype = None
_lib.{c_prefix}_free_bytes.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t]
_lib.{c_prefix}_free_bytes.restype = None

_callback_refs: List[Any] = []


def _check_error(err: _WeaveffiErrorStruct) -> None:
    if err.code != 0:
        code = err.code
        message = err.message.decode("utf-8") if err.message else ""
        _lib.{c_prefix}_error_clear(ctypes.byref(err))
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


def _string_to_byteslice(s: str) -> tuple:
    _bytes = s.encode("utf-8")
    _arr = (ctypes.c_uint8 * len(_bytes))(*_bytes)
    return (_arr, len(_bytes))


def _bytes_to_string(ptr) -> Optional[str]:
    if ptr is None:
        return None
    return ptr.decode("utf-8")
"#,
    ));
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

fn render_callback(out: &mut String, cb: &CallbackDef) {
    let ret = match &cb.returns {
        None => "None".to_string(),
        Some(ty) => py_ctypes_scalar(ty).to_string(),
    };
    let mut parts = vec![ret, "ctypes.c_void_p".to_string()];
    for p in &cb.params {
        parts.extend(py_param_argtypes(&p.ty));
    }
    out.push_str(&format!(
        "\n\n_{} = ctypes.CFUNCTYPE({})\n",
        cb.name,
        parts.join(", ")
    ));
}

/// Emit a Python wrapper class for a listener. The class exposes:
///   - `register(callback)`: builds a trampoline, wraps it in the
///     callback's `CFUNCTYPE`, calls `{c_prefix}_{module}_register_{listener}`,
///     stores the cfunc in a class-level dict keyed by the returned id to
///     keep it alive against GC, and returns the id.
///   - `unregister(id)`: calls `{c_prefix}_{module}_unregister_{listener}` and
///     pops the cfunc from the class-level dict.
fn render_listener(out: &mut String, module_path: &str, l: &ListenerDef, c_prefix: &str) {
    let class_name = snake_to_pascal(&l.name);
    let reg_fn = format!("{c_prefix}_{module_path}_register_{}", l.name);
    let unreg_fn = format!("{c_prefix}_{module_path}_unregister_{}", l.name);
    let cfunctype = format!("_{}", l.event_callback);

    out.push_str(&format!("\n\nclass {class_name}:"));
    if let Some(doc) = &l.doc {
        out.push_str(&format!("\n    \"\"\"{}\"\"\"", doc));
    }
    out.push_str("\n    _cfuncs: Dict[int, Any] = {}");

    out.push_str("\n\n    @staticmethod");
    out.push_str("\n    def register(callback: Callable[..., Any]) -> int:");
    out.push_str("\n        def _tramp(_ctx, *args):");
    out.push_str("\n            return callback(*args)");
    out.push_str(&format!("\n        _cfunc = {cfunctype}(_tramp)"));
    out.push_str(&format!("\n        _fn = _lib.{reg_fn}"));
    out.push_str(&format!(
        "\n        _fn.argtypes = [{cfunctype}, ctypes.c_void_p]"
    ));
    out.push_str("\n        _fn.restype = ctypes.c_uint64");
    out.push_str("\n        _id = _fn(_cfunc, ctypes.c_void_p(0))");
    out.push_str(&format!("\n        {class_name}._cfuncs[_id] = _cfunc"));
    out.push_str("\n        return _id");

    out.push_str("\n\n    @staticmethod");
    out.push_str("\n    def unregister(id: int) -> None:");
    out.push_str(&format!("\n        _fn = _lib.{unreg_fn}"));
    out.push_str("\n        _fn.argtypes = [ctypes.c_uint64]");
    out.push_str("\n        _fn.restype = None");
    out.push_str("\n        _fn(id)");
    out.push_str(&format!("\n        {class_name}._cfuncs.pop(id, None)"));
    out.push('\n');
}

fn render_struct(out: &mut String, module_name: &str, s: &StructDef, c_prefix: &str) {
    let prefix = format!("{c_prefix}_{}_{}", module_name, s.name);

    out.push_str(&format!("\n\nclass {}:", s.name));
    if let Some(doc) = &s.doc {
        out.push_str(&format!("\n    \"\"\"{}\"\"\"", doc));
    }

    out.push_str("\n\n    def __init__(self, _ptr: int) -> None:");
    out.push_str("\n        self._ptr = _ptr");

    out.push_str("\n\n    def _dispose(self) -> None:");
    out.push_str("\n        if self._ptr is not None:");
    out.push_str(&format!(
        "\n            _lib.{prefix}_destroy.argtypes = [ctypes.c_void_p]"
    ));
    out.push_str(&format!(
        "\n            _lib.{prefix}_destroy.restype = None"
    ));
    out.push_str(&format!("\n            _lib.{prefix}_destroy(self._ptr)"));
    out.push_str("\n            self._ptr = None");

    out.push_str("\n\n    def __del__(self) -> None:");
    out.push_str("\n        self._dispose()");

    out.push_str(&format!("\n\n    def __enter__(self) -> \"{}\":", s.name));
    out.push_str("\n        return self");

    out.push_str("\n\n    def __exit__(self, exc_type, exc_val, exc_tb) -> bool:");
    out.push_str("\n        self._dispose()");
    out.push_str("\n        return False");

    for field in &s.fields {
        render_getter(out, &prefix, field, c_prefix);
    }
    out.push('\n');
}

fn render_builder(out: &mut String, s: &StructDef) {
    let builder_name = format!("{}Builder", s.name);
    out.push_str(&format!("\n\nclass {}:", builder_name));
    out.push_str("\n    def __init__(self) -> None:");
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

fn render_getter(out: &mut String, prefix: &str, field: &StructField, c_prefix: &str) {
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

    render_return_value(out, &field.ty, ind, c_prefix);
}

fn render_function(
    out: &mut String,
    module_name: &str,
    f: &Function,
    strip_module_prefix: bool,
    c_prefix: &str,
) {
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
        render_iterator_class(out, module_name, &f.name, inner, c_prefix);
    }

    let mut sync_params_sig = params_sig.clone();
    if f.r#async && f.cancellable {
        sync_params_sig.push("_cancel_token: Any".into());
    }

    out.push_str(&format!(
        "\n\ndef {}({}) -> {}:\n",
        def_name,
        sync_params_sig.join(", "),
        ret_hint
    ));

    let ind = "    ";

    if let Some(msg) = &f.deprecated {
        out.push_str(&format!(
            "{ind}import warnings\n{ind}warnings.warn(\"{}\", DeprecationWarning, stacklevel=2)\n",
            msg.replace('"', "\\\"")
        ));
    }

    if f.r#async {
        render_async_ffi_call_body(out, module_name, f, c_prefix);
    } else {
        let c_sym = c_symbol_name(c_prefix, module_name, &f.name);
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
                render_iterator_return(out, module_name, &f.name, inner, ind, c_prefix);
            } else {
                render_return_value(out, ret_ty, ind, c_prefix);
            }
        }
    }

    if f.r#async {
        let params_joined = params_sig.join(", ");
        out.push_str(&format!(
            "\n\nasync def {}({}) -> {}:\n",
            func_name, params_joined, ret_hint
        ));
        let arg_names: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
        if f.cancellable {
            // Create a native cancel token and forward `asyncio.CancelledError`
            // from the awaiting Task to `{c_prefix}_cancel_token_cancel` so the
            // C side can observe cooperative cancellation.
            let mut executor_args_vec: Vec<&str> = vec![&def_name];
            executor_args_vec.extend(arg_names.iter().copied());
            executor_args_vec.push("_cancel_token");
            let executor_args = executor_args_vec.join(", ");
            out.push_str(&format!(
                "    _cancel_token = _lib.{c_prefix}_cancel_token_create()\n"
            ));
            out.push_str("    try:\n");
            out.push_str("        _loop = asyncio.get_event_loop()\n");
            out.push_str("        try:\n");
            if f.returns.is_some() {
                out.push_str(&format!(
                    "            return await _loop.run_in_executor(None, {executor_args})\n"
                ));
            } else {
                out.push_str(&format!(
                    "            await _loop.run_in_executor(None, {executor_args})\n"
                ));
            }
            out.push_str("        except asyncio.CancelledError:\n");
            out.push_str(&format!(
                "            _lib.{c_prefix}_cancel_token_cancel(_cancel_token)\n"
            ));
            out.push_str("            raise\n");
            out.push_str("    finally:\n");
            out.push_str(&format!(
                "        _lib.{c_prefix}_cancel_token_destroy(_cancel_token)\n"
            ));
        } else {
            out.push_str("    _loop = asyncio.get_event_loop()\n");
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
}

// ── Param helpers ──

fn py_list_convert_expr(name: &str, elem: &TypeRef) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("*[v.encode(\"utf-8\") if v is not None else None for v in {name}]")
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
            format!(
                "*[{var}.encode(\"utf-8\") if {var} is not None else None for {var} in {list_name}]"
            )
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
        TypeRef::StringUtf8 => {
            vec![format!(
                "{ind}_{name}_arr, _{name}_len = _string_to_byteslice({name})"
            )]
        }
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
            TypeRef::StringUtf8 => {
                vec![
                    format!("{ind}if {name} is not None:"),
                    format!("{ind}    _{name}_arr, _{name}_len = _string_to_byteslice({name})"),
                    format!("{ind}else:"),
                    format!("{ind}    _{name}_arr = None"),
                    format!("{ind}    _{name}_len = 0"),
                ]
            }
            TypeRef::BorrowedStr => {
                vec![format!(
                    "{ind}_{name}_c = {name}.encode(\"utf-8\") if {name} is not None else None"
                )]
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
        TypeRef::Callback(cb_name) => vec![
            format!("{ind}def _{name}_tramp(_ctx, *args):"),
            format!("{ind}    return {name}(*args)"),
            format!("{ind}_{name}_cfunc = _{cb_name}(_{name}_tramp)"),
            format!("{ind}_callback_refs.append(_{name}_cfunc)"),
        ],
        _ => vec![],
    }
}

fn py_param_call_args(name: &str, ty: &TypeRef) -> Vec<String> {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Handle => {
            vec![name.to_string()]
        }
        TypeRef::Bool => vec![format!("1 if {name} else 0")],
        TypeRef::StringUtf8 => {
            vec![format!("_{name}_arr"), format!("_{name}_len")]
        }
        TypeRef::BorrowedStr => vec![format!("{name}.encode(\"utf-8\")")],
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![format!("_{name}_arr"), format!("len({name})")]
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => vec![format!("{name}._ptr")],
        TypeRef::Enum(_) => vec![format!("{name}.value")],
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 => {
                vec![format!("_{name}_arr"), format!("_{name}_len")]
            }
            TypeRef::BorrowedStr => vec![format!("_{name}_c")],
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
        TypeRef::Callback(_) => {
            vec![format!("_{name}_cfunc"), "ctypes.c_void_p(0)".into()]
        }
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

fn render_return_value(out: &mut String, ty: &TypeRef, ind: &str, c_prefix: &str) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Handle => {
            out.push_str(&format!("{ind}return _result\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{ind}return bool(_result)\n"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{ind}_ptr = _result\n"));
            out.push_str(&format!("{ind}if not _ptr:\n"));
            out.push_str(&format!("{ind}    return \"\"\n"));
            out.push_str(&format!(
                "{ind}_s = ctypes.cast(_ptr, ctypes.c_char_p).value.decode(\"utf-8\")\n"
            ));
            out.push_str(&format!("{ind}_lib.{c_prefix}_free_string(_ptr)\n"));
            out.push_str(&format!("{ind}return _s\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return b\"\"\n"));
            out.push_str(&format!("{ind}_n = int(_out_len.value)\n"));
            out.push_str(&format!("{ind}_b = bytes(_result[:_n])\n"));
            out.push_str(&format!(
                "{ind}_lib.{c_prefix}_free_bytes(_result, ctypes.c_size_t(_n))\n"
            ));
            out.push_str(&format!("{ind}return _b\n"));
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
        TypeRef::Optional(inner) => render_optional_return(out, inner, ind, c_prefix),
        TypeRef::List(inner) => render_list_return(out, inner, ind),
        TypeRef::Map(k, v) => render_map_return(out, k, v, ind),
        TypeRef::Iterator(_) => unreachable!("iterator return handled in render_function"),
        TypeRef::Callback(_) => {
            unreachable!("validator should have rejected callback Python return")
        }
    }
}

fn render_optional_return(out: &mut String, inner: &TypeRef, ind: &str, c_prefix: &str) {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{ind}_ptr = _result\n"));
            out.push_str(&format!("{ind}if not _ptr:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!(
                "{ind}_s = ctypes.cast(_ptr, ctypes.c_char_p).value.decode(\"utf-8\")\n"
            ));
            out.push_str(&format!("{ind}_lib.{c_prefix}_free_string(_ptr)\n"));
            out.push_str(&format!("{ind}return _s\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{ind}if not _result:\n"));
            out.push_str(&format!("{ind}    return None\n"));
            out.push_str(&format!("{ind}_n = int(_out_len.value)\n"));
            out.push_str(&format!("{ind}_b = bytes(_result[:_n])\n"));
            out.push_str(&format!(
                "{ind}_lib.{c_prefix}_free_bytes(_result, ctypes.c_size_t(_n))\n"
            ));
            out.push_str(&format!("{ind}return _b\n"));
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

fn render_iterator_class(
    out: &mut String,
    module_name: &str,
    func_name: &str,
    inner: &TypeRef,
    c_prefix: &str,
) {
    let iter_tag = iter_type_name(func_name, module_name, c_prefix);
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
    c_prefix: &str,
) {
    let iter_tag = iter_type_name(func_name, module_name, c_prefix);
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
requires = ["setuptools>=61", "wheel"]
build-backend = "setuptools.build_meta"

[project]
name = "{package_name}"
description = "Python bindings for WeaveFFI (auto-generated)"
requires-python = ">=3.8"
dynamic = ["version"]

[project.optional-dependencies]
dev = ["pytest", "mypy"]

[tool.setuptools]
packages = ["{package_name}"]

[tool.setuptools.package-data]
{package_name} = ["*.dylib", "*.so", "*.dll"]

[tool.setuptools.dynamic.version]
attr = "{package_name}.__version__"

# cibuildwheel builds Linux, macOS, and Windows wheels in CI for PyPI upload.
# Run `pipx run cibuildwheel` (or use the pypa/cibuildwheel GitHub Action) from
# this directory to produce wheels. See https://cibuildwheel.pypa.io/.
[tool.cibuildwheel]
build = "cp38-* cp39-* cp310-* cp311-* cp312-*"
skip = "*-musllinux_* pp*"
# Regenerate bindings and rebuild the native cdylib for the current target
# before each wheel. Adjust the path to match where your IDL lives relative
# to this pyproject.toml (by default cibuildwheel runs here, so `../../api.yml`
# points at the repo root when bindings are produced under `generated/python/`).
before-build = "weaveffi build ../../api.yml"

[tool.cibuildwheel.linux]
archs = ["x86_64", "aarch64"]

[tool.cibuildwheel.macos]
archs = ["x86_64", "arm64"]

[tool.cibuildwheel.windows]
archs = ["AMD64"]
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

/// Package `__init__.py`: declares `__version__` as a plain top-level assignment
/// so `[tool.setuptools.dynamic.version]` can read it via AST, then re-exports
/// the ctypes wrapper module.
fn render_init_py() -> String {
    "__version__ = \"0.1.0\"\n\nfrom .weaveffi import *  # noqa: E402,F401,F403\n".to_string()
}

fn render_manifest_in(package_name: &str) -> String {
    format!(
        "include {package_name}/*.dylib\ninclude {package_name}/*.so\ninclude {package_name}/*.dll\n"
    )
}

fn render_smoke_test(api: &Api, package_name: &str, strip_module_prefix: bool) -> String {
    let mut out = format!(
        "\"\"\"Smoke tests for the generated {package_name} bindings.\"\"\"\n\nimport {package_name}\n\n\ndef test_module_is_importable() -> None:\n    assert {package_name} is not None\n    assert {package_name}.__version__\n"
    );
    if let Some((func_name, args)) = find_smoke_test_call(&api.modules, strip_module_prefix) {
        let arg_list = args.join(", ");
        out.push_str(&format!(
            "\n\ndef test_{func_name}_is_callable() -> None:\n    _ = {package_name}.{func_name}({arg_list})\n"
        ));
    }
    out
}

/// Walks the API looking for the first function whose parameters and return
/// are all scalar (integers, floats, bool). If one is found, returns the
/// Python wrapper name and a list of literal arguments to pass. Used to seed
/// the generated smoke test with a concrete callable example.
fn find_smoke_test_call(
    modules: &[Module],
    strip_module_prefix: bool,
) -> Option<(String, Vec<String>)> {
    for m in modules {
        for f in &m.functions {
            if f.r#async || f.cancellable {
                continue;
            }
            let mut args = Vec::with_capacity(f.params.len());
            let mut all_scalar = true;
            for p in &f.params {
                match scalar_literal(&p.ty) {
                    Some(v) => args.push(v.to_string()),
                    None => {
                        all_scalar = false;
                        break;
                    }
                }
            }
            if !all_scalar {
                continue;
            }
            if let Some(ret) = &f.returns {
                if scalar_literal(ret).is_none() {
                    continue;
                }
            }
            return Some((wrapper_name(&m.name, &f.name, strip_module_prefix), args));
        }
        if let Some(found) = find_smoke_test_call(&m.modules, strip_module_prefix) {
            return Some(found);
        }
    }
    None
}

fn scalar_literal(ty: &TypeRef) -> Option<&'static str> {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 => Some("0"),
        TypeRef::F64 => Some("0.0"),
        TypeRef::Bool => Some("False"),
        _ => None,
    }
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
        "from enum import IntEnum\nfrom typing import Any, Callable, Dict, Iterator, List, Optional\n",
    );
    for (m, path) in collect_modules_with_path(&api.modules) {
        for e in &m.enums {
            render_pyi_enum(&mut out, e);
        }
        for s in &m.structs {
            render_pyi_struct(&mut out, s);
        }
        for l in &m.listeners {
            render_pyi_listener(&mut out, l);
        }
        for f in &m.functions {
            render_pyi_function(&mut out, &path, f, strip_module_prefix);
        }
    }
    out
}

fn render_pyi_enum(out: &mut String, e: &EnumDef) {
    out.push_str(&format!("\nclass {}(IntEnum):\n", e.name));
    for v in &e.variants {
        out.push_str(&format!("    {}: int\n", v.name));
    }
}

fn render_pyi_struct(out: &mut String, s: &StructDef) {
    out.push_str(&format!("\nclass {}:\n", s.name));
    for field in &s.fields {
        let py_ty = py_type_hint(&field.ty);
        out.push_str(&format!(
            "    @property\n    def {}(self) -> {}: ...\n",
            field.name, py_ty
        ));
    }
}

fn render_pyi_listener(out: &mut String, l: &ListenerDef) {
    let class_name = snake_to_pascal(&l.name);
    out.push_str(&format!("\nclass {class_name}:\n"));
    out.push_str("    @staticmethod\n");
    out.push_str("    def register(callback: Callable[..., Any]) -> int: ...\n");
    out.push_str("    @staticmethod\n");
    out.push_str("    def unregister(id: int) -> None: ...\n");
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
    out.push_str(&format!(
        "\n{async_kw}def {}({}) -> {}: ...\n",
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
        Api, CallbackDef, EnumDef, EnumVariant, Function, ListenerDef, Module, Param, StructDef,
        StructField, TypeRef,
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
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
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
                out.join("python/MANIFEST.in").to_string(),
                out.join("python/tests/__init__.py").to_string(),
                out.join("python/tests/test_smoke.py").to_string(),
                out.join("python/README.md").to_string(),
            ]
        );
    }

    #[test]
    fn python_output_files_with_config_respects_naming() {
        let api = make_api(vec![]);
        let out = Utf8Path::new("/tmp/out");

        let default_files =
            PythonGenerator.output_files_with_config(&api, out, &GeneratorConfig::default());
        assert_eq!(
            default_files,
            vec![
                out.join("python/weaveffi/__init__.py").to_string(),
                out.join("python/weaveffi/weaveffi.py").to_string(),
                out.join("python/weaveffi/weaveffi.pyi").to_string(),
                out.join("python/pyproject.toml").to_string(),
                out.join("python/setup.py").to_string(),
                out.join("python/MANIFEST.in").to_string(),
                out.join("python/tests/__init__.py").to_string(),
                out.join("python/tests/test_smoke.py").to_string(),
                out.join("python/README.md").to_string(),
            ]
        );

        let config = GeneratorConfig {
            python_package_name: Some("mypkg".into()),
            ..GeneratorConfig::default()
        };
        let custom_files = PythonGenerator.output_files_with_config(&api, out, &config);
        assert_eq!(
            custom_files,
            vec![
                out.join("python/mypkg/__init__.py").to_string(),
                out.join("python/mypkg/weaveffi.py").to_string(),
                out.join("python/mypkg/weaveffi.pyi").to_string(),
                out.join("python/pyproject.toml").to_string(),
                out.join("python/setup.py").to_string(),
                out.join("python/MANIFEST.in").to_string(),
                out.join("python/tests/__init__.py").to_string(),
                out.join("python/tests/test_smoke.py").to_string(),
                out.join("python/README.md").to_string(),
            ]
        );
    }

    #[test]
    fn preamble_has_load_library() {
        let api = make_api(vec![]);
        let py = render_python_module(&api, true, "weaveffi");
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
        let py = render_python_module(&api, true, "weaveffi");
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
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");
        assert!(
            py.contains("def echo(msg: str) -> str:"),
            "missing signature: {py}"
        );
        assert!(
            py.contains("_fn.restype = ctypes.POINTER(ctypes.c_char)"),
            "string return must use raw POINTER(c_char) so the buffer is not auto-copied: {py}"
        );
        assert!(
            py.contains("_string_to_byteslice(msg)"),
            "missing _string_to_byteslice call: {py}"
        );
        assert!(
            py.contains("ctypes.cast(_ptr, ctypes.c_char_p).value.decode(\"utf-8\")"),
            "string return must cast the raw pointer to c_char_p and decode it: {py}"
        );
        assert!(
            py.contains("_lib.weaveffi_free_string(_ptr)"),
            "string return must call weaveffi_free_string on the raw pointer to release the C buffer: {py}"
        );
    }

    #[test]
    fn python_string_return_calls_free_string() {
        let api = make_api(vec![Module {
            name: "text".into(),
            functions: vec![Function {
                name: "echo".into(),
                params: vec![Param {
                    name: "s".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
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

        let py = render_python_module(&api, true, "weaveffi");

        let cast_pos = py
            .find("ctypes.cast(_ptr, ctypes.c_char_p).value.decode(\"utf-8\")")
            .unwrap_or_else(|| {
                panic!("expected raw pointer cast/decode in generated module: {py}")
            });
        let free_pos = py
            .find("_lib.weaveffi_free_string(_ptr)")
            .unwrap_or_else(|| panic!("expected weaveffi_free_string(_ptr) call to release the owned string buffer: {py}"));
        assert!(
            free_pos > cast_pos,
            "weaveffi_free_string(_ptr) must come after the cast/decode so the value is read before the buffer is freed: {py}"
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

        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");
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
            py.contains("ctypes.cast(_ptr, ctypes.c_char_p).value.decode(\"utf-8\")"),
            "struct string getter must cast the raw pointer to c_char_p and decode it: {py}"
        );
        assert!(
            py.contains("_lib.weaveffi_free_string(_ptr)"),
            "struct string getter must call weaveffi_free_string to release the C buffer: {py}"
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

        let py = render_python_module(&api, true, "weaveffi");
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
            }],
            returns: Some(TypeRef::Bool),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");
        assert!(
            py.contains("-> Optional[str]:"),
            "missing optional str return: {py}"
        );
        assert!(
            py.contains("ctypes.cast(_ptr, ctypes.c_char_p).value.decode(\"utf-8\")"),
            "optional string return must cast the raw pointer to c_char_p and decode it: {py}"
        );
        assert!(
            py.contains("_lib.weaveffi_free_string(_ptr)"),
            "optional string return must call weaveffi_free_string to release the C buffer: {py}"
        );
        assert!(
            py.contains("return None"),
            "optional string return must still return None for null pointers: {py}"
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

        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");
        assert!(
            py.contains("def email(self) -> Optional[str]:"),
            "missing optional getter: {py}"
        );
        assert!(
            py.contains("ctypes.cast(_ptr, ctypes.c_char_p).value.decode(\"utf-8\")"),
            "optional struct string getter must cast the raw pointer to c_char_p and decode it: {py}"
        );
        assert!(
            py.contains("_lib.weaveffi_free_string(_ptr)"),
            "optional struct string getter must call weaveffi_free_string to release the C buffer: {py}"
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

        let py = render_python_module(&api, true, "weaveffi");
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
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                        },
                        Param {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            mutable: false,
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

        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");
        assert!(
            py.contains("def data(self) -> bytes:"),
            "missing bytes getter: {py}"
        );
        assert!(
            py.contains("_out_len = ctypes.c_size_t(0)"),
            "missing out_len in bytes getter: {py}"
        );
        assert!(
            py.contains("_n = int(_out_len.value)"),
            "missing _n length capture: {py}"
        );
        assert!(
            py.contains("_b = bytes(_result[:_n])"),
            "missing bytes slice: {py}"
        );
        assert!(
            py.contains("_lib.weaveffi_free_bytes(_result, ctypes.c_size_t(_n))"),
            "struct getter must free returned bytes via weaveffi_free_bytes: {py}"
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
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
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
            pyi.contains("from typing import Any, Callable, Dict, Iterator, List, Optional"),
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
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
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
        assert!(py.contains("from typing import Any, Callable, Dict, Iterator, List, Optional"));
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

        let py = render_python_module(&api, true, "weaveffi");

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

        let py = render_python_module(&api, true, "weaveffi");

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

        let py = render_python_module(&api, true, "weaveffi");

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
            py.contains("_string_to_byteslice(prefix)"),
            "missing optional _string_to_byteslice"
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

        let py = render_python_module(&api, true, "weaveffi");

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

        let py = render_python_module(&api, true, "weaveffi");

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
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
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
        assert!(pyi.contains("from typing import Any, Callable, Dict, Iterator, List, Optional"));

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
                        },
                        Param {
                            name: "last_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                        },
                        Param {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            mutable: false,
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
        assert!(py.contains("_string_to_byteslice(first_name)"));
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
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
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
            pyproject.contains("dynamic = [\"version\"]"),
            "missing dynamic version declaration: {pyproject}"
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
        let py = render_python_module(&api, true, "weaveffi");
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
            py.contains("def _string_to_byteslice("),
            "missing _string_to_byteslice helper"
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
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
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
    fn python_load_library_respects_c_prefix() {
        let api = make_api(vec![Module {
            name: "math".into(),
            functions: vec![Function {
                name: "add".into(),
                params: vec![
                    Param {
                        name: "a".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                    },
                    Param {
                        name: "b".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                    },
                ],
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
            c_prefix: Some("myffi".into()),
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_python_c_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let py = std::fs::read_to_string(tmp.join("python/weaveffi/weaveffi.py")).unwrap();

        assert!(
            py.contains("name = \"libmyffi.dylib\""),
            "_load_library must use libmyffi.dylib for Darwin: {py}"
        );
        assert!(
            py.contains("name = \"libmyffi.so\""),
            "_load_library must use libmyffi.so for Linux: {py}"
        );
        assert!(
            py.contains("name = \"myffi.dll\""),
            "_load_library must use myffi.dll for Windows: {py}"
        );
        assert!(
            !py.contains("libweaveffi.dylib")
                && !py.contains("libweaveffi.so")
                && !py.contains("weaveffi.dll"),
            "must not retain default weaveffi library names when c_prefix is set: {py}"
        );

        assert!(
            py.contains("_lib.myffi_math_add"),
            "function body must call _lib.{{c_prefix}}_X_Y: {py}"
        );
        assert!(
            !py.contains("_lib.weaveffi_math_add"),
            "function body must not retain default weaveffi prefix: {py}"
        );

        assert!(
            py.contains("_lib.myffi_error_clear"),
            "preamble bindings must use c_prefix: {py}"
        );
        assert!(
            py.contains("_lib.myffi_free_string"),
            "preamble bindings must use c_prefix: {py}"
        );
        assert!(
            py.contains("_lib.myffi_free_bytes"),
            "preamble bindings must use c_prefix: {py}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
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
        let py = render_python_module(&api, true, "weaveffi");
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

        let py = render_python_module(&api, true, "weaveffi");

        assert!(
            py.contains("_string_to_byteslice(name)"),
            "string param should use _string_to_byteslice(name): {py}"
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

        let py = render_python_module(&api, true, "weaveffi");

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
            }],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);
        let code = render_python_module(&api, true, "weaveffi");
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
        assert!(
            code.contains("ctypes.cast(_ptr, ctypes.c_char_p).value.decode(\"utf-8\")"),
            "async string delivery must cast the raw pointer to c_char_p and decode it: {code}"
        );
        assert!(
            code.contains("_lib.weaveffi_free_string(_ptr)"),
            "async string delivery must call weaveffi_free_string to release the C buffer: {code}"
        );
    }

    #[test]
    fn python_cancellable_async_wires_asyncio_cancel_to_token() {
        let api = make_api(vec![simple_module(vec![
            Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                cancellable: true,
                deprecated: None,
                since: None,
            },
            Function {
                name: "fire".into(),
                params: vec![],
                returns: None,
                doc: None,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ])]);

        let code = render_python_module(&api, true, "weaveffi");

        assert!(
            code.contains("_lib.weaveffi_cancel_token_create.restype = ctypes.c_void_p"),
            "should register cancel_token_create binding: {code}"
        );
        assert!(
            code.contains("_lib.weaveffi_cancel_token_cancel.argtypes = [ctypes.c_void_p]"),
            "should register cancel_token_cancel binding: {code}"
        );
        assert!(
            code.contains("_lib.weaveffi_cancel_token_destroy.argtypes = [ctypes.c_void_p]"),
            "should register cancel_token_destroy binding: {code}"
        );
        assert!(
            code.contains("def _run_sync(id: int, _cancel_token: Any) -> int:"),
            "cancellable sync helper must accept _cancel_token: {code}"
        );
        assert!(
            code.contains("_cancel_token = _lib.weaveffi_cancel_token_create()"),
            "async wrapper must create a native cancel token: {code}"
        );
        assert!(
            code.contains("run_in_executor(None, _run_sync, id, _cancel_token)"),
            "async wrapper must forward the cancel token to the executor: {code}"
        );
        assert!(
            code.contains("except asyncio.CancelledError:"),
            "async wrapper must handle asyncio.CancelledError: {code}"
        );
        assert!(
            code.contains("_lib.weaveffi_cancel_token_cancel(_cancel_token)"),
            "CancelledError handler must forward to weaveffi_cancel_token_cancel: {code}"
        );
        assert!(
            code.contains("_lib.weaveffi_cancel_token_destroy(_cancel_token)"),
            "cancellable async wrapper must destroy the token in finally: {code}"
        );

        let fire_line = code
            .lines()
            .find(|l| l.contains("run_in_executor(None, _fire_sync"))
            .expect("non-cancellable fire should still use run_in_executor");
        assert!(
            !fire_line.contains("_cancel_token"),
            "non-cancellable async must not forward a cancel token: {fire_line}"
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

        let code = render_python_module(&api, true, "weaveffi");
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
        let py = render_python_module(&api, true, "weaveffi");
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
        let py = render_python_module(&api, true, "weaveffi");
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
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: Some("Use add_v2 instead".into()),
            since: Some("0.1.0".into()),
        }])]);
        let py = render_python_module(&api, true, "weaveffi");
        assert!(
            py.contains("warnings.warn(\"Use add_v2 instead\", DeprecationWarning, stacklevel=2)"),
            "missing deprecation warning: {py}"
        );
    }

    #[test]
    fn python_string_param_uses_ptr_and_len() {
        let api = make_api(vec![Module {
            name: "io".into(),
            functions: vec![Function {
                name: "log".into(),
                params: vec![Param {
                    name: "msg".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
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

        let py = render_python_module(&api, true, "weaveffi");

        assert!(
            py.contains("def _string_to_byteslice(s: str) -> tuple:"),
            "preamble should define _string_to_byteslice helper: {py}"
        );
        assert!(
            !py.contains("def _string_to_bytes("),
            "old _string_to_bytes helper should be removed: {py}"
        );

        assert!(
            py.contains(
                "_fn.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.POINTER(_WeaveffiErrorStruct)]"
            ),
            "argtypes should use (POINTER(c_uint8), c_size_t) for string param: {py}"
        );

        assert!(
            py.contains("_msg_arr, _msg_len = _string_to_byteslice(msg)"),
            "string param should call _string_to_byteslice helper: {py}"
        );

        assert!(
            py.contains("_fn(_msg_arr, _msg_len, ctypes.byref(_err))"),
            "call should pass _msg_arr, _msg_len: {py}"
        );

        let pyi = render_pyi_module(&api, true);
        assert!(
            pyi.contains("def log(msg: str) -> None: ..."),
            "pyi user-facing signature should still take str: {pyi}"
        );
    }

    #[test]
    fn python_bytes_param_uses_canonical_shape() {
        let api = make_api(vec![Module {
            name: "io".into(),
            functions: vec![Function {
                name: "send".into(),
                params: vec![Param {
                    name: "payload".into(),
                    ty: TypeRef::Bytes,
                    mutable: false,
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
        let py = render_python_module(&api, true, "weaveffi");
        assert!(
            py.contains(
                "_fn.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.POINTER(_WeaveffiErrorStruct)]"
            ),
            "Python ctypes argtypes for Bytes param must lower to (uint8_t*, size_t): {py}"
        );
        assert!(
            py.contains("_payload_arr = (ctypes.c_uint8 * len(payload))(*payload)"),
            "Python wrapper must build a c_uint8 array from the bytes input: {py}"
        );
        assert!(
            py.contains("_fn(_payload_arr, len(payload), ctypes.byref(_err))"),
            "Python wrapper must call C with (ptr, len, &err) for Bytes param: {py}"
        );
    }

    #[test]
    fn python_bytes_return_uses_canonical_shape() {
        let api = make_api(vec![Module {
            name: "io".into(),
            functions: vec![Function {
                name: "read".into(),
                params: vec![],
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
        let py = render_python_module(&api, true, "weaveffi");
        assert!(
            py.contains("_fn.restype = ctypes.POINTER(ctypes.c_uint8)"),
            "Python ctypes restype for Bytes return must be uint8_t*: {py}"
        );
        assert!(
            py.contains(
                "_fn.argtypes = [ctypes.POINTER(ctypes.c_size_t), ctypes.POINTER(_WeaveffiErrorStruct)]"
            ),
            "Python ctypes argtypes for Bytes return must include size_t* out-param + weaveffi_error*: {py}"
        );
        assert!(
            py.contains("_out_len = ctypes.c_size_t(0)"),
            "Python wrapper must allocate _out_len out-param: {py}"
        );
        assert!(
            py.contains("_result = _fn(ctypes.byref(_out_len), ctypes.byref(_err))"),
            "Python wrapper must call C with (&out_len, &err) for Bytes return: {py}"
        );
        assert!(
            py.contains("_lib.weaveffi_free_bytes(_result, ctypes.c_size_t(_n))"),
            "Python wrapper must free the returned bytes via weaveffi_free_bytes: {py}"
        );
        assert!(
            py.contains(
                "_lib.weaveffi_free_bytes.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t]"
            ),
            "weaveffi_free_bytes must take (uint8_t*, size_t) (no const): {py}"
        );
    }

    #[test]
    fn python_check_error_calls_weaveffi_error_clear() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "math".into(),
                functions: vec![Function {
                    name: "add".into(),
                    params: vec![
                        Param {
                            name: "a".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                        Param {
                            name: "b".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                    ],
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
            generators: None,
        };

        let py = render_python_module(&api, true, "weaveffi");
        let def_pos = py
            .find("def _check_error(err: _WeaveffiErrorStruct) -> None:")
            .expect("_check_error must be defined");
        let msg_pos = py[def_pos..]
            .find("message = err.message.decode(\"utf-8\")")
            .map(|p| p + def_pos)
            .expect("_check_error must capture err.message into a Python str");
        let clear_pos = py[def_pos..]
            .find("_lib.weaveffi_error_clear(ctypes.byref(err))")
            .map(|p| p + def_pos)
            .expect("_check_error must call _lib.weaveffi_error_clear after capturing the message");
        let raise_pos = py[def_pos..]
            .find("raise WeaveffiError(code, message)")
            .map(|p| p + def_pos)
            .expect("_check_error must raise after clearing");
        assert!(
            msg_pos < clear_pos,
            "weaveffi_error_clear must run AFTER capturing err.message: {py}"
        );
        assert!(
            clear_pos < raise_pos,
            "weaveffi_error_clear must run BEFORE raising: {py}"
        );
    }

    #[test]
    fn python_bytes_return_calls_free_bytes() {
        let api = make_api(vec![Module {
            name: "parity".into(),
            functions: vec![Function {
                name: "echo".into(),
                params: vec![Param {
                    name: "b".into(),
                    ty: TypeRef::Bytes,
                    mutable: false,
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
        let py = render_python_module(&api, true, "weaveffi");

        let copy_pos = py
            .find("_b = bytes(_result[:_n])")
            .expect("Python wrapper must copy the returned bytes via bytes(_result[:_n])");
        let free_pos = py
            .find("_lib.weaveffi_free_bytes(_result, ctypes.c_size_t(_n))")
            .expect("Python wrapper must free the returned pointer via _lib.weaveffi_free_bytes");
        assert!(
            copy_pos < free_pos,
            "_lib.weaveffi_free_bytes must run AFTER the bytes have been copied into a Python bytes object: {py}"
        );
    }

    #[test]
    fn python_struct_wrapper_calls_destroy() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let py = render_python_module(&api, true, "weaveffi");

        assert!(py.contains("class Contact:"), "missing class Contact: {py}");
        let del_pos = py
            .find("def __del__(self) -> None:")
            .expect("class must define __del__");
        let exit_pos = py
            .find("def __exit__(self, exc_type, exc_val, exc_tb) -> bool:")
            .expect("class must define __exit__ for context-manager cleanup");
        let enter_pos = py
            .find("def __enter__(self)")
            .expect("class must define __enter__ for context-manager usage");
        assert!(enter_pos < exit_pos);

        let dispose_pos = py
            .find("def _dispose(self) -> None:")
            .expect("class must define _dispose helper");
        let destroy_pos = py[dispose_pos..]
            .find("weaveffi_contacts_Contact_destroy(self._ptr)")
            .map(|p| dispose_pos + p)
            .expect("_dispose must call the C destroy function");
        assert!(destroy_pos > dispose_pos);

        let del_body = &py[del_pos..];
        assert!(
            del_body[..120].contains("self._dispose()"),
            "__del__ must call _dispose(): {del_body}"
        );
        let exit_body = &py[exit_pos..];
        assert!(
            exit_body[..160].contains("self._dispose()"),
            "__exit__ must call _dispose(): {exit_body}"
        );
    }

    #[test]
    fn python_struct_setter_string_uses_ptr_and_len() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "set_contact_name".into(),
                params: vec![
                    Param {
                        name: "contact".into(),
                        ty: TypeRef::TypedHandle("Contact".into()),
                        mutable: false,
                    },
                    Param {
                        name: "new_name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    },
                ],
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
                builder: false,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi");

        assert!(
            py.contains(
                "_fn.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.POINTER(_WeaveffiErrorStruct)]"
            ),
            "struct setter argtypes should include (POINTER(c_uint8), c_size_t) for string param: {py}"
        );
        assert!(
            py.contains("_new_name_arr, _new_name_len = _string_to_byteslice(new_name)"),
            "struct setter must call _string_to_byteslice helper for string param: {py}"
        );
        assert!(
            py.contains("_fn(contact._ptr, _new_name_arr, _new_name_len, ctypes.byref(_err))"),
            "struct setter call must pass (handle, _arr, _len, &err): {py}"
        );

        let pyi = render_pyi_module(&api, true);
        assert!(
            pyi.contains("def set_contact_name(contact: \"Contact\", new_name: str) -> None: ..."),
            "pyi struct setter signature should still take str: {pyi}"
        );
    }

    #[test]
    fn python_builder_setter_string_uses_ptr_and_len() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "Contact_Builder_set_name".into(),
                params: vec![
                    Param {
                        name: "builder".into(),
                        ty: TypeRef::Handle,
                        mutable: true,
                    },
                    Param {
                        name: "value".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    },
                ],
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
                builder: true,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi");

        assert!(
            py.contains(
                "_fn.argtypes = [ctypes.c_uint64, ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.POINTER(_WeaveffiErrorStruct)]"
            ),
            "builder setter argtypes should include (POINTER(c_uint8), c_size_t) for string param: {py}"
        );
        assert!(
            py.contains("_value_arr, _value_len = _string_to_byteslice(value)"),
            "builder setter must call _string_to_byteslice helper for string param: {py}"
        );
        assert!(
            py.contains("_fn(builder, _value_arr, _value_len, ctypes.byref(_err))"),
            "builder setter call must pass (handle, _arr, _len, &err): {py}"
        );

        let pyi = render_pyi_module(&api, true);
        assert!(
            pyi.contains("def Contact_Builder_set_name(builder: int, value: str) -> None: ..."),
            "pyi builder setter signature should still take str: {pyi}"
        );
    }

    #[test]
    fn capabilities_includes_callbacks_and_listeners() {
        let caps = PythonGenerator.capabilities();
        assert!(
            caps.contains(&Capability::Callbacks),
            "Python generator must advertise Callbacks now that callback codegen is implemented"
        );
        assert!(
            caps.contains(&Capability::Listeners),
            "Python generator must advertise Listeners now that listener codegen is implemented"
        );
        for cap in Capability::ALL {
            assert!(caps.contains(cap), "Python generator must support {cap:?}");
        }
    }

    #[test]
    fn python_emits_callback_cfunctype() {
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![Function {
                name: "subscribe".into(),
                params: vec![Param {
                    name: "handler".into(),
                    ty: TypeRef::Callback("OnData".into()),
                    mutable: false,
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
            callbacks: vec![CallbackDef {
                name: "OnData".into(),
                params: vec![Param {
                    name: "value".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: None,
                doc: None,
            }],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi");

        assert!(
            py.contains("_OnData = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_int32)"),
            "missing CFUNCTYPE definition for callback: {py}"
        );
        assert!(
            py.contains("from typing import Any, Callable, Dict, Iterator, List, Optional"),
            "preamble must import Any and Callable: {py}"
        );
        assert!(
            py.contains("_callback_refs: List[Any] = []"),
            "preamble must define _callback_refs keep-alive list: {py}"
        );
        assert!(
            py.contains("def subscribe(handler: Callable[..., Any]) -> None:"),
            "wrapper must accept a Python Callable: {py}"
        );
        assert!(
            py.contains("_OnData, ctypes.c_void_p"),
            "argtypes must include the CFUNCTYPE and a c_void_p context: {py}"
        );
        assert!(
            py.contains("def _handler_tramp(_ctx, *args):"),
            "wrapper must emit a trampoline that ignores the context: {py}"
        );
        assert!(
            py.contains("return handler(*args)"),
            "trampoline must delegate to the user callable: {py}"
        );
        assert!(
            py.contains("_handler_cfunc = _OnData(_handler_tramp)"),
            "wrapper must wrap the trampoline via the CFUNCTYPE constructor: {py}"
        );
        assert!(
            py.contains("_callback_refs.append(_handler_cfunc)"),
            "wrapper must keep the cfunc alive in _callback_refs: {py}"
        );
        assert!(
            py.contains("_fn(_handler_cfunc, ctypes.c_void_p(0), ctypes.byref(_err))"),
            "C call must pass the cfunc and a null context: {py}"
        );

        let pyi = render_pyi_module(&api, true);
        assert!(
            pyi.contains("from typing import Any, Callable, Dict, Iterator, List, Optional"),
            "pyi must import Any and Callable: {pyi}"
        );
        assert!(
            pyi.contains("def subscribe(handler: Callable[..., Any]) -> None: ..."),
            "pyi must declare callback param as Python Callable: {pyi}"
        );
    }

    #[test]
    fn python_emits_listener_class() {
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnData".into(),
                params: vec![Param {
                    name: "value".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: None,
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "data_stream".into(),
                event_callback: "OnData".into(),
                doc: None,
            }],
            errors: None,
            modules: vec![],
        }]);

        let py = render_python_module(&api, true, "weaveffi");

        assert!(
            py.contains("class DataStream:"),
            "missing listener class: {py}"
        );
        assert!(
            py.contains("_cfuncs: Dict[int, Any] = {}"),
            "listener must hold cfunc refs in a class-level dict to prevent GC: {py}"
        );
        assert!(
            py.contains("@staticmethod\n    def register(callback: Callable[..., Any]) -> int:"),
            "listener must expose @staticmethod register(callback) -> int: {py}"
        );
        assert!(
            py.contains("@staticmethod\n    def unregister(id: int) -> None:"),
            "listener must expose @staticmethod unregister(id: int): {py}"
        );
        assert!(
            py.contains("def _tramp(_ctx, *args):"),
            "register must emit a trampoline that ignores the context: {py}"
        );
        assert!(
            py.contains("return callback(*args)"),
            "trampoline must delegate to the user callable: {py}"
        );
        assert!(
            py.contains("_cfunc = _OnData(_tramp)"),
            "register must wrap the trampoline via the event callback's CFUNCTYPE: {py}"
        );
        assert!(
            py.contains("_fn = _lib.weaveffi_events_register_data_stream"),
            "register must bind the C register symbol: {py}"
        );
        assert!(
            py.contains("_fn.argtypes = [_OnData, ctypes.c_void_p]"),
            "register argtypes must be the CFUNCTYPE and a c_void_p context: {py}"
        );
        assert!(
            py.contains("_fn.restype = ctypes.c_uint64"),
            "register restype must be uint64 for the listener id: {py}"
        );
        assert!(
            py.contains("_id = _fn(_cfunc, ctypes.c_void_p(0))"),
            "register must call the C symbol with the cfunc and a null context: {py}"
        );
        assert!(
            py.contains("DataStream._cfuncs[_id] = _cfunc"),
            "register must store the cfunc in the class-level dict keyed by id: {py}"
        );
        assert!(
            py.contains("_fn = _lib.weaveffi_events_unregister_data_stream"),
            "unregister must bind the C unregister symbol: {py}"
        );
        assert!(
            py.contains("DataStream._cfuncs.pop(id, None)"),
            "unregister must pop the cfunc from the class-level dict: {py}"
        );

        let pyi = render_pyi_module(&api, true);
        assert!(
            pyi.contains("class DataStream:"),
            "pyi must declare listener class: {pyi}"
        );
        assert!(
            pyi.contains("def register(callback: Callable[..., Any]) -> int: ..."),
            "pyi must declare static register(callback) -> int: {pyi}"
        );
        assert!(
            pyi.contains("def unregister(id: int) -> None: ..."),
            "pyi must declare static unregister(id: int) -> None: {pyi}"
        );
    }

    #[test]
    fn python_outputs_have_version_stamp() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
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
            generators: None,
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_python_stamp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).unwrap();

        PythonGenerator.generate(&api, out_dir).unwrap();

        for rel in [
            "python/weaveffi/__init__.py",
            "python/weaveffi/weaveffi.py",
            "python/weaveffi/weaveffi.pyi",
            "python/pyproject.toml",
            "python/setup.py",
            "python/MANIFEST.in",
            "python/tests/__init__.py",
            "python/tests/test_smoke.py",
        ] {
            let contents = std::fs::read_to_string(tmp.join(rel)).unwrap();
            assert!(
                contents.starts_with("# WeaveFFI "),
                "{rel} missing stamp: {contents}"
            );
            assert!(
                contents.contains(" python "),
                "{rel} stamp missing generator name"
            );
            assert!(
                contents.contains("DO NOT EDIT"),
                "{rel} missing DO NOT EDIT"
            );
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn builtin_python_templates_parse() {
        let engine = builtin_template_engine().expect("built-in templates should parse");
        assert!(engine.has_template("python/module.tera"));
        assert!(engine.has_template("python/stubs.tera"));
    }

    #[test]
    fn python_user_template_overrides_builtin() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![],
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
            }],
            generators: None,
        };

        let tpl_dir = tempfile::tempdir().unwrap();
        let tpl_path = Utf8Path::from_path(tpl_dir.path()).unwrap();
        std::fs::create_dir_all(tpl_path.join("python")).unwrap();
        std::fs::write(
            tpl_path.join("python").join("module.tera"),
            "# custom python module for {{ modules[0].name }}\n",
        )
        .unwrap();
        std::fs::write(
            tpl_path.join("python").join("stubs.tera"),
            "# custom python stubs for {{ modules[0].name }}\n",
        )
        .unwrap();

        let mut engine = TemplateEngine::new();
        engine
            .load_builtin("python/module.tera", BUILTIN_MODULE_TEMPLATE)
            .unwrap();
        engine
            .load_builtin("python/stubs.tera", BUILTIN_STUBS_TEMPLATE)
            .unwrap();
        engine.load_dir(tpl_path).unwrap();

        let out = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(out.path()).unwrap();
        let config = GeneratorConfig::default();
        PythonGenerator
            .generate_with_templates(&api, out_dir, &config, Some(&engine))
            .unwrap();

        let module = std::fs::read_to_string(out_dir.join("python/weaveffi/weaveffi.py")).unwrap();
        assert!(
            module.contains("# custom python module for math"),
            "user template output missing from generated module: {module}"
        );
        assert!(
            !module.contains("class WeaveffiError"),
            "built-in formatter output must not appear when user override is used: {module}"
        );
        assert!(
            module.starts_with("# WeaveFFI "),
            "stamp header should still be emitted: {module}"
        );

        let stubs = std::fs::read_to_string(out_dir.join("python/weaveffi/weaveffi.pyi")).unwrap();
        assert!(
            stubs.contains("# custom python stubs for math"),
            "user template output missing from generated stubs: {stubs}"
        );
        assert!(
            !stubs.contains("from typing import"),
            "built-in formatter output must not appear when user override is used: {stubs}"
        );
        assert!(
            stubs.starts_with("# WeaveFFI "),
            "stamp header should still be emitted: {stubs}"
        );

        let init = std::fs::read_to_string(out_dir.join("python/weaveffi/__init__.py")).unwrap();
        assert!(
            init.contains("from .weaveffi import *"),
            "__init__.py must still be produced alongside the overridden templates: {init}"
        );
        let pyproject = std::fs::read_to_string(out_dir.join("python/pyproject.toml")).unwrap();
        assert!(
            pyproject.contains("[project]"),
            "pyproject.toml must still be produced alongside the overridden templates: {pyproject}"
        );
    }

    #[test]
    fn python_pyproject_has_modern_metadata() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_python_modern_metadata");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir).unwrap();

        let pyproject = std::fs::read_to_string(tmp.join("python/pyproject.toml")).unwrap();
        assert!(
            pyproject.contains("requires-python = \">=3.8\""),
            "pyproject should pin requires-python to >=3.8: {pyproject}"
        );
        assert!(
            pyproject.contains("requires = [\"setuptools>=61\", \"wheel\"]"),
            "build-system requires should include setuptools>=61 and wheel: {pyproject}"
        );
        assert!(
            pyproject.contains("[project.optional-dependencies]"),
            "pyproject should declare optional-dependencies: {pyproject}"
        );
        assert!(
            pyproject.contains("dev = [\"pytest\", \"mypy\"]"),
            "optional dev deps should include pytest and mypy: {pyproject}"
        );
        assert!(
            pyproject.contains("dynamic = [\"version\"]"),
            "pyproject should declare dynamic version: {pyproject}"
        );
        assert!(
            pyproject.contains("[tool.setuptools.dynamic.version]"),
            "pyproject should declare dynamic version source: {pyproject}"
        );
        assert!(
            pyproject.contains("attr = \"weaveffi.__version__\""),
            "dynamic version should read weaveffi.__version__: {pyproject}"
        );

        let init = std::fs::read_to_string(tmp.join("python/weaveffi/__init__.py")).unwrap();
        assert!(
            init.contains("__version__ = \"0.1.0\""),
            "__init__.py must define __version__ so setuptools can read it: {init}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn python_includes_native_lib_in_package_data() {
        let api = make_api(vec![]);

        let tmp = std::env::temp_dir().join("weaveffi_test_python_package_data");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir).unwrap();

        let pyproject = std::fs::read_to_string(tmp.join("python/pyproject.toml")).unwrap();
        assert!(
            pyproject.contains("[tool.setuptools.package-data]"),
            "pyproject should declare package-data: {pyproject}"
        );
        assert!(
            pyproject.contains("weaveffi = [\"*.dylib\", \"*.so\", \"*.dll\"]"),
            "package-data must include cdylib patterns for all platforms: {pyproject}"
        );

        let manifest = std::fs::read_to_string(tmp.join("python/MANIFEST.in")).unwrap();
        for pattern in ["weaveffi/*.dylib", "weaveffi/*.so", "weaveffi/*.dll"] {
            assert!(
                manifest.contains(pattern),
                "MANIFEST.in must list native library pattern {pattern}: {manifest}"
            );
        }

        let smoke = std::fs::read_to_string(tmp.join("python/tests/test_smoke.py")).unwrap();
        assert!(
            smoke.contains("import weaveffi"),
            "test_smoke.py must import the generated module: {smoke}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn python_pyproject_has_cibuildwheel_config() {
        let api = make_api(vec![]);

        let tmp = std::env::temp_dir().join("weaveffi_test_python_cibw_config");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        PythonGenerator.generate(&api, out_dir).unwrap();

        let pyproject = std::fs::read_to_string(tmp.join("python/pyproject.toml")).unwrap();
        assert!(
            pyproject.contains("[tool.cibuildwheel]"),
            "pyproject should declare a [tool.cibuildwheel] section: {pyproject}"
        );
        assert!(
            pyproject.contains("[tool.cibuildwheel.linux]"),
            "pyproject should declare a Linux target for cibuildwheel: {pyproject}"
        );
        assert!(
            pyproject.contains("[tool.cibuildwheel.macos]"),
            "pyproject should declare a macOS target for cibuildwheel: {pyproject}"
        );
        assert!(
            pyproject.contains("[tool.cibuildwheel.windows]"),
            "pyproject should declare a Windows target for cibuildwheel: {pyproject}"
        );
        assert!(
            pyproject.contains("before-build ="),
            "pyproject should declare a before-build hook for cibuildwheel: {pyproject}"
        );
        assert!(
            pyproject.contains("weaveffi build"),
            "before-build hook must invoke `weaveffi build`: {pyproject}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
