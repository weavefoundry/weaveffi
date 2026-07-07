//! WebAssembly binding generator for WeaveFFI.
//!
//! Emits a JavaScript loader stub and TypeScript declarations targeting a
//! `wasm32-unknown-unknown` cdylib build of the same Rust source.
//! Implements [`LanguageBackend`]; the shared driver bridges it into the
//! generator pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use std::collections::HashMap;
use std::fmt::Write as _;

use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::{self, TargetCapabilities};
use weaveffi_core::codegen::common::{
    emit_doc as common_emit_doc, walk_modules, walk_modules_with_path, DocCommentStyle,
};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    BindingModel, CallShape, EnumBinding, FieldBinding, FnBinding, IteratorBinding,
    ListenerBinding, ModuleBinding, RichEnumBinding, RichVariantBinding, StructBinding,
};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::utils::{
    local_type_name, render_json_prelude, render_prelude, render_trailer, CommentStyle,
};
use weaveffi_ir::ir::{Api, EnumDef, Module, TypeRef};

/// WebAssembly backend: emits a JavaScript loader stub and TypeScript
/// declarations targeting a `wasm32-unknown-unknown` cdylib build of the same
/// Rust source.
pub struct WasmGenerator;

const DEFAULT_MODULE_NAME: &str = "weaveffi_wasm";

/// Per-target configuration for [`WasmGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WasmConfig {
    /// Module name used for the emitted `<name>.js` loader and
    /// `<name>.d.ts` (default `"weaveffi_wasm"`).
    pub module_name: Option<String>,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the wasm glue calls the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Opt in to generating wasm bindings for an IDL that uses features the
    /// wasm target does not support (callbacks and listeners). The supported
    /// surface is generated normally; each unsupported entry point becomes an
    /// explicit stub that throws at call time, and the orchestrator prints a
    /// warning listing what was skipped. Without this flag, generation fails.
    pub allow_unsupported: bool,
    /// Target an Emscripten build instead of a bare `wasm32-unknown-unknown`
    /// one. The loader then accepts a pre-initialized Emscripten `Module`
    /// object (or the promise returned by its `MODULARIZE` factory) instead
    /// of a `.wasm` URL, and binds the module's underscore-prefixed exports
    /// to the symbol names the glue calls. Async functions are not supported
    /// in this mode; each one becomes an explicit stub that throws at call
    /// time and is omitted from the TypeScript declarations.
    pub emscripten: bool,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl WasmConfig {
    /// Returns the configured module name used for the emitted `<name>.js`
    /// loader and `<name>.d.ts`, falling back to `"weaveffi_wasm"`.
    pub fn module_name(&self) -> &str {
        self.module_name.as_deref().unwrap_or(DEFAULT_MODULE_NAME)
    }

    /// Returns the configured C ABI symbol prefix, falling back to
    /// `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

impl LanguageBackend for WasmGenerator {
    type Config = WasmConfig;

    fn name(&self) -> &'static str {
        "wasm"
    }

    /// Callbacks and listeners are not supported. Async completion works
    /// because each call registers a single-shot trampoline in the wasm
    /// function table that the producer invokes before the launcher returns
    /// control; module callbacks/listeners are long-lived and producer-
    /// initiated (typically from worker threads), and single-threaded
    /// `wasm32-unknown-unknown` has no thread to deliver them from. Rather
    /// than pretend (the pre-capability generator silently dropped both),
    /// declare them unsupported; `allow_unsupported: true` opts in to
    /// generating the supported surface with explicit throwing stubs.
    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities {
            async_functions: true,
            callbacks: false,
            listeners: false,
            iterators: true,
        }
    }

    fn allows_unsupported(&self, config: &Self::Config) -> bool {
        config.allow_unsupported
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn files(
        &self,
        api: &Api,
        _model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let wasm_dir = out_dir.join("wasm");
        let module_name = config.module_name();
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let js_filename = format!("{module_name}.js");
        let dts_filename = format!("{module_name}.d.ts");
        let package = pkg::resolve(api, None, config.input_basename.as_deref());
        vec![
            OutputFile::new(
                wasm_dir.join("README.md"),
                render_wasm_readme(api, prefix, input_basename, config.emscripten),
            ),
            OutputFile::new(
                wasm_dir.join("package.json"),
                render_wasm_package_json(&package, &js_filename, &dts_filename, input_basename),
            ),
            OutputFile::new(
                wasm_dir.join(&js_filename),
                render_wasm_js_stub(
                    api,
                    module_name,
                    prefix,
                    input_basename,
                    &js_filename,
                    config.emscripten,
                ),
            ),
            OutputFile::new(
                wasm_dir.join(&dts_filename),
                render_wasm_dts(
                    api,
                    module_name,
                    input_basename,
                    &dts_filename,
                    config.emscripten,
                ),
            ),
        ]
    }
}

weaveffi_core::impl_generator_via_backend!(WasmGenerator);

fn render_wasm_package_json(
    package: &ResolvedPackage,
    js_filename: &str,
    dts_filename: &str,
    input_basename: &str,
) -> String {
    let prelude = render_json_prelude(input_basename);
    let name = &package.name;
    let version = &package.version;
    let description = package.description_or_default();
    let mut optional = String::new();
    if let Some(license) = &package.license {
        optional.push_str(&format!("  \"license\": \"{license}\",\n"));
    }
    if let Some(author) = package.authors.first() {
        optional.push_str(&format!("  \"author\": \"{author}\",\n"));
    }
    if let Some(homepage) = &package.homepage {
        optional.push_str(&format!("  \"homepage\": \"{homepage}\",\n"));
    }
    format!(
        "{{\n{prelude}  \"name\": \"{name}\",\n  \"version\": \"{version}\",\n  \"description\": \"{description}\",\n{optional}  \"type\": \"module\",\n  \"main\": \"{js_filename}\",\n  \"types\": \"{dts_filename}\"\n}}\n"
    )
}

fn wasm_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::Bool
        | TypeRef::Enum(_) => "i32",
        TypeRef::I64
        | TypeRef::U64
        | TypeRef::Handle
        | TypeRef::TypedHandle(_)
        | TypeRef::Struct(_)
        | TypeRef::Iterator(_)
        | TypeRef::Map(_, _) => "i64",
        TypeRef::F32 => "f32",
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
    }
}

fn wasm_type_note(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "8-bit signed mapped to i32",
        TypeRef::I16 => "16-bit signed mapped to i32",
        TypeRef::I32 => "native WASM i32",
        TypeRef::U8 => "8-bit unsigned mapped to i32",
        TypeRef::U16 => "16-bit unsigned mapped to i32",
        TypeRef::U32 => "unsigned mapped to i32",
        TypeRef::I64 => "native WASM i64",
        TypeRef::U64 => "unsigned mapped to i64",
        TypeRef::F32 => "native WASM f32",
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
    }
}

fn type_display(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "i8".into(),
        TypeRef::I16 => "i16".into(),
        TypeRef::I32 => "i32".into(),
        TypeRef::U8 => "u8".into(),
        TypeRef::U16 => "u16".into(),
        TypeRef::U32 => "u32".into(),
        TypeRef::I64 => "i64".into(),
        TypeRef::U64 => "u64".into(),
        TypeRef::F32 => "f32".into(),
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
    }
}

fn render_wasm_readme(api: &Api, prefix: &str, input_basename: &str, emscripten: bool) -> String {
    let mut out = render_prelude(CommentStyle::Xml, input_basename);
    out.push_str("# WeaveFFI WASM (experimental)\n\n");
    if emscripten {
        out.push_str("This folder contains a minimal stub to help you load an Emscripten build of your WeaveFFI library.\n\n");
        out.push_str("Build (example):\n\n");
        out.push_str("```bash\n");
        out.push_str("emcc your_library.c -o your_library.js \\\n");
        out.push_str("  -sMODULARIZE=1 -sEXPORT_ES6=1 \\\n");
        out.push_str("  -sEXPORTED_RUNTIME_METHODS=HEAPU8 \\\n");
        out.push_str("  -sALLOW_MEMORY_GROWTH=1\n");
        out.push_str("```\n\n");
        out.push_str(&format!(
            "The `{prefix}_*` symbols are kept alive and exported automatically: the \
             generated header tags them with `{}_API`, which expands to \
             `__attribute__((used, visibility(\"default\")))` under Emscripten.\n\n",
            prefix.to_uppercase()
        ));
        out.push_str("Then construct the Emscripten module yourself (so you control options like `locateFile`) and pass it to the loader:\n\n");
        out.push_str("```js\n");
        out.push_str("import Module from './your_library.js';\n");
        out.push_str("import { loadWeaveffiWasm } from './weaveffi_wasm.js';\n\n");
        out.push_str("const api = await loadWeaveffiWasm(Module());\n");
        out.push_str("```\n\n");
        if walk_modules(&api.modules).any(|m| m.functions.iter().any(|f| f.r#async)) {
            out.push_str("## Async Functions\n\n");
            out.push_str(
                "Async functions are not supported in Emscripten mode. Each one is \
                 generated as an explicit stub that throws at call time and is omitted \
                 from the TypeScript declarations. Use the standard \
                 `wasm32-unknown-unknown` loader or a native target when you need \
                 them.\n\n",
            );
        }
    } else {
        out.push_str("This folder contains a minimal stub to help you load a `wasm32-unknown-unknown` build of your WeaveFFI library.\n\n");
        out.push_str("Build (example):\n\n");
        out.push_str("```bash\n");
        out.push_str("cargo build --target wasm32-unknown-unknown --release\n");
        out.push_str("```\n\n");
        out.push_str("Then serve the `.wasm` and use `weaveffi_wasm.js` to load it.\n\n");
    }
    out.push_str("## Complex Type Handling\n\n");
    out.push_str("WASM only supports numeric types natively (`i32`, `i64`, `f32`, `f64`). ");
    out.push_str("Complex types are encoded at the boundary as follows:\n\n");
    out.push_str("### Structs\n\n");
    out.push_str("Structs are passed as **opaque handles** (`i64` pointers into linear memory). ");
    out.push_str(
        "The host cannot inspect struct fields directly; use the generated accessor functions ",
    );
    out.push_str(&format!(
        "(`{prefix}_{{module}}_{{struct}}_get_{{field}}`) to read/write fields.\n\n"
    ));
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
    out.push_str("- `weaveffi_alloc(size: i32) -> i32`: allocate `size` bytes in linear memory\n");
    out.push_str("- `weaveffi_error_clear(err_ptr: i32)`: clear and free error resources\n");

    render_unsupported_section(&mut out, api);

    if !api.modules.is_empty() {
        let model = BindingModel::build(api, prefix);
        render_api_reference(&mut out, api, &model);
    }

    out.push('\n');
    out.push_str(&render_trailer(CommentStyle::Xml, "README.md"));
    out
}

/// When the IDL uses features the wasm target does not support (callbacks,
/// listeners; generation only proceeds under `allow_unsupported`), document
/// exactly what is missing and how it behaves, listing each declaration.
fn render_unsupported_section(out: &mut String, api: &Api) {
    let used = capabilities::used_features(api);
    let caps = LanguageBackend::capabilities(&WasmGenerator);
    let unsupported: Vec<_> = used
        .iter()
        .filter(|(feature, _)| !caps.supports(**feature))
        .collect();
    if unsupported.is_empty() {
        return;
    }
    out.push_str("\n## Unsupported Features\n\n");
    out.push_str(
        "This IDL uses features the wasm target does not support (generated because\n\
         `allow_unsupported` is set). Single-threaded `wasm32-unknown-unknown` has no\n\
         producer thread to deliver events from, so:\n\n",
    );
    for (feature, locations) in unsupported {
        out.push_str(&format!("- **{feature}**: {}\n", locations.join(", ")));
    }
    out.push_str(
        "\nThe TypeScript declarations omit these entry points; the JS module exposes\n\
         explicit stubs that throw on call. Use a native target (node, python, …) when\n\
         you need them.\n",
    );
}

fn render_api_reference(out: &mut String, api: &Api, model: &BindingModel) {
    let by_path: HashMap<&str, &ModuleBinding> =
        model.modules.iter().map(|m| (m.path.as_str(), m)).collect();
    out.push_str("\n## API Reference\n");
    for module in &api.modules {
        out.push_str(&format!("\n### Module: `{}`\n", module.name));
        let mb = by_path[module.name.as_str()];

        if !mb.functions.is_empty() {
            out.push_str("\n#### Functions\n");
            for f in &mb.functions {
                render_function_ref(out, f);
            }
        }

        if !mb.structs.is_empty() {
            out.push_str("\n#### Structs\n");
            for s in &mb.structs {
                render_struct_ref(out, s);
                if s.builder.is_some() {
                    render_builder_ref(out, s);
                }
            }
        }

        if !mb.enums.is_empty() {
            out.push_str("\n#### Enums\n");
            for e in &mb.enums {
                render_enum_ref(out, e);
            }
        }
    }
}

fn render_function_ref(out: &mut String, f: &FnBinding) {
    let abi_name = &f.c_base;
    out.push_str(&format!("\n##### `{abi_name}`\n\n"));

    if let Some(doc) = &f.doc {
        out.push_str(doc);
        out.push_str("\n\n");
    }

    if let Some(msg) = &f.deprecated {
        out.push_str(&format!("**Deprecated:** {msg}\n\n"));
    }

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, wasm_type(&p.ty)))
        .collect();
    let ret_sig = f.ret.as_ref().map_or("void", wasm_type);
    out.push_str(&format!(
        "`{abi_name}({}) -> {ret_sig}`\n\n",
        params_sig.join(", ")
    ));

    out.push_str("| Param | API Type | WASM | Notes |\n");
    out.push_str("|-------|----------|------|-------|\n");
    for param in &f.params {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | {} |\n",
            param.name,
            type_display(&param.ty),
            wasm_type(&param.ty),
            wasm_type_note(&param.ty)
        ));
    }
    if let Some(ret) = &f.ret {
        out.push_str(&format!(
            "| _returns_ | `{}` | `{}` | {} |\n",
            type_display(ret),
            wasm_type(ret),
            wasm_type_note(ret)
        ));
    }
}

fn render_struct_ref(out: &mut String, s: &StructBinding) {
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
                "| `{}` | `{}` |\n",
                field.getter_symbol,
                wasm_type(&field.ty)
            ));
        }
    }
}

fn render_builder_ref(out: &mut String, s: &StructBinding) {
    let name = &s.name;
    let Some(b) = &s.builder else {
        return;
    };
    out.push_str(&format!("\n##### `{name}Builder`\n\n"));
    out.push_str(&format!("Builder for `{name}`.\n\n"));
    out.push_str("| Function | Args | Return |\n");
    out.push_str("|----------|------|--------|\n");
    out.push_str(&format!("| `{}` | none | `i32` (handle) |\n", b.new_symbol));
    for (field, (_field_name, setter)) in s.fields.iter().zip(&b.setters) {
        let wt = wasm_type(&field.ty);
        out.push_str(&format!(
            "| `{setter}` | `i32` handle, `{wt}` value | none |\n"
        ));
    }
    out.push_str(&format!(
        "| `{}` | `i32` handle | `i32` (handle) |\n",
        b.build_symbol
    ));
    out.push_str(&format!(
        "| `{}` | `i32` handle | none |\n",
        b.destroy_symbol
    ));
}

fn render_enum_ref(out: &mut String, e: &EnumBinding) {
    out.push_str(&format!("\n##### `{}`\n\n", e.name));

    if let Some(doc) = &e.doc {
        out.push_str(doc);
        out.push_str("\n\n");
    }

    if let Some(rich) = &e.rich {
        render_rich_enum_ref(out, rich);
        return;
    }

    out.push_str("Passed as `i32` discriminant.\n\n");
    out.push_str("| Variant | Value |\n");
    out.push_str("|---------|-------|\n");
    for v in &e.variants {
        out.push_str(&format!("| `{}` | `{}` |\n", v.name, v.value));
    }
}

/// Document a rich (algebraic) enum: an opaque handle constructed via per-variant
/// factories, with a `tag` discriminant reader and namespaced field getters,
/// not a by-value `i32` discriminant like a plain enum.
fn render_rich_enum_ref(out: &mut String, rich: &RichEnumBinding) {
    out.push_str(
        "Rich (algebraic) enum, passed as an **opaque handle** (`i64`). Construct one with a \
         per-variant factory, read the active variant via the `tag` discriminant, and access \
         associated data through the namespaced getters.\n\n",
    );
    out.push_str("| Variant | Tag | Fields |\n");
    out.push_str("|---------|-----|--------|\n");
    for v in &rich.variants {
        let fields = if v.fields.is_empty() {
            "(none)".to_string()
        } else {
            v.fields
                .iter()
                .map(|f| format!("`{}: {}`", f.name, type_display(&f.ty)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!("| `{}` | `{}` | {} |\n", v.name, v.value, fields));
    }
}

/// True if `ty` is one of the UTF-8 string spellings.
fn is_string_type(ty: &TypeRef) -> bool {
    matches!(ty, TypeRef::StringUtf8 | TypeRef::BorrowedStr)
}

/// Visit every boundary-crossing type in `api` (function params + returns and
/// struct field types), recursing into composite types, and return whether any
/// satisfies `pred`.
fn api_deep_any(api: &Api, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
    fn deep(ty: &TypeRef, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
        if pred(ty) {
            return true;
        }
        match ty {
            TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
                deep(inner, pred)
            }
            TypeRef::Map(k, v) => deep(k, pred) || deep(v, pred),
            _ => false,
        }
    }
    fn module_any(m: &Module, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
        m.functions.iter().any(|f| {
            f.params.iter().any(|p| deep(&p.ty, pred))
                || f.returns.as_ref().is_some_and(|r| deep(r, pred))
        }) || m
            .structs
            .iter()
            .any(|s| s.fields.iter().any(|f| deep(&f.ty, pred)))
            // Rich (algebraic) enums marshal their variant fields exactly like
            // struct fields, so a string/bytes/list living only inside a variant
            // payload still pulls in the corresponding linear-memory helpers.
            || m.enums.iter().any(|e| {
                e.variants
                    .iter()
                    .any(|v| v.fields.iter().any(|f| deep(&f.ty, pred)))
            })
            || m.modules.iter().any(|sub| module_any(sub, pred))
    }
    api.modules.iter().any(|m| module_any(m, pred))
}

/// The byte stride of one element of `ty` packed in a C array in linear memory
/// (wasm32: pointers and 32-bit scalars are 4 bytes, 64-bit values 8, bool 1).
fn wasm_stride(ty: &TypeRef) -> u32 {
    match ty {
        TypeRef::Bool | TypeRef::I8 | TypeRef::U8 => 1,
        TypeRef::I16 | TypeRef::U16 => 2,
        TypeRef::I64 | TypeRef::U64 | TypeRef::F64 | TypeRef::Handle => 8,
        _ => 4,
    }
}

/// Emit a JS expression that reads a single element of scalar/handle `ty` from
/// the C array `base` at element index `idx` using DataView `dv`. Strings and
/// structs are handled by the callers (they need freeing / class wrapping).
fn wasm_read_scalar_elem(ty: &TypeRef, dv: &str, base: &str, idx: &str) -> String {
    let stride = wasm_stride(ty);
    let off = format!("{base} + {idx} * {stride}");
    match ty {
        TypeRef::Bool => format!("{dv}.getUint8({off}) !== 0"),
        TypeRef::I8 => format!("{dv}.getInt8({off})"),
        TypeRef::U8 => format!("{dv}.getUint8({off})"),
        TypeRef::I16 => format!("{dv}.getInt16({off}, true)"),
        TypeRef::U16 => format!("{dv}.getUint16({off}, true)"),
        TypeRef::U32 => format!("{dv}.getUint32({off}, true)"),
        TypeRef::I32 | TypeRef::Enum(_) => format!("{dv}.getInt32({off}, true)"),
        TypeRef::I64 => format!("{dv}.getBigInt64({off}, true)"),
        TypeRef::U64 | TypeRef::Handle => format!("{dv}.getBigUint64({off}, true)"),
        TypeRef::F32 => format!("{dv}.getFloat32({off}, true)"),
        TypeRef::F64 => format!("{dv}.getFloat64({off}, true)"),
        _ => format!("{dv}.getInt32({off}, true)"),
    }
}

/// Byte width of a scalar `ty` when boxed by pointer (optional-scalar ABI).
fn scalar_width(ty: &TypeRef) -> u32 {
    match ty {
        TypeRef::Bool | TypeRef::I8 | TypeRef::U8 => 1,
        TypeRef::I16 | TypeRef::U16 => 2,
        TypeRef::I64 | TypeRef::U64 | TypeRef::F64 | TypeRef::Handle => 8,
        _ => 4,
    }
}

/// Emit a `DataView` write of scalar `ty` at `off` from JS value `val`.
fn emit_write_scalar(out: &mut String, indent: &str, ty: &TypeRef, dv: &str, off: &str, val: &str) {
    let stmt = match ty {
        TypeRef::Bool => format!("{dv}.setUint8({off}, {val} ? 1 : 0);"),
        TypeRef::I8 => format!("{dv}.setInt8({off}, {val});"),
        TypeRef::U8 => format!("{dv}.setUint8({off}, {val});"),
        TypeRef::I16 => format!("{dv}.setInt16({off}, {val}, true);"),
        TypeRef::U16 => format!("{dv}.setUint16({off}, {val}, true);"),
        TypeRef::U32 => format!("{dv}.setUint32({off}, {val}, true);"),
        TypeRef::I32 | TypeRef::Enum(_) => format!("{dv}.setInt32({off}, {val}, true);"),
        TypeRef::I64 => format!("{dv}.setBigInt64({off}, BigInt({val}), true);"),
        TypeRef::U64 | TypeRef::Handle => {
            format!("{dv}.setBigUint64({off}, BigInt({val}), true);")
        }
        TypeRef::F32 => format!("{dv}.setFloat32({off}, {val}, true);"),
        TypeRef::F64 => format!("{dv}.setFloat64({off}, {val}, true);"),
        _ => format!("{dv}.setInt32({off}, {val}, true);"),
    };
    let _ = writeln!(out, "{indent}{stmt}");
}

/// A direct JS call argument for a scalar/handle value (coercing bool→0/1 and
/// 64-bit values→BigInt as the wasm calling convention requires).
fn js_arg_scalar(ty: &TypeRef, val: &str) -> String {
    match ty {
        TypeRef::Bool => format!("{val} ? 1 : 0"),
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle => format!("BigInt({val})"),
        _ => val.to_string(),
    }
}

/// Stage one idiomatic input `value` of type `ty` into the WASM ABI.
///
/// Pushes any pre-call statements to `out` (at `indent`), the produced i32/i64
/// call arguments to `args`, and any post-call cleanup statements to `cleanup`.
/// `tmp` is a collision-free local-name base. Assumes `wasm` is in scope.
fn emit_stage_input(
    out: &mut String,
    indent: &str,
    ty: &TypeRef,
    value: &str,
    tmp: &str,
    args: &mut Vec<String>,
    cleanup: &mut Vec<String>,
) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("const [{tmp}_p, {tmp}_s] = _cstr(wasm, {value});"));
            args.push(format!("{tmp}_p"));
            cleanup.push(format!("wasm.weaveffi_dealloc({tmp}_p, {tmp}_s);"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("const [{tmp}_p, {tmp}_l] = _bytes(wasm, {value});"));
            args.push(format!("{tmp}_p"));
            args.push(format!("{tmp}_l"));
            cleanup.push(format!("wasm.weaveffi_dealloc({tmp}_p, {tmp}_l);"));
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
            args.push(format!("{value}._handle"));
        }
        TypeRef::Bool
        | TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Handle
        | TypeRef::Enum(_) => {
            args.push(js_arg_scalar(ty, value));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                args.push(format!("({value} ? {value}._handle : 0)"));
            }
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!("let {tmp}_p = 0, {tmp}_s = 0;"));
                w.line(format!(
                    "if ({value} !== null && {value} !== undefined) {{ [{tmp}_p, {tmp}_s] = _cstr(wasm, {value}); }}"
                ));
                args.push(format!("{tmp}_p"));
                cleanup.push(format!(
                    "if ({tmp}_p !== 0) wasm.weaveffi_dealloc({tmp}_p, {tmp}_s);"
                ));
            }
            scalar => {
                let width = scalar_width(scalar);
                w.line(format!("let {tmp}_p = 0;"));
                w.block(
                    format!("if ({value} !== null && {value} !== undefined) {{"),
                    "}",
                    |w| {
                        w.line(format!("{tmp}_p = wasm.weaveffi_alloc({width});"));
                        w.line(format!(
                            "const {tmp}_dv = new DataView(wasm.memory.buffer);"
                        ));
                        let ind = w.indent_str();
                        let mut sc = String::new();
                        emit_write_scalar(
                            &mut sc,
                            &ind,
                            scalar,
                            &format!("{tmp}_dv"),
                            &format!("{tmp}_p"),
                            value,
                        );
                        w.raw(sc);
                    },
                );
                args.push(format!("{tmp}_p"));
                cleanup.push(format!(
                    "if ({tmp}_p !== 0) wasm.weaveffi_dealloc({tmp}_p, {width});"
                ));
            }
        },
        TypeRef::List(inner) => {
            let mut staged = String::new();
            emit_stage_list(&mut staged, indent, inner, value, tmp, args, cleanup);
            w.raw(staged);
        }
        TypeRef::Map(k, v) => {
            let kt = format!("{tmp}_k");
            let vt = format!("{tmp}_v");
            w.line(format!("const {tmp}_m = {value} || {{}};"));
            w.line(format!(
                "const {tmp}_ks = ({tmp}_m instanceof Map) ? [...{tmp}_m.keys()] : Object.keys({tmp}_m);"
            ));
            w.line(format!(
                "const {tmp}_vs = ({tmp}_m instanceof Map) ? [...{tmp}_m.values()] : Object.values({tmp}_m);"
            ));
            let mut kargs = Vec::new();
            let mut vargs = Vec::new();
            let mut staged = String::new();
            emit_stage_list(
                &mut staged,
                indent,
                k,
                &format!("{tmp}_ks"),
                &kt,
                &mut kargs,
                cleanup,
            );
            emit_stage_list(
                &mut staged,
                indent,
                v,
                &format!("{tmp}_vs"),
                &vt,
                &mut vargs,
                cleanup,
            );
            w.raw(staged);
            // Each list staged `(base, len)`; the map ABI is `(keys, values, len)`.
            args.push(kargs[0].clone());
            args.push(vargs[0].clone());
            args.push(kargs[1].clone());
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as an input"),
    }
    out.push_str(&w.finish());
}

/// Stage a JS array `value` of element type `inner` as a packed C array,
/// pushing `(base, len)` to `args` and the frees to `cleanup`.
fn emit_stage_list(
    out: &mut String,
    indent: &str,
    inner: &TypeRef,
    value: &str,
    tmp: &str,
    args: &mut Vec<String>,
    cleanup: &mut Vec<String>,
) {
    let stride = wasm_stride(inner);
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.line(format!("const {tmp}_arr = {value} || [];"));
    w.line(format!("const {tmp}_n = {tmp}_arr.length;"));
    w.line(format!(
        "const {tmp}_base = wasm.weaveffi_alloc({tmp}_n ? {tmp}_n * {stride} : 1);"
    ));
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("const {tmp}_ep = [];"));
            w.line(format!(
                "for (let i = 0; i < {tmp}_n; i++) {tmp}_ep.push(_cstr(wasm, {tmp}_arr[i]));"
            ));
            w.block("{", "}", |w| {
                w.line("const dv = new DataView(wasm.memory.buffer);");
                w.line(format!("for (let i = 0; i < {tmp}_n; i++) dv.setUint32({tmp}_base + i * 4, {tmp}_ep[i][0], true);"));
            });
            cleanup.push(format!(
                "for (const [ep, es] of {tmp}_ep) wasm.weaveffi_dealloc(ep, es);"
            ));
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
            w.block("{", "}", |w| {
                w.line("const dv = new DataView(wasm.memory.buffer);");
                w.line(format!("for (let i = 0; i < {tmp}_n; i++) dv.setInt32({tmp}_base + i * 4, {tmp}_arr[i]._handle, true);"));
            });
        }
        scalar => {
            w.block("{", "}", |w| {
                w.line("const dv = new DataView(wasm.memory.buffer);");
                w.block(format!("for (let i = 0; i < {tmp}_n; i++) {{"), "}", |w| {
                    let ind = w.indent_str();
                    let mut sc = String::new();
                    emit_write_scalar(
                        &mut sc,
                        &ind,
                        scalar,
                        "dv",
                        &format!("{tmp}_base + i * {stride}"),
                        &format!("{tmp}_arr[i]"),
                    );
                    w.raw(sc);
                });
            });
        }
    }
    cleanup.push(format!(
        "wasm.weaveffi_dealloc({tmp}_base, {tmp}_n ? {tmp}_n * {stride} : 1);"
    ));
    args.push(format!("{tmp}_base"));
    args.push(format!("{tmp}_n"));
    out.push_str(&w.finish());
}

/// Emit `const {target} = ...;` building a JS array of `inner` elements from the
/// producer-owned C array at `base` (`len` elements). Assumes `wasm` in scope.
fn emit_read_list_into(
    out: &mut String,
    indent: &str,
    inner: &TypeRef,
    base: &str,
    len: &str,
    target: &str,
) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "const {target} = _takeStrArray(wasm, {base}, {len});"
            ));
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let cls = local_type_name(name);
            w.line(format!("const {target} = [];"));
            w.block("{", "}", |w| {
                w.line("const dv = new DataView(wasm.memory.buffer);");
                w.line(format!(
                    "for (let i = 0; i < {len}; i++) {target}.push(new {cls}(wasm, dv.getInt32({base} + i * 4, true)));"
                ));
            });
        }
        scalar => {
            w.line(format!("const {target} = [];"));
            let elem = wasm_read_scalar_elem(scalar, "dv", base, "i");
            w.block("{", "}", |w| {
                w.line("const dv = new DataView(wasm.memory.buffer);");
                w.line(format!(
                    "for (let i = 0; i < {len}; i++) {target}.push({elem});"
                ));
            });
        }
    }
    out.push_str(&w.finish());
}

/// Emit `const {target} = ...;` building a JS object (`Record`) from the
/// producer-owned parallel key/value C arrays. Assumes `wasm` in scope.
#[allow(clippy::too_many_arguments)]
fn emit_read_map_into(
    out: &mut String,
    indent: &str,
    k: &TypeRef,
    v: &TypeRef,
    ka: &str,
    va: &str,
    len: &str,
    target: &str,
) {
    emit_read_list_into(out, indent, k, ka, len, &format!("{target}_k"));
    emit_read_list_into(out, indent, v, va, len, &format!("{target}_v"));
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.line(format!("const {target} = {{}};"));
    w.line(format!(
        "for (let i = 0; i < {len}; i++) {target}[{target}_k[i]] = {target}_v[i];"
    ));
    out.push_str(&w.finish());
}

/// Emit the body that invokes `symbol` with the already-staged `in_args`, runs
/// `cleanup`, checks the error slot (when `with_err`), and decodes/returns the
/// idiomatic value for `ret`. Assumes `wasm` is in scope at `indent`.
fn emit_return_decode(
    out: &mut String,
    indent: &str,
    ret: Option<&TypeRef>,
    symbol: &str,
    in_args: &[String],
    cleanup: &[String],
    with_err: bool,
) {
    // Classify which trailing out-slots the return needs.
    let unwrapped = match ret {
        Some(TypeRef::Optional(inner)) => Some(inner.as_ref()),
        other => other,
    };
    let needs_len = matches!(
        unwrapped,
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_))
    );
    let needs_map = matches!(unwrapped, Some(TypeRef::Map(_, _)));

    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let mut call_args = in_args.to_vec();
    if needs_len {
        w.line("const _lp = wasm.weaveffi_alloc(4);");
        call_args.push("_lp".to_string());
    } else if needs_map {
        w.line("const _kp = wasm.weaveffi_alloc(4);");
        w.line("const _vp = wasm.weaveffi_alloc(4);");
        w.line("const _lp = wasm.weaveffi_alloc(4);");
        call_args.push("_kp".to_string());
        call_args.push("_vp".to_string());
        call_args.push("_lp".to_string());
    }
    if with_err {
        w.line("const _err = _allocErr(wasm);");
        call_args.push("_err".to_string());
    }

    let call = format!("wasm.{symbol}({})", call_args.join(", "));
    let captures_r = !needs_map && ret.is_some();
    if captures_r {
        w.line(format!("const _r = {call};"));
    } else {
        w.line(format!("{call};"));
    }

    for stmt in cleanup {
        w.line(stmt);
    }
    if with_err {
        w.line("_checkErr(wasm, _err);");
        w.line("_freeErr(wasm, _err);");
    }
    out.push_str(&w.finish());

    emit_decode_value(out, indent, ret, "_r");
}

/// Emit the `return ...;` (if any) that converts the raw result `r` plus any
/// `_lp`/`_kp`/`_vp` out-slots already in scope into the idiomatic value.
fn emit_decode_value(out: &mut String, indent: &str, ret: Option<&TypeRef>, r: &str) {
    let Some(ret) = ret else {
        return;
    };
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    match ret {
        TypeRef::Bool => {
            w.line(format!("return {r} !== 0;"));
        }
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Handle
        | TypeRef::Enum(_) => {
            w.line(format!("return {r};"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("return _takeCStr(wasm, {r});"));
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let cls = local_type_name(name);
            w.line(format!("return new {cls}(wasm, {r});"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("const _dv = new DataView(wasm.memory.buffer);");
            w.line("const _len = _dv.getUint32(_lp, true);");
            w.line("wasm.weaveffi_dealloc(_lp, 4);");
            w.line(format!("return _takeBytes(wasm, {r}, _len);"));
        }
        TypeRef::List(inner) => {
            w.line("const _dv = new DataView(wasm.memory.buffer);");
            w.line("const _len = _dv.getUint32(_lp, true);");
            w.line("wasm.weaveffi_dealloc(_lp, 4);");
            let mut tmp = String::new();
            emit_read_list_into(&mut tmp, indent, inner, r, "_len", "_out");
            w.raw(tmp);
            w.line("return _out;");
        }
        TypeRef::Map(k, v) => {
            w.line("const _dv = new DataView(wasm.memory.buffer);");
            w.line("const _ka = _dv.getUint32(_kp, true);");
            w.line("const _va = _dv.getUint32(_vp, true);");
            w.line("const _len = _dv.getUint32(_lp, true);");
            w.line("wasm.weaveffi_dealloc(_kp, 4);");
            w.line("wasm.weaveffi_dealloc(_vp, 4);");
            w.line("wasm.weaveffi_dealloc(_lp, 4);");
            let mut tmp = String::new();
            emit_read_map_into(&mut tmp, indent, k, v, "_ka", "_va", "_len", "_out");
            w.raw(tmp);
            w.line("return _out;");
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                let cls = local_type_name(name);
                w.line(format!("return {r} === 0 ? null : new {cls}(wasm, {r});"));
            }
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!("return _takeCStr(wasm, {r});"));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) | TypeRef::Map(_, _) => {
                // Aggregate optionals: a null base decodes to empty by the readers.
                let mut tmp = String::new();
                emit_decode_value(&mut tmp, indent, Some(inner), r);
                w.raw(tmp);
            }
            scalar => {
                let getter = wasm_read_scalar_elem(scalar, "_dv", r, "0")
                    .replace(&format!("{r} + 0 * {}", wasm_stride(scalar)), r);
                w.line(format!("if ({r} === 0) return null;"));
                w.line("const _dv = new DataView(wasm.memory.buffer);");
                w.line(format!("return {getter};"));
            }
        },
        TypeRef::Iterator(_) => unreachable!("iterator returns handled separately"),
    }
    out.push_str(&w.finish());
}

fn ts_type_for(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::F32
        | TypeRef::I64
        | TypeRef::F64 => "number".into(),
        TypeRef::Bool => "boolean".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        // Bytes cross the boundary as plain `Uint8Array` copies; the Node-only
        // `Buffer` type does not exist in browsers and is never returned here.
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Uint8Array".into(),
        TypeRef::U64 | TypeRef::Handle => "bigint".into(),
        // Structs, enums, and typed handles surface as bare local TS names; a
        // cross-module typed handle (resolved to e.g. `kv.Store`) must name the
        // local `Store`, not the qualified IR name which is undeclared here.
        TypeRef::TypedHandle(name) | TypeRef::Enum(name) | TypeRef::Struct(name) => {
            local_type_name(name).to_string()
        }
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
    }
}

/// Emits a JSDoc comment at `indent`. Single-line docs collapse to
/// `/** text */`; multi-line docs expand to a block with ` * ` prefixed lines.
fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Javadoc);
}

/// Emits a JSDoc block for a function: function doc, `@param name desc` for
/// each documented parameter, and an optional trailing tag list.
fn emit_fn_doc(
    out: &mut String,
    doc: &Option<String>,
    params: &[weaveffi_ir::ir::Param],
    indent: &str,
    extra_tags: &[String],
) {
    let has_param_docs = params.iter().any(|p| p.doc.is_some());
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    if trimmed_doc.is_none() && !has_param_docs && extra_tags.is_empty() {
        return;
    }
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.line("/**");
    if let Some(d) = trimmed_doc {
        for line in d.lines() {
            if line.is_empty() {
                w.line(" *");
            } else {
                w.line(format!(" * {line}"));
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
                w.line(format!(" * @param {} {}", p.name, first));
            }
            for line in lines {
                if line.is_empty() {
                    w.line(" *");
                } else {
                    w.line(format!(" *   {line}"));
                }
            }
        }
    }
    for tag in extra_tags {
        w.line(format!(" * {tag}"));
    }
    w.line(" */");
    out.push_str(&w.finish());
}

fn render_wasm_dts(
    api: &Api,
    module_name: &str,
    input_basename: &str,
    filename: &str,
    emscripten: bool,
) -> String {
    let pascal_name = module_name.to_upper_camel_case();
    let interface_name = format!("{pascal_name}Module");
    let load_fn = format!("load{pascal_name}");
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    out.push_str("// Generated TypeScript declarations for WeaveFFI WASM bindings\n\n");

    for (m, _path) in walk_modules_with_path(&api.modules) {
        for s in &m.structs {
            emit_doc(&mut out, &s.doc, "");
            out.push_str(&format!("export interface {} {{\n", s.name));
            for field in &s.fields {
                emit_doc(&mut out, &field.doc, "  ");
                out.push_str(&format!(
                    "  readonly {}: {};\n",
                    field.name,
                    ts_type_for(&field.ty)
                ));
            }
            out.push_str("}\n\n");
        }

        for e in &m.enums {
            // A rich (algebraic) enum is an opaque-object wrapper class, not a
            // by-value discriminant constant.
            if e.is_rich() {
                emit_dts_rich_enum_class(&mut out, e);
                continue;
            }
            emit_doc(&mut out, &e.doc, "");
            out.push_str(&format!("export declare const {}: Readonly<{{\n", e.name));
            for v in &e.variants {
                emit_doc(&mut out, &v.doc, "  ");
                out.push_str(&format!("  {}: {};\n", v.name, v.value));
            }
            out.push_str("}>;\n\n");
        }
    }

    out.push_str(&format!("export interface {interface_name} {{\n"));
    let all_mods = walk_modules(&api.modules).collect::<Vec<_>>();
    if all_mods.iter().any(|m| !m.functions.is_empty()) {
        // In Emscripten mode `_raw` is the loader's export-binding object, a
        // plain record, not a `WebAssembly.Exports`.
        if emscripten {
            out.push_str("  _raw: Record<string, unknown>;\n");
        } else {
            out.push_str("  _raw: WebAssembly.Exports;\n");
        }
        for module in &api.modules {
            render_dts_module_interface(&mut out, module, &module.name, "  ", emscripten);
        }
    }
    out.push_str("}\n\n");

    if emscripten {
        out.push_str(&format!(
            "export function {load_fn}(module: object | Promise<object>): Promise<{interface_name}>;\n\n"
        ));
    } else {
        out.push_str(&format!(
            "export function {load_fn}(url: string): Promise<{interface_name}>;\n\n"
        ));
    }
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

/// Emit the TypeScript declaration for a rich (algebraic) enum: an opaque
/// handle wrapper `class` exposing the `tag` discriminant reader, a frozen
/// `Tag` map, one static factory per variant (`Shape.circle(...)`), the
/// camelCase namespaced field getters, and `free()`. Mirrors the runtime JS
/// class emitted by [`emit_rich_enum_class`].
fn emit_dts_rich_enum_class(out: &mut String, e: &EnumDef) {
    let name = &e.name;
    let mut w = CodeWriter::two_space();
    w.doc(&e.doc, DocCommentStyle::Javadoc);
    w.block(format!("export declare class {name} {{"), "}", |w| {
        w.line("get tag(): number;");
        w.block("static readonly Tag: Readonly<{", "}>;", |w| {
            for v in &e.variants {
                w.line(format!("{}: {};", v.name, v.value));
            }
        });
        for v in &e.variants {
            w.doc(&v.doc, DocCommentStyle::Javadoc);
            let factory = v.name.to_lower_camel_case();
            let params: Vec<String> = v
                .fields
                .iter()
                .map(|f| format!("{}: {}", f.name, ts_type_for(&f.ty)))
                .collect();
            w.line(format!("static {factory}({}): {name};", params.join(", ")));
        }
        for v in &e.variants {
            for f in &v.fields {
                let js_name = format!("{}_{}", v.name, f.name).to_lower_camel_case();
                w.line(format!("get {js_name}(): {};", ts_type_for(&f.ty)));
            }
        }
        w.line("free(): void;");
    });
    w.blank();
    out.push_str(&w.finish());
}

fn render_dts_module_interface(
    out: &mut String,
    m: &Module,
    module_path: &str,
    indent: &str,
    emscripten: bool,
) {
    let has_content = !m.functions.is_empty()
        || m.modules
            .iter()
            .any(|sub| !sub.functions.is_empty() || !sub.modules.is_empty());
    if !has_content {
        return;
    }
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(format!("{}: {{", m.name), "};", |w| {
        let inner = w.indent_str();
        for func in &m.functions {
            // Async functions are throwing stubs in Emscripten mode; omitting
            // them here makes the gap a compile-time error for TS consumers.
            if emscripten && func.r#async {
                continue;
            }
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
            let mut tags = vec!["@throws {Error} if the native call fails".to_string()];
            if let Some(msg) = &func.deprecated {
                tags.insert(0, format!("@deprecated {msg}"));
            }
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &func.doc, &func.params, &inner, &tags);
            w.raw(doc);
            w.line(format!("{}({}): {};", func.name, params.join(", "), ret));
        }
        for sub in &m.modules {
            let sub_path = format!("{module_path}_{}", sub.name);
            let mut tmp = String::new();
            render_dts_module_interface(&mut tmp, sub, &sub_path, &inner, emscripten);
            w.raw(tmp);
        }
    });
    out.push_str(&w.finish());
}

/// Every producer-exported symbol the generated JS body calls through the
/// bound `wasm` object, in model traversal order. The Emscripten loader
/// prologue binds each one from its underscore-prefixed `Module` property, so
/// this list must cover every call site the body emits. Async launchers are
/// excluded: in Emscripten mode (the only caller) they are throwing stubs.
fn collect_called_symbols(model: &BindingModel) -> Vec<String> {
    fn push_unique(syms: &mut Vec<String>, s: &str) {
        if !syms.iter().any(|x| x == s) {
            syms.push(s.to_string());
        }
    }
    let mut syms = Vec::new();
    for m in &model.modules {
        for e in &m.enums {
            if let Some(rich) = &e.rich {
                push_unique(&mut syms, &rich.tag_symbol);
                for v in &rich.variants {
                    push_unique(&mut syms, &v.create.symbol);
                    for f in &v.fields {
                        push_unique(&mut syms, &f.getter_symbol);
                    }
                }
                push_unique(&mut syms, &rich.destroy_symbol);
            }
        }
        for s in &m.structs {
            push_unique(&mut syms, &s.create.symbol);
            for f in &s.fields {
                push_unique(&mut syms, &f.getter_symbol);
            }
            if let Some(b) = &s.builder {
                push_unique(&mut syms, &b.new_symbol);
                for (_field, setter) in &b.setters {
                    push_unique(&mut syms, setter);
                }
                push_unique(&mut syms, &b.build_symbol);
            }
        }
        for f in &m.functions {
            match &f.shape {
                CallShape::Iterator(it) => {
                    push_unique(&mut syms, &f.c_base);
                    push_unique(&mut syms, &it.next.symbol);
                    push_unique(&mut syms, &it.destroy_symbol);
                }
                CallShape::Async(_) => {}
                CallShape::Sync(_) => push_unique(&mut syms, &f.c_base),
            }
        }
    }
    syms
}

fn render_wasm_js_stub(
    api: &Api,
    module_name: &str,
    prefix: &str,
    input_basename: &str,
    filename: &str,
    emscripten: bool,
) -> String {
    let pascal_name = module_name.to_upper_camel_case();
    let load_fn = format!("load{pascal_name}");
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let model = BindingModel::build(api, prefix);
    let by_path: HashMap<&str, &ModuleBinding> =
        model.modules.iter().map(|m| (m.path.as_str(), m)).collect();

    let has_functions = model.modules.iter().any(|m| !m.functions.is_empty());
    // In Emscripten mode async functions are throwing stubs, so none of the
    // trampoline machinery (or its helpers) is emitted.
    let has_async = !emscripten && model.functions().any(|(_, f)| f.is_async);
    // Opaque-object wrappers (structs and rich/algebraic enums) construct via a
    // fallible `*_new`/`*_create` that threads an `out_err`, so they need the
    // error helpers even in a module that declares no free functions.
    let has_opaque = model
        .modules
        .iter()
        .any(|m| !m.structs.is_empty() || m.enums.iter().any(|e| e.is_rich()));
    let needs_err = has_functions || has_opaque;
    // Error messages always cross as C strings, so anything needing the error
    // helpers also needs the string-read helpers regardless of declared types.
    let needs_strings = needs_err || api_deep_any(api, &|t| is_string_type(t));
    let needs_bytes = api_deep_any(api, &|t| {
        matches!(t, TypeRef::Bytes | TypeRef::BorrowedBytes)
    });
    let needs_str_array = api_deep_any(api, &|t| match t {
        TypeRef::List(inner) => is_string_type(inner),
        TypeRef::Map(k, v) => is_string_type(k) || is_string_type(v),
        TypeRef::Iterator(inner) => is_string_type(inner),
        _ => false,
    });

    out.push_str("// WeaveFFI WASM bindings (auto-generated)\n");
    out.push_str("//\n");
    if emscripten {
        out.push_str("// Boundary conventions for an Emscripten build:\n");
    } else {
        out.push_str("// Boundary conventions for a wasm32-unknown-unknown build:\n");
    }
    out.push_str("//\n");
    out.push_str("//   Handles   -> i32 pointer into linear memory (0 = null/absent)\n");
    out.push_str("//   Enums     -> i32 discriminant value\n");
    out.push_str("//   i64/u64   -> JavaScript BigInt\n");
    out.push_str("//   Strings   -> NUL-terminated UTF-8 (const char*); a single i32 pointer\n");
    out.push_str("//   Bytes     -> i32 data pointer + i32 length (out_len for returns)\n");
    out.push_str("//   Optionals -> null handle / null pointer (0); scalars boxed by pointer\n");
    out.push('\n');

    if needs_strings {
        out.push_str("const _enc = new TextEncoder();\n");
        out.push_str("const _dec = new TextDecoder();\n\n");
        out.push_str("// Stage a JS string as a NUL-terminated C string in linear memory.\n");
        out.push_str("// Returns [ptr, size] (size includes the NUL); release with _free.\n");
        out.push_str("function _cstr(wasm, str) {\n");
        out.push_str("  const bytes = _enc.encode(str);\n");
        out.push_str("  const size = bytes.length + 1;\n");
        out.push_str("  const ptr = wasm.weaveffi_alloc(size);\n");
        out.push_str("  const mem = new Uint8Array(wasm.memory.buffer, ptr, size);\n");
        out.push_str("  mem.set(bytes);\n");
        out.push_str("  mem[bytes.length] = 0;\n");
        out.push_str("  return [ptr, size];\n");
        out.push_str("}\n\n");
        out.push_str("// Read a NUL-terminated C string (0 => null). Does not free.\n");
        out.push_str("function _readCStr(wasm, ptr) {\n");
        out.push_str("  if (ptr === 0) return null;\n");
        out.push_str("  const mem = new Uint8Array(wasm.memory.buffer);\n");
        out.push_str("  let end = ptr;\n");
        out.push_str("  while (mem[end] !== 0) end++;\n");
        out.push_str("  return _dec.decode(mem.subarray(ptr, end));\n");
        out.push_str("}\n\n");
        out.push_str("// Read then free a producer-owned C string.\n");
        out.push_str("function _takeCStr(wasm, ptr) {\n");
        out.push_str("  const s = _readCStr(wasm, ptr);\n");
        out.push_str("  if (ptr !== 0) wasm.weaveffi_free_string(ptr);\n");
        out.push_str("  return s;\n");
        out.push_str("}\n\n");
    }

    if needs_bytes {
        out.push_str("// Stage a byte buffer; returns [ptr, len]; release with _free(ptr, len).\n");
        out.push_str("function _bytes(wasm, data) {\n");
        out.push_str("  const u8 = data instanceof Uint8Array ? data : new Uint8Array(data);\n");
        out.push_str("  const ptr = wasm.weaveffi_alloc(u8.length);\n");
        out.push_str(
            "  if (u8.length) new Uint8Array(wasm.memory.buffer, ptr, u8.length).set(u8);\n",
        );
        out.push_str("  return [ptr, u8.length];\n");
        out.push_str("}\n\n");
        out.push_str("// Copy then free a producer-owned byte buffer.\n");
        out.push_str("function _takeBytes(wasm, ptr, len) {\n");
        out.push_str("  if (ptr === 0 || len === 0) return new Uint8Array(0);\n");
        out.push_str("  const copy = new Uint8Array(wasm.memory.buffer, ptr, len).slice();\n");
        out.push_str("  wasm.weaveffi_free_bytes(ptr, len);\n");
        out.push_str("  return copy;\n");
        out.push_str("}\n\n");
    }

    if needs_str_array {
        out.push_str("// Decode a producer-owned array of `len` C strings at `base` (each\n");
        out.push_str("// freed); the array container itself is owned by the producer.\n");
        out.push_str("function _takeStrArray(wasm, base, len) {\n");
        out.push_str("  const out = [];\n");
        out.push_str("  if (base === 0) return out;\n");
        out.push_str("  const dv = new DataView(wasm.memory.buffer);\n");
        out.push_str("  const ptrs = [];\n");
        out.push_str(
            "  for (let i = 0; i < len; i++) ptrs.push(dv.getUint32(base + i * 4, true));\n",
        );
        out.push_str("  for (const p of ptrs) out.push(_takeCStr(wasm, p));\n");
        out.push_str("  return out;\n");
        out.push_str("}\n\n");
    }

    if needs_err {
        out.push_str("// Allocate a zeroed {i32 code, i32 message} error slot.\n");
        out.push_str("function _allocErr(wasm) {\n");
        out.push_str("  const ptr = wasm.weaveffi_alloc(8);\n");
        out.push_str("  new Uint8Array(wasm.memory.buffer, ptr, 8).fill(0);\n");
        out.push_str("  return ptr;\n");
        out.push_str("}\n\n");
        out.push_str("// Throw (and free the slot) if the error slot carries a non-zero code.\n");
        out.push_str("function _checkErr(wasm, errPtr) {\n");
        out.push_str("  const dv = new DataView(wasm.memory.buffer);\n");
        out.push_str("  const code = dv.getInt32(errPtr, true);\n");
        out.push_str("  if (code !== 0) {\n");
        out.push_str("    const msgPtr = dv.getUint32(errPtr + 4, true);\n");
        out.push_str("    const msg = _readCStr(wasm, msgPtr) || '';\n");
        out.push_str("    wasm.weaveffi_error_clear(errPtr);\n");
        out.push_str("    wasm.weaveffi_dealloc(errPtr, 8);\n");
        out.push_str("    throw new Error(`WeaveFFI error ${code}: ${msg}`);\n");
        out.push_str("  }\n");
        out.push_str("}\n\n");
        out.push_str("// Release an error slot on the success path.\n");
        out.push_str("function _freeErr(wasm, errPtr) {\n");
        out.push_str("  wasm.weaveffi_dealloc(errPtr, 8);\n");
        out.push_str("}\n\n");
        if has_async {
            out.push_str("// Throw if a borrowed (producer-owned) error carries a non-zero\n");
            out.push_str("// code. Used by async callbacks: the producer owns and frees the\n");
            out.push_str("// error struct, so the slot is read but never deallocated here.\n");
            out.push_str("function _checkErrRef(wasm, errPtr) {\n");
            out.push_str("  const dv = new DataView(wasm.memory.buffer);\n");
            out.push_str("  const code = dv.getInt32(errPtr, true);\n");
            out.push_str("  if (code === 0) return;\n");
            out.push_str("  const msgPtr = dv.getUint32(errPtr + 4, true);\n");
            out.push_str("  const msg = _readCStr(wasm, msgPtr) || '';\n");
            out.push_str("  throw new Error(`WeaveFFI error ${code}: ${msg}`);\n");
            out.push_str("}\n\n");
        }
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

    for (module, _path) in walk_modules_with_path(&api.modules) {
        for e in &module.enums {
            // Rich (algebraic) enums cross the ABI as opaque object handles, so
            // they are emitted as wrapper classes below, never as a plain
            // by-value discriminant object (which would also collide with the
            // class declaration of the same name).
            if e.is_rich() {
                continue;
            }
            out.push_str(&format!("export const {} = Object.freeze({{\n", e.name));
            for v in &e.variants {
                out.push_str(&format!("  {}: {},\n", v.name, v.value));
            }
            out.push_str("});\n\n");
        }
    }

    for module in &model.modules {
        for s in &module.structs {
            emit_struct_class(&mut out, s);
        }
        for e in &module.enums {
            if e.is_rich() {
                emit_rich_enum_class(&mut out, e);
            }
        }
    }

    out.push_str("/**\n");
    if emscripten {
        out.push_str(" * Load a WeaveFFI API from a pre-initialized Emscripten module.\n");
        out.push_str(" *\n");
        out.push_str(" * @param {Object|Promise<Object>} module - The initialized Emscripten\n");
        out.push_str(" *   module, or the promise returned by its `MODULARIZE` factory.\n");
        if api.modules.is_empty() {
            out.push_str(" * @returns {Promise<Object>} The Emscripten module.\n");
        } else {
            out.push_str(" * @returns {Promise<Object>} The API bindings.\n");
        }
    } else {
        out.push_str(" * Load a WeaveFFI WASM module from the given URL.\n");
        out.push_str(" *\n");
        out.push_str(" * @param {string} url - URL to the `.wasm` file.\n");
        if api.modules.is_empty() {
            out.push_str(
                " * @returns {Promise<WebAssembly.Exports>} The exported WASM functions.\n",
            );
        } else {
            out.push_str(" * @returns {Promise<Object>} The API bindings.\n");
        }
    }
    out.push_str(" *\n");
    out.push_str(" * Exported functions follow the C ABI naming convention:\n");
    out.push_str(&format!(
        " *   {prefix}_{{module}}_{{function}}(params...) -> result\n"
    ));
    out.push_str(" *\n");
    out.push_str(" * @example\n");
    if emscripten {
        out.push_str(" * import Module from './your_library.js';\n");
        out.push_str(&format!(" * const api = await {load_fn}(Module());\n"));
    } else {
        out.push_str(&format!(" * const api = await {load_fn}('lib.wasm');\n"));
    }
    out.push_str(" *\n");
    out.push_str(" * // Primitive: plain numbers in, number out.\n");
    out.push_str(" * const sum = api.math.add(1, 2);\n");
    out.push_str(" *\n");
    out.push_str(" * // Struct: returns a wrapper instance exposing field getters.\n");
    out.push_str(" * const person = api.contacts.create();\n");
    out.push_str(" * console.log(person.name);\n");
    out.push_str(" *\n");
    out.push_str(" * // Enum: pass the integer discriminant.\n");
    out.push_str(" * api.ui.set_color(0); // 0 = first variant\n");
    out.push_str(" *\n");
    out.push_str(" * // Optional: pass null to omit, a value to provide.\n");
    out.push_str(" * api.config.set_timeout(5000); // present\n");
    out.push_str(" * api.config.set_timeout(null); // absent\n");
    out.push_str(" *\n");
    out.push_str(" * // List/Map: pass arrays/objects; receive arrays/objects.\n");
    out.push_str(" * const names = api.data.all_names();\n");
    out.push_str(" */\n");
    if emscripten {
        out.push_str(&format!("export async function {load_fn}(module) {{\n"));
        out.push_str("  const m = await Promise.resolve(module);\n");
    } else {
        out.push_str(&format!("export async function {load_fn}(url) {{\n"));
        out.push_str("  const response = await fetch(url);\n");
        out.push_str("  const bytes = await response.arrayBuffer();\n");
        out.push_str("  const { instance } = await WebAssembly.instantiate(bytes, {});\n");
    }

    if api.modules.is_empty() {
        if emscripten {
            out.push_str("  return m;\n");
        } else {
            out.push_str("  return instance.exports;\n");
        }
    } else {
        if emscripten {
            // Bind the Emscripten exports once, up front, to the exact symbol
            // names the glue above calls. Module access stays in quoted
            // bracket notation so Closure Compiler's advanced property
            // renaming cannot break it, while the rest of the glue keeps
            // consistent dot access on this locally-constructed object.
            let mut bindings: Vec<(String, String)> = vec![
                ("weaveffi_alloc".to_string(), format!("{prefix}_alloc")),
                ("weaveffi_dealloc".to_string(), format!("{prefix}_dealloc")),
            ];
            if needs_strings {
                bindings.push((
                    "weaveffi_free_string".to_string(),
                    format!("{prefix}_free_string"),
                ));
            }
            if needs_bytes {
                bindings.push((
                    "weaveffi_free_bytes".to_string(),
                    format!("{prefix}_free_bytes"),
                ));
            }
            if needs_err {
                bindings.push((
                    "weaveffi_error_clear".to_string(),
                    format!("{prefix}_error_clear"),
                ));
            }
            bindings.extend(collect_called_symbols(&model).into_iter().map(|s| {
                let export = s.clone();
                (s, export)
            }));
            out.push_str("  // Bind the underscore-prefixed Emscripten exports to the symbol\n");
            out.push_str("  // names the glue above calls. Quoted bracket access keeps the\n");
            out.push_str("  // bindings safe under Closure Compiler's property renaming.\n");
            out.push_str("  const wasm = {\n");
            out.push_str("    // Emscripten replaces HEAPU8 when linear memory grows, so the\n");
            out.push_str("    // buffer is re-read on every access instead of captured once.\n");
            out.push_str("    get memory() { return { buffer: m['HEAPU8'].buffer }; },\n");
            for (name, export) in &bindings {
                out.push_str(&format!("    {name}: m['_{export}'],\n"));
            }
            out.push_str("  };\n\n");
        } else {
            out.push_str("  const wasm = instance.exports;\n\n");
        }

        if has_async {
            out.push_str("  let _nextCtxId = 1;\n");
            out.push_str("  const _asyncContexts = new Map();\n");
            out.push_str("  const _table = wasm.__indirect_function_table;\n\n");
            out.push_str("  function _asyncHandler(ctxId, errPtr, ...results) {\n");
            out.push_str("    const ctx = _asyncContexts.get(ctxId);\n");
            out.push_str("    if (!ctx) return;\n");
            out.push_str("    _asyncContexts.delete(ctxId);\n");
            out.push_str("    try {\n");
            out.push_str("      if (errPtr !== 0) _checkErrRef(wasm, errPtr);\n");
            out.push_str(
                "      ctx.resolve(ctx.unwrap ? ctx.unwrap(wasm, ...results) : results[0]);\n",
            );
            out.push_str("    } catch (e) {\n");
            out.push_str("      ctx.reject(e);\n");
            out.push_str("    }\n");
            out.push_str("  }\n\n");

            let mut trampolines: Vec<(String, Vec<&'static str>)> = Vec::new();
            for (_m, f) in model.functions() {
                if f.is_async {
                    let params = async_cb_wasm_params(f.ret.as_ref());
                    let key = params.join("_");
                    if !trampolines.iter().any(|(k, _)| k == &key) {
                        trampolines.push((key, params));
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
            render_js_module_object(&mut out, module, &module.name, &by_path, "    ", emscripten);
        }
        out.push_str("  };\n");
    }

    out.push_str("}\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

/// Whether a module subtree exposes anything (functions or struct factories),
/// so empty namespace objects are not emitted.
fn module_tree_has_content(
    m: &Module,
    path: &str,
    by_path: &HashMap<&str, &ModuleBinding>,
) -> bool {
    let here = by_path.get(path).is_some_and(|mb| {
        !mb.functions.is_empty()
            || !mb.structs.is_empty()
            || !mb.listeners.is_empty()
            || mb.enums.iter().any(|e| e.is_rich())
    });
    here || m
        .modules
        .iter()
        .any(|sub| module_tree_has_content(sub, &format!("{path}_{}", sub.name), by_path))
}

fn render_js_module_object(
    out: &mut String,
    m: &Module,
    module_path: &str,
    by_path: &HashMap<&str, &ModuleBinding>,
    indent: &str,
    emscripten: bool,
) {
    if !module_tree_has_content(m, module_path, by_path) {
        return;
    }
    let mb = by_path[module_path];
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(format!("{}: {{", m.name), "},", |w| {
        let inner = w.indent_str();
        for f in &mb.functions {
            let mut tmp = String::new();
            match &f.shape {
                CallShape::Iterator(ib) => {
                    emit_js_iterator_function_wrapper(&mut tmp, f, ib, &inner)
                }
                _ if f.is_async && emscripten => emit_js_async_stub(&mut tmp, f, &inner),
                _ if f.is_async => emit_js_async_function_wrapper(&mut tmp, f, &inner),
                _ => emit_js_function_wrapper(&mut tmp, f, &inner),
            }
            w.raw(tmp);
        }
        for l in &mb.listeners {
            let mut tmp = String::new();
            emit_js_listener_stub(&mut tmp, l, &inner);
            w.raw(tmp);
        }
        for s in &mb.structs {
            let mut tmp = String::new();
            emit_js_struct_factory(&mut tmp, s, &inner);
            w.raw(tmp);
        }
        for e in &mb.enums {
            if e.is_rich() {
                let mut tmp = String::new();
                emit_js_rich_enum_factory(&mut tmp, e, &inner);
                w.raw(tmp);
            }
        }
        for sub in &m.modules {
            let sub_path = format!("{module_path}_{}", sub.name);
            let mut tmp = String::new();
            render_js_module_object(&mut tmp, sub, &sub_path, by_path, &inner, emscripten);
            w.raw(tmp);
        }
    });
    out.push_str(&w.finish());
}

/// Async functions are unsupported in Emscripten mode: the trampoline
/// registration relies on `WebAssembly.Function` and a growable
/// `__indirect_function_table`, neither of which an Emscripten module exposes
/// portably. Each async entry point becomes an explicit stub that throws at
/// call time, so the gap is impossible to miss from JS even though the
/// `.d.ts` deliberately omits it (a compile-time error for TS users).
fn emit_js_async_stub(out: &mut String, f: &FnBinding, indent: &str) {
    let js_params: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(
        format!("{}({}) {{", f.name, js_params.join(", ")),
        "},",
        |w| {
            w.line(format!(
                "throw new Error(\"weaveffi: async function '{}' is not supported in \
                 Emscripten mode; use the wasm32-unknown-unknown loader or a native \
                 target\");",
                f.name
            ));
        },
    );
    out.push_str(&w.finish());
}

/// Listeners are unsupported on wasm (see `WasmGenerator::capabilities`);
/// generation only reaches here when `allow_unsupported` is set. Each
/// register/unregister entry point becomes an explicit stub that throws at
/// call time, so the gap is impossible to miss from JS even though the
/// `.d.ts` deliberately omits the pair (a compile-time error for TS users).
fn emit_js_listener_stub(out: &mut String, l: &ListenerBinding, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    for op in ["register", "unregister"] {
        w.block(format!("{op}_{}() {{", l.name), "},", |w| {
            w.line(format!(
                "throw new Error(\"weaveffi: listener '{}' is not supported by the wasm \
                 target (single-threaded wasm has no producer thread to deliver events); use a \
                 native target for listeners\");",
                l.name
            ));
        });
    }
    out.push_str(&w.finish());
}

/// Expose a struct's `create(...)` and (when present) `builder()` on the module
/// object, bound to the loaded `wasm` instance.
fn emit_js_struct_factory(out: &mut String, s: &StructBinding, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(format!("{}: {{", s.name), "},", |w| {
        w.line(format!(
            "create: (...args) => {}.create(wasm, ...args),",
            s.name
        ));
        if s.builder.is_some() {
            w.line(format!("builder: () => new {}Builder(wasm),", s.name));
        }
    });
    out.push_str(&w.finish());
}

/// Expose a rich (algebraic) enum on the module object: one per-variant factory
/// (`api.shapes.Shape.circle(2.5)`) plus the `Tag` discriminant map
/// (`api.shapes.Shape.Tag.Circle`), all bound to the loaded `wasm` instance.
fn emit_js_rich_enum_factory(out: &mut String, e: &EnumBinding, indent: &str) {
    let Some(rich) = e.rich.as_ref() else {
        return;
    };
    let name = &e.name;
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(format!("{name}: {{"), "},", |w| {
        for v in &rich.variants {
            let factory = v.name.to_lower_camel_case();
            w.line(format!(
                "{factory}: (...args) => {name}.{factory}(wasm, ...args),"
            ));
        }
        w.line(format!("Tag: {name}.Tag,"));
    });
    out.push_str(&w.finish());
}

/// Emit a synchronous function as a method `name(params) { ... }` at `indent`,
/// staging idiomatic inputs, calling the C symbol, and decoding the return.
fn emit_js_function_wrapper(out: &mut String, f: &FnBinding, indent: &str) {
    let body = format!("{indent}  ");
    let js_params: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);

    if let Some(msg) = &f.deprecated {
        w.line(format!("/** @deprecated {msg} */"));
    }
    w.line(format!("{}({}) {{", f.name, js_params.join(", ")));

    let mut inner = String::new();
    let mut args = Vec::new();
    let mut cleanup = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            &mut inner,
            &body,
            &p.ty,
            &p.name,
            &format!("a{i}"),
            &mut args,
            &mut cleanup,
        );
    }
    emit_return_decode(
        &mut inner,
        &body,
        f.ret.as_ref(),
        &f.c_base,
        &args,
        &cleanup,
        true,
    );
    w.raw(inner);
    w.line("},");
    out.push_str(&w.finish());
}

/// Emit an iterator-returning function as a method that drains the iterator
/// eagerly into a JS array (matching the `T[]` TypeScript shape).
fn emit_js_iterator_function_wrapper(
    out: &mut String,
    f: &FnBinding,
    ib: &IteratorBinding,
    indent: &str,
) {
    let body = format!("{indent}  ");
    let js_params: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);

    if let Some(msg) = &f.deprecated {
        w.line(format!("/** @deprecated {msg} */"));
    }
    w.line(format!("{}({}) {{", f.name, js_params.join(", ")));

    let mut args = Vec::new();
    let mut cleanup = Vec::new();
    let mut staged = String::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            &mut staged,
            &body,
            &p.ty,
            &p.name,
            &format!("a{i}"),
            &mut args,
            &mut cleanup,
        );
    }
    if f.cancellable {
        args.push("0".to_string());
    }
    args.push("_err".to_string());
    let stride = wasm_stride(&ib.elem);
    w.scope(|w| {
        w.raw(&staged);
        w.line("const _err = _allocErr(wasm);");
        w.line(format!(
            "const _it = wasm.{}({});",
            f.c_base,
            args.join(", ")
        ));
        for stmt in &cleanup {
            w.line(stmt);
        }
        w.line("_checkErr(wasm, _err);");
        w.line("_freeErr(wasm, _err);");
        w.line("const _out = [];");
        w.line(format!("const _ip = wasm.weaveffi_alloc({stride});"));
        w.line("const _ierr = _allocErr(wasm);");
        w.block(
            format!("while (wasm.{}(_it, _ip, _ierr) !== 0) {{", ib.next.symbol),
            "}",
            |w| {
                w.line("_checkErr(wasm, _ierr);");
                w.line("const _dv = new DataView(wasm.memory.buffer);");
                match &ib.elem {
                    TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                        w.line("_out.push(_takeCStr(wasm, _dv.getUint32(_ip, true)));");
                    }
                    TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                        let cls = local_type_name(name);
                        w.line(format!(
                            "_out.push(new {cls}(wasm, _dv.getInt32(_ip, true)));"
                        ));
                    }
                    scalar => {
                        let elem = wasm_read_scalar_elem(scalar, "_dv", "_ip", "0")
                            .replace(&format!("_ip + 0 * {stride}"), "_ip");
                        w.line(format!("_out.push({elem});"));
                    }
                }
            },
        );
        w.line("_checkErr(wasm, _ierr);");
        w.line("_freeErr(wasm, _ierr);");
        w.line(format!("wasm.weaveffi_dealloc(_ip, {stride});"));
        w.line(format!("wasm.{}(_it);", ib.destroy_symbol));
        w.line("return _out;");
    });
    w.line("},");
    out.push_str(&w.finish());
}

/// The wasm callback param-type list for an async function with the given
/// return: always `(ctx i32, err i32, ...result)`. Pointers/handles are i32 on
/// wasm32; only `i64`/`u64` widen to i64.
fn async_cb_wasm_params(returns: Option<&TypeRef>) -> Vec<&'static str> {
    let mut params = vec!["i32", "i32"];
    match returns {
        None => {}
        Some(
            TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::Bool
            | TypeRef::Enum(_)
            | TypeRef::StringUtf8
            | TypeRef::BorrowedStr
            | TypeRef::Struct(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Iterator(_),
        ) => {
            params.push("i32");
        }
        Some(TypeRef::I64 | TypeRef::U64 | TypeRef::Handle) => {
            params.push("i64");
        }
        Some(TypeRef::F32) => {
            params.push("f32");
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
            TypeRef::Handle => params.push("i64"),
            TypeRef::Map(_, _) => {
                params.push("i32");
                params.push("i32");
                params.push("i32");
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => {
                params.push("i32");
                params.push("i32");
            }
            // struct/typed-handle/iterator (null pointer) and scalars/strings
            // (boxed by pointer) all arrive as a single i32.
            _ => params.push("i32"),
        },
    }
    params
}

/// Emit the `unwrap` clause for an async result, or `None` for a void/raw-scalar
/// result (where `results[0]` is already idiomatic). Assumes the callback was
/// registered with [`async_cb_wasm_params`] widths.
fn emit_async_unwrap(out: &mut String, indent: &str, ret: Option<&TypeRef>) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let Some(ret) = ret else {
        w.line("_asyncContexts.set(ctxId, { resolve, reject });");
        out.push_str(&w.finish());
        return;
    };
    let open = "_asyncContexts.set(ctxId, { resolve, reject, unwrap: ";
    match ret {
        TypeRef::Bool => {
            w.line(format!("{open}(w, r) => r !== 0 }});"));
        }
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Handle
        | TypeRef::Enum(_) => {
            w.line("_asyncContexts.set(ctxId, { resolve, reject });");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("{open}(w, p) => _takeCStr(w, p) }});"));
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let cls = local_type_name(name);
            w.line(format!("{open}(w, h) => new {cls}(w, h) }});"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                let cls = local_type_name(name);
                w.line(format!(
                    "{open}(w, h) => h === 0 ? null : new {cls}(w, h) }});"
                ));
            }
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!("{open}(w, p) => _takeCStr(w, p) }});"));
            }
            _ => {
                w.line("_asyncContexts.set(ctxId, { resolve, reject });");
            }
        },
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!(
                "{open}(w, ptr, len) => _takeBytes(w, ptr, len) }});"
            ));
        }
        TypeRef::List(inner) => {
            w.block(format!("{open}(w, base, len) => {{"), "} });", |w| {
                w.line("const wasm = w;");
                let ind = w.indent_str();
                let mut tmp = String::new();
                emit_read_list_into(&mut tmp, &ind, inner, "base", "len", "_out");
                w.raw(tmp);
                w.line("return _out;");
            });
        }
        TypeRef::Map(k, v) => {
            w.block(format!("{open}(w, ka, va, len) => {{"), "} });", |w| {
                w.line("const wasm = w;");
                let ind = w.indent_str();
                let mut tmp = String::new();
                emit_read_map_into(&mut tmp, &ind, k, v, "ka", "va", "len", "_out");
                w.raw(tmp);
                w.line("return _out;");
            });
        }
        TypeRef::Iterator(_) => {
            w.line("_asyncContexts.set(ctxId, { resolve, reject });");
        }
    }
    out.push_str(&w.finish());
}

/// Emit an async function as a method returning a `Promise` at `indent`.
fn emit_js_async_function_wrapper(out: &mut String, f: &FnBinding, indent: &str) {
    let body2 = format!("{indent}    ");
    let js_params: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);

    if let Some(msg) = &f.deprecated {
        w.line(format!("/** @deprecated {msg} */"));
    }

    // Pre-render the inner-most (depth + 2) fragments that delegate to helpers,
    // so the nested blocks below can splice them at the right depth.
    let mut unwrap = String::new();
    emit_async_unwrap(&mut unwrap, &body2, f.ret.as_ref());
    let mut staged = String::new();
    let mut args = Vec::new();
    let mut cleanup = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            &mut staged,
            &body2,
            &p.ty,
            &p.name,
            &format!("a{i}"),
            &mut args,
            &mut cleanup,
        );
    }
    let cb_params = async_cb_wasm_params(f.ret.as_ref());
    let sig_key = cb_params.join("_");
    if f.cancellable {
        args.push("0".to_string());
    }
    args.push(format!("_cbPtr_{sig_key}"));
    args.push("ctxId".to_string());

    w.block(
        format!("{}({}) {{", f.name, js_params.join(", ")),
        "},",
        |w| {
            w.block("return new Promise((resolve, reject) => {", "});", |w| {
                w.line("const ctxId = _nextCtxId++;");
                w.raw(&unwrap);
                w.raw(&staged);
                w.line(format!("wasm.{}_async({});", f.c_base, args.join(", ")));
                for stmt in &cleanup {
                    w.line(stmt);
                }
            });
        },
    );
    out.push_str(&w.finish());
}

/// Emit the module-level `class` for a struct: constructor, field getters, and
/// a static `create(...)` factory.
fn emit_struct_class(out: &mut String, s: &StructBinding) {
    let cls = &s.name;
    let mut w = CodeWriter::two_space();
    w.block(format!("class {cls} {{"), "}", |w| {
        w.block("constructor(wasm, handle) {", "}", |w| {
            w.line("this._wasm = wasm;");
            w.line("this._handle = handle;");
        });
        for field in &s.fields {
            let mut tmp = String::new();
            emit_struct_getter(&mut tmp, field);
            w.raw(tmp);
        }
        let mut tmp = String::new();
        emit_struct_create(&mut tmp, s);
        w.raw(tmp);
    });
    w.blank();
    out.push_str(&w.finish());
    if s.builder.is_some() {
        emit_builder_class(out, s);
    }
}

/// Emit the module-level `class` for a rich (algebraic) enum: an opaque-handle
/// wrapper mirroring [`emit_struct_class`]. Adds a `tag` discriminant reader, a
/// frozen `Tag` map (`Shape.Tag.Circle`), one static factory per variant
/// (`Shape.circle(...)`), per-variant field getters namespaced in camelCase
/// (`circleRadius`), and an explicit `free()` releasing the handle once. The
/// constructor signature and `_handle` field match the struct wrapper, so the
/// existing function-wrapper marshalling (`x._handle` in, `new Shape(wasm, r)`
/// out; a rich enum lowers to `TypeRef::Struct`) works unchanged.
fn emit_rich_enum_class(out: &mut String, e: &EnumBinding) {
    let Some(rich) = e.rich.as_ref() else {
        return;
    };
    let cls = &e.name;
    let mut w = CodeWriter::two_space();
    w.block(format!("class {cls} {{"), "}", |w| {
        w.block("constructor(wasm, handle) {", "}", |w| {
            w.line("this._wasm = wasm;");
            w.line("this._handle = handle;");
        });

        // Active variant discriminant (an i32, comparable to the `Tag` members).
        w.block("get tag() {", "}", |w| {
            w.line("const wasm = this._wasm;");
            let ind = w.indent_str();
            let mut tmp = String::new();
            emit_return_decode(
                &mut tmp,
                &ind,
                Some(&TypeRef::I32),
                &rich.tag_symbol,
                &["this._handle".to_string()],
                &[],
                false,
            );
            w.raw(tmp);
        });

        // One static factory per variant (`Shape.circle(2.5)`).
        for v in &rich.variants {
            let mut tmp = String::new();
            emit_rich_enum_factory(&mut tmp, &e.name, v);
            w.raw(tmp);
        }

        // Per-variant field getters, namespaced in camelCase to avoid collisions.
        // Reuse the struct getter renderer by projecting the camelCase name onto the
        // field's precomputed getter symbol (identical marshalling).
        for v in &rich.variants {
            for f in &v.fields {
                let mut namespaced = f.clone();
                namespaced.name = format!("{}_{}", v.name, f.name).to_lower_camel_case();
                let mut tmp = String::new();
                emit_struct_getter(&mut tmp, &namespaced);
                w.raw(tmp);
            }
        }

        // Explicit cleanup: release the producer-owned handle exactly once.
        w.block("free() {", "}", |w| {
            w.block("if (this._handle !== 0) {", "}", |w| {
                w.line(format!("this._wasm.{}(this._handle);", rich.destroy_symbol));
                w.line("this._handle = 0;");
            });
        });
    });

    // Frozen discriminant map (`Shape.Tag.Circle === 1`).
    w.block(format!("{cls}.Tag = Object.freeze({{"), "});", |w| {
        for v in &e.variants {
            w.line(format!("{}: {},", v.name, v.value));
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit `static <variant>(wasm, <fields...>)` for one rich-enum variant: stage
/// each associated-data field (reusing the struct-field input marshalling),
/// invoke the variant constructor, and return the wrapped instance. A unit
/// variant takes only `wasm`.
fn emit_rich_enum_factory(out: &mut String, enum_name: &str, v: &RichVariantBinding) {
    let factory = v.name.to_lower_camel_case();
    let params: Vec<String> = v.fields.iter().map(|f| f.name.clone()).collect();
    let sig = if params.is_empty() {
        "wasm".to_string()
    } else {
        format!("wasm, {}", params.join(", "))
    };
    let mut w = CodeWriter::two_space().with_depth(1);
    w.block(format!("static {factory}({sig}) {{"), "}", |w| {
        let ind = w.indent_str();
        let mut inner = String::new();
        let mut args = Vec::new();
        let mut cleanup = Vec::new();
        for (i, f) in v.fields.iter().enumerate() {
            emit_stage_input(
                &mut inner,
                &ind,
                &f.ty,
                &f.name,
                &format!("a{i}"),
                &mut args,
                &mut cleanup,
            );
        }
        let ret = TypeRef::Struct(enum_name.to_string());
        emit_return_decode(
            &mut inner,
            &ind,
            Some(&ret),
            &v.create.symbol,
            &args,
            &cleanup,
            true,
        );
        w.raw(inner);
    });
    out.push_str(&w.finish());
}

/// Emit one `get field() { ... }` accessor that decodes the C getter's return.
fn emit_struct_getter(out: &mut String, field: &FieldBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.block(format!("get {}() {{", field.name), "}", |w| {
        w.line("const wasm = this._wasm;");
        let ind = w.indent_str();
        let mut tmp = String::new();
        emit_return_decode(
            &mut tmp,
            &ind,
            Some(&field.ty),
            &field.getter_symbol,
            &["this._handle".to_string()],
            &[],
            false,
        );
        w.raw(tmp);
    });
    out.push_str(&w.finish());
}

/// Emit `static create(wasm, <fields...>)` that stages every field and returns a
/// wrapped instance.
fn emit_struct_create(out: &mut String, s: &StructBinding) {
    let params: Vec<String> = s.fields.iter().map(|f| f.name.clone()).collect();
    let mut w = CodeWriter::two_space().with_depth(1);
    w.block(
        format!("static create(wasm, {}) {{", params.join(", ")),
        "}",
        |w| {
            let ind = w.indent_str();
            let mut inner = String::new();
            let mut args = Vec::new();
            let mut cleanup = Vec::new();
            for (i, f) in s.fields.iter().enumerate() {
                emit_stage_input(
                    &mut inner,
                    &ind,
                    &f.ty,
                    &f.name,
                    &format!("a{i}"),
                    &mut args,
                    &mut cleanup,
                );
            }
            let ret = TypeRef::Struct(s.name.clone());
            emit_return_decode(
                &mut inner,
                &ind,
                Some(&ret),
                &s.create.symbol,
                &args,
                &cleanup,
                true,
            );
            w.raw(inner);
        },
    );
    out.push_str(&w.finish());
}

/// Emit the fluent `class XBuilder` for a struct that opted into a builder.
fn emit_builder_class(out: &mut String, s: &StructBinding) {
    let Some(b) = &s.builder else {
        return;
    };
    let cls = &s.name;
    let mut w = CodeWriter::two_space();
    w.block(format!("class {cls}Builder {{"), "}", |w| {
        w.block("constructor(wasm) {", "}", |w| {
            w.line("this._wasm = wasm;");
            w.line(format!("this._b = wasm.{}();", b.new_symbol));
        });
        for (field, (_fname, setter)) in s.fields.iter().zip(&b.setters) {
            w.block(format!("{}(value) {{", field.name), "}", |w| {
                w.line("const wasm = this._wasm;");
                let ind = w.indent_str();
                let mut args = vec!["this._b".to_string()];
                let mut cleanup = Vec::new();
                let mut staged = String::new();
                emit_stage_input(
                    &mut staged,
                    &ind,
                    &field.ty,
                    "value",
                    "a0",
                    &mut args,
                    &mut cleanup,
                );
                w.raw(staged);
                w.line(format!("wasm.{}({});", setter, args.join(", ")));
                for stmt in &cleanup {
                    w.line(stmt);
                }
                w.line("return this;");
            });
        }
        w.block("build() {", "}", |w| {
            w.line("const wasm = this._wasm;");
            let ind = w.indent_str();
            let ret = TypeRef::Struct(cls.clone());
            let mut tmp = String::new();
            emit_return_decode(
                &mut tmp,
                &ind,
                Some(&ret),
                &b.build_symbol,
                &["this._b".to_string()],
                &[],
                true,
            );
            w.raw(tmp);
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField};

    fn empty_api() -> Api {
        Api {
            version: "0.4.0".into(),
            modules: vec![],
            generators: None,
            package: None,
        }
    }

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.4.0".into(),
            modules,
            generators: None,
            package: None,
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
                doc: Some("Add two numbers".into()),
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Point".into(),
                doc: Some("A 2D point".into()),
                fields: vec![
                    StructField {
                        name: "x".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "y".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: Some("Primary colors".into()),
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    /// An API with a callback + listener, which the wasm target declares
    /// unsupported.
    fn listener_api() -> Api {
        make_api(vec![Module {
            name: "events".into(),
            functions: vec![Function {
                name: "send".into(),
                params: vec![Param {
                    name: "text".into(),
                    ty: TypeRef::StringUtf8,
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
            callbacks: vec![weaveffi_ir::ir::CallbackDef {
                name: "OnMessage".into(),
                params: vec![Param {
                    name: "message".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                doc: None,
            }],
            listeners: vec![weaveffi_ir::ir::ListenerDef {
                name: "message_listener".into(),
                event_callback: "OnMessage".into(),
                doc: None,
            }],
            errors: None,
            modules: vec![],
        }])
    }

    #[test]
    fn capabilities_declare_callbacks_and_listeners_unsupported() {
        let caps = LanguageBackend::capabilities(&WasmGenerator);
        assert!(caps.async_functions);
        assert!(caps.iterators);
        assert!(!caps.callbacks);
        assert!(!caps.listeners);
    }

    #[test]
    fn allow_unsupported_flag_flows_from_config() {
        assert!(!LanguageBackend::allows_unsupported(
            &WasmGenerator,
            &WasmConfig::default()
        ));
        let cfg = WasmConfig {
            allow_unsupported: true,
            ..WasmConfig::default()
        };
        assert!(LanguageBackend::allows_unsupported(&WasmGenerator, &cfg));
    }

    #[test]
    fn listeners_emit_throwing_stubs_in_js() {
        let js = render_wasm_js_stub(
            &listener_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(js.contains("register_message_listener() {"), "{js}");
        assert!(js.contains("unregister_message_listener() {"), "{js}");
        assert!(
            js.contains("listener 'message_listener' is not supported by the wasm target"),
            "{js}"
        );
    }

    #[test]
    fn listeners_omitted_from_dts() {
        let api = listener_api();
        let dts = render_wasm_dts(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
        assert!(!dts.contains("register_message_listener"), "{dts}");
        assert!(dts.contains("send(text: string)"), "{dts}");
    }

    #[test]
    fn readme_documents_unsupported_features() {
        let readme = render_wasm_readme(&listener_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("## Unsupported Features"), "{readme}");
        assert!(readme.contains("events.message_listener"), "{readme}");
        assert!(readme.contains("events.OnMessage"), "{readme}");
        assert!(readme.contains("throw on call"), "{readme}");
    }

    #[test]
    fn supported_only_api_has_no_unsupported_section() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(!readme.contains("## Unsupported Features"));
    }

    #[test]
    fn readme_documents_structs() {
        let readme = render_wasm_readme(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Structs"));
        assert!(readme.contains("opaque handles"));
        assert!(readme.contains("`i64` pointers"));
    }

    #[test]
    fn readme_documents_enums() {
        let readme = render_wasm_readme(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Enums"));
        assert!(readme.contains("`i32` values"));
        assert!(readme.contains("discriminant"));
    }

    #[test]
    fn readme_documents_optionals() {
        let readme = render_wasm_readme(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Optionals"));
        assert!(readme.contains("`0` / `null`"));
        assert!(readme.contains("_is_present"));
    }

    #[test]
    fn readme_documents_lists() {
        let readme = render_wasm_readme(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Lists"));
        assert!(readme.contains("pointer + length"));
        assert!(readme.contains("`i32` pointer, `i32` length"));
    }

    #[test]
    fn js_stub_has_jsdoc() {
        let js = render_wasm_js_stub(
            &empty_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(js.contains("@param {string} url"));
        assert!(js.contains("@returns {Promise<WebAssembly.Exports>}"));
        assert!(js.contains("@example"));
    }

    #[test]
    fn js_stub_documents_complex_types() {
        let js = render_wasm_js_stub(
            &empty_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(js.contains("Struct: returns a wrapper instance exposing field getters."));
        assert!(js.contains("Enum: pass the integer discriminant."));
        assert!(js.contains("Optional: pass null to omit, a value to provide."));
        assert!(js.contains("List/Map: pass arrays/objects; receive arrays/objects."));
    }

    #[test]
    fn js_stub_has_type_convention_header() {
        let js = render_wasm_js_stub(
            &empty_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(js.contains("Handles   -> i32 pointer into linear memory (0 = null/absent)"));
        assert!(js.contains("Enums     -> i32 discriminant value"));
        assert!(js.contains("Optionals -> null handle / null pointer (0)"));
        assert!(js.contains("Bytes     -> i32 data pointer + i32 length"));
    }

    #[test]
    fn generate_writes_both_files() {
        let tmp = std::env::temp_dir().join("weaveffi_test_wasm_gen");
        let _ = std::fs::remove_dir_all(&tmp);
        let out = Utf8Path::from_path(tmp.as_path()).unwrap();
        let api = make_api(vec![]);
        WasmGenerator
            .generate(&api, out, &WasmConfig::default())
            .unwrap();

        let readme = std::fs::read_to_string(out.join("wasm/README.md")).unwrap();
        assert!(readme.contains("## Complex Type Handling"));

        let js = std::fs::read_to_string(out.join("wasm/weaveffi_wasm.js")).unwrap();
        assert!(js.contains("export async function loadWeaveffiWasm"));
        assert!(js.contains("@param {string} url"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_api_has_no_api_reference() {
        let readme = render_wasm_readme(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(!readme.contains("## API Reference"));
    }

    #[test]
    fn api_reference_lists_module() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("## API Reference"));
        assert!(readme.contains("### Module: `math`"));
    }

    #[test]
    fn api_reference_function_abi_name() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("##### `weaveffi_math_add`"));
    }

    #[test]
    fn api_reference_function_signature() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("`weaveffi_math_add(a: i32, b: i32) -> i32`"));
    }

    #[test]
    fn api_reference_function_param_table() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("| `a` | `i32` | `i32` | native WASM i32 |"));
        assert!(readme.contains("| `b` | `i32` | `i32` | native WASM i32 |"));
        assert!(readme.contains("| _returns_ | `i32` | `i32` | native WASM i32 |"));
    }

    #[test]
    fn api_reference_function_doc() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("Add two numbers"));
    }

    #[test]
    fn api_reference_struct_accessors() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("##### `Point`"));
        assert!(readme.contains("opaque handle (`i64`)"));
        assert!(readme.contains("| `weaveffi_math_Point_get_x` | `f64` |"));
        assert!(readme.contains("| `weaveffi_math_Point_get_y` | `f64` |"));
    }

    #[test]
    fn api_reference_enum_discriminants() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml", false);
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
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
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
        let readme = render_wasm_readme(&api, "weaveffi", "weaveffi.yml", false);
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
        let readme = render_wasm_readme(&api, "weaveffi", "weaveffi.yml", false);
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
        let readme = render_wasm_readme(&api, "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Module: `math`"));
        assert!(readme.contains("### Module: `io`"));
    }

    #[test]
    fn generate_writes_api_reference() {
        let tmp = std::env::temp_dir().join("weaveffi_test_wasm_gen_api");
        let _ = std::fs::remove_dir_all(&tmp);
        let out = Utf8Path::from_path(tmp.as_path()).unwrap();
        let api = sample_api();
        WasmGenerator
            .generate(&api, out, &WasmConfig::default())
            .unwrap();

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
        let js = render_wasm_js_stub(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
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
        WasmGenerator
            .generate(&api, out, &WasmConfig::default())
            .unwrap();

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
        let js = render_wasm_js_stub(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(js.contains("function _cstr(wasm, str)"));
        assert!(js.contains("function _readCStr(wasm, ptr)"));
        assert!(js.contains("function _takeCStr(wasm, ptr)"));
        assert!(js.contains("TextEncoder"));
        assert!(js.contains("TextDecoder"));
        assert!(js.contains("_cstr(wasm, name)"));
        assert!(js.contains("_takeCStr(wasm,"));
        assert!(js.contains("greet(name)"));
        assert!(js.contains("wasm.weaveffi_greeting_greet("));
    }

    #[test]
    fn wasm_js_has_error_helpers() {
        let api = sample_api();
        let js = render_wasm_js_stub(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(js.contains("function _allocErr(wasm)"));
        assert!(js.contains("function _checkErr(wasm, errPtr)"));
    }

    #[test]
    fn wasm_js_function_passes_err() {
        let api = sample_api();
        let js = render_wasm_js_stub(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(js.contains("const _err = _allocErr(wasm)"));
        assert!(js.contains("_checkErr(wasm, _err)"));
    }

    #[test]
    fn wasm_dts_has_throws_doc() {
        let api = sample_api();
        let dts = render_wasm_dts(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
        assert!(
            dts.contains("@throws"),
            "Expected .d.ts to contain @throws JSDoc comment"
        );
        assert!(dts.contains("@throws {Error} if the native call fails"));
    }

    #[test]
    fn wasm_custom_module_name() {
        let tmp = std::env::temp_dir().join("weaveffi_test_wasm_custom_name");
        let _ = std::fs::remove_dir_all(&tmp);
        let out = Utf8Path::from_path(tmp.as_path()).unwrap();
        let api = sample_api();
        let config = WasmConfig {
            module_name: Some("my_bindings".into()),
            ..WasmConfig::default()
        };
        WasmGenerator.generate(&api, out, &config).unwrap();

        assert!(out.join("wasm/my_bindings.js").exists());
        assert!(out.join("wasm/my_bindings.d.ts").exists());

        let js = std::fs::read_to_string(out.join("wasm/my_bindings.js")).unwrap();
        assert!(js.contains("loadMyBindings"));

        let dts = std::fs::read_to_string(out.join("wasm/my_bindings.d.ts")).unwrap();
        assert!(dts.contains("MyBindingsModule"));
        assert!(dts.contains("loadMyBindings"));

        let files = WasmGenerator.output_files(&api, out, &config);
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
        let dts = render_wasm_dts(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
        assert!(
            dts.contains("contact: Contact"),
            "TypedHandle should use class type not bigint: {dts}"
        );
        let js = render_wasm_js_stub(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
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
        let dts = render_wasm_dts(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
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
        let dts = render_wasm_dts(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
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
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dts = render_wasm_dts(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
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
        let js = render_wasm_js_stub(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(
            js.contains("_cstr(wasm, name)"),
            "string param should be copied to WASM memory via _cstr"
        );
        assert!(
            !js.contains("free(name"),
            "caller must not free the JS string input"
        );
        let check_err = js
            .find("_checkErr(wasm, _err)")
            .expect("_checkErr(wasm, _err) should appear in generated JS");
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
        let js = render_wasm_js_stub(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(
            js.contains("_r === 0 ? null : new Contact(wasm, _r)"),
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
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
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
        let js = render_wasm_js_stub(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
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

    /// The WASM bindings register one trampoline per async-callback
    /// signature on the indirect function table for the lifetime of the API
    /// instance and route per-call resolve/reject through the
    /// `_asyncContexts` map. Each entry is `set(ctxId, ...)` once and
    /// `delete(ctxId)` once on the callback path so the resolver closures do
    /// not leak.
    #[test]
    fn wasm_async_pins_callback_for_lifetime() {
        let api = make_api(vec![Module {
            name: "math".into(),
            functions: vec![Function {
                name: "compute".into(),
                params: vec![Param {
                    name: "x".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
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
        let js = render_wasm_js_stub(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        let trampoline_count = js.matches("_registerTrampoline").count();
        let set_count = js.matches("_asyncContexts.set(ctxId").count();
        let delete_count = js.matches("_asyncContexts.delete(ctxId)").count();
        // Trampoline is defined once and registered once per signature.
        assert_eq!(
            trampoline_count, 2,
            "expected one definition and one registration of the trampoline, got {trampoline_count}: {js}"
        );
        assert_eq!(
            set_count, delete_count,
            "every _asyncContexts.set must be matched by a delete: set={set_count} delete={delete_count}: {js}"
        );
        assert!(
            set_count >= 1,
            "expected at least one _asyncContexts.set per async fn: {js}"
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
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
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
                },
            ],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dts = render_wasm_dts(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
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
                deprecated: None,
                since: None,
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
        let dts = render_wasm_dts(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
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
        let js = render_wasm_js_stub(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(
            js.contains("weaveffi_parent_outer_fn"),
            "parent C ABI call in JS missing: {js}"
        );
        assert!(
            js.contains("weaveffi_parent_child_inner_fn"),
            "nested child C ABI call in JS missing: {js}"
        );
    }

    fn doc_module() -> Module {
        Module {
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
                    fields: vec![],
                }],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    #[test]
    fn wasm_emits_doc_on_function() {
        let dts = render_wasm_dts(
            &make_api(vec![doc_module()]),
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.d.ts",
            false,
        );
        assert!(dts.contains("Performs a thing."), "{dts}");
    }

    #[test]
    fn wasm_emits_doc_on_struct() {
        let dts = render_wasm_dts(
            &make_api(vec![doc_module()]),
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.d.ts",
            false,
        );
        assert!(dts.contains("/** An item we track. */"), "{dts}");
    }

    #[test]
    fn wasm_emits_doc_on_enum_variant() {
        let dts = render_wasm_dts(
            &make_api(vec![doc_module()]),
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.d.ts",
            false,
        );
        assert!(dts.contains("/** Kind of item. */"), "{dts}");
        assert!(dts.contains("/** A small one */"), "{dts}");
    }

    #[test]
    fn wasm_emits_doc_on_field() {
        let dts = render_wasm_dts(
            &make_api(vec![doc_module()]),
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.d.ts",
            false,
        );
        assert!(dts.contains("/** Stable id */"), "{dts}");
    }

    #[test]
    fn wasm_emits_doc_on_param() {
        let dts = render_wasm_dts(
            &make_api(vec![doc_module()]),
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.d.ts",
            false,
        );
        assert!(dts.contains("@param x the input value"), "{dts}");
    }

    #[test]
    fn wasm_custom_prefix_threads_to_user_symbols() {
        let js = render_wasm_js_stub(
            &sample_api(),
            DEFAULT_MODULE_NAME,
            "myffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        // User-exported symbols honor the configured C ABI prefix.
        assert!(
            js.contains("myffi_math_add"),
            "user export should use the custom prefix: {js}"
        );
        assert!(
            !js.contains("weaveffi_math_add"),
            "user export must not hard-code the weaveffi_ prefix: {js}"
        );
        // Runtime ABI helpers exported by weaveffi-abi stay literal.
        assert!(
            js.contains("weaveffi_alloc"),
            "runtime alloc helper must stay literal: {js}"
        );
        assert!(
            js.contains("weaveffi_error_clear"),
            "runtime error_clear helper must stay literal: {js}"
        );
    }

    /// A rich (algebraic) enum mirroring `samples/shapes`: a unit variant, an
    /// f64 payload, two f32 payloads, and a string + u8 payload, plus a plain
    /// sibling enum and free functions taking/returning the rich enum (already
    /// lowered to `TypeRef::Struct`) so the handle marshalling is exercised too.
    fn rich_enum_api() -> Api {
        fn field(name: &str, ty: TypeRef) -> StructField {
            StructField {
                name: name.into(),
                ty,
                doc: None,
                default: None,
            }
        }
        make_api(vec![Module {
            name: "shapes".into(),
            functions: vec![
                Function {
                    name: "describe".into(),
                    params: vec![Param {
                        name: "shape".into(),
                        ty: TypeRef::Struct("Shape".into()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "scale".into(),
                    params: vec![
                        Param {
                            name: "shape".into(),
                            ty: TypeRef::Struct("Shape".into()),
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "factor".into(),
                            ty: TypeRef::F64,
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Struct("Shape".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "sum_bytes".into(),
                    params: vec![Param {
                        name: "values".into(),
                        ty: TypeRef::List(Box::new(TypeRef::U8)),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::U64),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![
                EnumDef {
                    name: "Shape".into(),
                    doc: Some("An algebraic shape".into()),
                    variants: vec![
                        EnumVariant {
                            name: "Empty".into(),
                            value: 0,
                            doc: Some("The empty shape".into()),
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Circle".into(),
                            value: 1,
                            doc: None,
                            fields: vec![field("radius", TypeRef::F64)],
                        },
                        EnumVariant {
                            name: "Rectangle".into(),
                            value: 2,
                            doc: None,
                            fields: vec![
                                field("width", TypeRef::F32),
                                field("height", TypeRef::F32),
                            ],
                        },
                        EnumVariant {
                            name: "Labeled".into(),
                            value: 3,
                            doc: None,
                            fields: vec![
                                field("label", TypeRef::StringUtf8),
                                field("count", TypeRef::U8),
                            ],
                        },
                    ],
                },
                EnumDef {
                    name: "Channel".into(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Red".into(),
                            value: 0,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Green".into(),
                            value: 1,
                            doc: None,
                            fields: vec![],
                        },
                    ],
                },
            ],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    #[test]
    fn wasm_rich_enum_emits_wrapper_class() {
        let js = render_wasm_js_stub(
            &rich_enum_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        // Opaque-handle wrapper class, like a struct.
        assert!(
            js.contains("class Shape {"),
            "missing rich enum class: {js}"
        );
        assert!(
            js.contains("  constructor(wasm, handle) {"),
            "rich enum must wrap a handle: {js}"
        );
        // Tag reader + frozen discriminant map.
        assert!(js.contains("  get tag() {"), "missing tag reader: {js}");
        assert!(
            js.contains("wasm.weaveffi_shapes_Shape_tag(this._handle)"),
            "tag reader must call the tag symbol: {js}"
        );
        assert!(
            js.contains("Shape.Tag = Object.freeze({"),
            "missing Tag map: {js}"
        );
        // One static factory per variant; unit variant takes only wasm.
        assert!(js.contains("static empty(wasm) {"), "missing empty(): {js}");
        assert!(
            js.contains("static circle(wasm, radius) {"),
            "missing circle(radius): {js}"
        );
        assert!(
            js.contains("static rectangle(wasm, width, height) {"),
            "missing rectangle(width, height): {js}"
        );
        assert!(
            js.contains("static labeled(wasm, label, count) {"),
            "missing labeled(label, count): {js}"
        );
        assert!(
            js.contains("wasm.weaveffi_shapes_Shape_Circle_new(radius, _err)"),
            "circle factory must call the variant constructor: {js}"
        );
        // String payload staged via the shared _cstr helper.
        assert!(
            js.contains("_cstr(wasm, label)"),
            "labeled factory must stage its string payload: {js}"
        );
        // Per-variant getters, namespaced in camelCase.
        assert!(
            js.contains("  get circleRadius() {"),
            "missing circleRadius getter: {js}"
        );
        assert!(
            js.contains("  get rectangleWidth() {") && js.contains("  get rectangleHeight() {"),
            "missing rectangle getters: {js}"
        );
        assert!(
            js.contains("  get labeledLabel() {") && js.contains("  get labeledCount() {"),
            "missing labeled getters: {js}"
        );
        assert!(
            js.contains("wasm.weaveffi_shapes_Shape_Circle_get_radius(this._handle)"),
            "getter must call the field getter symbol: {js}"
        );
        // Explicit cleanup via the destroy symbol.
        assert!(js.contains("  free() {"), "missing free(): {js}");
        assert!(
            js.contains("this._wasm.weaveffi_shapes_Shape_destroy(this._handle)"),
            "free must call the destroy symbol: {js}"
        );
    }

    #[test]
    fn wasm_rich_enum_not_emitted_as_plain_enum_object() {
        let js = render_wasm_js_stub(
            &rich_enum_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        // The rich enum must NOT be emitted as a by-value discriminant object
        // (which would also collide with the class declaration).
        assert!(
            !js.contains("export const Shape = Object.freeze("),
            "rich enum must not be a plain enum object: {js}"
        );
        // A plain sibling enum is still emitted the by-value way.
        assert!(
            js.contains("export const Channel = Object.freeze("),
            "plain enum should still be a frozen object: {js}"
        );
    }

    #[test]
    fn wasm_rich_enum_module_factory_and_tag() {
        let js = render_wasm_js_stub(
            &rich_enum_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(
            js.contains("Shape: {"),
            "missing module factory object: {js}"
        );
        assert!(
            js.contains("circle: (...args) => Shape.circle(wasm, ...args),"),
            "missing variant factory binding: {js}"
        );
        assert!(
            js.contains("empty: (...args) => Shape.empty(wasm, ...args),"),
            "missing unit-variant factory binding: {js}"
        );
        assert!(
            js.contains("Tag: Shape.Tag,"),
            "module factory should expose the Tag map: {js}"
        );
    }

    #[test]
    fn wasm_rich_enum_function_marshals_handle() {
        let js = render_wasm_js_stub(
            &rich_enum_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        // A rich enum lowers to TypeRef::Struct, so functions pass the handle in
        // and wrap the returned handle out, identical to a struct.
        assert!(
            js.contains("wasm.weaveffi_shapes_describe(shape._handle, _err)"),
            "describe must pass the enum handle: {js}"
        );
        assert!(
            js.contains("wasm.weaveffi_shapes_scale(shape._handle, factor, _err)"),
            "scale must pass the enum handle: {js}"
        );
        assert!(
            js.contains("return new Shape(wasm, _r);"),
            "scale must wrap the returned handle: {js}"
        );
        // Errors are checked before the result wrapper is constructed.
        let check = js
            .find("_checkErr(wasm, _err)")
            .expect("scale should check the error slot");
        let wrap = js
            .find("return new Shape(wasm, _r);")
            .expect("scale should wrap the result");
        assert!(check < wrap, "errors must be checked before wrapping: {js}");
    }

    #[test]
    fn wasm_rich_enum_dts_class() {
        let dts = render_wasm_dts(
            &rich_enum_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
        assert!(
            dts.contains("export declare class Shape {"),
            "missing rich enum class declaration: {dts}"
        );
        assert!(
            dts.contains("  get tag(): number;"),
            "missing tag type: {dts}"
        );
        assert!(
            dts.contains("  static readonly Tag: Readonly<{"),
            "missing Tag map type: {dts}"
        );
        assert!(
            dts.contains("  static empty(): Shape;"),
            "missing empty factory type: {dts}"
        );
        assert!(
            dts.contains("  static circle(radius: number): Shape;"),
            "missing circle factory type: {dts}"
        );
        assert!(
            dts.contains("  static labeled(label: string, count: number): Shape;"),
            "missing labeled factory type: {dts}"
        );
        assert!(
            dts.contains("  get circleRadius(): number;"),
            "missing circleRadius type: {dts}"
        );
        assert!(
            dts.contains("  get labeledLabel(): string;"),
            "missing labeledLabel type: {dts}"
        );
        assert!(dts.contains("  free(): void;"), "missing free type: {dts}");
        // Not a by-value const, and the function signatures reference the class.
        assert!(
            !dts.contains("export declare const Shape"),
            "rich enum must not be a const map in d.ts: {dts}"
        );
        assert!(
            dts.contains("scale(shape: Shape, factor: number): Shape"),
            "functions should reference the Shape class type: {dts}"
        );
    }

    #[test]
    fn wasm_rich_enum_readme() {
        let readme = render_wasm_readme(&rich_enum_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("##### `Shape`"), "{readme}");
        assert!(
            readme.contains("Rich (algebraic) enum"),
            "rich enum readme should call it out: {readme}"
        );
        assert!(
            readme.contains("| Variant | Tag | Fields |"),
            "rich enum readme should tabulate variants: {readme}"
        );
        assert!(
            readme.contains("`radius: f64`"),
            "rich enum readme should list field types: {readme}"
        );
    }

    /// A one-function async API for the Emscripten stub tests.
    fn async_api() -> Api {
        make_api(vec![Module {
            name: "math".into(),
            functions: vec![Function {
                name: "compute".into(),
                params: vec![Param {
                    name: "x".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
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
        }])
    }

    #[test]
    fn emscripten_loader_accepts_module_and_binds_exports() {
        let js = render_wasm_js_stub(
            &sample_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            true,
        );
        assert!(
            js.contains("export async function loadWeaveffiWasm(module) {"),
            "loader should accept the Emscripten module: {js}"
        );
        assert!(
            js.contains("const m = await Promise.resolve(module);"),
            "loader should accept the MODULARIZE factory promise too: {js}"
        );
        assert!(
            !js.contains("fetch(url)") && !js.contains("WebAssembly.instantiate"),
            "Emscripten mode must not instantiate the wasm itself: {js}"
        );
        // Runtime helpers and business symbols bind from the underscore-
        // prefixed Module properties, in quoted bracket notation.
        assert!(
            js.contains("weaveffi_alloc: m['_weaveffi_alloc'],"),
            "missing alloc binding: {js}"
        );
        assert!(
            js.contains("weaveffi_math_add: m['_weaveffi_math_add'],"),
            "missing business symbol binding: {js}"
        );
        assert!(
            js.contains("weaveffi_math_Point_get_x: m['_weaveffi_math_Point_get_x'],"),
            "missing struct getter binding: {js}"
        );
        assert!(
            js.contains("get memory() { return { buffer: m['HEAPU8'].buffer }; },"),
            "memory must be a live getter over HEAPU8: {js}"
        );
    }

    #[test]
    fn emscripten_body_stays_identical_to_standard_mode() {
        let standard = render_wasm_js_stub(
            &sample_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        let emscripten = render_wasm_js_stub(
            &sample_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            true,
        );
        // The adapter confines the divergence to the loader prologue; every
        // call site keeps the same dot access on the bound `wasm` object.
        assert!(
            emscripten.contains("wasm.weaveffi_math_add(a, b, _err)"),
            "call sites must not fork per mode: {emscripten}"
        );
        for helper in ["function _cstr(wasm, str)", "function _allocErr(wasm)"] {
            let body = |s: &str| {
                let start = s.find(helper).unwrap_or_else(|| panic!("missing {helper}"));
                s[start..s[start..].find("\n\n").map_or(s.len(), |e| start + e)].to_string()
            };
            assert_eq!(
                body(&standard),
                body(&emscripten),
                "shared helpers must be byte-identical between modes"
            );
        }
    }

    #[test]
    fn emscripten_binds_prefixed_runtime_helpers() {
        let js = render_wasm_js_stub(
            &sample_api(),
            DEFAULT_MODULE_NAME,
            "acme",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            true,
        );
        // The glue's hardcoded helper names bind to the producer's prefixed
        // exports, matching the runtime declarations in the generated header.
        assert!(
            js.contains("weaveffi_alloc: m['_acme_alloc'],"),
            "alloc must map to the prefixed export: {js}"
        );
        assert!(
            js.contains("weaveffi_error_clear: m['_acme_error_clear'],"),
            "error_clear must map to the prefixed export: {js}"
        );
    }

    #[test]
    fn emscripten_async_functions_become_throwing_stubs() {
        let js = render_wasm_js_stub(
            &async_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            true,
        );
        assert!(
            js.contains("async function 'compute' is not supported in Emscripten mode"),
            "async stub should throw with a clear message: {js}"
        );
        assert!(
            !js.contains("_registerTrampoline") && !js.contains("WebAssembly.Function"),
            "no trampoline machinery in Emscripten mode: {js}"
        );
        assert!(
            !js.contains("weaveffi_math_compute_async"),
            "the async launcher must not be bound or called: {js}"
        );
    }

    #[test]
    fn emscripten_dts_loader_signature_and_async_omission() {
        let dts = render_wasm_dts(
            &async_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            true,
        );
        assert!(
            dts.contains(
                "export function loadWeaveffiWasm(module: object | Promise<object>): \
                 Promise<WeaveffiWasmModule>;"
            ),
            "loader signature should take the Emscripten module: {dts}"
        );
        assert!(
            !dts.contains("compute("),
            "async stubs must be omitted from the d.ts: {dts}"
        );
        assert!(
            dts.contains("_raw: Record<string, unknown>;"),
            "_raw is the export-binding object in Emscripten mode: {dts}"
        );
    }

    #[test]
    fn emscripten_readme_documents_emcc_build() {
        let readme = render_wasm_readme(&async_api(), "weaveffi", "weaveffi.yml", true);
        assert!(
            readme.contains("emcc"),
            "readme should show an emcc invocation: {readme}"
        );
        assert!(
            readme.contains("EXPORTED_RUNTIME_METHODS=HEAPU8"),
            "readme should list the required runtime method export: {readme}"
        );
        assert!(
            readme.contains("Async functions are not supported in Emscripten mode"),
            "readme should call out the async gap: {readme}"
        );
    }

    #[test]
    fn dts_bytes_map_to_uint8array() {
        assert_eq!(ts_type_for(&TypeRef::Bytes), "Uint8Array");
        assert_eq!(ts_type_for(&TypeRef::BorrowedBytes), "Uint8Array");
    }
}
