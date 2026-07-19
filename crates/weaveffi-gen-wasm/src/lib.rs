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
use heck::{ToLowerCamelCase, ToShoutySnakeCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::abi::CType;
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{
    emit_doc as common_emit_doc, walk_modules, walk_modules_with_path, DocCommentStyle,
};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FieldBinding, FnBinding,
    InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding,
    RichEnumBinding, RichVariantBinding, StructBinding,
};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::plan::ErrorStrategy;
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
    /// Target an Emscripten build instead of a bare `wasm32-unknown-unknown`
    /// one. The loader then accepts a pre-initialized Emscripten `Module`
    /// object (or the promise returned by its `MODULARIZE` factory) instead
    /// of a `.wasm` URL, and binds the module's underscore-prefixed exports
    /// to the symbol names the glue calls. Async functions, callbacks, and
    /// listeners are not supported in this mode; each one becomes an explicit
    /// stub that throws at call time and is omitted from the TypeScript
    /// declarations.
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

    /// Every gated feature is supported. Callbacks and listeners share the
    /// async machinery: the loader installs one long-lived trampoline per
    /// callback typedef in the wasm function table and hands its index to the
    /// producer's `register_*` symbol, so `emit_*` dispatches straight back
    /// into JavaScript. Because `wasm32-unknown-unknown` is single-threaded,
    /// delivery is always synchronous: events fire only while a call into the
    /// module is on the stack (a producer that emits from a spawned thread
    /// cannot run on this target at all). Emscripten mode emits explicit
    /// throwing stubs for callbacks, listeners, and async functions instead;
    /// see [`WasmConfig::emscripten`].
    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn files(
        &self,
        api: &Api,
        model: &BindingModel,
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
                render_wasm_readme(api, model, prefix, input_basename, config.emscripten),
            ),
            OutputFile::new(
                wasm_dir.join("package.json"),
                render_wasm_package_json(&package, &js_filename, &dts_filename, input_basename),
            ),
            OutputFile::new(
                wasm_dir.join(&js_filename),
                render_wasm_js_stub(
                    api,
                    model,
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
                    model,
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
        | TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::Interface(_)
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
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::Interface(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Iterator(_)
            | TypeRef::Map(_, _) => "i64",
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => "i32, i32",
            // Optional strings are a nullable char*; optional scalars
            // (including `handle`) are boxed by pointer. Both are one i32.
            _ => "i32",
        },
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

fn wasm_type_note(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "8-bit signed mapped to i32",
        TypeRef::I16 => "16-bit signed mapped to i32",
        TypeRef::I32 => "native Wasm i32",
        TypeRef::U8 => "8-bit unsigned mapped to i32",
        TypeRef::U16 => "16-bit unsigned mapped to i32",
        TypeRef::U32 => "unsigned mapped to i32",
        TypeRef::I64 => "native Wasm i64",
        TypeRef::U64 => "unsigned mapped to i64",
        TypeRef::F32 => "native Wasm f32",
        TypeRef::F64 => "native Wasm f64",
        TypeRef::Bool => "0 = false, 1 = true",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            "ptr + len in linear memory"
        }
        TypeRef::TypedHandle(_) | TypeRef::Handle => "opaque pointer",
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::Interface(_) => {
            "opaque handle in linear memory"
        }
        TypeRef::Enum(_) => "variant discriminant",
        TypeRef::List(_) => "ptr + len in linear memory",
        TypeRef::Map(_, _) => "opaque handle in linear memory",
        TypeRef::Iterator(_) => "opaque iterator handle",
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::Interface(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Iterator(_)
            | TypeRef::Map(_, _) => "opaque handle, 0 = absent",
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => "nullable pointer, 0 = absent",
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => {
                "ptr + len, null ptr = absent"
            }
            _ => "boxed scalar pointer, 0 = absent",
        },
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
        TypeRef::Record(n) | TypeRef::RichEnum(n) => local_type_name(n).to_string(),
        TypeRef::Enum(n) => n.clone(),
        TypeRef::Optional(inner) => format!("{}?", type_display(inner)),
        TypeRef::List(inner) => format!("[{}]", type_display(inner)),
        TypeRef::Iterator(inner) => format!("iter<{}>", type_display(inner)),
        TypeRef::Map(k, v) => format!("{{{}:{}}}", type_display(k), type_display(v)),
        TypeRef::Interface(n) => local_type_name(n).to_string(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

fn render_wasm_readme(
    api: &Api,
    model: &BindingModel,
    prefix: &str,
    input_basename: &str,
    emscripten: bool,
) -> String {
    let mut out = render_prelude(CommentStyle::Xml, input_basename);
    out.push_str("# WeaveFFI Wasm (experimental)\n\n");
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
        if walk_modules(&api.modules).any(|m| !m.listeners.is_empty()) {
            out.push_str("## Callbacks and Listeners\n\n");
            out.push_str(
                "Callbacks and listeners are not supported in Emscripten mode: their \
                 trampolines rely on `WebAssembly.Function` and a growable \
                 `__indirect_function_table`, neither of which an Emscripten module \
                 exposes portably. Each register/unregister entry point is generated \
                 as an explicit stub that throws at call time and is omitted from the \
                 TypeScript declarations. Use the standard `wasm32-unknown-unknown` \
                 loader or a native target when you need them.\n\n",
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
    out.push_str("Wasm only supports numeric types natively (`i32`, `i64`, `f32`, `f64`). ");
    out.push_str("Complex types are encoded at the boundary as follows:\n\n");
    out.push_str("### Structs\n\n");
    out.push_str("Structs are passed as **opaque handles** (`i64` pointers into linear memory). ");
    out.push_str(
        "The host cannot inspect struct fields directly; use the generated accessor functions ",
    );
    out.push_str(&format!(
        "(`{prefix}_{{module}}_{{struct}}_get_{{field}}`) to read/write fields. ",
    ));
    out.push_str(
        "A returned struct handle is owned by the consumer; the JS wrapper's `free()` \
         releases it via the struct's `_destroy` symbol.\n\n",
    );
    out.push_str("### Enums\n\n");
    out.push_str("Enums are passed as **`i32` values** corresponding to the variant's integer discriminant.\n\n");
    out.push_str("### Optionals\n\n");
    out.push_str("Optional values use **`0` / `null`** to represent the absent case. ");
    out.push_str("Scalar optionals are boxed by pointer (a null pointer signals absence); ");
    out.push_str("handle-typed and string optionals pass a null pointer (`0`) directly.\n\n");
    out.push_str("### Lists\n\n");
    out.push_str("Lists are passed as a **pointer + length** pair (`i32` pointer, `i32` length) ");
    out.push_str("referencing a contiguous region in linear memory. The caller is responsible ");
    out.push_str("for allocating and freeing the backing memory.\n\n");
    out.push_str("### Iterators\n\n");
    out.push_str("`iter<T>` functions return a **lazy JS iterator** (typed ");
    out.push_str("`IterableIterator<T>`): each `next()` issues exactly one producer call, so ");
    out.push_str("iteration streams in constant memory. The producer handle is destroyed ");
    out.push_str("exactly once, on exhaustion or via `return()` when iteration stops early ");
    out.push_str("(a `for...of` loop calls `return()` automatically on `break` or `throw`). ");
    out.push_str("Abandoning an iterator without exhausting or closing it leaks the handle.\n");
    if !emscripten && walk_modules(&api.modules).any(|m| !m.listeners.is_empty()) {
        out.push_str("\n### Callbacks and Listeners\n\n");
        out.push_str(
            "Each listener surfaces as a `register.../unregister...` pair. `register` \
             takes a plain JS function and returns a numeric subscription id; \
             `unregister` takes that id and stops delivery. Delivery is **synchronous \
             and same-thread**: `wasm32-unknown-unknown` is single-threaded, so events \
             fire only while a call into the module is on the stack (for example, a \
             producer function that emits during its own execution). A producer that \
             emits from a spawned thread cannot run on this target at all.\n\n",
        );
        out.push_str(
            "Callback arguments are **borrowed for the duration of the callback**: \
             strings and byte buffers are copied into JS values before your function \
             runs, but struct, rich-enum, and interface arguments wrap producer-owned \
             memory. Read what you need inside the callback and do not retain the \
             wrapper or call `free()` on it.\n",
        );
    }
    out.push_str("\n### Error Handling\n\n");
    out.push_str("The generated JS wrappers automatically handle errors by passing an error\n");
    out.push_str("pointer as the last argument to each Wasm function. Your Wasm module must\n");
    out.push_str("export the following functions:\n\n");
    out.push_str("- `weaveffi_alloc(size: i32) -> i32`: allocate `size` bytes in linear memory\n");
    out.push_str("- `weaveffi_dealloc(ptr: i32, size: i32)`: release a `weaveffi_alloc` block\n");
    out.push_str("- `weaveffi_error_clear(err_ptr: i32)`: clear and free error resources\n");
    out.push_str("- `weaveffi_free_string(ptr: i32)`: free a producer-returned C string\n");
    out.push_str("- `weaveffi_free_bytes(ptr: i32, len: i32)`: free a producer-returned buffer\n");
    out.push_str("\nWrappers of functions declared `throws` raise the declaring module's typed\n");
    out.push_str("error class (a `WeaveFFIError` subclass with a per-code subclass, such as\n");
    out.push_str("`KeyNotFound`); every other wrapper raises the generic `WeaveFFIError` only\n");
    out.push_str("for producer panics and marshalling failures.\n");

    if !api.modules.is_empty() {
        render_api_reference(&mut out, api, model);
    }

    out.push('\n');
    out.push_str(&render_trailer(CommentStyle::Xml, "README.md"));
    out
}

fn render_api_reference(out: &mut String, api: &Api, model: &BindingModel) {
    let by_path: HashMap<&str, &ModuleBinding> =
        model.modules.iter().map(|m| (m.path.as_str(), m)).collect();
    out.push_str("\n## API Reference\n");
    for module in &api.modules {
        out.push_str(&format!("\n### Module: `{}`\n", module.name));
        let mb = by_path[module.name.as_str()];

        if let Some(eb) = mb.error.as_ref().filter(|eb| eb.declared_here) {
            render_error_ref(out, eb);
        }

        if !mb.functions.is_empty() {
            out.push_str("\n#### Functions\n");
            for f in &mb.functions {
                render_function_ref(out, f);
            }
        }

        if !mb.interfaces.is_empty() {
            out.push_str("\n#### Interfaces\n");
            for i in &mb.interfaces {
                render_interface_ref(out, i);
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

/// Document a module's declared error domain: the JS class hierarchy it
/// generates and the stable ABI code of each subclass.
fn render_error_ref(out: &mut String, eb: &ErrorBinding) {
    out.push_str(&format!("\n#### Error Domain: `{}`\n\n", eb.type_name));
    out.push_str(&format!(
        "Throwing wrappers in this module raise `{}` (a `{ERROR_BRAND}` subclass); \
         each code below is its own subclass carrying the stable `code`.\n\n",
        eb.type_name
    ));
    out.push_str("| Class | Code | Default Message |\n");
    out.push_str("|-------|------|-----------------|\n");
    for c in &eb.codes {
        out.push_str(&format!(
            "| `{}` | `{}` | {} |\n",
            js_code_class_name(&c.name),
            c.value,
            c.message
        ));
    }
}

/// Document one interface: an opaque handle wrapped by a JS class, with the
/// member entry points listed at the ABI level like free functions.
fn render_interface_ref(out: &mut String, i: &InterfaceBinding) {
    out.push_str(&format!("\n##### `{}`\n\n", i.name));
    if let Some(doc) = &i.doc {
        out.push_str(doc);
        out.push_str("\n\n");
    }
    out.push_str(
        "Passed as an **opaque handle** (`i64`), wrapped by a JS class. Constructors \
         return an owned handle; methods pass the handle as the implicit leading `self` \
         argument; `free()` releases the handle via the destroy symbol.\n",
    );
    for f in i
        .constructors
        .iter()
        .chain(i.methods.iter())
        .chain(i.statics.iter())
    {
        render_function_ref(out, f);
    }
    out.push_str(&format!(
        "\n##### `{}`\n\nReleases the object reference. Called by the wrapper's `free()`.\n",
        i.destroy_symbol
    ));
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

    out.push_str("| Param | API Type | Wasm | Notes |\n");
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
        out.push_str("| Accessor | Wasm Return |\n");
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

/// True when an async return carries a list or map with string elements,
/// whose borrowed buffers the completion callback reads without freeing.
fn async_ret_has_string_buffers(ret: Option<&TypeRef>) -> bool {
    match ret {
        Some(TypeRef::List(inner)) => is_string_type(inner),
        Some(TypeRef::Map(k, v)) => is_string_type(k) || is_string_type(v),
        Some(TypeRef::Optional(inner)) => async_ret_has_string_buffers(Some(inner)),
        _ => false,
    }
}

/// Whether `ty` or any type nested inside it (optional payloads, list and
/// iterator elements, map keys/values) satisfies `pred`.
fn typeref_deep_any(ty: &TypeRef, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
    if pred(ty) {
        return true;
    }
    match ty {
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            typeref_deep_any(inner, pred)
        }
        TypeRef::Map(k, v) => typeref_deep_any(k, pred) || typeref_deep_any(v, pred),
        _ => false,
    }
}

/// Visit every boundary-crossing type in `api` (function params + returns and
/// struct field types), recursing into composite types, and return whether any
/// satisfies `pred`.
fn api_deep_any(api: &Api, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
    fn deep(ty: &TypeRef, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
        typeref_deep_any(ty, pred)
    }
    fn fn_any(f: &weaveffi_ir::ir::Function, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
        f.params.iter().any(|p| deep(&p.ty, pred))
            || f.returns.as_ref().is_some_and(|r| deep(r, pred))
    }
    fn module_any(m: &Module, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
        m.functions.iter().any(|f| fn_any(f, pred))
            // Interface members marshal exactly like free functions.
            || m.interfaces.iter().any(|i| {
                i.constructors
                    .iter()
                    .chain(i.methods.iter())
                    .chain(i.statics.iter())
                    .any(|f| fn_any(f, pred))
            })
            || m
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

/// Stage one idiomatic input `value` of type `ty` into the Wasm ABI.
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
        TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => {
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
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Interface(_) => {
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
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
        TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => {
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

/// How string elements read out of a producer array are released, per the
/// [`weaveffi_core::plan`] ownership contracts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ElemOwnership {
    /// Synchronous-return elements: the consumer owns each string and frees it
    /// with `{prefix}_free_string` after copying.
    Owned,
    /// Async-callback elements: string buffers are borrowed for the callback's
    /// duration, so they are copied out but never freed here.
    Borrowed,
}

/// Emit `const {target} = ...;` building a JS array of `inner` elements from
/// the C array at `base` (`len` elements). String elements are freed or left
/// alone per `ownership`; record/rich-enum and interface elements are owned
/// object pointers adopted by their wrapper classes either way. Assumes
/// `wasm` in scope.
fn emit_read_list_into(
    out: &mut String,
    indent: &str,
    inner: &TypeRef,
    base: &str,
    len: &str,
    target: &str,
    ownership: ElemOwnership,
) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => match ownership {
            ElemOwnership::Owned => {
                w.line(format!(
                    "const {target} = _takeStrArray(wasm, {base}, {len});"
                ));
            }
            ElemOwnership::Borrowed => {
                w.line(format!(
                    "const {target} = _readStrArray(wasm, {base}, {len});"
                ));
            }
        },
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
            let cls = local_type_name(name);
            w.line(format!("const {target} = [];"));
            w.block("{", "}", |w| {
                w.line("const dv = new DataView(wasm.memory.buffer);");
                w.line(format!(
                    "for (let i = 0; i < {len}; i++) {target}.push(new {cls}(wasm, dv.getInt32({base} + i * 4, true)));"
                ));
            });
        }
        TypeRef::Interface(name) => {
            let cls = local_type_name(name);
            w.line(format!("const {target} = [];"));
            w.block("{", "}", |w| {
                w.line("const dv = new DataView(wasm.memory.buffer);");
                w.line(format!(
                    "for (let i = 0; i < {len}; i++) {target}.push({cls}._wrap(dv.getInt32({base} + i * 4, true)));"
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
/// parallel key/value C arrays, with string elements released per
/// `ownership`. Assumes `wasm` in scope.
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
    ownership: ElemOwnership,
) {
    emit_read_list_into(out, indent, k, ka, len, &format!("{target}_k"), ownership);
    emit_read_list_into(out, indent, v, va, len, &format!("{target}_v"), ownership);
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.line(format!("const {target} = {{}};"));
    w.line(format!(
        "for (let i = 0; i < {len}; i++) {target}[{target}_k[i]] = {target}_v[i];"
    ));
    out.push_str(&w.finish());
}

/// Emit the body that invokes `symbol` with the already-staged `in_args`, runs
/// `cleanup`, routes the error slot through the `checker` helper (when
/// `Some`), and decodes/returns the idiomatic value for `ret`. Assumes `wasm`
/// is in scope at `indent`.
fn emit_return_decode(
    out: &mut String,
    indent: &str,
    ret: Option<&TypeRef>,
    symbol: &str,
    in_args: &[String],
    cleanup: &[String],
    checker: Option<&str>,
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
    if checker.is_some() {
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
    if let Some(checker) = checker {
        w.line(format!("{checker}(wasm, _err);"));
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
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
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
            let stride = wasm_stride(inner);
            w.line("const _dv = new DataView(wasm.memory.buffer);");
            w.line("const _len = _dv.getUint32(_lp, true);");
            w.line("wasm.weaveffi_dealloc(_lp, 4);");
            let mut tmp = String::new();
            emit_read_list_into(
                &mut tmp,
                indent,
                inner,
                r,
                "_len",
                "_out",
                ElemOwnership::Owned,
            );
            w.raw(tmp);
            // The consumer owns the returned array buffer; release it after
            // copying the elements out (each element already freed per its
            // own plan above).
            w.line(format!(
                "if ({r} !== 0 && _len !== 0) wasm.weaveffi_free_bytes({r}, _len * {stride});"
            ));
            w.line("return _out;");
        }
        TypeRef::Map(k, v) => {
            let k_stride = wasm_stride(k);
            let v_stride = wasm_stride(v);
            w.line("const _dv = new DataView(wasm.memory.buffer);");
            w.line("const _ka = _dv.getUint32(_kp, true);");
            w.line("const _va = _dv.getUint32(_vp, true);");
            w.line("const _len = _dv.getUint32(_lp, true);");
            w.line("wasm.weaveffi_dealloc(_kp, 4);");
            w.line("wasm.weaveffi_dealloc(_vp, 4);");
            w.line("wasm.weaveffi_dealloc(_lp, 4);");
            let mut tmp = String::new();
            emit_read_map_into(
                &mut tmp,
                indent,
                k,
                v,
                "_ka",
                "_va",
                "_len",
                "_out",
                ElemOwnership::Owned,
            );
            w.raw(tmp);
            // The consumer owns both parallel arrays; release them after
            // copying the entries out.
            w.line(format!(
                "if (_ka !== 0 && _len !== 0) wasm.weaveffi_free_bytes(_ka, _len * {k_stride});"
            ));
            w.line(format!(
                "if (_va !== 0 && _len !== 0) wasm.weaveffi_free_bytes(_va, _len * {v_stride});"
            ));
            w.line("return _out;");
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
                let cls = local_type_name(name);
                w.line(format!("return {r} === 0 ? null : new {cls}(wasm, {r});"));
            }
            TypeRef::Interface(name) => {
                let cls = local_type_name(name);
                w.line(format!("return {r} === 0 ? null : {cls}._wrap({r});"));
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
                // Boxed optional scalar: dereference the box, then release it
                // (the consumer owns the box per the return-free plan).
                let width = scalar_width(scalar);
                let getter = wasm_read_scalar_elem(scalar, "_dv", r, "0")
                    .replace(&format!("{r} + 0 * {}", wasm_stride(scalar)), r);
                w.line(format!("if ({r} === 0) return null;"));
                w.line("const _dv = new DataView(wasm.memory.buffer);");
                w.line(format!("const _v = {getter};"));
                w.line(format!("wasm.weaveffi_free_bytes({r}, {width});"));
                w.line("return _v;");
            }
        },
        TypeRef::Iterator(_) => unreachable!("iterator returns handled separately"),
        TypeRef::Interface(name) => {
            let cls = local_type_name(name);
            w.line(format!("return {cls}._wrap({r});"));
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
        | TypeRef::F64 => "number".into(),
        TypeRef::Bool => "boolean".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        // Bytes cross the boundary as plain `Uint8Array` copies; the Node-only
        // `Buffer` type does not exist in browsers and is never returned here.
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Uint8Array".into(),
        // Every 64-bit integer crosses the JS boundary as a BigInt: wasm i64
        // results arrive as BigInt and i64 arguments are BigInt-coerced.
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle => "bigint".into(),
        // Records, rich enums, plain enums, typed handles, and interfaces
        // surface as bare local TS names; a cross-module reference (resolved
        // to e.g. `kv.Store`) must name the local `Store`, not the qualified
        // IR name which is undeclared here.
        TypeRef::TypedHandle(name)
        | TypeRef::Enum(name)
        | TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::Interface(name) => local_type_name(name).to_string(),
        TypeRef::Optional(inner) => format!("{} | null", ts_type_for(inner)),
        TypeRef::List(inner) => {
            let inner_ts = ts_type_for(inner);
            if matches!(inner.as_ref(), TypeRef::Optional(_)) {
                format!("({inner_ts})[]")
            } else {
                format!("{inner_ts}[]")
            }
        }
        // `iter<T>` streams lazily; the wrapper is a JS iterator, never a
        // drained array.
        TypeRef::Iterator(inner) => {
            let t = ts_type_for(inner);
            format!("IterableIterator<{t}>")
        }
        TypeRef::Map(k, v) => format!("Record<{}, {}>", ts_type_for(k), ts_type_for(v)),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// Emits a JSDoc comment at `indent`. Single-line docs collapse to
/// `/** text */`; multi-line docs expand to a block with ` * ` prefixed lines.
fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Javadoc);
}

/// Emits a JSDoc block for a function: function doc, `@param name desc` for
/// each documented parameter (named as the camelCase JS parameter), and an
/// optional trailing tag list.
fn emit_fn_doc(
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
                w.line(format!(" * @param {} {}", js_param_name(p), first));
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

// ── Naming and error-surface policy ──

/// The lowerCamelCase JS name a callable is exposed under (`list_keys` becomes
/// `listKeys`). Functions are namespaced by module object, so exported names
/// never carry a module prefix in the first place.
fn js_fn_name(f: &FnBinding) -> String {
    f.name.to_lower_camel_case()
}

/// The camelCase JS spelling of one parameter (`ttl_seconds` becomes
/// `ttlSeconds`).
fn js_param_name(p: &ParamBinding) -> String {
    p.name.to_lower_camel_case()
}

/// The JS class name for one error code: plain PascalCase with no forced
/// suffix (`KeyNotFound`, not `KeyNotFoundError`). Code names are validated
/// to be globally unique across domains, so the flat name cannot collide.
fn js_code_class_name(name: &str) -> String {
    weaveffi_core::errors::pascal(name)
}

/// `_{typeName}From` (lowerCamel): builds the domain error matching an ABI
/// code, e.g. `_kvErrorFrom`.
fn js_error_factory_name(eb: &ErrorBinding) -> String {
    format!("_{}From", eb.type_name.to_lower_camel_case())
}

/// `_check{TypeName}`: throws the domain error for a non-zero out-err slot,
/// e.g. `_checkKvError`.
fn js_error_checker_name(eb: &ErrorBinding) -> String {
    format!("_check{}", eb.type_name)
}

/// The error-check helper a callable's out-err slot routes through, per its
/// [`ErrorStrategy`]: the module domain's typed checker for
/// [`ErrorStrategy::Throws`], the generic `_checkErr` (plain `WeaveFFIError`;
/// panics and marshalling failures only) for [`ErrorStrategy::Trap`].
fn js_checker_name(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match (f.error_strategy(), error) {
        (ErrorStrategy::Throws, Some(eb)) => js_error_checker_name(eb),
        _ => "_checkErr".to_string(),
    }
}

/// The rejection factory a throwing async callable stores in its context so
/// the completion callback maps domain codes to the typed error, or `None`
/// for [`ErrorStrategy::Trap`] callables (which reject with the generic
/// brand error).
fn js_err_factory(f: &FnBinding, error: Option<&ErrorBinding>) -> Option<String> {
    match (f.error_strategy(), error) {
        (ErrorStrategy::Throws, Some(eb)) => Some(js_error_factory_name(eb)),
        _ => None,
    }
}

/// Escape a string for embedding in a double-quoted JS literal.
fn js_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// How a generated JS callable is declared: as a property of a module object
/// literal (`name() {...},`), as an instance member of an interface class
/// (`name() {...}`), or as a static member (`static name() {...}`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum JsDecl {
    /// Object-literal property (module objects); comma-terminated.
    Object,
    /// Class instance method; no terminator comma.
    Method,
    /// Class static method; no terminator comma.
    Static,
}

impl JsDecl {
    /// The declaration keyword prefix (`static ` for statics).
    fn prefix(self) -> &'static str {
        match self {
            JsDecl::Static => "static ",
            _ => "",
        }
    }

    /// The block terminator (object-literal members carry a trailing comma).
    fn close(self) -> &'static str {
        match self {
            JsDecl::Object => "},",
            _ => "}",
        }
    }
}

fn render_wasm_dts(
    api: &Api,
    model: &BindingModel,
    module_name: &str,
    input_basename: &str,
    filename: &str,
    emscripten: bool,
) -> String {
    let pascal_name = module_name.to_upper_camel_case();
    let interface_name = format!("{pascal_name}Module");
    let load_fn = format!("load{pascal_name}");
    let by_path: HashMap<&str, &ModuleBinding> =
        model.modules.iter().map(|m| (m.path.as_str(), m)).collect();
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    out.push_str("// Generated TypeScript declarations for WeaveFFI Wasm bindings\n\n");

    emit_dts_error_classes(&mut out, model);

    for (m, path) in walk_modules_with_path(&api.modules) {
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
            out.push_str("  /** Releases the producer-owned handle exactly once. */\n");
            out.push_str("  free(): void;\n");
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

        if let Some(mb) = by_path.get(path.as_str()) {
            for i in &mb.interfaces {
                emit_dts_interface_class(&mut out, mb, i, emscripten);
            }
        }
    }

    out.push_str(&format!("export interface {interface_name} {{\n"));
    if model
        .modules
        .iter()
        .any(|m| !m.functions.is_empty() || !m.interfaces.is_empty())
    {
        // In Emscripten mode `_raw` is the loader's export-binding object, a
        // plain record, not a `WebAssembly.Exports`.
        if emscripten {
            out.push_str("  _raw: Record<string, unknown>;\n");
        } else {
            out.push_str("  _raw: WebAssembly.Exports;\n");
        }
        for module in &api.modules {
            render_dts_module_interface(&mut out, module, &module.name, &by_path, "  ", emscripten);
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

/// The TypeScript parameter list for one callable: camelCase names typed by
/// [`ts_type_for`].
fn dts_params(f: &FnBinding) -> String {
    f.params
        .iter()
        .map(|p| format!("{}: {}", js_param_name(p), ts_type_for(&p.ty)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The TypeScript return annotation for one callable (`Promise<...>` when
/// async, `void` for no return).
fn dts_ret(f: &FnBinding) -> String {
    let base = f
        .ret
        .as_ref()
        .map(ts_type_for)
        .unwrap_or_else(|| "void".into());
    if f.is_async {
        format!("Promise<{base}>")
    } else {
        base
    }
}

/// The JSDoc tag list for one callable: `@deprecated` first when present, a
/// streaming note for iterator-returning callables, then the `@throws` tag
/// matching the throws split (the typed domain error for throwing callables,
/// the generic brand error otherwise).
fn dts_fn_tags(f: &FnBinding, error: Option<&ErrorBinding>) -> Vec<String> {
    let mut tags = Vec::new();
    if let Some(msg) = &f.deprecated {
        tags.push(format!("@deprecated {msg}"));
    }
    if matches!(f.shape, CallShape::Iterator(_)) {
        tags.push(
            "@returns A lazy iterator: one producer step per `next()` call. Exhaust it or \
             call `return()` to release the producer handle (a `for...of` loop does both \
             automatically); an abandoned iterator leaks the handle."
                .to_string(),
        );
    }
    match error {
        Some(eb) if f.throws => tags.push(format!(
            "@throws {{{}}} on a domain error code",
            eb.type_name
        )),
        _ => tags.push(format!(
            "@throws {{{ERROR_BRAND}}} if the native call fails"
        )),
    }
    tags
}

fn render_dts_module_interface(
    out: &mut String,
    m: &Module,
    module_path: &str,
    by_path: &HashMap<&str, &ModuleBinding>,
    indent: &str,
    emscripten: bool,
) {
    fn tree_has_content(
        m: &Module,
        path: &str,
        by_path: &HashMap<&str, &ModuleBinding>,
        include_listeners: bool,
    ) -> bool {
        let here = by_path.get(path).is_some_and(|mb| {
            !mb.functions.is_empty()
                || !mb.interfaces.is_empty()
                || (include_listeners && !mb.listeners.is_empty())
        });
        here || m.modules.iter().any(|sub| {
            tree_has_content(
                sub,
                &format!("{path}_{}", sub.name),
                by_path,
                include_listeners,
            )
        })
    }
    if !tree_has_content(m, module_path, by_path, !emscripten) {
        return;
    }
    let mb = by_path[module_path];
    let error = mb.error.as_ref();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(format!("{}: {{", m.name), "};", |w| {
        let inner = w.indent_str();
        for f in &mb.functions {
            // Async functions are throwing stubs in Emscripten mode; omitting
            // them here makes the gap a compile-time error for TS consumers.
            if emscripten && f.is_async {
                continue;
            }
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &f.doc, &f.params, &inner, &dts_fn_tags(f, error));
            w.raw(doc);
            w.line(format!(
                "{}({}): {};",
                js_fn_name(f),
                dts_params(f),
                dts_ret(f)
            ));
        }
        // Listeners are throwing stubs in Emscripten mode; omitting them here
        // makes the gap a compile-time error for TS consumers.
        if !emscripten {
            for l in &mb.listeners {
                let mut tmp = String::new();
                render_dts_listener(&mut tmp, mb, l, &inner);
                w.raw(tmp);
            }
        }
        // The module object carries the interface class itself, so statics,
        // factories, and `new` are reachable as `api.kv.Store...`.
        for i in &mb.interfaces {
            w.line(format!("{}: typeof {};", i.name, i.name));
        }
        for sub in &m.modules {
            let sub_path = format!("{module_path}_{}", sub.name);
            let mut tmp = String::new();
            render_dts_module_interface(&mut tmp, sub, &sub_path, by_path, &inner, emscripten);
            w.raw(tmp);
        }
    });
    out.push_str(&w.finish());
}

/// Emit the TypeScript declarations for one listener's register/unregister
/// pair. The callback parameter types come from the referenced callback
/// typedef; the subscription id is a plain `number` (the loader keys
/// subscriptions by its own context id, so the producer's `uint64_t` id never
/// reaches the public surface).
fn render_dts_listener(out: &mut String, mb: &ModuleBinding, l: &ListenerBinding, indent: &str) {
    let Some(cb) = mb.callback(&l.event_callback) else {
        // Validation guarantees the referenced callback exists in-module.
        unreachable!("listener '{}' references unknown callback", l.name);
    };
    let register_name = format!("register_{}", l.name).to_lower_camel_case();
    let unregister_name = format!("unregister_{}", l.name).to_lower_camel_case();
    let cb_params: Vec<String> = cb
        .params
        .iter()
        .map(|p| format!("{}: {}", js_param_name(p), ts_type_for(&p.ty)))
        .collect();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let register_doc = match &l.doc {
        Some(d) => format!(
            "{}\n\n@returns A subscription id for `{unregister_name}()`.",
            d.trim()
        ),
        None => format!(
            "Register a listener for the `{}` callback.\n\n@returns A \
             subscription id for `{unregister_name}()`.",
            cb.name
        ),
    };
    let mut doc = String::new();
    emit_doc(&mut doc, &Some(register_doc), indent);
    w.raw(doc);
    w.line(format!(
        "{register_name}(callback: ({}) => void): number;",
        cb_params.join(", ")
    ));
    let mut doc = String::new();
    emit_doc(
        &mut doc,
        &Some(format!(
            "Unregister a listener previously registered with `{register_name}()`."
        )),
        indent,
    );
    w.raw(doc);
    w.line(format!("{unregister_name}(id: number): void;"));
    out.push_str(&w.finish());
}

/// Emit the TypeScript declarations for the error surface: the generic brand
/// error, then one domain class per declaring module with its per-code
/// subclasses (each carrying a literal-typed `CODE`) and the static aliases
/// hung on the domain class.
fn emit_dts_error_classes(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::two_space();
    w.line("/** Base error for WeaveFFI failures: domain errors extend it, and it is");
    w.line(" * thrown directly for unknown codes, marshalling failures, and producer");
    w.line(" * panics. Carries the stable ABI `code`. */");
    w.block(
        format!("export declare class {ERROR_BRAND} extends Error {{"),
        "}",
        |w| {
            w.line("constructor(code: number, message?: string);");
            w.line("code: number;");
        },
    );
    w.blank();
    for m in &model.modules {
        let Some(eb) = m.error.as_ref().filter(|eb| eb.declared_here) else {
            continue;
        };
        let domain = &eb.type_name;
        w.line(format!(
            "/** Base error for the `{}` module's error domain. */",
            m.path
        ));
        w.block(
            format!("export declare class {domain} extends {ERROR_BRAND} {{"),
            "}",
            |w| {
                for c in &eb.codes {
                    let class = js_code_class_name(&c.name);
                    w.line(format!("static readonly {class}: typeof {class};"));
                }
            },
        );
        w.blank();
        for c in &eb.codes {
            let class = js_code_class_name(&c.name);
            let doc = c
                .doc
                .clone()
                .filter(|d| !d.trim().is_empty())
                .or_else(|| Some(c.message.clone()));
            w.doc(&doc, DocCommentStyle::Javadoc);
            w.block(
                format!("export declare class {class} extends {domain} {{"),
                "}",
                |w| {
                    w.line("constructor(message?: string);");
                    w.line(format!("static readonly CODE: {};", c.value));
                },
            );
            w.blank();
        }
    }
    out.push_str(&w.finish());
}

/// Emit the TypeScript declaration for an interface: an ambient class whose
/// runtime binding is reached through the module object (`api.kv.Store`). The
/// canonical `new` constructor declares `constructor`; other constructors and
/// statics are static members; async members are omitted in Emscripten mode
/// (they are throwing stubs at runtime).
fn emit_dts_interface_class(
    out: &mut String,
    mb: &ModuleBinding,
    i: &InterfaceBinding,
    emscripten: bool,
) {
    let error = mb.error.as_ref();
    let mut w = CodeWriter::two_space();
    w.doc(&i.doc, DocCommentStyle::Javadoc);
    w.block(format!("export declare class {} {{", i.name), "}", |w| {
        let inner = w.indent_str();
        if let Some(c) = i.constructors.iter().find(|c| c.name == "new") {
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &c.doc, &c.params, &inner, &dts_fn_tags(c, error));
            w.raw(doc);
            w.line(format!("constructor({});", dts_params(c)));
        }
        for c in i.constructors.iter().filter(|c| c.name != "new") {
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &c.doc, &c.params, &inner, &dts_fn_tags(c, error));
            w.raw(doc);
            w.line(format!(
                "static {}({}): {};",
                js_fn_name(c),
                dts_params(c),
                dts_ret(c)
            ));
        }
        for f in &i.methods {
            if emscripten && f.is_async {
                continue;
            }
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &f.doc, &f.params, &inner, &dts_fn_tags(f, error));
            w.raw(doc);
            w.line(format!(
                "{}({}): {};",
                js_fn_name(f),
                dts_params(f),
                dts_ret(f)
            ));
        }
        for f in &i.statics {
            if emscripten && f.is_async {
                continue;
            }
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &f.doc, &f.params, &inner, &dts_fn_tags(f, error));
            w.raw(doc);
            w.line(format!(
                "static {}({}): {};",
                js_fn_name(f),
                dts_params(f),
                dts_ret(f)
            ));
        }
        w.line("/** Releases the producer-owned handle exactly once. */");
        w.line("free(): void;");
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the module-scope error classes: the generic `WeaveFFIError` base
/// (unknown codes, marshalling failures, panics), then one domain class per
/// declaring module (`class KvError extends WeaveFFIError`) with one subclass
/// per code carrying its stable `CODE` and default message. Each code class
/// is also aliased onto its domain class (`KvError.KeyNotFound`), which stays
/// unambiguous even if two domains were to share a code spelling.
fn emit_js_error_classes(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::two_space();
    w.line("/** Base error for WeaveFFI failures: domain errors extend it, and it is");
    w.line(" * thrown directly for unknown codes, marshalling failures, and producer");
    w.line(" * panics. Carries the stable ABI `code`. */");
    w.block(format!("export class {ERROR_BRAND} extends Error {{"), "}", |w| {
        w.block("constructor(code, message) {", "}", |w| {
            w.line("super(message ? `WeaveFFI error ${code}: ${message}` : `WeaveFFI error ${code}`);");
            w.line("this.name = new.target.name;");
            w.line("this.code = code;");
        });
    });
    w.blank();

    for m in &model.modules {
        let Some(eb) = m.error.as_ref().filter(|eb| eb.declared_here) else {
            continue;
        };
        let domain = &eb.type_name;
        w.line(format!(
            "/** Base error for the `{}` module's error domain. */",
            m.path
        ));
        w.line(format!("export class {domain} extends {ERROR_BRAND} {{}}"));
        w.blank();
        for c in &eb.codes {
            let class = js_code_class_name(&c.name);
            let message = js_str_literal(&c.message);
            let doc = c
                .doc
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .unwrap_or(&c.message);
            for line in doc.lines() {
                w.line(format!("// {line}"));
            }
            w.block(
                format!("export class {class} extends {domain} {{"),
                "}",
                |w| {
                    w.block(
                        format!("constructor(message = \"{message}\") {{"),
                        "}",
                        |w| {
                            w.line(format!("super({}, message);", c.value));
                        },
                    );
                },
            );
            w.line(format!("{class}.CODE = {};", c.value));
            w.line(format!("{domain}.{class} = {class};"));
            w.blank();
        }

        let table = js_error_code_table_name(eb);
        let factory = js_error_factory_name(eb);
        w.block(format!("const {table} = Object.freeze({{"), "});", |w| {
            for c in &eb.codes {
                w.line(format!("{}: {},", c.value, js_code_class_name(&c.name)));
            }
        });
        w.blank();
        w.line(format!(
            "// Build the {domain} subclass matching `code`, or a generic"
        ));
        w.line(format!(
            "// {ERROR_BRAND} for codes outside the domain (panics, marshalling)."
        ));
        w.block(format!("function {factory}(code, message) {{"), "}", |w| {
            w.line(format!("const _cls = {table}[code];"));
            w.line(format!(
                "if (!_cls) return new {ERROR_BRAND}(code, message);"
            ));
            w.line("return message ? new _cls(message) : new _cls();");
        });
        w.blank();
    }
    out.push_str(&w.finish());
}

/// `_{TYPE_NAME}_CODES`: the frozen code-to-class table for one domain.
fn js_error_code_table_name(eb: &ErrorBinding) -> String {
    format!("_{}_CODES", eb.type_name.to_shouty_snake_case())
}

/// Emit one `_check{Domain}(wasm, errPtr)` helper per declaring module:
/// identical to the generic `_checkErr` except the thrown error is built by
/// the domain's factory, so domain codes surface as their typed subclasses.
fn emit_js_error_checkers(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::two_space();
    for m in &model.modules {
        let Some(eb) = m.error.as_ref().filter(|eb| eb.declared_here) else {
            continue;
        };
        let checker = js_error_checker_name(eb);
        let factory = js_error_factory_name(eb);
        w.line(format!(
            "// Throw the `{}` domain error (and free the slot) if the error slot",
            eb.type_name
        ));
        w.line("// carries a non-zero code.");
        w.block(format!("function {checker}(wasm, errPtr) {{"), "}", |w| {
            w.line("const dv = new DataView(wasm.memory.buffer);");
            w.line("const code = dv.getInt32(errPtr, true);");
            w.block("if (code !== 0) {", "}", |w| {
                w.line("const msgPtr = dv.getUint32(errPtr + 4, true);");
                w.line("const msg = _readCStr(wasm, msgPtr) || '';");
                w.line("wasm.weaveffi_error_clear(errPtr);");
                w.line("wasm.weaveffi_dealloc(errPtr, 8);");
                w.line(format!("throw {factory}(code, msg);"));
            });
        });
        w.blank();
    }
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
            push_unique(&mut syms, &s.destroy_symbol);
            if let Some(b) = &s.builder {
                push_unique(&mut syms, &b.new_symbol);
                for (_field, setter) in &b.setters {
                    push_unique(&mut syms, setter);
                }
                push_unique(&mut syms, &b.build_symbol);
                push_unique(&mut syms, &b.destroy_symbol);
            }
        }
        for f in m.callables() {
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
        for i in &m.interfaces {
            push_unique(&mut syms, &i.destroy_symbol);
        }
    }
    syms
}

fn render_wasm_js_stub(
    api: &Api,
    model: &BindingModel,
    module_name: &str,
    prefix: &str,
    input_basename: &str,
    filename: &str,
    emscripten: bool,
) -> String {
    let pascal_name = module_name.to_upper_camel_case();
    let load_fn = format!("load{pascal_name}");
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let by_path: HashMap<&str, &ModuleBinding> =
        model.modules.iter().map(|m| (m.path.as_str(), m)).collect();

    // Interface members marshal like free functions, so every callable counts.
    let has_functions = model.modules.iter().any(|m| m.callables().next().is_some());
    // In Emscripten mode async functions are throwing stubs, so none of the
    // trampoline machinery (or its helpers) is emitted.
    let has_async = !emscripten
        && model
            .modules
            .iter()
            .flat_map(ModuleBinding::callables)
            .any(|f| f.is_async);
    // Listeners get real dispatch only in the standard loader; Emscripten
    // mode emits throwing stubs, so no trampolines or registry there either.
    let listener_cbs: Vec<&CallbackBinding> = if emscripten {
        Vec::new()
    } else {
        collect_listener_callbacks(model)
    };
    let has_listeners = !listener_cbs.is_empty();
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
    // Listener callback arguments arrive as borrowed C strings read through
    // the same helper, so their parameter types count too.
    let needs_strings = needs_err
        || api_deep_any(api, &|t| is_string_type(t))
        || listener_cbs.iter().any(|cb| {
            cb.params
                .iter()
                .any(|p| typeref_deep_any(&p.ty, &is_string_type))
        });
    let needs_bytes = api_deep_any(api, &|t| {
        matches!(t, TypeRef::Bytes | TypeRef::BorrowedBytes)
    });
    // `weaveffi_free_bytes` releases byte returns, list/map container
    // buffers, and boxed optional-scalar returns.
    let needs_free_bytes = needs_bytes
        || api_deep_any(api, &|t| match t {
            TypeRef::List(_) | TypeRef::Map(_, _) => true,
            TypeRef::Optional(inner) => !weaveffi_core::codegen::common::is_c_pointer_type(inner),
            _ => false,
        });
    let needs_str_array = api_deep_any(api, &|t| match t {
        TypeRef::List(inner) => is_string_type(inner),
        TypeRef::Map(k, v) => is_string_type(k) || is_string_type(v),
        TypeRef::Iterator(inner) => is_string_type(inner),
        _ => false,
    });
    // Async list/map results and listener callback arguments arrive borrowed:
    // their string elements are read without freeing, through a separate
    // helper.
    let needs_borrowed_str_array = (has_async
        && model
            .modules
            .iter()
            .flat_map(ModuleBinding::callables)
            .filter(|f| f.is_async)
            .any(|f| async_ret_has_string_buffers(f.ret.as_ref())))
        || listener_cbs.iter().any(|cb| {
            cb.params
                .iter()
                .any(|p| async_ret_has_string_buffers(Some(&p.ty)))
        });
    // Any iterator-returning callable pulls in the shared lazy-iterator
    // wrapper class.
    let has_iterators = model
        .modules
        .iter()
        .flat_map(ModuleBinding::callables)
        .any(|f| matches!(f.shape, CallShape::Iterator(_)));

    out.push_str("// WeaveFFI Wasm bindings (auto-generated)\n");
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

    if needs_err {
        emit_js_error_classes(&mut out, model);
    }

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
        out.push_str("// Decode an array of `len` C strings at `base`, freeing each string\n");
        out.push_str("// after copying (the consumer owns the elements); the caller releases\n");
        out.push_str("// the array container separately.\n");
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

    if needs_borrowed_str_array {
        out.push_str("// Decode an array of `len` borrowed C strings at `base` without\n");
        out.push_str("// freeing anything: async completion callbacks receive buffers the\n");
        out.push_str("// producer owns and reclaims after the callback returns.\n");
        out.push_str("function _readStrArray(wasm, base, len) {\n");
        out.push_str("  const out = [];\n");
        out.push_str("  if (base === 0) return out;\n");
        out.push_str("  const dv = new DataView(wasm.memory.buffer);\n");
        out.push_str(
            "  for (let i = 0; i < len; i++) out.push(_readCStr(wasm, dv.getUint32(base + i * 4, true)));\n",
        );
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
        out.push_str("// Non-throwing wrappers route here: a non-zero code can only be a\n");
        out.push_str("// producer panic or a marshalling failure, surfaced as the generic\n");
        out.push_str(&format!("// {ERROR_BRAND}.\n"));
        out.push_str("function _checkErr(wasm, errPtr) {\n");
        out.push_str("  const dv = new DataView(wasm.memory.buffer);\n");
        out.push_str("  const code = dv.getInt32(errPtr, true);\n");
        out.push_str("  if (code !== 0) {\n");
        out.push_str("    const msgPtr = dv.getUint32(errPtr + 4, true);\n");
        out.push_str("    const msg = _readCStr(wasm, msgPtr) || '';\n");
        out.push_str("    wasm.weaveffi_error_clear(errPtr);\n");
        out.push_str("    wasm.weaveffi_dealloc(errPtr, 8);\n");
        out.push_str(&format!("    throw new {ERROR_BRAND}(code, msg);\n"));
        out.push_str("  }\n");
        out.push_str("}\n\n");
        out.push_str("// Release an error slot on the success path.\n");
        out.push_str("function _freeErr(wasm, errPtr) {\n");
        out.push_str("  wasm.weaveffi_dealloc(errPtr, 8);\n");
        out.push_str("}\n\n");
        emit_js_error_checkers(&mut out, model);
        if has_async {
            out.push_str("// Throw if a borrowed (producer-owned) error carries a non-zero\n");
            out.push_str("// code. Used by async callbacks: the producer owns and frees the\n");
            out.push_str("// error struct, so the slot is read but never deallocated here.\n");
            out.push_str("// `mkErr` maps domain codes for throwing callables; without it the\n");
            out.push_str(&format!("// generic {ERROR_BRAND} is thrown.\n"));
            out.push_str("function _checkErrRef(wasm, errPtr, mkErr) {\n");
            out.push_str("  const dv = new DataView(wasm.memory.buffer);\n");
            out.push_str("  const code = dv.getInt32(errPtr, true);\n");
            out.push_str("  if (code === 0) return;\n");
            out.push_str("  const msgPtr = dv.getUint32(errPtr + 4, true);\n");
            out.push_str("  const msg = _readCStr(wasm, msgPtr) || '';\n");
            out.push_str(&format!(
                "  throw mkErr ? mkErr(code, msg) : new {ERROR_BRAND}(code, msg);\n"
            ));
            out.push_str("}\n\n");
        }
    }

    if has_iterators {
        out.push_str("// Lazy wrapper over a producer iterator handle, implementing the JS\n");
        out.push_str("// iterator protocol: each next() issues exactly one producer next call\n");
        out.push_str("// and yields one converted element, so iteration streams in constant\n");
        out.push_str("// memory. The handle is destroyed exactly once: eagerly on exhaustion,\n");
        out.push_str("// on a next error, or from return() when iteration stops early (a\n");
        out.push_str("// for...of loop calls return() automatically on break or throw).\n");
        out.push_str("// Abandoning an iterator without exhausting or closing it leaks the\n");
        out.push_str("// producer handle: JS has no finalization hook that is reliable across\n");
        out.push_str("// every target this loader supports.\n");
        out.push_str("class _WeaveFFIIterator {\n");
        out.push_str("  constructor(wasm, handle, stride, callNext, destroy, check, decode) {\n");
        out.push_str("    this._wasm = wasm;\n");
        out.push_str("    this._handle = handle;\n");
        out.push_str("    this._stride = stride;\n");
        out.push_str("    this._callNext = callNext;\n");
        out.push_str("    this._destroyFn = destroy;\n");
        out.push_str("    this._check = check;\n");
        out.push_str("    this._decode = decode;\n");
        out.push_str("    this._slot = wasm.weaveffi_alloc(stride);\n");
        out.push_str("  }\n");
        out.push_str("  // Destroy the handle and release the element slot exactly once.\n");
        out.push_str("  _close() {\n");
        out.push_str("    if (this._handle === 0) return;\n");
        out.push_str("    this._destroyFn(this._handle);\n");
        out.push_str("    this._handle = 0;\n");
        out.push_str("    this._wasm.weaveffi_dealloc(this._slot, this._stride);\n");
        out.push_str("    this._slot = 0;\n");
        out.push_str("  }\n");
        out.push_str("  next() {\n");
        out.push_str("    if (this._handle === 0) return { done: true, value: undefined };\n");
        out.push_str("    const wasm = this._wasm;\n");
        out.push_str("    const _err = _allocErr(wasm);\n");
        out.push_str("    let _has;\n");
        out.push_str("    try {\n");
        out.push_str("      _has = this._callNext(this._handle, this._slot, _err);\n");
        out.push_str("      // Throws (and releases the slot) on a non-zero code.\n");
        out.push_str("      this._check(wasm, _err);\n");
        out.push_str("    } catch (e) {\n");
        out.push_str("      this._close();\n");
        out.push_str("      throw e;\n");
        out.push_str("    }\n");
        out.push_str("    _freeErr(wasm, _err);\n");
        out.push_str("    if (_has === 0) {\n");
        out.push_str("      this._close();\n");
        out.push_str("      return { done: true, value: undefined };\n");
        out.push_str("    }\n");
        out.push_str("    return { done: false, value: this._decode(wasm, this._slot) };\n");
        out.push_str("  }\n");
        out.push_str("  // Early-exit cleanup; for...of calls this on break/throw.\n");
        out.push_str("  return(value) {\n");
        out.push_str("    this._close();\n");
        out.push_str("    return { done: true, value };\n");
        out.push_str("  }\n");
        out.push_str("  [Symbol.iterator]() {\n");
        out.push_str("    return this;\n");
        out.push_str("  }\n");
        out.push_str("}\n\n");
    }

    if has_async || has_listeners {
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
        out.push_str(" * Load a WeaveFFI Wasm module from the given URL.\n");
        out.push_str(" *\n");
        out.push_str(" * @param {string} url - URL to the `.wasm` file.\n");
        if api.modules.is_empty() {
            out.push_str(
                " * @returns {Promise<WebAssembly.Exports>} The exported Wasm functions.\n",
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
            if needs_free_bytes {
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
            bindings.extend(collect_called_symbols(model).into_iter().map(|s| {
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

        if has_async || has_listeners {
            out.push_str("  const _table = wasm.__indirect_function_table;\n\n");
        }

        if has_async {
            out.push_str("  let _nextCtxId = 1;\n");
            out.push_str("  const _asyncContexts = new Map();\n\n");
            out.push_str("  function _asyncHandler(ctxId, errPtr, ...results) {\n");
            out.push_str("    const ctx = _asyncContexts.get(ctxId);\n");
            out.push_str("    if (!ctx) return;\n");
            out.push_str("    _asyncContexts.delete(ctxId);\n");
            out.push_str("    try {\n");
            out.push_str("      if (errPtr !== 0) _checkErrRef(wasm, errPtr, ctx.mkErr);\n");
            out.push_str(
                "      ctx.resolve(ctx.unwrap ? ctx.unwrap(wasm, ...results) : results[0]);\n",
            );
            out.push_str("    } catch (e) {\n");
            out.push_str("      ctx.reject(e);\n");
            out.push_str("    }\n");
            out.push_str("  }\n\n");

            let mut trampolines: Vec<(String, Vec<&'static str>)> = Vec::new();
            for f in model.modules.iter().flat_map(ModuleBinding::callables) {
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

        if has_listeners {
            out.push_str("  // Listener subscriptions, keyed by the context id the loader\n");
            out.push_str("  // threads through the C ABI's void* context slot. Each entry\n");
            out.push_str("  // holds the JS callback and the producer's subscription id.\n");
            out.push_str("  let _nextLsnId = 1;\n");
            out.push_str("  const _listeners = new Map();\n\n");
            for cb in &listener_cbs {
                emit_js_listener_trampoline(&mut out, cb, "  ");
            }
            out.push('\n');
        }

        // Interface classes close over the loaded `wasm` instance (and the
        // async machinery above), so they live inside the loader rather than
        // at module scope like the struct wrappers.
        for module in &model.modules {
            for i in &module.interfaces {
                emit_interface_class(&mut out, module, i, "  ", emscripten);
            }
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

/// Whether a module subtree exposes anything (functions, interface classes,
/// or struct factories), so empty namespace objects are not emitted.
fn module_tree_has_content(
    m: &Module,
    path: &str,
    by_path: &HashMap<&str, &ModuleBinding>,
) -> bool {
    let here = by_path.get(path).is_some_and(|mb| {
        !mb.functions.is_empty()
            || !mb.interfaces.is_empty()
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
    let error = mb.error.as_ref();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(format!("{}: {{", m.name), "},", |w| {
        let inner = w.indent_str();
        for f in &mb.functions {
            let mut tmp = String::new();
            emit_js_callable(&mut tmp, f, error, JsDecl::Object, None, &inner, emscripten);
            w.raw(tmp);
        }
        for l in &mb.listeners {
            let mut tmp = String::new();
            if emscripten {
                emit_js_listener_stub(&mut tmp, l, &inner);
            } else {
                emit_js_listener_api(&mut tmp, l, &inner);
            }
            w.raw(tmp);
        }
        // The interface class itself is exposed on the module object, so
        // factories, statics, and `instanceof` checks all reach it.
        for i in &mb.interfaces {
            w.line(format!("{}: {},", i.name, i.name));
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

/// Emit one callable in the shape its [`CallShape`] and the mode call for:
/// iterator members return a lazy JS iterator, async members return a
/// `Promise` (or an explicit throwing stub in Emscripten mode), and
/// everything else is a plain synchronous wrapper. `self_arg` threads the
/// instance handle for interface methods; `error` is the module's effective
/// domain for the throws split.
fn emit_js_callable(
    out: &mut String,
    f: &FnBinding,
    error: Option<&ErrorBinding>,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
    emscripten: bool,
) {
    match &f.shape {
        CallShape::Iterator(ib) => {
            emit_js_iterator_function_wrapper(out, f, ib, error, decl, self_arg, indent);
        }
        _ if f.is_async && emscripten => emit_js_async_stub(out, f, decl, indent),
        _ if f.is_async => emit_js_async_function_wrapper(out, f, error, decl, self_arg, indent),
        _ => emit_js_function_wrapper(out, f, error, decl, self_arg, indent),
    }
}

/// Async functions are unsupported in Emscripten mode: the trampoline
/// registration relies on `WebAssembly.Function` and a growable
/// `__indirect_function_table`, neither of which an Emscripten module exposes
/// portably. Each async entry point becomes an explicit stub that throws at
/// call time, so the gap is impossible to miss from JS even though the
/// `.d.ts` deliberately omits it (a compile-time error for TS users).
fn emit_js_async_stub(out: &mut String, f: &FnBinding, decl: JsDecl, indent: &str) {
    let js_params: Vec<String> = f.params.iter().map(js_param_name).collect();
    let name = js_fn_name(f);
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(
        format!("{}{name}({}) {{", decl.prefix(), js_params.join(", ")),
        decl.close(),
        |w| {
            w.line(format!(
                "throw new Error(\"weaveffi: async function '{name}' is not supported in \
                 Emscripten mode; use the wasm32-unknown-unknown loader or a native \
                 target\");"
            ));
        },
    );
    out.push_str(&w.finish());
}

/// Listeners are unsupported in Emscripten mode: their trampolines rely on
/// `WebAssembly.Function` and a growable `__indirect_function_table`, exactly
/// like the async machinery. Each register/unregister entry point becomes an
/// explicit stub that throws at call time, so the gap is impossible to miss
/// from JS even though the `.d.ts` deliberately omits the pair (a
/// compile-time error for TS users).
fn emit_js_listener_stub(out: &mut String, l: &ListenerBinding, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    for op in ["register", "unregister"] {
        let name = format!("{op}_{}", l.name).to_lower_camel_case();
        w.block(format!("{name}() {{"), "},", |w| {
            w.line(format!(
                "throw new Error(\"weaveffi: listener '{}' is not supported in \
                 Emscripten mode; use the wasm32-unknown-unknown loader or a native \
                 target\");",
                l.name
            ));
        });
    }
    out.push_str(&w.finish());
}

/// Every callback typedef referenced by at least one listener, deduplicated
/// by `c_fn_type` in declaration order. Each gets one long-lived trampoline
/// in the wasm function table, shared by all of its subscriptions (the
/// per-subscription context id disambiguates), so register/unregister churn
/// never grows the table.
fn collect_listener_callbacks(model: &BindingModel) -> Vec<&CallbackBinding> {
    let mut cbs: Vec<&CallbackBinding> = Vec::new();
    for m in &model.modules {
        for l in &m.listeners {
            let Some(cb) = m.callback(&l.event_callback) else {
                // Validation guarantees the referenced callback exists
                // in-module.
                unreachable!("listener '{}' references unknown callback", l.name);
            };
            if !cbs.iter().any(|c| c.c_fn_type == cb.c_fn_type) {
                cbs.push(cb);
            }
        }
    }
    cbs
}

/// The wasm value type of one C ABI slot: pointers and 32-bit-or-smaller
/// scalars are `i32` on wasm32, 64-bit integers and handles widen to `i64`,
/// and floats keep their width.
fn cb_slot_wasm_type(ty: &CType) -> &'static str {
    match ty {
        CType::Int64 | CType::Uint64 | CType::Handle => "i64",
        CType::Float => "f32",
        CType::Double => "f64",
        _ => "i32",
    }
}

/// The JS-side name of the long-lived trampoline registered for one callback
/// typedef. `c_fn_type` is a C identifier, so it is a valid JS identifier
/// suffix.
fn js_listener_tramp_name(c_fn_type: &str) -> String {
    format!("_lsnPtr_{c_fn_type}")
}

/// Emit `const {target} = ...;` decoding one callback argument from its raw
/// wasm slot values into the idiomatic JS value the subscriber sees.
///
/// The producer owns every argument for the duration of the dispatch (the
/// `emit_*` helper frees lowered payloads after the last subscriber returns),
/// so this is the borrowing side of the marshalling table: strings and byte
/// buffers are copied out of linear memory and never freed here, and opaque
/// pointers (records, rich enums, interfaces) are wrapped without taking
/// ownership.
fn emit_cb_param_decode(out: &mut String, indent: &str, ty: &TypeRef, slots: &[String], target: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let a = &slots[0];
    // Optional aggregates share the plain aggregate decoding: the readers
    // decode a null base to an empty aggregate (matching the async path).
    let ty = match ty {
        TypeRef::Optional(inner)
            if matches!(
                inner.as_ref(),
                TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) | TypeRef::Map(_, _)
            ) =>
        {
            inner.as_ref()
        }
        other => other,
    };
    match ty {
        TypeRef::Bool => {
            w.line(format!("const {target} = {a} !== 0;"));
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
            w.line(format!("const {target} = {a};"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("const {target} = _readCStr(wasm, {a});"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let b = &slots[1];
            w.line(format!(
                "const {target} = ({a} === 0 || {b} === 0) ? new Uint8Array(0) : new Uint8Array(wasm.memory.buffer, {a}, {b}).slice();"
            ));
        }
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
            let cls = local_type_name(name);
            w.line(format!("const {target} = new {cls}(wasm, {a});"));
        }
        TypeRef::Interface(name) => {
            let cls = local_type_name(name);
            w.line(format!("const {target} = {cls}._wrap({a});"));
        }
        TypeRef::List(inner) => {
            let mut tmp = String::new();
            emit_read_list_into(
                &mut tmp,
                indent,
                inner,
                a,
                &slots[1],
                target,
                ElemOwnership::Borrowed,
            );
            w.raw(tmp);
        }
        TypeRef::Map(k, v) => {
            let mut tmp = String::new();
            emit_read_map_into(
                &mut tmp,
                indent,
                k,
                v,
                a,
                &slots[1],
                &slots[2],
                target,
                ElemOwnership::Borrowed,
            );
            w.raw(tmp);
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!("const {target} = _readCStr(wasm, {a});"));
            }
            TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
                let cls = local_type_name(name);
                w.line(format!(
                    "const {target} = {a} === 0 ? null : new {cls}(wasm, {a});"
                ));
            }
            TypeRef::Interface(name) => {
                let cls = local_type_name(name);
                w.line(format!(
                    "const {target} = {a} === 0 ? null : {cls}._wrap({a});"
                ));
            }
            scalar => {
                // Boxed optional scalar: the box is borrowed, so dereference
                // without freeing.
                let read = wasm_read_scalar_elem(scalar, "new DataView(wasm.memory.buffer)", a, "0")
                    .replace(&format!("{a} + 0 * {}", wasm_stride(scalar)), a);
                w.line(format!("const {target} = {a} === 0 ? null : {read};"));
            }
        },
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    out.push_str(&w.finish());
}

/// Emit the long-lived trampoline for one callback typedef at `indent`
/// (loader scope). The trampoline's wasm signature mirrors the callback's ABI
/// slots (the trailing `void* context` slot carries the subscription's
/// context id); it looks up the subscription, decodes each argument per the
/// borrowing contract, and invokes the JS callback synchronously.
fn emit_js_listener_trampoline(out: &mut String, cb: &CallbackBinding, indent: &str) {
    let tramp = js_listener_tramp_name(&cb.c_fn_type);
    let param_types: Vec<String> = cb
        .abi_params
        .iter()
        .map(|p| format!("'{}'", cb_slot_wasm_type(&p.ty)))
        .collect();
    // Positional slot names: one per ABI slot, with the trailing context slot
    // named _ctx.
    let mut slot_names: Vec<String> = (0..cb.abi_params.len() - 1)
        .map(|i| format!("a{i}"))
        .collect();
    slot_names.push("_ctx".to_string());

    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(
        format!(
            "const {tramp} = _registerTrampoline(_table, [{}], ({}) => {{",
            param_types.join(", "),
            slot_names.join(", ")
        ),
        "});",
        |w| {
            w.line("const _l = _listeners.get(_ctx);");
            w.line("if (_l === undefined) return;");
            let inner = w.indent_str();
            let mut slot_idx = 0usize;
            let mut call_args: Vec<String> = Vec::new();
            for (i, p) in cb.params.iter().enumerate() {
                let n = p.abi.len();
                let slots = &slot_names[slot_idx..slot_idx + n];
                slot_idx += n;
                let target = format!("_p{i}");
                let mut tmp = String::new();
                emit_cb_param_decode(&mut tmp, &inner, &p.ty, slots, &target);
                w.raw(tmp);
                call_args.push(target);
            }
            w.line(format!("_l.callback({});", call_args.join(", ")));
        },
    );
    out.push_str(&w.finish());
}

/// Emit one listener's register/unregister pair as module-object members.
///
/// `register` allocates a context id, hands the shared trampoline and that id
/// to the producer's `register_*` symbol, and returns the context id as the
/// consumer-facing subscription id (a plain number; the producer's `uint64_t`
/// id stays internal so the public surface avoids `BigInt`). `unregister`
/// releases both sides and is a no-op for an unknown id.
fn emit_js_listener_api(out: &mut String, l: &ListenerBinding, indent: &str) {
    let tramp = js_listener_tramp_name(&l.callback_c_fn_type);
    let register_name = format!("register_{}", l.name).to_lower_camel_case();
    let unregister_name = format!("unregister_{}", l.name).to_lower_camel_case();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let mut doc = String::new();
    emit_doc(&mut doc, &l.doc, indent);
    w.raw(doc);
    w.block(format!("{register_name}(callback) {{"), "},", |w| {
        w.line("const _id = _nextLsnId++;");
        w.line(format!(
            "const _rid = wasm.{}({tramp}, _id);",
            l.register_symbol
        ));
        w.line("_listeners.set(_id, { callback, rid: _rid });");
        w.line("return _id;");
    });
    w.block(format!("{unregister_name}(id) {{"), "},", |w| {
        w.line("const _l = _listeners.get(id);");
        w.line("if (_l === undefined) return;");
        w.line("_listeners.delete(id);");
        w.line(format!("wasm.{}(_l.rid);", l.unregister_symbol));
    });
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
/// `self_arg` (an expression such as `this._handle`) becomes the implicit
/// leading argument for interface methods; the checker selected by
/// [`js_checker_name`] enforces the throws split on the out-err slot.
fn emit_js_function_wrapper(
    out: &mut String,
    f: &FnBinding,
    error: Option<&ErrorBinding>,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
) {
    let body = format!("{indent}  ");
    let js_params: Vec<String> = f.params.iter().map(js_param_name).collect();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);

    if let Some(msg) = &f.deprecated {
        w.line(format!("/** @deprecated {msg} */"));
    }
    w.line(format!(
        "{}{}({}) {{",
        decl.prefix(),
        js_fn_name(f),
        js_params.join(", ")
    ));

    let mut inner = String::new();
    let mut args: Vec<String> = self_arg.iter().map(ToString::to_string).collect();
    let mut cleanup = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            &mut inner,
            &body,
            &p.ty,
            &js_param_name(p),
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
        Some(&js_checker_name(f, error)),
    );
    w.raw(inner);
    w.line(decl.close());
    out.push_str(&w.finish());
}

/// The single-expression `(w, p) => ...` closure converting one element out
/// of an iterator's `next` slot at pointer `p`, applying the per-element
/// release plan (`ElemFree`): a string is copied out of wasm memory and then
/// freed with `{prefix}_free_string`, a record or rich-enum pointer is
/// adopted by its wrapper class, an interface pointer by `_wrap`, and a
/// by-value element is read directly.
fn js_iter_decode_closure(elem: &TypeRef, stride: u32) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            "(w, p) => _takeCStr(w, new DataView(w.memory.buffer).getUint32(p, true))".into()
        }
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
            let cls = local_type_name(name);
            format!("(w, p) => new {cls}(w, new DataView(w.memory.buffer).getInt32(p, true))")
        }
        TypeRef::Interface(name) => {
            let cls = local_type_name(name);
            format!("(w, p) => {cls}._wrap(new DataView(w.memory.buffer).getInt32(p, true))")
        }
        scalar => {
            let read = wasm_read_scalar_elem(scalar, "new DataView(w.memory.buffer)", "p", "0")
                .replace(&format!("p + 0 * {stride}"), "p");
            format!("(w, p) => {read}")
        }
    }
}

/// Emit an iterator-returning function as a method returning a lazy JS
/// iterator over the producer's iterator handle (the TypeScript type is
/// `IterableIterator<T>`). The wrapper issues one producer `next` call per
/// consumer step, converts and frees each element per its plan, and destroys
/// the handle exactly once: on exhaustion, on a `next` error, or from
/// `return()` when the consumer stops early. Both the launch call and every
/// `next` route their out-err slot through the throws-aware checker, so a
/// throwing function's domain errors keep their typed class.
fn emit_js_iterator_function_wrapper(
    out: &mut String,
    f: &FnBinding,
    ib: &IteratorBinding,
    error: Option<&ErrorBinding>,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
) {
    let body = format!("{indent}  ");
    let js_params: Vec<String> = f.params.iter().map(js_param_name).collect();
    let checker = js_checker_name(f, error);
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);

    if let Some(msg) = &f.deprecated {
        w.line(format!("/** @deprecated {msg} */"));
    }
    w.line(format!(
        "{}{}({}) {{",
        decl.prefix(),
        js_fn_name(f),
        js_params.join(", ")
    ));

    let mut args: Vec<String> = self_arg.iter().map(ToString::to_string).collect();
    let mut cleanup = Vec::new();
    let mut staged = String::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            &mut staged,
            &body,
            &p.ty,
            &js_param_name(p),
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
    let decode = js_iter_decode_closure(&ib.elem, stride);
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
        w.line(format!("{checker}(wasm, _err);"));
        w.line("_freeErr(wasm, _err);");
        w.line(format!("return new _WeaveFFIIterator(wasm, _it, {stride},"));
        w.line(format!(
            "  (it, slot, ep) => wasm.{}(it, slot, ep),",
            ib.next.symbol
        ));
        w.line(format!("  (it) => wasm.{}(it),", ib.destroy_symbol));
        w.line(format!("  {checker}, {decode});"));
    });
    w.line(decl.close());
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
            | TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::Interface(_)
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
            TypeRef::Map(_, _) => {
                params.push("i32");
                params.push("i32");
                params.push("i32");
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => {
                params.push("i32");
                params.push("i32");
            }
            // Record/rich-enum/interface/typed-handle (null pointer = absent),
            // strings (nullable pointer), and every scalar including `handle`
            // (boxed by pointer) all arrive as a single i32 pointer.
            _ => params.push("i32"),
        },
        Some(TypeRef::Named(_)) => unreachable!("unresolved type reference"),
    }
    params
}

/// Emit the `unwrap` clause for an async result, or none for a void/raw-scalar
/// result (where `results[0]` is already idiomatic). Assumes the callback was
/// registered with [`async_cb_wasm_params`] widths. `mk_err` is the domain
/// factory stored as the context's `mkErr` for throwing callables, so the
/// completion callback rejects with the typed error.
///
/// The unwrap runs inside the completion callback, so it follows the async
/// borrowing contract: string, byte, and array buffers are producer-owned and
/// valid only for the callback's duration, so they are deep-copied out of
/// wasm memory and never freed here. Owned-object results (records, rich
/// enums, interfaces) are the exception: the callback receives ownership and
/// the pointer is adopted by its wrapper class.
fn emit_async_unwrap(out: &mut String, indent: &str, ret: Option<&TypeRef>, mk_err: Option<&str>) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let base = match mk_err {
        Some(factory) => format!("resolve, reject, mkErr: {factory}"),
        None => "resolve, reject".to_string(),
    };
    let plain = format!("_asyncContexts.set(ctxId, {{ {base} }});");
    // Optional aggregates share the plain aggregate decoding: the readers
    // decode a null base to an empty aggregate.
    let ret = match ret {
        Some(TypeRef::Optional(inner))
            if matches!(
                inner.as_ref(),
                TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) | TypeRef::Map(_, _)
            ) =>
        {
            Some(inner.as_ref())
        }
        other => other,
    };
    let Some(ret) = ret else {
        w.line(plain);
        out.push_str(&w.finish());
        return;
    };
    let open = format!("_asyncContexts.set(ctxId, {{ {base}, unwrap: ");
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
            w.line(plain);
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            // Borrowed: copy out of wasm memory, never free.
            w.line(format!("{open}(w, p) => _readCStr(w, p) }});"));
        }
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
            let cls = local_type_name(name);
            w.line(format!("{open}(w, h) => new {cls}(w, h) }});"));
        }
        TypeRef::Interface(name) => {
            let cls = local_type_name(name);
            w.line(format!("{open}(w, h) => {cls}._wrap(h) }});"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
                let cls = local_type_name(name);
                w.line(format!(
                    "{open}(w, h) => h === 0 ? null : new {cls}(w, h) }});"
                ));
            }
            TypeRef::Interface(name) => {
                let cls = local_type_name(name);
                w.line(format!(
                    "{open}(w, h) => h === 0 ? null : {cls}._wrap(h) }});"
                ));
            }
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!("{open}(w, p) => _readCStr(w, p) }});"));
            }
            scalar => {
                // Boxed optional scalar: the box is borrowed, so dereference
                // without freeing.
                let read = wasm_read_scalar_elem(scalar, "new DataView(w.memory.buffer)", "p", "0")
                    .replace(&format!("p + 0 * {}", wasm_stride(scalar)), "p");
                w.line(format!("{open}(w, p) => p === 0 ? null : {read} }});"));
            }
        },
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            // Borrowed: slice() deep-copies out of wasm memory, never free.
            w.line(format!(
                "{open}(w, ptr, len) => ptr === 0 || len === 0 ? new Uint8Array(0) : new Uint8Array(w.memory.buffer, ptr, len).slice() }});"
            ));
        }
        TypeRef::List(inner) => {
            w.block(format!("{open}(w, base, len) => {{"), "} });", |w| {
                w.line("const wasm = w;");
                let ind = w.indent_str();
                let mut tmp = String::new();
                emit_read_list_into(
                    &mut tmp,
                    &ind,
                    inner,
                    "base",
                    "len",
                    "_out",
                    ElemOwnership::Borrowed,
                );
                w.raw(tmp);
                w.line("return _out;");
            });
        }
        TypeRef::Map(k, v) => {
            w.block(format!("{open}(w, ka, va, len) => {{"), "} });", |w| {
                w.line("const wasm = w;");
                let ind = w.indent_str();
                let mut tmp = String::new();
                emit_read_map_into(
                    &mut tmp,
                    &ind,
                    k,
                    v,
                    "ka",
                    "va",
                    "len",
                    "_out",
                    ElemOwnership::Borrowed,
                );
                w.raw(tmp);
                w.line("return _out;");
            });
        }
        TypeRef::Iterator(_) => {
            w.line(plain);
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    out.push_str(&w.finish());
}

/// Emit an async function as a method returning a `Promise` at `indent`.
/// Throwing callables store the domain's error factory in the async context,
/// so the completion callback rejects with the typed error; non-throwing ones
/// reject with the generic brand error only for panics.
fn emit_js_async_function_wrapper(
    out: &mut String,
    f: &FnBinding,
    error: Option<&ErrorBinding>,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
) {
    let body2 = format!("{indent}    ");
    let js_params: Vec<String> = f.params.iter().map(js_param_name).collect();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);

    if let Some(msg) = &f.deprecated {
        w.line(format!("/** @deprecated {msg} */"));
    }

    // Pre-render the inner-most (depth + 2) fragments that delegate to helpers,
    // so the nested blocks below can splice them at the right depth.
    let mut unwrap = String::new();
    emit_async_unwrap(
        &mut unwrap,
        &body2,
        f.ret.as_ref(),
        js_err_factory(f, error).as_deref(),
    );
    let mut staged = String::new();
    let mut args: Vec<String> = self_arg.iter().map(ToString::to_string).collect();
    let mut cleanup = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            &mut staged,
            &body2,
            &p.ty,
            &js_param_name(p),
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
        format!(
            "{}{}({}) {{",
            decl.prefix(),
            js_fn_name(f),
            js_params.join(", ")
        ),
        decl.close(),
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

/// Emit the loader-scoped `class` for an interface: an opaque-handle wrapper
/// closing over the loaded `wasm` instance. The canonical `new` constructor
/// maps to `constructor`; other constructors and statics are static methods;
/// methods pass `this._handle` as the implicit leading `self` argument. The
/// internal `_wrap(handle)` adopts an owned handle without invoking the
/// constructor (mirroring the struct wrappers' raw `(wasm, handle)` path),
/// and `free()` releases the handle exactly once via the destroy symbol,
/// matching the rich-enum cleanup idiom.
fn emit_interface_class(
    out: &mut String,
    module: &ModuleBinding,
    i: &InterfaceBinding,
    indent: &str,
    emscripten: bool,
) {
    let cls = &i.name;
    let error = module.error.as_ref();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    if let Some(doc) = i.doc.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        for line in doc.lines() {
            w.line(format!("// {line}"));
        }
    }
    w.block(format!("class {cls} {{"), "}", |w| {
        let inner = w.indent_str();

        // Canonical constructor: `new(...)` becomes `constructor(...)`,
        // assigning the owned handle rather than returning a wrapped value.
        if let Some(c) = i.constructors.iter().find(|c| c.name == "new") {
            let body = format!("{inner}  ");
            let js_params: Vec<String> = c.params.iter().map(js_param_name).collect();
            let checker = js_checker_name(c, error);
            w.block(
                format!("constructor({}) {{", js_params.join(", ")),
                "}",
                |w| {
                    let mut staged = String::new();
                    let mut args = Vec::new();
                    let mut cleanup = Vec::new();
                    for (idx, p) in c.params.iter().enumerate() {
                        emit_stage_input(
                            &mut staged,
                            &body,
                            &p.ty,
                            &js_param_name(p),
                            &format!("a{idx}"),
                            &mut args,
                            &mut cleanup,
                        );
                    }
                    args.push("_err".to_string());
                    w.raw(staged);
                    w.line("const _err = _allocErr(wasm);");
                    w.line(format!(
                        "const _r = wasm.{}({});",
                        c.c_base,
                        args.join(", ")
                    ));
                    for stmt in &cleanup {
                        w.line(stmt);
                    }
                    w.line(format!("{checker}(wasm, _err);"));
                    w.line("_freeErr(wasm, _err);");
                    w.line("this._handle = _r;");
                },
            );
        }

        // Internal: adopt an owned handle (returns, list/iterator elements)
        // without running the constructor.
        w.block("static _wrap(handle) {", "}", |w| {
            w.line(format!("const _o = Object.create({cls}.prototype);"));
            w.line("_o._handle = handle;");
            w.line("return _o;");
        });

        // Explicit cleanup: release the producer-owned handle exactly once.
        w.block("free() {", "}", |w| {
            w.block("if (this._handle !== 0) {", "}", |w| {
                w.line(format!("wasm.{}(this._handle);", i.destroy_symbol));
                w.line("this._handle = 0;");
            });
        });

        for c in i.constructors.iter().filter(|c| c.name != "new") {
            let mut tmp = String::new();
            emit_js_callable(&mut tmp, c, error, JsDecl::Static, None, &inner, emscripten);
            w.raw(tmp);
        }
        for m in &i.methods {
            let mut tmp = String::new();
            emit_js_callable(
                &mut tmp,
                m,
                error,
                JsDecl::Method,
                Some("this._handle"),
                &inner,
                emscripten,
            );
            w.raw(tmp);
        }
        for s in &i.statics {
            let mut tmp = String::new();
            emit_js_callable(&mut tmp, s, error, JsDecl::Static, None, &inner, emscripten);
            w.raw(tmp);
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the module-level `class` for a struct: constructor, field getters, a
/// static `create(...)` factory, and `free()` releasing the producer-owned
/// handle exactly once (the consumer owns every struct handle it receives).
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
        // Explicit cleanup: release the producer-owned handle exactly once.
        w.block("free() {", "}", |w| {
            w.block("if (this._handle !== 0) {", "}", |w| {
                w.line(format!("this._wasm.{}(this._handle);", s.destroy_symbol));
                w.line("this._handle = 0;");
            });
        });
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
/// out; a rich enum crosses the ABI exactly like a record) works unchanged.
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
                None,
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
        let ret = TypeRef::RichEnum(enum_name.to_string());
        emit_return_decode(
            &mut inner,
            &ind,
            Some(&ret),
            &v.create.symbol,
            &args,
            &cleanup,
            Some("_checkErr"),
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
            None,
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
            let ret = TypeRef::Record(s.name.clone());
            emit_return_decode(
                &mut inner,
                &ind,
                Some(&ret),
                &s.create.symbol,
                &args,
                &cleanup,
                Some("_checkErr"),
            );
            w.raw(inner);
        },
    );
    out.push_str(&w.finish());
}

/// Emit the fluent `class XBuilder` for a struct that opted into a builder.
/// The builder is single-use: `build()` releases the builder handle via its
/// destroy symbol (whether or not the build succeeds), so abandoning a
/// builder before `build()` leaks the handle unless `free()` is called.
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
            let ret = TypeRef::Record(cls.clone());
            let mut tmp = String::new();
            // The builder handle is released right after the build call (the
            // destroy symbol is valid whether or not the build succeeded), so
            // a failed build cannot leak the builder.
            emit_return_decode(
                &mut tmp,
                &ind,
                Some(&ret),
                &b.build_symbol,
                &["this._b".to_string()],
                &[
                    format!("wasm.{}(this._b);", b.destroy_symbol),
                    "this._b = 0;".to_string(),
                ],
                Some("_checkErr"),
            );
            w.raw(tmp);
        });
        // Explicit cleanup for an abandoned builder: release the handle
        // exactly once (a completed build() has already released it).
        w.block("free() {", "}", |w| {
            w.block("if (this._b !== 0) {", "}", |w| {
                w.line(format!("this._wasm.{}(this._b);", b.destroy_symbol));
                w.line("this._b = 0;");
            });
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
            version: "0.5.0".into(),
            modules: vec![],
            generators: None,
            package: None,
        }
    }

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.5.0".into(),
            modules,
            generators: None,
            package: None,
        }
    }

    /// Test-only shim: build the model (the driver's job in production) and
    /// render the JS stub with the historical argument order.
    fn js_stub_for(
        api: &Api,
        module_name: &str,
        prefix: &str,
        input_basename: &str,
        filename: &str,
        emscripten: bool,
    ) -> String {
        let model = BindingModel::build(api, prefix);
        render_wasm_js_stub(
            api,
            &model,
            module_name,
            prefix,
            input_basename,
            filename,
            emscripten,
        )
    }

    /// Test-only shim mirroring [`js_stub_for`] for the `.d.ts` renderer.
    fn dts_for(
        api: &Api,
        module_name: &str,
        input_basename: &str,
        filename: &str,
        emscripten: bool,
    ) -> String {
        let model = BindingModel::build(api, "weaveffi");
        render_wasm_dts(
            api,
            &model,
            module_name,
            input_basename,
            filename,
            emscripten,
        )
    }

    /// Test-only shim mirroring [`js_stub_for`] for the README renderer.
    fn readme_for(api: &Api, prefix: &str, input_basename: &str, emscripten: bool) -> String {
        let model = BindingModel::build(api, prefix);
        render_wasm_readme(api, &model, prefix, input_basename, emscripten)
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
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }])
    }

    /// An API with a callback + listener, delivered synchronously through a
    /// long-lived function-table trampoline in the standard loader (and
    /// stubbed in Emscripten mode).
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
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }])
    }

    #[test]
    fn capabilities_declare_full_support() {
        let caps = LanguageBackend::capabilities(&WasmGenerator);
        assert_eq!(caps, TargetCapabilities::full());
    }

    #[test]
    fn listeners_emit_register_unregister_in_js() {
        let js = js_stub_for(
            &listener_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        // One long-lived trampoline per callback typedef, in the function
        // table, decoding the borrowed string argument without freeing it.
        assert!(
            js.contains(
                "const _lsnPtr_weaveffi_events_OnMessage_fn = _registerTrampoline(_table, ['i32', 'i32'],"
            ),
            "{js}"
        );
        assert!(js.contains("const _p0 = _readCStr(wasm, a0);"), "{js}");
        assert!(js.contains("_l.callback(_p0);"), "{js}");
        // Register hands the trampoline and a context id to the producer and
        // returns the numeric context id; unregister releases both sides.
        assert!(js.contains("registerMessageListener(callback) {"), "{js}");
        assert!(
            js.contains(
                "wasm.weaveffi_events_register_message_listener(_lsnPtr_weaveffi_events_OnMessage_fn, _id)"
            ),
            "{js}"
        );
        assert!(js.contains("unregisterMessageListener(id) {"), "{js}");
        assert!(
            js.contains("wasm.weaveffi_events_unregister_message_listener(_l.rid);"),
            "{js}"
        );
        assert!(!js.contains("is not supported"), "{js}");
    }

    #[test]
    fn listeners_declared_in_dts() {
        let api = listener_api();
        let dts = dts_for(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
        assert!(
            dts.contains("registerMessageListener(callback: (message: string) => void): number;"),
            "{dts}"
        );
        assert!(
            dts.contains("unregisterMessageListener(id: number): void;"),
            "{dts}"
        );
        assert!(dts.contains("send(text: string)"), "{dts}");
    }

    #[test]
    fn readme_documents_listeners() {
        let readme = readme_for(&listener_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Callbacks and Listeners"), "{readme}");
        assert!(readme.contains("synchronous"), "{readme}");
        assert!(readme.contains("subscription id"), "{readme}");
        assert!(!readme.contains("## Unsupported Features"), "{readme}");
    }

    #[test]
    fn listener_free_api_has_no_listener_section() {
        let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(!readme.contains("### Callbacks and Listeners"));
    }

    #[test]
    fn listeners_emit_throwing_stubs_in_emscripten_mode() {
        let js = js_stub_for(
            &listener_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            true,
        );
        assert!(js.contains("registerMessageListener() {"), "{js}");
        assert!(js.contains("unregisterMessageListener() {"), "{js}");
        assert!(
            js.contains("listener 'message_listener' is not supported in Emscripten mode"),
            "{js}"
        );
        assert!(
            !js.contains("_lsnPtr_") && !js.contains("_listeners"),
            "no listener machinery in Emscripten mode: {js}"
        );
    }

    #[test]
    fn listeners_omitted_from_dts_in_emscripten_mode() {
        let api = listener_api();
        let dts = dts_for(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            true,
        );
        assert!(!dts.contains("registerMessageListener"), "{dts}");
        assert!(dts.contains("send(text: string)"), "{dts}");
    }

    #[test]
    fn readme_documents_listener_gap_in_emscripten_mode() {
        let readme = readme_for(&listener_api(), "weaveffi", "weaveffi.yml", true);
        assert!(readme.contains("## Callbacks and Listeners"), "{readme}");
        assert!(
            readme.contains("not supported in Emscripten mode"),
            "{readme}"
        );
    }

    #[test]
    fn readme_documents_structs() {
        let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Structs"));
        assert!(readme.contains("opaque handles"));
        assert!(readme.contains("`i64` pointers"));
    }

    #[test]
    fn readme_documents_enums() {
        let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Enums"));
        assert!(readme.contains("`i32` values"));
        assert!(readme.contains("discriminant"));
    }

    #[test]
    fn readme_documents_optionals() {
        let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Optionals"));
        assert!(readme.contains("`0` / `null`"));
        assert!(readme.contains("boxed by pointer"));
    }

    #[test]
    fn readme_documents_lazy_iterators() {
        let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Iterators"));
        assert!(readme.contains("lazy JS iterator"));
        assert!(readme.contains("`return()`"));
        assert!(readme.contains("destroyed"));
    }

    #[test]
    fn readme_documents_lists() {
        let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Lists"));
        assert!(readme.contains("pointer + length"));
        assert!(readme.contains("`i32` pointer, `i32` length"));
    }

    #[test]
    fn js_stub_has_jsdoc() {
        let js = js_stub_for(
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
        let js = js_stub_for(
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
        let js = js_stub_for(
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
        let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(!readme.contains("## API Reference"));
    }

    #[test]
    fn api_reference_lists_module() {
        let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("## API Reference"));
        assert!(readme.contains("### Module: `math`"));
    }

    #[test]
    fn api_reference_function_abi_name() {
        let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("##### `weaveffi_math_add`"));
    }

    #[test]
    fn api_reference_function_signature() {
        let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("`weaveffi_math_add(a: i32, b: i32) -> i32`"));
    }

    #[test]
    fn api_reference_function_param_table() {
        let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("| `a` | `i32` | `i32` | native Wasm i32 |"));
        assert!(readme.contains("| `b` | `i32` | `i32` | native Wasm i32 |"));
        assert!(readme.contains("| _returns_ | `i32` | `i32` | native Wasm i32 |"));
    }

    #[test]
    fn api_reference_function_doc() {
        let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("Add two numbers"));
    }

    #[test]
    fn api_reference_struct_accessors() {
        let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("##### `Point`"));
        assert!(readme.contains("opaque handle (`i64`)"));
        assert!(readme.contains("| `weaveffi_math_Point_get_x` | `f64` |"));
        assert!(readme.contains("| `weaveffi_math_Point_get_y` | `f64` |"));
    }

    #[test]
    fn api_reference_enum_discriminants() {
        let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
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
        assert_eq!(wasm_type(&TypeRef::Record("Foo".into())), "i64");
        assert_eq!(wasm_type(&TypeRef::RichEnum("Shape".into())), "i64");
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
            wasm_type(&TypeRef::Optional(Box::new(TypeRef::Record("Foo".into())))),
            "i64"
        );
        // Optional scalars are boxed by pointer: one i32 slot.
        assert_eq!(wasm_type(&TypeRef::Optional(Box::new(TypeRef::I32))), "i32");
    }

    #[test]
    fn wasm_type_note_covers_all_variants() {
        assert_eq!(wasm_type_note(&TypeRef::I32), "native Wasm i32");
        assert_eq!(wasm_type_note(&TypeRef::U32), "unsigned mapped to i32");
        assert_eq!(wasm_type_note(&TypeRef::Bool), "0 = false, 1 = true");
        assert_eq!(
            wasm_type_note(&TypeRef::StringUtf8),
            "ptr + len in linear memory"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Record("X".into())),
            "opaque handle in linear memory"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::RichEnum("X".into())),
            "opaque handle in linear memory"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Enum("E".into())),
            "variant discriminant"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::Record("S".into())))),
            "opaque handle, 0 = absent"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "boxed scalar pointer, 0 = absent"
        );
    }

    #[test]
    fn type_display_round_trips() {
        assert_eq!(type_display(&TypeRef::I32), "i32");
        assert_eq!(type_display(&TypeRef::StringUtf8), "string");
        assert_eq!(type_display(&TypeRef::Record("Foo".into())), "Foo");
        assert_eq!(type_display(&TypeRef::RichEnum("Shape".into())), "Shape");
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
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
                doc: None,
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let readme = readme_for(&api, "weaveffi", "weaveffi.yml", false);
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
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let readme = readme_for(&api, "weaveffi", "weaveffi.yml", false);
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
                interfaces: vec![],
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
                interfaces: vec![],
                modules: vec![],
            },
        ]);
        let readme = readme_for(&api, "weaveffi", "weaveffi.yml", false);
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
        let js = js_stub_for(
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
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let js = js_stub_for(
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
        let js = js_stub_for(
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
        let js = js_stub_for(
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
        let dts = dts_for(
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
        assert!(dts.contains("@throws {WeaveFFIError} if the native call fails"));
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
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let dts = dts_for(
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
        let js = js_stub_for(
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
                        Box::new(TypeRef::Record("Contact".into())),
                    ))))),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let dts = dts_for(
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
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let dts = dts_for(
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
                        Box::new(TypeRef::Record("Contact".into())),
                    ),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let dts = dts_for(
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
                returns: Some(TypeRef::Record("Contact".into())),
                doc: None,
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let js = js_stub_for(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(
            js.contains("_cstr(wasm, name)"),
            "string param should be copied to Wasm memory via _cstr"
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
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
                doc: None,
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let js = js_stub_for(
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
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let js = js_stub_for(
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
            "should reference the Wasm function table: {js}"
        );
    }

    /// The Wasm bindings register one trampoline per async-callback
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
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let js = js_stub_for(
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
                    throws: false,
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
                    throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let dts = dts_for(
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
                throws: false,
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
            interfaces: vec![],
            modules: vec![Module {
                name: "child".into(),
                functions: vec![Function {
                    name: "inner_fn".into(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    throws: false,
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
                interfaces: vec![],
                modules: vec![],
            }],
        }]);
        let dts = dts_for(
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
            dts.contains("outerFn(): number"),
            "parent function in DTS missing: {dts}"
        );
        assert!(
            dts.contains("innerFn(): number"),
            "nested child function in DTS missing: {dts}"
        );
        let js = js_stub_for(
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
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }
    }

    #[test]
    fn wasm_emits_doc_on_function() {
        let dts = dts_for(
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
        let dts = dts_for(
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
        let dts = dts_for(
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
        let dts = dts_for(
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
        let dts = dts_for(
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
        let js = js_stub_for(
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
    /// resolved to `TypeRef::RichEnum`) so the handle marshalling is exercised too.
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
                        ty: TypeRef::RichEnum("Shape".into()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    throws: false,
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
                            ty: TypeRef::RichEnum("Shape".into()),
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
                    returns: Some(TypeRef::RichEnum("Shape".into())),
                    doc: None,
                    throws: false,
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
                    throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }])
    }

    #[test]
    fn wasm_rich_enum_emits_wrapper_class() {
        let js = js_stub_for(
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
        let js = js_stub_for(
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
        let js = js_stub_for(
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
        let js = js_stub_for(
            &rich_enum_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        // A rich enum crosses the ABI as an opaque handle, so functions pass it in
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
        let dts = dts_for(
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
        let readme = readme_for(&rich_enum_api(), "weaveffi", "weaveffi.yml", false);
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
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }])
    }

    #[test]
    fn emscripten_loader_accepts_module_and_binds_exports() {
        let js = js_stub_for(
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
        let standard = js_stub_for(
            &sample_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        let emscripten = js_stub_for(
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
        let js = js_stub_for(
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
        let js = js_stub_for(
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
        let dts = dts_for(
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
        let readme = readme_for(&async_api(), "weaveffi", "weaveffi.yml", true);
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

    // --- 0.5.0 overhaul: interfaces, typed errors, throws split, naming ---

    fn member(
        name: &str,
        params: Vec<Param>,
        returns: Option<TypeRef>,
        throws: bool,
        is_async: bool,
    ) -> Function {
        Function {
            name: name.into(),
            params,
            returns,
            doc: None,
            throws,
            r#async: is_async,
            cancellable: is_async,
            deprecated: None,
            since: None,
        }
    }

    fn str_param(name: &str) -> Param {
        Param {
            name: name.into(),
            ty: TypeRef::StringUtf8,
            mutable: false,
            doc: None,
        }
    }

    /// A kvstore-shaped module: a `Store` interface (canonical `new` plus an
    /// `open` factory, sync/iterator/async methods, one static), a `KvError`
    /// domain, and one non-throwing free function.
    fn kv_api() -> Api {
        make_api(vec![Module {
            name: "kv".into(),
            functions: vec![member("flush_all", vec![], None, false, false)],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(weaveffi_ir::ir::ErrorDomain {
                name: "KvError".into(),
                codes: vec![
                    weaveffi_ir::ir::ErrorCode {
                        name: "KeyNotFound".into(),
                        code: 1001,
                        message: "key not found".into(),
                        doc: None,
                    },
                    weaveffi_ir::ir::ErrorCode {
                        name: "StoreFull".into(),
                        code: 1003,
                        message: "store is full".into(),
                        doc: None,
                    },
                ],
            }),
            interfaces: vec![weaveffi_ir::ir::InterfaceDef {
                name: "Store".into(),
                doc: Some("A key-value store handle.".into()),
                constructors: vec![
                    member("new", vec![str_param("path")], None, true, false),
                    member("open", vec![str_param("path")], None, true, false),
                ],
                methods: vec![
                    member(
                        "put",
                        vec![
                            str_param("key"),
                            Param {
                                name: "ttl_seconds".into(),
                                ty: TypeRef::I64,
                                mutable: false,
                                doc: None,
                            },
                        ],
                        None,
                        true,
                        false,
                    ),
                    member(
                        "get",
                        vec![str_param("key")],
                        Some(TypeRef::StringUtf8),
                        true,
                        false,
                    ),
                    member(
                        "list_keys",
                        vec![],
                        Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                        false,
                        false,
                    ),
                    member("compact", vec![], None, true, true),
                ],
                statics: vec![member(
                    "default_capacity",
                    vec![],
                    Some(TypeRef::U64),
                    false,
                    false,
                )],
            }],
            modules: vec![],
        }])
    }

    fn kv_js() -> String {
        js_stub_for(
            &kv_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        )
    }

    #[test]
    fn interface_class_has_ctor_wrap_free_and_members() {
        let js = kv_js();
        assert!(js.contains("class Store {"), "{js}");
        // Canonical `new` becomes `constructor`, assigning the owned handle.
        assert!(js.contains("constructor(path) {"), "{js}");
        assert!(
            js.contains("const _r = wasm.weaveffi_kv_Store_new(a0_p, _err);"),
            "{js}"
        );
        assert!(js.contains("this._handle = _r;"), "{js}");
        // Internal adoption path used by returns and element decoding.
        assert!(js.contains("static _wrap(handle) {"), "{js}");
        assert!(
            js.contains("const _o = Object.create(Store.prototype);"),
            "{js}"
        );
        // Non-canonical constructor is a static factory returning a wrapped
        // owned handle via the ordinary return path.
        assert!(js.contains("static open(path) {"), "{js}");
        assert!(js.contains("return Store._wrap(_r);"), "{js}");
        // Methods pass the instance handle as the implicit leading argument.
        assert!(js.contains("put(key, ttlSeconds) {"), "{js}");
        assert!(
            js.contains("wasm.weaveffi_kv_Store_put(this._handle, "),
            "{js}"
        );
        // Statics are static methods.
        assert!(js.contains("static defaultCapacity() {"), "{js}");
        // Disposal mirrors the rich-enum idiom: free() releases exactly once.
        assert!(js.contains("free() {"), "{js}");
        assert!(
            js.contains("wasm.weaveffi_kv_Store_destroy(this._handle);"),
            "{js}"
        );
        // The class itself is exposed on the module object.
        assert!(js.contains("Store: Store,"), "{js}");
    }

    #[test]
    fn interface_iterator_member_returns_lazy_iterator_with_self() {
        let js = kv_js();
        assert!(js.contains("listKeys() {"), "{js}");
        // The launch call threads the instance handle and the throws-aware
        // error slot.
        assert!(
            js.contains("const _it = wasm.weaveffi_kv_Store_list_keys(this._handle, _err);"),
            "{js}"
        );
        // The wrapper hands the handle to the lazy iterator instead of
        // draining it into an array.
        assert!(
            js.contains("return new _WeaveFFIIterator(wasm, _it, 4,"),
            "{js}"
        );
        assert!(
            js.contains(
                "(it, slot, ep) => wasm.weaveffi_kv_Store_ListKeysIterator_next(it, slot, ep),"
            ),
            "{js}"
        );
        assert!(
            js.contains("(it) => wasm.weaveffi_kv_Store_ListKeysIterator_destroy(it),"),
            "{js}"
        );
        // No eager while-drain remains anywhere in the glue.
        assert!(!js.contains("while (wasm."), "{js}");
    }

    #[test]
    fn lazy_iterator_class_implements_protocol_and_destroys_once() {
        let js = kv_js();
        assert!(js.contains("class _WeaveFFIIterator {"), "{js}");
        // Iterator protocol: next(), return() for early exit, and
        // [Symbol.iterator]() making it iterable.
        assert!(js.contains("  next() {"), "{js}");
        assert!(js.contains("  return(value) {"), "{js}");
        assert!(js.contains("  [Symbol.iterator]() {"), "{js}");
        // One producer next call per consumer step.
        assert!(
            js.contains("_has = this._callNext(this._handle, this._slot, _err);"),
            "{js}"
        );
        // Destroy exactly once: _close() nulls the handle, and every path
        // (exhaustion, next error, early return) funnels through it.
        assert!(js.contains("if (this._handle === 0) return;"), "{js}");
        assert!(js.contains("this._destroyFn(this._handle);"), "{js}");
        assert_eq!(js.matches("this._close();").count(), 3, "{js}");
        // Abandonment leak is documented at the class site.
        assert!(js.contains("leaks the"), "{js}");
    }

    #[test]
    fn lazy_iterator_frees_string_elements_per_plan() {
        let js = kv_js();
        // Each yielded string element is copied out of wasm memory and then
        // freed with the runtime's free_string (via _takeCStr).
        assert!(
            js.contains(
                "(w, p) => _takeCStr(w, new DataView(w.memory.buffer).getUint32(p, true)));"
            ),
            "{js}"
        );
    }

    #[test]
    fn lazy_iterator_next_errors_follow_error_strategy() {
        let js = kv_js();
        // list_keys does not throw, so both launch and next route through the
        // generic trap checker.
        let list_keys = js
            .split("listKeys() {")
            .nth(1)
            .and_then(|s| s.split("\n  }").next())
            .expect("listKeys body");
        assert!(list_keys.contains("_checkErr(wasm, _err);"), "{list_keys}");
        assert!(
            list_keys.contains("_checkErr, (w, p) =>"),
            "next checker must match the function's error strategy: {list_keys}"
        );
    }

    #[test]
    fn typed_error_classes_and_factory() {
        let js = kv_js();
        assert!(
            js.contains("export class WeaveFFIError extends Error {"),
            "{js}"
        );
        assert!(
            js.contains("export class KvError extends WeaveFFIError {}"),
            "{js}"
        );
        assert!(
            js.contains("export class KeyNotFound extends KvError {"),
            "{js}"
        );
        assert!(js.contains("KeyNotFound.CODE = 1001;"), "{js}");
        assert!(js.contains("KvError.KeyNotFound = KeyNotFound;"), "{js}");
        assert!(js.contains("StoreFull.CODE = 1003;"), "{js}");
        // The factory maps unknown codes to the generic brand error.
        assert!(
            js.contains("function _kvErrorFrom(code, message) {"),
            "{js}"
        );
        assert!(
            js.contains("if (!_cls) return new WeaveFFIError(code, message);"),
            "{js}"
        );
    }

    #[test]
    fn throws_split_selects_typed_or_generic_checker() {
        let js = kv_js();
        // Throwing members route the out-err slot through the domain checker.
        assert!(
            js.contains("function _checkKvError(wasm, errPtr) {"),
            "{js}"
        );
        assert!(js.contains("_checkKvError(wasm, _err);"), "{js}");
        assert!(js.contains("throw _kvErrorFrom(code, msg);"), "{js}");
        // The non-throwing free function keeps the generic checker.
        assert!(js.contains("flushAll() {"), "{js}");
        let flush = js
            .split("flushAll() {")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .expect("flushAll body");
        assert!(flush.contains("_checkErr(wasm, _err);"), "{flush}");
        assert!(!flush.contains("_checkKvError"), "{flush}");
    }

    #[test]
    fn async_throwing_member_rejects_with_domain_error() {
        let js = kv_js();
        // The async context carries the domain factory for typed rejection.
        assert!(
            js.contains("_asyncContexts.set(ctxId, { resolve, reject, mkErr: _kvErrorFrom });"),
            "{js}"
        );
        assert!(
            js.contains("if (errPtr !== 0) _checkErrRef(wasm, errPtr, ctx.mkErr);"),
            "{js}"
        );
        assert!(
            js.contains("throw mkErr ? mkErr(code, msg) : new WeaveFFIError(code, msg);"),
            "{js}"
        );
        // The launcher passes the cancel slot and callback as usual.
        assert!(
            js.contains(
                "wasm.weaveffi_kv_Store_compact_async(this._handle, 0, _cbPtr_i32_i32, ctxId);"
            ),
            "{js}"
        );
    }

    #[test]
    fn naming_lower_camel_functions_and_params() {
        let js = kv_js();
        assert!(js.contains("flushAll() {"), "{js}");
        assert!(js.contains("put(key, ttlSeconds) {"), "{js}");
        assert!(!js.contains("ttl_seconds"), "{js}");
        assert!(!js.contains("list_keys() {"), "{js}");
    }

    #[test]
    fn kv_dts_declares_errors_interface_and_throws_tags() {
        let dts = dts_for(
            &kv_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
        assert!(
            dts.contains("export declare class WeaveFFIError extends Error {"),
            "{dts}"
        );
        assert!(
            dts.contains("export declare class KvError extends WeaveFFIError {"),
            "{dts}"
        );
        assert!(
            dts.contains("static readonly KeyNotFound: typeof KeyNotFound;"),
            "{dts}"
        );
        assert!(
            dts.contains("export declare class KeyNotFound extends KvError {"),
            "{dts}"
        );
        assert!(dts.contains("static readonly CODE: 1001;"), "{dts}");
        assert!(dts.contains("export declare class Store {"), "{dts}");
        assert!(dts.contains("constructor(path: string);"), "{dts}");
        assert!(dts.contains("static open(path: string): Store;"), "{dts}");
        assert!(
            dts.contains("put(key: string, ttlSeconds: bigint): void;"),
            "{dts}"
        );
        assert!(
            dts.contains("listKeys(): IterableIterator<string>;"),
            "{dts}"
        );
        assert!(
            dts.contains("@returns A lazy iterator"),
            "iterator members should document the streaming contract: {dts}"
        );
        assert!(dts.contains("compact(): Promise<void>;"), "{dts}");
        assert!(dts.contains("static defaultCapacity(): bigint;"), "{dts}");
        assert!(dts.contains("free(): void;"), "{dts}");
        assert!(dts.contains("Store: typeof Store;"), "{dts}");
        assert!(
            dts.contains("@throws {KvError} on a domain error code"),
            "{dts}"
        );
        assert!(
            dts.contains("@throws {WeaveFFIError} if the native call fails"),
            "{dts}"
        );
    }

    #[test]
    fn kv_readme_documents_error_domain_and_interface() {
        let readme = readme_for(&kv_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("Error Domain: `KvError`"), "{readme}");
        assert!(readme.contains("| `KeyNotFound` | `1001` |"), "{readme}");
        assert!(readme.contains("##### `Store`"), "{readme}");
        assert!(readme.contains("weaveffi_kv_Store_destroy"), "{readme}");
    }

    #[test]
    fn emscripten_binds_interface_member_and_destroy_symbols() {
        let js = js_stub_for(
            &kv_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            true,
        );
        assert!(
            js.contains("weaveffi_kv_Store_put: m['_weaveffi_kv_Store_put'],"),
            "{js}"
        );
        assert!(
            js.contains("weaveffi_kv_Store_destroy: m['_weaveffi_kv_Store_destroy'],"),
            "{js}"
        );
        // The async member is a throwing stub; its launcher is never bound.
        assert!(
            js.contains("async function 'compact' is not supported in Emscripten mode"),
            "{js}"
        );
        assert!(!js.contains("weaveffi_kv_Store_compact_async"), "{js}");
        // Iterator surface symbols are bound so the lazy wrapper can call them.
        assert!(
            js.contains(
                "weaveffi_kv_Store_ListKeysIterator_next: m['_weaveffi_kv_Store_ListKeysIterator_next'],"
            ),
            "{js}"
        );
    }

    // --- Ownership audit: return-marshalling release calls ---

    /// A one-module API with a single free function of the given return type.
    fn returning_api(ret: TypeRef, is_async: bool) -> Api {
        make_api(vec![Module {
            name: "m".into(),
            functions: vec![member("get_it", vec![], Some(ret), false, is_async)],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }])
    }

    fn js_for_api(api: &Api) -> String {
        js_stub_for(
            api,
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        )
    }

    #[test]
    fn list_return_frees_elements_and_container() {
        let js = js_for_api(&returning_api(
            TypeRef::List(Box::new(TypeRef::StringUtf8)),
            false,
        ));
        // Each string element is freed by _takeStrArray; the array buffer
        // itself is then released with free_bytes.
        assert!(js.contains("_takeStrArray(wasm, _r, _len);"), "{js}");
        assert!(
            js.contains("if (_r !== 0 && _len !== 0) wasm.weaveffi_free_bytes(_r, _len * 4);"),
            "{js}"
        );
    }

    #[test]
    fn map_return_frees_both_parallel_arrays() {
        let js = js_for_api(&returning_api(
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            false,
        ));
        assert!(
            js.contains("if (_ka !== 0 && _len !== 0) wasm.weaveffi_free_bytes(_ka, _len * 4);"),
            "{js}"
        );
        assert!(
            js.contains("if (_va !== 0 && _len !== 0) wasm.weaveffi_free_bytes(_va, _len * 4);"),
            "{js}"
        );
    }

    #[test]
    fn optional_scalar_return_frees_the_box() {
        let js = js_for_api(&returning_api(
            TypeRef::Optional(Box::new(TypeRef::I32)),
            false,
        ));
        assert!(js.contains("if (_r === 0) return null;"), "{js}");
        assert!(js.contains("wasm.weaveffi_free_bytes(_r, 4);"), "{js}");
    }

    #[test]
    fn struct_wrapper_releases_handle_via_free() {
        let js = js_stub_for(
            &sample_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(
            js.contains("this._wasm.weaveffi_math_Point_destroy(this._handle);"),
            "struct free() must call the destroy symbol: {js}"
        );
        let dts = dts_for(
            &sample_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
        assert!(
            dts.contains("free(): void;"),
            "struct interface must declare free(): {dts}"
        );
    }

    #[test]
    fn builder_build_releases_builder_handle() {
        let api = make_api(vec![Module {
            name: "geo".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Point".into(),
                doc: None,
                fields: vec![StructField {
                    name: "x".into(),
                    ty: TypeRef::F64,
                    doc: None,
                    default: None,
                }],
                builder: true,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let js = js_for_api(&api);
        // build() destroys the single-use builder handle before the error
        // check, so a failed build cannot leak it.
        assert!(
            js.contains("wasm.weaveffi_geo_Point_Builder_destroy(this._b);"),
            "{js}"
        );
        assert!(js.contains("this._b = 0;"), "{js}");
        // An abandoned builder can still be released explicitly.
        assert!(
            js.contains("this._wasm.weaveffi_geo_Point_Builder_destroy(this._b);"),
            "{js}"
        );
    }

    // --- Async completion contract: borrowed buffers are copied, not freed ---

    #[test]
    fn async_string_result_is_copied_not_freed() {
        let js = js_for_api(&returning_api(TypeRef::StringUtf8, true));
        assert!(
            js.contains("unwrap: (w, p) => _readCStr(w, p) });"),
            "async string results are borrowed and must not be freed: {js}"
        );
        assert!(
            !js.contains("unwrap: (w, p) => _takeCStr"),
            "async unwrap must not free the producer's string: {js}"
        );
    }

    #[test]
    fn async_bytes_result_is_copied_not_freed() {
        let js = js_for_api(&returning_api(TypeRef::Bytes, true));
        assert!(
            js.contains("new Uint8Array(w.memory.buffer, ptr, len).slice() });"),
            "async bytes results must be deep-copied: {js}"
        );
        assert!(
            !js.contains("unwrap: (w, ptr, len) => _takeBytes"),
            "async unwrap must not free the producer's buffer: {js}"
        );
    }

    #[test]
    fn async_string_list_result_reads_borrowed_elements() {
        let js = js_for_api(&returning_api(
            TypeRef::List(Box::new(TypeRef::StringUtf8)),
            true,
        ));
        assert!(
            js.contains("function _readStrArray(wasm, base, len) {"),
            "{js}"
        );
        assert!(js.contains("_readStrArray(wasm, base, len);"), "{js}");
        // The borrowed array buffer is never freed by the callback.
        assert!(!js.contains("wasm.weaveffi_free_bytes(base"), "{js}");
    }

    #[test]
    fn async_optional_scalar_result_unboxes_without_freeing() {
        let js = js_for_api(&returning_api(
            TypeRef::Optional(Box::new(TypeRef::I32)),
            true,
        ));
        assert!(
            js.contains(
                "unwrap: (w, p) => p === 0 ? null : new DataView(w.memory.buffer).getInt32(p, true) });"
            ),
            "async optional scalars arrive as a borrowed box: {js}"
        );
    }

    #[test]
    fn async_record_result_is_adopted() {
        let api = make_api(vec![Module {
            name: "m".into(),
            functions: vec![member(
                "get_it",
                vec![],
                Some(TypeRef::Record("Contact".into())),
                false,
                true,
            )],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let js = js_for_api(&api);
        // An owned-object result transfers ownership: the callback adopts the
        // pointer into a wrapper whose free() calls the destroy symbol.
        assert!(
            js.contains("unwrap: (w, h) => new Contact(w, h) });"),
            "{js}"
        );
    }
}
