use anyhow::Result;
use camino::Utf8Path;
use heck::ToUpperCamelCase;
use weaveffi_core::codegen::Generator;
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::local_type_name;
use weaveffi_ir::ir::{Api, EnumDef, Function, Module, StructDef, TypeRef};

pub struct WasmGenerator;

const DEFAULT_MODULE_NAME: &str = "weaveffi_wasm";

impl WasmGenerator {
    fn generate_impl(&self, api: &Api, out_dir: &Utf8Path, module_name: &str) -> Result<()> {
        let wasm_dir = out_dir.join("wasm");
        std::fs::create_dir_all(&wasm_dir)?;
        std::fs::write(wasm_dir.join("README.md"), render_wasm_readme(api))?;
        std::fs::write(
            wasm_dir.join(format!("{module_name}.js")),
            render_wasm_js_stub(api, module_name),
        )?;
        std::fs::write(
            wasm_dir.join(format!("{module_name}.d.ts")),
            render_wasm_dts(api, module_name),
        )?;
        Ok(())
    }
}

impl Generator for WasmGenerator {
    fn name(&self) -> &'static str {
        "wasm"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, DEFAULT_MODULE_NAME)
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(api, out_dir, config.wasm_module_name())
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        output_file_list(out_dir, DEFAULT_MODULE_NAME)
    }

    fn output_files_with_config(
        &self,
        _api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Vec<String> {
        output_file_list(out_dir, config.wasm_module_name())
    }
}

fn output_file_list(out_dir: &Utf8Path, module_name: &str) -> Vec<String> {
    vec![
        out_dir.join("wasm/README.md").to_string(),
        out_dir.join(format!("wasm/{module_name}.js")).to_string(),
        out_dir.join(format!("wasm/{module_name}.d.ts")).to_string(),
    ]
}

fn wasm_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::Bool | TypeRef::Enum(_) => "i32",
        TypeRef::I64
        | TypeRef::Handle
        | TypeRef::TypedHandle(_)
        | TypeRef::Struct(_)
        | TypeRef::Iterator(_)
        | TypeRef::Map(_, _) => "i64",
        TypeRef::F64 => "f64",
        TypeRef::StringUtf8
        | TypeRef::BorrowedStr
        | TypeRef::Bytes
        | TypeRef::BorrowedBytes
        | TypeRef::List(_) => "i32, i32",
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(_)
            | TypeRef::Handle
            | TypeRef::TypedHandle(_)
            | TypeRef::Iterator(_)
            | TypeRef::Map(_, _) => "i64",
            _ => "i32, i32",
        },
        TypeRef::Callback(_) => todo!("callback WASM type"),
    }
}

fn wasm_type_note(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 => "native WASM i32",
        TypeRef::U32 => "unsigned mapped to i32",
        TypeRef::I64 => "native WASM i64",
        TypeRef::F64 => "native WASM f64",
        TypeRef::Bool => "0 = false, 1 = true",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            "ptr + len in linear memory"
        }
        TypeRef::TypedHandle(_) | TypeRef::Handle => "opaque pointer",
        TypeRef::Struct(_) => "opaque handle in linear memory",
        TypeRef::Enum(_) => "variant discriminant",
        TypeRef::List(_) => "ptr + len in linear memory",
        TypeRef::Map(_, _) => "opaque handle in linear memory",
        TypeRef::Iterator(_) => "opaque iterator handle",
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(_)
            | TypeRef::Handle
            | TypeRef::TypedHandle(_)
            | TypeRef::Iterator(_)
            | TypeRef::Map(_, _) => "opaque handle, 0 = absent",
            _ => "is_present flag + value",
        },
        TypeRef::Callback(_) => todo!("callback WASM type"),
    }
}

fn type_display(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 => "i32".into(),
        TypeRef::U32 => "u32".into(),
        TypeRef::I64 => "i64".into(),
        TypeRef::F64 => "f64".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "bytes".into(),
        TypeRef::TypedHandle(_) | TypeRef::Handle => "handle".into(),
        TypeRef::Struct(n) => local_type_name(n).to_string(),
        TypeRef::Enum(n) => n.clone(),
        TypeRef::Optional(inner) => format!("{}?", type_display(inner)),
        TypeRef::List(inner) => format!("[{}]", type_display(inner)),
        TypeRef::Iterator(inner) => format!("iter<{}>", type_display(inner)),
        TypeRef::Map(k, v) => format!("{{{}:{}}}", type_display(k), type_display(v)),
        TypeRef::Callback(_) => todo!("callback WASM type"),
    }
}

fn render_wasm_readme(api: &Api) -> String {
    let mut out = String::new();
    out.push_str("# WeaveFFI WASM (experimental)\n\n");
    out.push_str("This folder contains a minimal stub to help you load a `wasm32-unknown-unknown` build of your WeaveFFI library.\n\n");
    out.push_str("Build (example):\n\n");
    out.push_str("```bash\n");
    out.push_str("cargo build --target wasm32-unknown-unknown --release\n");
    out.push_str("```\n\n");
    out.push_str("Then serve the `.wasm` and use `weaveffi_wasm.js` to load it.\n\n");
    out.push_str("## Complex Type Handling\n\n");
    out.push_str("WASM only supports numeric types natively (`i32`, `i64`, `f32`, `f64`). ");
    out.push_str("Complex types are encoded at the boundary as follows:\n\n");
    out.push_str("### Structs\n\n");
    out.push_str("Structs are passed as **opaque handles** (`i64` pointers into linear memory). ");
    out.push_str(
        "The host cannot inspect struct fields directly; use the generated accessor functions ",
    );
    out.push_str("(`weaveffi_{module}_{struct}_get_{field}`) to read/write fields.\n\n");
    out.push_str("### Enums\n\n");
    out.push_str("Enums are passed as **`i32` values** corresponding to the variant's integer discriminant.\n\n");
    out.push_str("### Optionals\n\n");
    out.push_str("Optional values use **`0` / `null`** to represent the absent case. ");
    out.push_str("For numeric optionals, a separate `_is_present` flag (`i32`: 0 or 1) is used. ");
    out.push_str("For handle-typed optionals, a null pointer (`0`) signals absence.\n\n");
    out.push_str("### Lists\n\n");
    out.push_str("Lists are passed as a **pointer + length** pair (`i32` pointer, `i32` length) ");
    out.push_str("referencing a contiguous region in linear memory. The caller is responsible ");
    out.push_str("for allocating and freeing the backing memory.\n");
    out.push_str("\n### Error Handling\n\n");
    out.push_str("The generated JS wrappers automatically handle errors by passing an error\n");
    out.push_str("pointer as the last argument to each WASM function. Your WASM module must\n");
    out.push_str("export the following functions:\n\n");
    out.push_str("- `weaveffi_alloc(size: i32) -> i32` — allocate `size` bytes in linear memory\n");
    out.push_str("- `weaveffi_error_clear(err_ptr: i32)` — clear and free error resources\n");

    if !api.modules.is_empty() {
        render_api_reference(&mut out, api);
    }

    out
}

fn render_api_reference(out: &mut String, api: &Api) {
    out.push_str("\n## API Reference\n");
    for module in &api.modules {
        out.push_str(&format!("\n### Module: `{}`\n", module.name));

        if !module.functions.is_empty() {
            out.push_str("\n#### Functions\n");
            for func in &module.functions {
                render_function_ref(out, &module.name, func);
            }
        }

        if !module.structs.is_empty() {
            out.push_str("\n#### Structs\n");
            for s in &module.structs {
                render_struct_ref(out, &module.name, s);
            }
        }

        if !module.enums.is_empty() {
            out.push_str("\n#### Enums\n");
            for e in &module.enums {
                render_enum_ref(out, e);
            }
        }
    }
}

fn render_function_ref(out: &mut String, module_name: &str, func: &Function) {
    let abi_name = format!("weaveffi_{}_{}", module_name, func.name);
    out.push_str(&format!("\n##### `{abi_name}`\n\n"));

    if let Some(doc) = &func.doc {
        out.push_str(doc);
        out.push_str("\n\n");
    }

    let params_sig: Vec<String> = func
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, wasm_type(&p.ty)))
        .collect();
    let ret_sig = func.returns.as_ref().map_or("void", wasm_type);
    out.push_str(&format!(
        "`{abi_name}({}) -> {ret_sig}`\n\n",
        params_sig.join(", ")
    ));

    out.push_str("| Param | API Type | WASM | Notes |\n");
    out.push_str("|-------|----------|------|-------|\n");
    for param in &func.params {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | {} |\n",
            param.name,
            type_display(&param.ty),
            wasm_type(&param.ty),
            wasm_type_note(&param.ty)
        ));
    }
    if let Some(ret) = &func.returns {
        out.push_str(&format!(
            "| _returns_ | `{}` | `{}` | {} |\n",
            type_display(ret),
            wasm_type(ret),
            wasm_type_note(ret)
        ));
    }
}

fn render_struct_ref(out: &mut String, module_name: &str, s: &StructDef) {
    out.push_str(&format!("\n##### `{}`\n\n", s.name));

    if let Some(doc) = &s.doc {
        out.push_str(doc);
        out.push_str("\n\n");
    }

    out.push_str("Passed as opaque handle (`i64`).\n\n");

    if !s.fields.is_empty() {
        out.push_str("| Accessor | WASM Return |\n");
        out.push_str("|----------|-------------|\n");
        for field in &s.fields {
            out.push_str(&format!(
                "| `weaveffi_{}_{}_get_{}` | `{}` |\n",
                module_name,
                s.name,
                field.name,
                wasm_type(&field.ty)
            ));
        }
    }
}

fn render_enum_ref(out: &mut String, e: &EnumDef) {
    out.push_str(&format!("\n##### `{}`\n\n", e.name));

    if let Some(doc) = &e.doc {
        out.push_str(doc);
        out.push_str("\n\n");
    }

    out.push_str("Passed as `i32` discriminant.\n\n");
    out.push_str("| Variant | Value |\n");
    out.push_str("|---------|-------|\n");
    for v in &e.variants {
        out.push_str(&format!("| `{}` | `{}` |\n", v.name, v.value));
    }
}

fn api_needs_string_helpers(api: &Api) -> bool {
    fn module_needs_string_helpers(m: &Module) -> bool {
        m.functions.iter().any(|f| {
            f.params.iter().any(|p| matches!(p.ty, TypeRef::StringUtf8))
                || matches!(f.returns.as_ref(), Some(TypeRef::StringUtf8))
        }) || m
            .structs
            .iter()
            .any(|s| s.fields.iter().any(|f| matches!(f.ty, TypeRef::StringUtf8)))
            || m.modules.iter().any(module_needs_string_helpers)
    }
    api.modules.iter().any(module_needs_string_helpers)
}

fn ts_type_for(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 => "number".into(),
        TypeRef::Bool => "boolean".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Buffer".into(),
        TypeRef::Handle => "bigint".into(),
        TypeRef::TypedHandle(name) | TypeRef::Enum(name) => name.clone(),
        TypeRef::Struct(name) => local_type_name(name).to_string(),
        TypeRef::Optional(inner) => format!("{} | null", ts_type_for(inner)),
        TypeRef::List(inner) => {
            let inner_ts = ts_type_for(inner);
            if matches!(inner.as_ref(), TypeRef::Optional(_)) {
                format!("({inner_ts})[]")
            } else {
                format!("{inner_ts}[]")
            }
        }
        TypeRef::Iterator(inner) => {
            let t = ts_type_for(inner);
            format!("{t}[]")
        }
        TypeRef::Map(k, v) => format!("Record<{}, {}>", ts_type_for(k), ts_type_for(v)),
        TypeRef::Callback(_) => todo!("callback WASM type"),
    }
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

fn render_wasm_dts(api: &Api, module_name: &str) -> String {
    let pascal_name = module_name.to_upper_camel_case();
    let interface_name = format!("{pascal_name}Module");
    let load_fn = format!("load{pascal_name}");
    let mut out =
        String::from("// Generated TypeScript declarations for WeaveFFI WASM bindings\n\n");

    for (m, _path) in collect_modules_with_path(&api.modules) {
        for s in &m.structs {
            out.push_str(&format!("export interface {} {{\n", s.name));
            for field in &s.fields {
                out.push_str(&format!(
                    "  readonly {}: {};\n",
                    field.name,
                    ts_type_for(&field.ty)
                ));
            }
            out.push_str("}\n\n");
        }

        for e in &m.enums {
            out.push_str(&format!("export declare const {}: Readonly<{{\n", e.name));
            for v in &e.variants {
                out.push_str(&format!("  {}: {};\n", v.name, v.value));
            }
            out.push_str("}>;\n\n");
        }
    }

    out.push_str(&format!("export interface {interface_name} {{\n"));
    let all_mods = collect_all_modules(&api.modules);
    if all_mods.iter().any(|m| !m.functions.is_empty()) {
        out.push_str("  _raw: WebAssembly.Exports;\n");
        for module in &api.modules {
            render_dts_module_interface(&mut out, module, &module.name, "  ");
        }
    }
    out.push_str("}\n\n");

    out.push_str(&format!(
        "export function {load_fn}(url: string): Promise<{interface_name}>;\n"
    ));
    out
}

fn render_dts_module_interface(out: &mut String, m: &Module, module_path: &str, indent: &str) {
    let has_content = !m.functions.is_empty()
        || m.modules
            .iter()
            .any(|sub| !sub.functions.is_empty() || !sub.modules.is_empty());
    if !has_content {
        return;
    }
    out.push_str(&format!("{indent}{}: {{\n", m.name));
    let inner = format!("{indent}  ");
    for func in &m.functions {
        let params: Vec<String> = func
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, ts_type_for(&p.ty)))
            .collect();
        let base_ret = match &func.returns {
            Some(ty) => ts_type_for(ty),
            None => "void".into(),
        };
        let ret = if func.r#async {
            format!("Promise<{base_ret}>")
        } else {
            base_ret
        };
        out.push_str(&format!(
            "{inner}/** @throws {{Error}} if the native call fails */\n"
        ));
        out.push_str(&format!(
            "{inner}{}({}): {};\n",
            func.name,
            params.join(", "),
            ret
        ));
    }
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        render_dts_module_interface(out, sub, &sub_path, &inner);
    }
    out.push_str(&format!("{indent}}};\n"));
}

fn render_wasm_js_stub(api: &Api, module_name: &str) -> String {
    let pascal_name = module_name.to_upper_camel_case();
    let load_fn = format!("load{pascal_name}");
    let mut out = String::new();
    let needs_strings = api_needs_string_helpers(api);

    out.push_str("// WeaveFFI WASM bindings (auto-generated)\n");
    out.push_str("//\n");
    out.push_str("// Complex type conventions at the WASM boundary:\n");
    out.push_str("//\n");
    out.push_str("//   Structs   -> i64 opaque handle (pointer into linear memory)\n");
    out.push_str("//   Enums     -> i32 discriminant value\n");
    out.push_str(
        "//   Optionals -> 0/null for absent; for numerics a separate _is_present i32 flag\n",
    );
    out.push_str("//   Lists     -> (i32 pointer, i32 length) pair into linear memory\n");
    out.push('\n');

    if needs_strings {
        out.push_str("const _encoder = new TextEncoder();\n");
        out.push_str("const _decoder = new TextDecoder();\n\n");
        out.push_str("function _encodeString(wasm, str) {\n");
        out.push_str("  const bytes = _encoder.encode(str);\n");
        out.push_str("  const ptr = wasm.weaveffi_alloc(bytes.length);\n");
        out.push_str("  new Uint8Array(wasm.memory.buffer, ptr, bytes.length).set(bytes);\n");
        out.push_str("  return [ptr, bytes.length];\n");
        out.push_str("}\n\n");
        out.push_str("function _decodeString(wasm, ptr, len) {\n");
        out.push_str("  return _decoder.decode(new Uint8Array(wasm.memory.buffer, ptr, len));\n");
        out.push_str("}\n\n");
    }

    let all_mods = collect_all_modules(&api.modules);
    let has_functions = all_mods.iter().any(|m| !m.functions.is_empty());
    let has_async = api_has_async(api);
    if has_functions {
        out.push_str("function _allocError(wasm) {\n");
        out.push_str("  return wasm.weaveffi_alloc(8);\n");
        out.push_str("}\n\n");
        out.push_str("function _checkError(wasm, errPtr) {\n");
        out.push_str("  const buffer = wasm.memory.buffer;\n");
        out.push_str("  const code = new Int32Array(buffer, errPtr, 1)[0];\n");
        out.push_str("  if (code !== 0) {\n");
        out.push_str("    const msgPtr = new Uint32Array(buffer, errPtr + 4, 1)[0];\n");
        out.push_str("    const bytes = new Uint8Array(buffer, msgPtr);\n");
        out.push_str("    let end = 0;\n");
        out.push_str("    while (bytes[end] !== 0) end++;\n");
        out.push_str(
            "    const msg = new TextDecoder().decode(new Uint8Array(buffer, msgPtr, end));\n",
        );
        out.push_str("    wasm.weaveffi_error_clear(errPtr);\n");
        out.push_str("    throw new Error(`WeaveFFI error ${code}: ${msg}`);\n");
        out.push_str("  }\n");
        out.push_str("}\n\n");
    }

    if has_async {
        out.push_str("function _registerTrampoline(table, paramTypes, handler) {\n");
        out.push_str("  const idx = table.grow(1);\n");
        out.push_str("  table.set(idx, new WebAssembly.Function(\n");
        out.push_str("    { parameters: paramTypes, results: [] },\n");
        out.push_str("    handler\n");
        out.push_str("  ));\n");
        out.push_str("  return idx;\n");
        out.push_str("}\n\n");
    }

    for (module, _path) in collect_modules_with_path(&api.modules) {
        for e in &module.enums {
            out.push_str(&format!("export const {} = Object.freeze({{\n", e.name));
            for v in &e.variants {
                out.push_str(&format!("  {}: {},\n", v.name, v.value));
            }
            out.push_str("});\n\n");
        }
    }

    for (module, path) in collect_modules_with_path(&api.modules) {
        for s in &module.structs {
            out.push_str(&format!("class {} {{\n", s.name));
            out.push_str("  constructor(wasm, handle) {\n");
            out.push_str("    this._wasm = wasm;\n");
            out.push_str("    this._handle = handle;\n");
            out.push_str("  }\n");
            for field in &s.fields {
                let accessor = format!("weaveffi_{}_{}_get_{}", path, s.name, field.name);
                match &field.ty {
                    TypeRef::Iterator(_) => {
                        unreachable!("iterator not valid as struct field")
                    }
                    TypeRef::Bool => {
                        out.push_str(&format!("  get {}() {{\n", field.name));
                        out.push_str(&format!(
                            "    return this._wasm.{}(this._handle) !== 0;\n",
                            accessor
                        ));
                        out.push_str("  }\n");
                    }
                    TypeRef::StringUtf8 => {
                        out.push_str(&format!("  get {}() {{\n", field.name));
                        out.push_str("    const retptr = this._wasm.weaveffi_alloc(8);\n");
                        out.push_str(&format!(
                            "    this._wasm.{}(retptr, this._handle);\n",
                            accessor
                        ));
                        out.push_str("    const view = new DataView(this._wasm.memory.buffer);\n");
                        out.push_str("    return _decodeString(this._wasm, view.getInt32(retptr, true), view.getInt32(retptr + 4, true));\n");
                        out.push_str("  }\n");
                    }
                    _ => {
                        out.push_str(&format!("  get {}() {{\n", field.name));
                        out.push_str(&format!(
                            "    return this._wasm.{}(this._handle);\n",
                            accessor
                        ));
                        out.push_str("  }\n");
                    }
                }
            }
            out.push_str("}\n\n");
        }
    }

    out.push_str("/**\n");
    out.push_str(" * Load a WeaveFFI WASM module from the given URL.\n");
    out.push_str(" *\n");
    out.push_str(" * @param {string} url - URL to the `.wasm` file.\n");
    if api.modules.is_empty() {
        out.push_str(" * @returns {Promise<WebAssembly.Exports>} The exported WASM functions.\n");
    } else {
        out.push_str(" * @returns {Promise<Object>} The API bindings.\n");
    }
    out.push_str(" *\n");
    out.push_str(" * Exported functions follow the C ABI naming convention:\n");
    out.push_str(" *   weaveffi_{module}_{function}(params...) -> result\n");
    out.push_str(" *\n");
    out.push_str(" * @example\n");
    out.push_str(&format!(" * const api = await {load_fn}('lib.wasm');\n"));
    out.push_str(" *\n");
    out.push_str(" * // Primitive: (i32, i32) -> i32\n");
    out.push_str(" * const sum = api.math.add(1, 2);\n");
    out.push_str(" *\n");
    out.push_str(" * // Struct handle: () -> i64 (opaque pointer)\n");
    out.push_str(" * const handle = api.contacts.create();\n");
    out.push_str(" *\n");
    out.push_str(" * // Enum: (i32 discriminant) -> void\n");
    out.push_str(" * api.ui.set_color(0); // 0 = first variant\n");
    out.push_str(" *\n");
    out.push_str(" * // Optional: (i32 is_present, i32 value) -> void\n");
    out.push_str(" * api.config.set_timeout(1, 5000); // present\n");
    out.push_str(" * api.config.set_timeout(0, 0);    // absent\n");
    out.push_str(" *\n");
    out.push_str(" * // List: (i32 pointer, i32 length) -> void\n");
    out.push_str(" * api.data.process(ptr, len);\n");
    out.push_str(" */\n");
    out.push_str(&format!("export async function {load_fn}(url) {{\n"));
    out.push_str("  const response = await fetch(url);\n");
    out.push_str("  const bytes = await response.arrayBuffer();\n");
    out.push_str("  const { instance } = await WebAssembly.instantiate(bytes, {});\n");

    if api.modules.is_empty() {
        out.push_str("  return instance.exports;\n");
    } else {
        out.push_str("  const wasm = instance.exports;\n\n");

        if has_async {
            out.push_str("  let _nextCtxId = 1;\n");
            out.push_str("  const _asyncContexts = new Map();\n");
            out.push_str("  const _table = wasm.__indirect_function_table;\n\n");
            out.push_str("  function _asyncHandler(ctxId, errPtr, ...results) {\n");
            out.push_str("    const ctx = _asyncContexts.get(ctxId);\n");
            out.push_str("    if (!ctx) return;\n");
            out.push_str("    _asyncContexts.delete(ctxId);\n");
            out.push_str("    try {\n");
            out.push_str("      if (errPtr !== 0) _checkError(wasm, errPtr);\n");
            out.push_str(
                "      ctx.resolve(ctx.unwrap ? ctx.unwrap(wasm, ...results) : results[0]);\n",
            );
            out.push_str("    } catch (e) {\n");
            out.push_str("      ctx.reject(e);\n");
            out.push_str("    }\n");
            out.push_str("  }\n\n");

            let mut trampolines: Vec<(String, Vec<&'static str>)> = Vec::new();
            for m in collect_all_modules(&api.modules) {
                for f in &m.functions {
                    if f.r#async {
                        let params = async_cb_wasm_params(f.returns.as_ref());
                        let key = params.join("_");
                        if !trampolines.iter().any(|(k, _)| k == &key) {
                            trampolines.push((key, params));
                        }
                    }
                }
            }
            for (sig_key, params) in &trampolines {
                let params_js: Vec<String> = params.iter().map(|p| format!("'{p}'")).collect();
                out.push_str(&format!(
                    "  const _cbPtr_{sig_key} = _registerTrampoline(_table, [{}], _asyncHandler);\n",
                    params_js.join(", ")
                ));
            }
            out.push('\n');
        }

        out.push_str("  return {\n");
        out.push_str("    _raw: wasm,\n");
        for module in &api.modules {
            render_js_module_object(&mut out, module, &module.name, "    ");
        }
        out.push_str("  };\n");
    }

    out.push_str("}\n");
    out
}

fn render_js_module_object(out: &mut String, m: &Module, module_path: &str, indent: &str) {
    let has_content = !m.functions.is_empty()
        || m.modules
            .iter()
            .any(|sub| !sub.functions.is_empty() || !sub.modules.is_empty());
    if !has_content {
        return;
    }
    out.push_str(&format!("{indent}{}: {{\n", m.name));
    let extra = indent.len().saturating_sub(4);
    for func in &m.functions {
        let mut buf = String::new();
        if func.r#async {
            emit_js_async_function_wrapper(&mut buf, module_path, func);
        } else {
            emit_js_function_wrapper(&mut buf, module_path, func);
        }
        if extra > 0 {
            let pad = " ".repeat(extra);
            for line in buf.lines() {
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str(&pad);
                    out.push_str(line);
                    out.push('\n');
                }
            }
        } else {
            out.push_str(&buf);
        }
    }
    let child_indent = format!("{indent}  ");
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        render_js_module_object(out, sub, &sub_path, &child_indent);
    }
    out.push_str(&format!("{indent}}},\n"));
}

fn emit_js_function_wrapper(out: &mut String, module_name: &str, func: &Function) {
    let abi_name = format!("weaveffi_{}_{}", module_name, func.name);
    let js_params: Vec<&str> = func.params.iter().map(|p| p.name.as_str()).collect();
    let indent = "        ";

    out.push_str(&format!(
        "      {}({}) {{\n",
        func.name,
        js_params.join(", ")
    ));

    out.push_str(&format!("{indent}const _err = _allocError(wasm);\n"));

    let mut wasm_args = Vec::new();
    let returns_string = matches!(func.returns.as_ref(), Some(TypeRef::StringUtf8));

    if returns_string {
        out.push_str(&format!("{indent}const retptr = wasm.weaveffi_alloc(8);\n"));
        wasm_args.push("retptr".to_string());
    }

    for param in &func.params {
        match &param.ty {
            TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
            TypeRef::StringUtf8 => {
                out.push_str(&format!(
                    "{indent}const [{name}_ptr, {name}_len] = _encodeString(wasm, {name});\n",
                    name = param.name
                ));
                wasm_args.push(format!("{}_ptr", param.name));
                wasm_args.push(format!("{}_len", param.name));
            }
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                wasm_args.push(format!("{}._handle", param.name));
            }
            _ => {
                wasm_args.push(param.name.clone());
            }
        }
    }

    wasm_args.push("_err".to_string());
    let wasm_call = format!("wasm.{}({})", abi_name, wasm_args.join(", "));

    match func.returns.as_ref() {
        None => {
            out.push_str(&format!("{indent}{wasm_call};\n"));
            out.push_str(&format!("{indent}_checkError(wasm, _err);\n"));
        }
        Some(TypeRef::Bool) => {
            out.push_str(&format!("{indent}const _result = {wasm_call};\n"));
            out.push_str(&format!("{indent}_checkError(wasm, _err);\n"));
            out.push_str(&format!("{indent}return _result !== 0;\n"));
        }
        Some(TypeRef::StringUtf8) => {
            out.push_str(&format!("{indent}{wasm_call};\n"));
            out.push_str(&format!("{indent}_checkError(wasm, _err);\n"));
            out.push_str(&format!(
                "{indent}const view = new DataView(wasm.memory.buffer);\n"
            ));
            out.push_str(&format!("{indent}return _decodeString(wasm, view.getInt32(retptr, true), view.getInt32(retptr + 4, true));\n"));
        }
        Some(TypeRef::Struct(name)) => {
            let cls = local_type_name(name);
            out.push_str(&format!("{indent}const _result = {wasm_call};\n"));
            out.push_str(&format!("{indent}_checkError(wasm, _err);\n"));
            out.push_str(&format!("{indent}return new {cls}(wasm, _result);\n"));
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::Struct(name) => {
                let cls = local_type_name(name);
                out.push_str(&format!("{indent}const result = {wasm_call};\n"));
                out.push_str(&format!("{indent}_checkError(wasm, _err);\n"));
                out.push_str(&format!(
                    "{indent}return result === 0n ? null : new {cls}(wasm, result);\n"
                ));
            }
            _ => {
                out.push_str(&format!("{indent}const _result = {wasm_call};\n"));
                out.push_str(&format!("{indent}_checkError(wasm, _err);\n"));
                out.push_str(&format!("{indent}return _result;\n"));
            }
        },
        _ => {
            out.push_str(&format!("{indent}const _result = {wasm_call};\n"));
            out.push_str(&format!("{indent}_checkError(wasm, _err);\n"));
            out.push_str(&format!("{indent}return _result;\n"));
        }
    }

    out.push_str("      },\n");
}

fn api_has_async(api: &Api) -> bool {
    collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async))
}

fn async_cb_wasm_params(returns: Option<&TypeRef>) -> Vec<&'static str> {
    let mut params = vec!["i32", "i32"];
    match returns {
        None => {}
        Some(
            TypeRef::I32
            | TypeRef::U32
            | TypeRef::Bool
            | TypeRef::Enum(_)
            | TypeRef::StringUtf8
            | TypeRef::BorrowedStr,
        ) => {
            params.push("i32");
        }
        Some(
            TypeRef::I64
            | TypeRef::Handle
            | TypeRef::TypedHandle(_)
            | TypeRef::Struct(_)
            | TypeRef::Iterator(_),
        ) => {
            params.push("i64");
        }
        Some(TypeRef::F64) => {
            params.push("f64");
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_)) => {
            params.push("i32");
            params.push("i32");
        }
        Some(TypeRef::Map(_, _)) => {
            params.push("i32");
            params.push("i32");
            params.push("i32");
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::Struct(_)
            | TypeRef::Handle
            | TypeRef::TypedHandle(_)
            | TypeRef::Iterator(_)
            | TypeRef::Map(_, _) => {
                params.push("i64");
            }
            _ => {
                params.push("i32");
                params.push("i32");
            }
        },
        Some(TypeRef::Callback(_)) => todo!("callback WASM type"),
    }
    params
}

fn emit_js_async_function_wrapper(out: &mut String, module_name: &str, func: &Function) {
    let abi_name = format!("weaveffi_{}_{}", module_name, func.name);
    let js_params: Vec<&str> = func.params.iter().map(|p| p.name.as_str()).collect();
    let indent = "        ";
    let indent2 = "          ";

    out.push_str(&format!(
        "      {}({}) {{\n",
        func.name,
        js_params.join(", ")
    ));
    out.push_str(&format!(
        "{indent}return new Promise((resolve, reject) => {{\n"
    ));
    out.push_str(&format!("{indent2}const ctxId = _nextCtxId++;\n"));

    match func.returns.as_ref() {
        None => {
            out.push_str(&format!(
                "{indent2}_asyncContexts.set(ctxId, {{ resolve, reject }});\n"
            ));
        }
        Some(TypeRef::Bool) => {
            out.push_str(&format!(
                "{indent2}_asyncContexts.set(ctxId, {{ resolve, reject, unwrap: (w, r) => r !== 0 }});\n"
            ));
        }
        Some(TypeRef::StringUtf8) => {
            out.push_str(&format!(
                "{indent2}_asyncContexts.set(ctxId, {{ resolve, reject, unwrap: (w, ptr) => {{\n"
            ));
            out.push_str(&format!(
                "{indent2}  const b = new Uint8Array(w.memory.buffer, ptr);\n"
            ));
            out.push_str(&format!("{indent2}  let e = 0; while (b[e] !== 0) e++;\n"));
            out.push_str(&format!(
                "{indent2}  return new TextDecoder().decode(new Uint8Array(w.memory.buffer, ptr, e));\n"
            ));
            out.push_str(&format!("{indent2}}} }});\n"));
        }
        Some(TypeRef::Struct(name)) => {
            let cls = local_type_name(name);
            out.push_str(&format!(
                "{indent2}_asyncContexts.set(ctxId, {{ resolve, reject, unwrap: (w, handle) => new {cls}(w, handle) }});\n"
            ));
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::Struct(name) => {
                let cls = local_type_name(name);
                out.push_str(&format!(
                    "{indent2}_asyncContexts.set(ctxId, {{ resolve, reject, unwrap: (w, handle) => handle === 0n ? null : new {cls}(w, handle) }});\n"
                ));
            }
            _ => {
                out.push_str(&format!(
                    "{indent2}_asyncContexts.set(ctxId, {{ resolve, reject }});\n"
                ));
            }
        },
        _ => {
            out.push_str(&format!(
                "{indent2}_asyncContexts.set(ctxId, {{ resolve, reject }});\n"
            ));
        }
    }

    let mut wasm_args = Vec::new();
    for param in &func.params {
        match &param.ty {
            TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
            TypeRef::StringUtf8 => {
                out.push_str(&format!(
                    "{indent2}const [{name}_ptr, {name}_len] = _encodeString(wasm, {name});\n",
                    name = param.name
                ));
                wasm_args.push(format!("{}_ptr", param.name));
                wasm_args.push(format!("{}_len", param.name));
            }
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                wasm_args.push(format!("{}._handle", param.name));
            }
            _ => {
                wasm_args.push(param.name.clone());
            }
        }
    }

    let cb_params = async_cb_wasm_params(func.returns.as_ref());
    let sig_key = cb_params.join("_");
    if func.cancellable {
        wasm_args.push("0".to_string());
    }
    wasm_args.push(format!("_cbPtr_{sig_key}"));
    wasm_args.push("ctxId".to_string());

    out.push_str(&format!(
        "{indent2}wasm.{abi_name}_async({});\n",
        wasm_args.join(", ")
    ));

    out.push_str(&format!("{indent}}}));\n"));
    out.push_str("      },\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;
    use weaveffi_core::config::GeneratorConfig;
    use weaveffi_ir::ir::{EnumVariant, Module, Param, StructField};

    fn empty_api() -> Api {
        Api {
            version: "0.1.0".into(),
            modules: vec![],
            generators: None,
        }
    }

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".into(),
            modules,
            generators: None,
        }
    }

    fn sample_api() -> Api {
        make_api(vec![Module {
            name: "math".into(),
            functions: vec![Function {
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
                doc: Some("Add two numbers".into()),
                r#async: false,
                cancellable: false,
            }],
            structs: vec![StructDef {
                name: "Point".into(),
                doc: Some("A 2D point".into()),
                fields: vec![
                    StructField {
                        name: "x".into(),
                        ty: TypeRef::F64,
                        doc: None,
                    },
                    StructField {
                        name: "y".into(),
                        ty: TypeRef::F64,
                        doc: None,
                    },
                ],
            }],
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
        }])
    }

    #[test]
    fn readme_documents_structs() {
        let readme = render_wasm_readme(&empty_api());
        assert!(readme.contains("### Structs"));
        assert!(readme.contains("opaque handles"));
        assert!(readme.contains("`i64` pointers"));
    }

    #[test]
    fn readme_documents_enums() {
        let readme = render_wasm_readme(&empty_api());
        assert!(readme.contains("### Enums"));
        assert!(readme.contains("`i32` values"));
        assert!(readme.contains("discriminant"));
    }

    #[test]
    fn readme_documents_optionals() {
        let readme = render_wasm_readme(&empty_api());
        assert!(readme.contains("### Optionals"));
        assert!(readme.contains("`0` / `null`"));
        assert!(readme.contains("_is_present"));
    }

    #[test]
    fn readme_documents_lists() {
        let readme = render_wasm_readme(&empty_api());
        assert!(readme.contains("### Lists"));
        assert!(readme.contains("pointer + length"));
        assert!(readme.contains("`i32` pointer, `i32` length"));
    }

    #[test]
    fn js_stub_has_jsdoc() {
        let js = render_wasm_js_stub(&empty_api(), DEFAULT_MODULE_NAME);
        assert!(js.contains("@param {string} url"));
        assert!(js.contains("@returns {Promise<WebAssembly.Exports>}"));
        assert!(js.contains("@example"));
    }

    #[test]
    fn js_stub_documents_complex_types() {
        let js = render_wasm_js_stub(&empty_api(), DEFAULT_MODULE_NAME);
        assert!(js.contains("Struct handle: () -> i64 (opaque pointer)"));
        assert!(js.contains("Enum: (i32 discriminant) -> void"));
        assert!(js.contains("Optional: (i32 is_present, i32 value) -> void"));
        assert!(js.contains("List: (i32 pointer, i32 length) -> void"));
    }

    #[test]
    fn js_stub_has_type_convention_header() {
        let js = render_wasm_js_stub(&empty_api(), DEFAULT_MODULE_NAME);
        assert!(js.contains("Structs   -> i64 opaque handle"));
        assert!(js.contains("Enums     -> i32 discriminant value"));
        assert!(js.contains("Optionals -> 0/null for absent"));
        assert!(js.contains("Lists     -> (i32 pointer, i32 length)"));
    }

    #[test]
    fn generate_writes_both_files() {
        let tmp = std::env::temp_dir().join("weaveffi_test_wasm_gen");
        let _ = std::fs::remove_dir_all(&tmp);
        let out = Utf8Path::from_path(tmp.as_path()).unwrap();
        let api = make_api(vec![]);
        WasmGenerator.generate(&api, out).unwrap();

        let readme = std::fs::read_to_string(out.join("wasm/README.md")).unwrap();
        assert!(readme.contains("## Complex Type Handling"));

        let js = std::fs::read_to_string(out.join("wasm/weaveffi_wasm.js")).unwrap();
        assert!(js.contains("export async function loadWeaveffiWasm"));
        assert!(js.contains("@param {string} url"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_api_has_no_api_reference() {
        let readme = render_wasm_readme(&empty_api());
        assert!(!readme.contains("## API Reference"));
    }

    #[test]
    fn api_reference_lists_module() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("## API Reference"));
        assert!(readme.contains("### Module: `math`"));
    }

    #[test]
    fn api_reference_function_abi_name() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("##### `weaveffi_math_add`"));
    }

    #[test]
    fn api_reference_function_signature() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("`weaveffi_math_add(a: i32, b: i32) -> i32`"));
    }

    #[test]
    fn api_reference_function_param_table() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("| `a` | `i32` | `i32` | native WASM i32 |"));
        assert!(readme.contains("| `b` | `i32` | `i32` | native WASM i32 |"));
        assert!(readme.contains("| _returns_ | `i32` | `i32` | native WASM i32 |"));
    }

    #[test]
    fn api_reference_function_doc() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("Add two numbers"));
    }

    #[test]
    fn api_reference_struct_accessors() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("##### `Point`"));
        assert!(readme.contains("opaque handle (`i64`)"));
        assert!(readme.contains("| `weaveffi_math_Point_get_x` | `f64` |"));
        assert!(readme.contains("| `weaveffi_math_Point_get_y` | `f64` |"));
    }

    #[test]
    fn api_reference_enum_discriminants() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("##### `Color`"));
        assert!(readme.contains("`i32` discriminant"));
        assert!(readme.contains("| `Red` | `0` |"));
        assert!(readme.contains("| `Green` | `1` |"));
        assert!(readme.contains("| `Blue` | `2` |"));
    }

    #[test]
    fn wasm_type_maps_all_variants() {
        assert_eq!(wasm_type(&TypeRef::I32), "i32");
        assert_eq!(wasm_type(&TypeRef::U32), "i32");
        assert_eq!(wasm_type(&TypeRef::I64), "i64");
        assert_eq!(wasm_type(&TypeRef::F64), "f64");
        assert_eq!(wasm_type(&TypeRef::Bool), "i32");
        assert_eq!(wasm_type(&TypeRef::StringUtf8), "i32, i32");
        assert_eq!(wasm_type(&TypeRef::Bytes), "i32, i32");
        assert_eq!(wasm_type(&TypeRef::Handle), "i64");
        assert_eq!(wasm_type(&TypeRef::Struct("Foo".into())), "i64");
        assert_eq!(wasm_type(&TypeRef::Enum("Bar".into())), "i32");
        assert_eq!(
            wasm_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "i32, i32"
        );
        assert_eq!(
            wasm_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "i64"
        );
        assert_eq!(
            wasm_type(&TypeRef::Optional(Box::new(TypeRef::Struct("Foo".into())))),
            "i64"
        );
        assert_eq!(
            wasm_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "i32, i32"
        );
    }

    #[test]
    fn wasm_type_note_covers_all_variants() {
        assert_eq!(wasm_type_note(&TypeRef::I32), "native WASM i32");
        assert_eq!(wasm_type_note(&TypeRef::U32), "unsigned mapped to i32");
        assert_eq!(wasm_type_note(&TypeRef::Bool), "0 = false, 1 = true");
        assert_eq!(
            wasm_type_note(&TypeRef::StringUtf8),
            "ptr + len in linear memory"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Struct("X".into())),
            "opaque handle in linear memory"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Enum("E".into())),
            "variant discriminant"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::Struct("S".into())))),
            "opaque handle, 0 = absent"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "is_present flag + value"
        );
    }

    #[test]
    fn type_display_round_trips() {
        assert_eq!(type_display(&TypeRef::I32), "i32");
        assert_eq!(type_display(&TypeRef::StringUtf8), "string");
        assert_eq!(type_display(&TypeRef::Struct("Foo".into())), "Foo");
        assert_eq!(
            type_display(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "i32?"
        );
        assert_eq!(
            type_display(&TypeRef::List(Box::new(TypeRef::StringUtf8))),
            "[string]"
        );
        assert_eq!(
            type_display(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "{string:i32}"
        );
    }

    #[test]
    fn api_reference_complex_types() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        doc: None,
                    },
                    StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                ],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let readme = render_wasm_readme(&api);
        assert!(readme.contains("| `name` | `string` | `i32, i32` | ptr + len in linear memory |"));
        assert!(readme.contains("| _returns_ | `Contact?` | `i64` | opaque handle, 0 = absent |"));
        assert!(readme.contains("| `weaveffi_contacts_Contact_get_id` | `i32` |"));
        assert!(readme.contains("| `weaveffi_contacts_Contact_get_name` | `i32, i32` |"));
    }

    #[test]
    fn api_reference_void_return() {
        let api = make_api(vec![Module {
            name: "io".into(),
            functions: vec![Function {
                name: "print".into(),
                params: vec![Param {
                    name: "msg".into(),
                    ty: TypeRef::StringUtf8,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let readme = render_wasm_readme(&api);
        assert!(readme.contains("-> void`"));
        assert!(!readme.contains("_returns_"));
    }

    #[test]
    fn api_reference_multiple_modules() {
        let api = make_api(vec![
            Module {
                name: "math".into(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
            Module {
                name: "io".into(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
        ]);
        let readme = render_wasm_readme(&api);
        assert!(readme.contains("### Module: `math`"));
        assert!(readme.contains("### Module: `io`"));
    }

    #[test]
    fn generate_writes_api_reference() {
        let tmp = std::env::temp_dir().join("weaveffi_test_wasm_gen_api");
        let _ = std::fs::remove_dir_all(&tmp);
        let out = Utf8Path::from_path(tmp.as_path()).unwrap();
        let api = sample_api();
        WasmGenerator.generate(&api, out).unwrap();

        let readme = std::fs::read_to_string(out.join("wasm/README.md")).unwrap();
        assert!(readme.contains("## API Reference"));
        assert!(readme.contains("weaveffi_math_add"));
        assert!(readme.contains("##### `Point`"));
        assert!(readme.contains("##### `Color`"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn wasm_js_has_api_functions() {
        let api = sample_api();
        let js = render_wasm_js_stub(&api, DEFAULT_MODULE_NAME);
        assert!(js.contains("add(a, b)"));
        assert!(js.contains("wasm.weaveffi_math_add(a, b, _err)"));
        assert!(js.contains("class Point"));
        assert!(js.contains("get x()"));
        assert!(js.contains("get y()"));
        assert!(js.contains("weaveffi_math_Point_get_x"));
        assert!(js.contains("weaveffi_math_Point_get_y"));
        assert!(js.contains("export const Color = Object.freeze("));
        assert!(js.contains("Red: 0"));
        assert!(js.contains("Green: 1"));
        assert!(js.contains("Blue: 2"));
        assert!(js.contains("_raw: wasm"));
        assert!(js.contains("math: {"));
    }

    #[test]
    fn wasm_generates_dts() {
        let tmp = std::env::temp_dir().join("weaveffi_test_wasm_dts");
        let _ = std::fs::remove_dir_all(&tmp);
        let out = Utf8Path::from_path(tmp.as_path()).unwrap();
        let api = sample_api();
        WasmGenerator.generate(&api, out).unwrap();

        let dts = std::fs::read_to_string(out.join("wasm/weaveffi_wasm.d.ts")).unwrap();
        assert!(dts.contains("export interface WeaveffiWasmModule"));
        assert!(dts.contains(
            "export function loadWeaveffiWasm(url: string): Promise<WeaveffiWasmModule>"
        ));
        assert!(dts.contains("add(a: number, b: number): number"));
        assert!(dts.contains("export interface Point"));
        assert!(dts.contains("readonly x: number"));
        assert!(dts.contains("readonly y: number"));
        assert!(dts.contains("export declare const Color"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn wasm_js_has_string_helpers() {
        let api = make_api(vec![Module {
            name: "greeting".into(),
            functions: vec![Function {
                name: "greet".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let js = render_wasm_js_stub(&api, DEFAULT_MODULE_NAME);
        assert!(js.contains("function _encodeString(wasm, str)"));
        assert!(js.contains("function _decodeString(wasm, ptr, len)"));
        assert!(js.contains("TextEncoder"));
        assert!(js.contains("TextDecoder"));
        assert!(js.contains("_encodeString(wasm, name)"));
        assert!(js.contains("_decodeString(wasm,"));
        assert!(js.contains("greet(name)"));
        assert!(js.contains("wasm.weaveffi_greeting_greet("));
    }

    #[test]
    fn wasm_js_has_error_helpers() {
        let api = sample_api();
        let js = render_wasm_js_stub(&api, DEFAULT_MODULE_NAME);
        assert!(js.contains("function _allocError(wasm)"));
        assert!(js.contains("function _checkError(wasm, errPtr)"));
    }

    #[test]
    fn wasm_js_function_passes_err() {
        let api = sample_api();
        let js = render_wasm_js_stub(&api, DEFAULT_MODULE_NAME);
        assert!(js.contains("const _err = _allocError(wasm)"));
        assert!(js.contains("_checkError(wasm, _err)"));
    }

    #[test]
    fn wasm_dts_has_throws_doc() {
        let api = sample_api();
        let dts = render_wasm_dts(&api, DEFAULT_MODULE_NAME);
        assert!(
            dts.contains("@throws"),
            "Expected .d.ts to contain @throws JSDoc comment"
        );
        assert!(dts.contains("/** @throws {Error} if the native call fails */"));
    }

    #[test]
    fn wasm_custom_module_name() {
        let tmp = std::env::temp_dir().join("weaveffi_test_wasm_custom_name");
        let _ = std::fs::remove_dir_all(&tmp);
        let out = Utf8Path::from_path(tmp.as_path()).unwrap();
        let api = sample_api();
        let config = GeneratorConfig {
            wasm_module_name: Some("my_bindings".into()),
            ..GeneratorConfig::default()
        };
        WasmGenerator
            .generate_with_config(&api, out, &config)
            .unwrap();

        assert!(out.join("wasm/my_bindings.js").exists());
        assert!(out.join("wasm/my_bindings.d.ts").exists());

        let js = std::fs::read_to_string(out.join("wasm/my_bindings.js")).unwrap();
        assert!(js.contains("loadMyBindings"));

        let dts = std::fs::read_to_string(out.join("wasm/my_bindings.d.ts")).unwrap();
        assert!(dts.contains("MyBindingsModule"));
        assert!(dts.contains("loadMyBindings"));

        let files = WasmGenerator.output_files_with_config(&api, out, &config);
        assert!(files.iter().any(|f| f.contains("my_bindings.js")));
        assert!(files.iter().any(|f| f.contains("my_bindings.d.ts")));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn wasm_typed_handle_type() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "get_info".into(),
                params: vec![Param {
                    name: "contact".into(),
                    ty: TypeRef::TypedHandle("Contact".into()),
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dts = render_wasm_dts(&api, DEFAULT_MODULE_NAME);
        assert!(
            dts.contains("contact: Contact"),
            "TypedHandle should use class type not bigint: {dts}"
        );
        let js = render_wasm_js_stub(&api, DEFAULT_MODULE_NAME);
        assert!(
            js.contains("contact._handle"),
            "TypedHandle should extract ._handle: {js}"
        );
    }

    #[test]
    fn wasm_deeply_nested_optional() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "data".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                        Box::new(TypeRef::Struct("Contact".into())),
                    ))))),
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dts = render_wasm_dts(&api, DEFAULT_MODULE_NAME);
        assert!(
            dts.contains("(Contact | null)[] | null"),
            "should contain deeply nested optional type: {dts}"
        );
    }

    #[test]
    fn wasm_map_of_lists() {
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
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dts = render_wasm_dts(&api, DEFAULT_MODULE_NAME);
        assert!(
            dts.contains("Record<string, number[]>"),
            "should contain map of lists type: {dts}"
        );
    }

    #[test]
    fn wasm_enum_keyed_map() {
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
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                }],
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
        let dts = render_wasm_dts(&api, DEFAULT_MODULE_NAME);
        assert!(
            dts.contains("Record<Color, Contact>"),
            "should contain enum-keyed map type: {dts}"
        );
    }

    #[test]
    fn wasm_no_double_free_on_error() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let js = render_wasm_js_stub(&api, DEFAULT_MODULE_NAME);
        assert!(
            js.contains("_encodeString(wasm, name)"),
            "string param should be copied to WASM memory via _encodeString"
        );
        assert!(
            !js.contains("free(name"),
            "caller must not free the JS string input"
        );
        let check_err = js
            .find("_checkError(wasm, _err)")
            .expect("_checkError(wasm, _err) should appear in generated JS");
        let return_contact = js
            .find("return new Contact(")
            .expect("return new Contact( should appear for struct return");
        assert!(
            check_err < return_contact,
            "errors must be checked before constructing the result wrapper"
        );
        assert!(
            js.contains("class Contact {\n  constructor(wasm, handle) {"),
            "struct returns should use a handle wrapper class"
        );
    }

    #[test]
    fn wasm_null_check_on_optional_return() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let js = render_wasm_js_stub(&api, DEFAULT_MODULE_NAME);
        assert!(
            js.contains("result === 0n ? null : new Contact(wasm, result)"),
            "optional struct return should null-check before wrapping"
        );
    }

    #[test]
    fn wasm_async_returns_promise() {
        let api = make_api(vec![Module {
            name: "math".into(),
            functions: vec![Function {
                name: "compute".into(),
                params: vec![Param {
                    name: "x".into(),
                    ty: TypeRef::I32,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                cancellable: false,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let js = render_wasm_js_stub(&api, DEFAULT_MODULE_NAME);
        assert!(
            js.contains("new Promise"),
            "async function should return a Promise: {js}"
        );
        assert!(
            js.contains("resolve"),
            "Promise should have resolve callback: {js}"
        );
        assert!(
            js.contains("reject"),
            "Promise should have reject callback: {js}"
        );
        assert!(
            js.contains("_asyncContexts"),
            "should use async context map: {js}"
        );
        assert!(
            js.contains("_registerTrampoline"),
            "should register trampoline in function table: {js}"
        );
        assert!(
            js.contains("weaveffi_math_compute_async("),
            "should call the _async export: {js}"
        );
        assert!(
            js.contains("__indirect_function_table"),
            "should reference the WASM function table: {js}"
        );
    }

    #[test]
    fn wasm_dts_async_function() {
        let api = make_api(vec![Module {
            name: "math".into(),
            functions: vec![
                Function {
                    name: "compute".into(),
                    params: vec![Param {
                        name: "x".into(),
                        ty: TypeRef::I32,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: true,
                    cancellable: false,
                },
                Function {
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
                    cancellable: false,
                },
            ],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dts = render_wasm_dts(&api, DEFAULT_MODULE_NAME);
        assert!(
            dts.contains("compute(x: number): Promise<number>"),
            "async function should return Promise<T> in .d.ts: {dts}"
        );
        assert!(
            dts.contains("add(a: number, b: number): number"),
            "sync function should not return Promise: {dts}"
        );
        assert!(
            !dts.contains("add(a: number, b: number): Promise"),
            "sync function must not return Promise: {dts}"
        );
    }

    #[test]
    fn wasm_nested_module_output() {
        let api = make_api(vec![Module {
            name: "parent".into(),
            functions: vec![Function {
                name: "outer_fn".into(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![Module {
                name: "child".into(),
                functions: vec![Function {
                    name: "inner_fn".into(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
        }]);
        let dts = render_wasm_dts(&api, DEFAULT_MODULE_NAME);
        assert!(
            dts.contains("parent:"),
            "parent module in DTS interface missing: {dts}"
        );
        assert!(
            dts.contains("child:"),
            "nested child module in DTS interface missing: {dts}"
        );
        assert!(
            dts.contains("outer_fn(): number"),
            "parent function in DTS missing: {dts}"
        );
        assert!(
            dts.contains("inner_fn(): number"),
            "nested child function in DTS missing: {dts}"
        );
        let js = render_wasm_js_stub(&api, DEFAULT_MODULE_NAME);
        assert!(
            js.contains("weaveffi_parent_outer_fn"),
            "parent C ABI call in JS missing: {js}"
        );
        assert!(
            js.contains("weaveffi_parent_child_inner_fn"),
            "nested child C ABI call in JS missing: {js}"
        );
    }
}
