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

use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToShoutySnakeCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::lower::split_qualified;
use weaveffi_core::abi::{is_buffered, CType};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{
    emit_doc as common_emit_doc, walk_modules, walk_modules_with_path, DocCommentStyle,
};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FnBinding,
    InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding, StructBinding,
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

/// The wasm value-type spelling of one IDL type at the boundary, for the
/// README's signature tables. Buffered types occupy two `i32` slots (pointer
/// plus length); everything else keeps its scalar or pointer slot.
fn wasm_type(ty: &TypeRef) -> &'static str {
    if is_buffered(ty) {
        return "i32, i32";
    }
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::Bool
        | TypeRef::Enum(_) => "i32",
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle => "i64",
        TypeRef::F32 => "f32",
        TypeRef::F64 => "f64",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "i32",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "i32, i32",
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) | TypeRef::Iterator(_) => "i32",
        // Only `Interface?` reaches here: a nullable object pointer.
        TypeRef::Optional(_) => "i32",
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

fn wasm_type_note(ty: &TypeRef) -> &'static str {
    if is_buffered(ty) {
        return "value buffer: ptr + len in linear memory";
    }
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
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "NUL-terminated C string pointer",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "ptr + len in linear memory",
        TypeRef::TypedHandle(_) => "opaque pointer",
        TypeRef::Handle => "opaque 64-bit handle",
        TypeRef::Interface(_) => "opaque object pointer",
        TypeRef::Enum(_) => "variant discriminant",
        TypeRef::Iterator(_) => "opaque iterator handle",
        TypeRef::Optional(_) => "nullable object pointer, 0 = absent",
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
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
    out.push_str("### Records\n\n");
    out.push_str("Records are **plain JavaScript objects**. A record crosses the boundary ");
    out.push_str("serialized in the WeaveFFI value-buffer format as one pointer + length ");
    out.push_str("pair in linear memory; the glue packs and unpacks it automatically, so ");
    out.push_str("there are no handles, no accessor functions, and nothing to free.\n\n");
    out.push_str("### Enums\n\n");
    out.push_str("C-style enums are passed as **`i32` values** corresponding to the variant's integer discriminant. ");
    out.push_str("Rich (algebraic) enums are plain objects tagged by variant name ");
    out.push_str("(`{ tag: \"Circle\", radius: 2 }`) and cross the boundary serialized in a ");
    out.push_str("value buffer, exactly like records.\n\n");
    out.push_str("### Optionals\n\n");
    out.push_str("Optional values map to **`null`** for the absent case. An optional crosses ");
    out.push_str("the boundary inside a value buffer as a one-byte presence flag followed by ");
    out.push_str("the value when present. The one exception is an optional interface, which ");
    out.push_str("stays a nullable object pointer (`0` = absent).\n\n");
    out.push_str("### Lists and Maps\n\n");
    out.push_str("Lists are JS arrays; maps are plain objects (a `Map` instance is also ");
    out.push_str("accepted on input). Both cross the boundary serialized in a value buffer ");
    out.push_str("as one pointer + length pair, recursing through element types.\n\n");
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
             strings, byte buffers, and buffered values (records, rich enums, \
             optionals, lists, maps) are copied or decoded into JS values before your \
             function runs, and interface arguments wrap producer-owned objects. Read \
             what you need inside the callback and do not retain an interface wrapper \
             or call `free()` on it.\n",
        );
    }
    out.push_str("\n### Error Handling\n\n");
    out.push_str("The generated JS wrappers automatically handle errors by passing an error\n");
    out.push_str("pointer as the last argument to each Wasm function. The error struct is\n");
    out.push_str("16 bytes on wasm32: `{ i32 code, char* message, uint8_t* payload_ptr,\n");
    out.push_str("size_t payload_len }`. Your Wasm module must export the following\n");
    out.push_str("functions:\n\n");
    out.push_str("- `weaveffi_alloc(size: i32) -> i32`: allocate `size` bytes in linear memory\n");
    out.push_str("- `weaveffi_dealloc(ptr: i32, size: i32)`: release a `weaveffi_alloc` block\n");
    out.push_str("- `weaveffi_error_clear(err_ptr: i32)`: clear and free error resources\n");
    out.push_str("- `weaveffi_free_string(ptr: i32)`: free a producer-returned C string\n");
    out.push_str("- `weaveffi_free_bytes(ptr: i32, len: i32)`: free a producer-returned buffer\n");
    out.push_str("\nWrappers of functions declared `throws` raise the declaring module's typed\n");
    out.push_str("error class (a `WeaveFFIError` subclass with a per-code subclass, such as\n");
    out.push_str("`KeyNotFound`); every other wrapper raises the generic `WeaveFFIError` only\n");
    out.push_str("for producer panics and marshalling failures. When the matched error code\n");
    out.push_str("declares payload fields, the wrapper decodes them from the error's value\n");
    out.push_str("buffer and attaches them as properties on the thrown error.\n");

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
/// generates, the stable ABI code of each subclass, and any structured
/// payload fields a code attaches to the thrown error.
fn render_error_ref(out: &mut String, eb: &ErrorBinding) {
    out.push_str(&format!("\n#### Error Domain: `{}`\n\n", eb.type_name));
    out.push_str(&format!(
        "Throwing wrappers in this module raise `{}` (a `{ERROR_BRAND}` subclass); \
         each code below is its own subclass carrying the stable `code`. Payload \
         fields are decoded from the error's value buffer and attached as \
         properties on the thrown error.\n\n",
        eb.type_name
    ));
    out.push_str("| Class | Code | Default Message | Payload Fields |\n");
    out.push_str("|-------|------|-----------------|----------------|\n");
    for c in &eb.codes {
        let fields = if c.fields.is_empty() {
            "(none)".to_string()
        } else {
            c.fields
                .iter()
                .map(|f| format!("`{}: {}`", f.name, type_display(&f.ty)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!(
            "| `{}` | `{}` | {} | {} |\n",
            js_code_class_name(&c.name),
            c.value,
            c.message,
            fields
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
        "Passed as an **opaque object pointer** (`i32`), wrapped by a JS class. \
         Constructors return an owned handle; methods pass the handle as the implicit \
         leading `self` argument; `free()` releases the handle via the destroy symbol.\n",
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

/// Document one record: a plain JS object serialized in a value buffer, with
/// its field schema in declaration (wire) order.
fn render_struct_ref(out: &mut String, s: &StructBinding) {
    out.push_str(&format!("\n##### `{}`\n\n", s.name));

    if let Some(doc) = &s.doc {
        out.push_str(doc);
        out.push_str("\n\n");
    }

    out.push_str(
        "A plain JS object, serialized in a value buffer (`i32` ptr + `i32` len) at \
         the boundary. Fields in declaration (wire) order:\n\n",
    );

    if !s.fields.is_empty() {
        out.push_str("| Field | Type |\n");
        out.push_str("|-------|------|\n");
        for field in &s.fields {
            out.push_str(&format!(
                "| `{}` | `{}` |\n",
                field.name,
                type_display(&field.ty)
            ));
        }
    }
}

fn render_enum_ref(out: &mut String, e: &EnumBinding) {
    out.push_str(&format!("\n##### `{}`\n\n", e.name));

    if let Some(doc) = &e.doc {
        out.push_str(doc);
        out.push_str("\n\n");
    }

    if e.is_rich() {
        render_rich_enum_ref(out, e);
        return;
    }

    out.push_str("Passed as `i32` discriminant.\n\n");
    out.push_str("| Variant | Value |\n");
    out.push_str("|---------|-------|\n");
    for v in &e.variants {
        out.push_str(&format!("| `{}` | `{}` |\n", v.name, v.value));
    }
}

/// Document a rich (algebraic) enum: a plain tagged object serialized in a
/// value buffer as an `i32` tag followed by the active variant's fields, not
/// a by-value `i32` discriminant like a plain enum.
fn render_rich_enum_ref(out: &mut String, e: &EnumBinding) {
    out.push_str(
        "Rich (algebraic) enum: a plain tagged object (`{ tag: \"Variant\", ...fields }`) \
         serialized in a value buffer as an `i32` tag followed by the active variant's \
         fields in declaration order.\n\n",
    );
    out.push_str("| Variant | Tag | Fields |\n");
    out.push_str("|---------|-----|--------|\n");
    for v in &e.variants {
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

/// Visit every boundary-crossing type in `api` (function, interface-member,
/// and callback params and returns; struct, variant, and error payload field
/// types), recursing into composite types, and return whether any satisfies
/// `pred`.
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
            // Rich (algebraic) enums serialize their variant fields exactly
            // like struct fields, so a string/bytes/list living only inside a
            // variant payload still pulls in the corresponding helpers.
            || m.enums.iter().any(|e| {
                e.variants
                    .iter()
                    .any(|v| v.fields.iter().any(|f| deep(&f.ty, pred)))
            })
            // Callback arguments are decoded by the listener trampolines.
            || m.callbacks
                .iter()
                .any(|c| c.params.iter().any(|p| deep(&p.ty, pred)))
            // Error payload fields are decoded from the error's value buffer.
            || m.errors.as_ref().is_some_and(|d| {
                d.codes
                    .iter()
                    .any(|c| c.fields.iter().any(|f| deep(&f.ty, pred)))
            })
            || m.modules.iter().any(|sub| module_any(sub, pred))
    }
    api.modules.iter().any(|m| module_any(m, pred))
}

/// The byte size of the linear-memory slot an iterator `next` writes one
/// element of `ty` into: 8 for a `ptr` + `len` pair (bytes and buffered
/// values), pointer or scalar width otherwise.
fn iter_slot_size(ty: &TypeRef) -> u32 {
    if is_buffered(ty) {
        return 8;
    }
    match ty {
        TypeRef::Bool | TypeRef::I8 | TypeRef::U8 => 1,
        TypeRef::I16 | TypeRef::U16 => 2,
        TypeRef::I64 | TypeRef::U64 | TypeRef::F64 | TypeRef::Handle => 8,
        TypeRef::Bytes | TypeRef::BorrowedBytes => 8,
        _ => 4,
    }
}

/// A JS expression reading one by-value scalar of `ty` from `DataView` `dv`
/// at byte offset `at`.
fn read_scalar_at(ty: &TypeRef, dv: &str, at: &str) -> String {
    match ty {
        TypeRef::Bool => format!("{dv}.getUint8({at}) !== 0"),
        TypeRef::I8 => format!("{dv}.getInt8({at})"),
        TypeRef::U8 => format!("{dv}.getUint8({at})"),
        TypeRef::I16 => format!("{dv}.getInt16({at}, true)"),
        TypeRef::U16 => format!("{dv}.getUint16({at}, true)"),
        TypeRef::U32 => format!("{dv}.getUint32({at}, true)"),
        TypeRef::I32 | TypeRef::Enum(_) => format!("{dv}.getInt32({at}, true)"),
        TypeRef::I64 => format!("{dv}.getBigInt64({at}, true)"),
        TypeRef::U64 | TypeRef::Handle => format!("{dv}.getBigUint64({at}, true)"),
        TypeRef::F32 => format!("{dv}.getFloat32({at}, true)"),
        TypeRef::F64 => format!("{dv}.getFloat64({at}, true)"),
        // Opaque pointers (typed handles, interfaces) are i32 slots.
        _ => format!("{dv}.getUint32({at}, true)"),
    }
}

/// A direct JS call argument for a scalar/handle value (coercing bool to 0/1
/// and 64-bit values to `BigInt` as the wasm calling convention requires).
fn js_arg_scalar(ty: &TypeRef, val: &str) -> String {
    match ty {
        TypeRef::Bool => format!("{val} ? 1 : 0"),
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle => format!("BigInt({val})"),
        _ => val.to_string(),
    }
}

// ── Value-buffer codegen ──

/// The `_write_*`/`_read_*` codec function names for a (possibly
/// `module.Name`-qualified) record or rich enum referenced from
/// `current_module`.
fn buf_codec_names(name: &str, current_module: &str) -> (String, String) {
    let (module, local) = split_qualified(name, current_module);
    (
        format!("_write_{module}_{local}"),
        format!("_read_{module}_{local}"),
    )
}

/// The buffer-writer method encoding one by-value scalar of `ty`, or `None`
/// for a composite type the caller must recurse into.
fn buf_scalar_method(ty: &TypeRef) -> Option<&'static str> {
    Some(match ty {
        TypeRef::Bool => "bool",
        TypeRef::I8 => "i8",
        TypeRef::U8 => "u8",
        TypeRef::I16 => "i16",
        TypeRef::U16 => "u16",
        TypeRef::I32 | TypeRef::Enum(_) => "i32",
        TypeRef::U32 => "u32",
        TypeRef::I64 => "i64",
        TypeRef::U64 | TypeRef::Handle | TypeRef::TypedHandle(_) => "u64",
        TypeRef::F32 => "f32",
        TypeRef::F64 => "f64",
        _ => return None,
    })
}

/// Append the statements serializing `val` (a JS expression of IDL type `ty`)
/// into the buffer writer named `wtr`, resolving record and rich-enum
/// references against `module`. `tmp` supplies collision-free local names.
fn emit_buf_write_stmts(
    w: &mut CodeWriter,
    ty: &TypeRef,
    wtr: &str,
    val: &str,
    module: &str,
    tmp: &mut u32,
) {
    if let Some(m) = buf_scalar_method(ty) {
        w.line(format!("{wtr}.{m}({val});"));
        return;
    }
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("{wtr}.str({val});"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("{wtr}.bytes({val});"));
        }
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            let (write_fn, _) = buf_codec_names(name, module);
            w.line(format!("{write_fn}({wtr}, {val});"));
        }
        TypeRef::Optional(inner) => {
            w.line(format!("if ({val} === null || {val} === undefined) {{"));
            w.scope(|w| {
                w.line(format!("{wtr}.flag(false);"));
            });
            w.line("} else {");
            w.scope(|w| {
                w.line(format!("{wtr}.flag(true);"));
                emit_buf_write_stmts(w, inner, wtr, val, module, tmp);
            });
            w.line("}");
        }
        TypeRef::List(inner) => {
            *tmp += 1;
            let arr = format!("_a{tmp}");
            let elem = format!("_e{tmp}");
            w.line(format!("const {arr} = {val} || [];"));
            w.line(format!("{wtr}.len({arr}.length);"));
            w.line(format!("for (const {elem} of {arr}) {{"));
            w.scope(|w| {
                emit_buf_write_stmts(w, inner, wtr, &elem, module, tmp);
            });
            w.line("}");
        }
        TypeRef::Map(k, v) => {
            *tmp += 1;
            let src = format!("_s{tmp}");
            let ents = format!("_m{tmp}");
            let key = format!("_k{tmp}");
            let value = format!("_v{tmp}");
            w.line(format!("const {src} = {val} || {{}};"));
            w.line(format!(
                "const {ents} = {src} instanceof Map ? [...{src}.entries()] : Object.entries({src});"
            ));
            w.line(format!("{wtr}.len({ents}.length);"));
            w.line(format!("for (const [{key}, {value}] of {ents}) {{"));
            w.scope(|w| {
                emit_buf_write_stmts(w, k, wtr, &key, module, tmp);
                emit_buf_write_stmts(w, v, wtr, &value, module, tmp);
            });
            w.line("}");
        }
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("interfaces and iterators never appear inside a value buffer")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        _ => unreachable!("scalar handled above"),
    }
}

/// A JS expression decoding one value of IDL type `ty` from the buffer reader
/// named `rdr`, resolving record and rich-enum references against `module`.
/// Composite types recurse; lists and maps expand to inline arrow IIFEs so
/// the whole decode stays a single expression.
fn buf_read_expr(ty: &TypeRef, module: &str, rdr: &str) -> String {
    match ty {
        TypeRef::Bool => format!("{rdr}.bool()"),
        TypeRef::I8 => format!("{rdr}.i8()"),
        TypeRef::U8 => format!("{rdr}.u8()"),
        TypeRef::I16 => format!("{rdr}.i16()"),
        TypeRef::U16 => format!("{rdr}.u16()"),
        TypeRef::I32 | TypeRef::Enum(_) => format!("{rdr}.i32()"),
        TypeRef::U32 => format!("{rdr}.u32()"),
        TypeRef::I64 => format!("{rdr}.i64()"),
        TypeRef::U64 | TypeRef::Handle => format!("{rdr}.u64()"),
        // A typed handle is an i32 pointer at the ABI but a u64 on the wire;
        // narrowing back to a JS number keeps the two spellings interchangeable.
        TypeRef::TypedHandle(_) => format!("Number({rdr}.u64())"),
        TypeRef::F32 => format!("{rdr}.f32()"),
        TypeRef::F64 => format!("{rdr}.f64()"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{rdr}.str()"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => format!("{rdr}.bytes()"),
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            let (_, read_fn) = buf_codec_names(name, module);
            format!("{read_fn}({rdr})")
        }
        TypeRef::Optional(inner) => {
            format!(
                "({rdr}.flag() ? {} : null)",
                buf_read_expr(inner, module, rdr)
            )
        }
        TypeRef::List(inner) => {
            let elem = buf_read_expr(inner, module, rdr);
            format!(
                "(() => {{ const _n = {rdr}.len(); const _arr = []; for (let _i = 0; _i < _n; _i++) _arr.push({elem}); return _arr; }})()"
            )
        }
        TypeRef::Map(k, v) => {
            let key = buf_read_expr(k, module, rdr);
            let value = buf_read_expr(v, module, rdr);
            format!(
                "(() => {{ const _n = {rdr}.len(); const _obj = {{}}; for (let _i = 0; _i < _n; _i++) {{ const _k = {key}; _obj[_k] = {value}; }} return _obj; }})()"
            )
        }
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("interfaces and iterators never appear inside a value buffer")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// Emit the private value-buffer runtime: a growable little-endian writer and
/// a strict reader implementing the WeaveFFI wire format. Malformed input (a
/// producer/consumer contract violation) throws the generic brand error with
/// code `-3`.
fn emit_js_buffer_runtime(out: &mut String) {
    let mut w = CodeWriter::two_space();
    w.line("// Growable little-endian byte writer implementing the WeaveFFI value-buffer");
    w.line("// wire format: values are packed back to back with no alignment; lengths and");
    w.line("// counts are u32; strings are u32 byte length + UTF-8 bytes (no NUL).");
    w.block("class _BufWriter {", "}", |w| {
        w.block("constructor() {", "}", |w| {
            w.line("this._u8 = new Uint8Array(64);");
            w.line("this._dv = new DataView(this._u8.buffer);");
            w.line("this._len = 0;");
        });
        w.block("_need(n) {", "}", |w| {
            w.line("if (this._len + n <= this._u8.length) return;");
            w.line("let cap = this._u8.length * 2;");
            w.line("while (cap < this._len + n) cap *= 2;");
            w.line("const u8 = new Uint8Array(cap);");
            w.line("u8.set(this._u8.subarray(0, this._len));");
            w.line("this._u8 = u8;");
            w.line("this._dv = new DataView(u8.buffer);");
        });
        w.line("bool(v) { this._need(1); this._dv.setUint8(this._len, v ? 1 : 0); this._len += 1; }");
        w.line("i8(v) { this._need(1); this._dv.setInt8(this._len, v); this._len += 1; }");
        w.line("u8(v) { this._need(1); this._dv.setUint8(this._len, v); this._len += 1; }");
        w.line("i16(v) { this._need(2); this._dv.setInt16(this._len, v, true); this._len += 2; }");
        w.line("u16(v) { this._need(2); this._dv.setUint16(this._len, v, true); this._len += 2; }");
        w.line("i32(v) { this._need(4); this._dv.setInt32(this._len, v, true); this._len += 4; }");
        w.line("u32(v) { this._need(4); this._dv.setUint32(this._len, v, true); this._len += 4; }");
        w.line("i64(v) { this._need(8); this._dv.setBigInt64(this._len, BigInt(v), true); this._len += 8; }");
        w.line("u64(v) { this._need(8); this._dv.setBigUint64(this._len, BigInt(v), true); this._len += 8; }");
        w.line("f32(v) { this._need(4); this._dv.setFloat32(this._len, v, true); this._len += 4; }");
        w.line("f64(v) { this._need(8); this._dv.setFloat64(this._len, v, true); this._len += 8; }");
        w.line("len(n) { this.u32(n); }");
        w.line("flag(present) { this.u8(present ? 1 : 0); }");
        w.block("str(v) {", "}", |w| {
            w.line("const b = _enc.encode(v);");
            w.line("this.len(b.length);");
            w.line("this._need(b.length);");
            w.line("this._u8.set(b, this._len);");
            w.line("this._len += b.length;");
        });
        w.block("bytes(v) {", "}", |w| {
            w.line("const b = v instanceof Uint8Array ? v : new Uint8Array(v);");
            w.line("this.len(b.length);");
            w.line("this._need(b.length);");
            w.line("this._u8.set(b, this._len);");
            w.line("this._len += b.length;");
        });
        w.line("finish() { return this._u8.subarray(0, this._len); }");
    });
    w.blank();
    w.line("const _bufDec = new TextDecoder('utf-8', { fatal: true });");
    w.blank();
    w.line("// Strict little-endian reader for the WeaveFFI value-buffer wire format. A");
    w.line("// malformed buffer (truncation, invalid bool or flag bytes, an oversized");
    w.line("// length prefix, invalid UTF-8, trailing bytes) is a producer/consumer");
    w.line("// contract violation and throws the generic brand error; code -3 marks a");
    w.line("// consumer-side marshalling failure.");
    w.block("class _BufReader {", "}", |w| {
        w.block("constructor(bytes) {", "}", |w| {
            w.line("this._u8 = bytes;");
            w.line("this._dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);");
            w.line("this._pos = 0;");
        });
        w.block("_bad(what) {", "}", |w| {
            w.line(format!(
                "throw new {ERROR_BRAND}(-3, 'malformed value buffer: ' + what);"
            ));
        });
        w.block("_take(n, what) {", "}", |w| {
            w.line("if (this._pos + n > this._u8.length) this._bad('truncated ' + what);");
            w.line("const at = this._pos;");
            w.line("this._pos += n;");
            w.line("return at;");
        });
        w.block("bool() {", "}", |w| {
            w.line("const b = this._u8[this._take(1, 'bool')];");
            w.line("if (b > 1) this._bad('bool byte out of range');");
            w.line("return b === 1;");
        });
        w.line("i8() { return this._dv.getInt8(this._take(1, 'i8')); }");
        w.line("u8() { return this._u8[this._take(1, 'u8')]; }");
        w.line("i16() { return this._dv.getInt16(this._take(2, 'i16'), true); }");
        w.line("u16() { return this._dv.getUint16(this._take(2, 'u16'), true); }");
        w.line("i32() { return this._dv.getInt32(this._take(4, 'i32'), true); }");
        w.line("u32() { return this._dv.getUint32(this._take(4, 'u32'), true); }");
        w.line("i64() { return this._dv.getBigInt64(this._take(8, 'i64'), true); }");
        w.line("u64() { return this._dv.getBigUint64(this._take(8, 'u64'), true); }");
        w.line("f32() { return this._dv.getFloat32(this._take(4, 'f32'), true); }");
        w.line("f64() { return this._dv.getFloat64(this._take(8, 'f64'), true); }");
        w.block("len() {", "}", |w| {
            w.line("const n = this.u32();");
            w.line("if (n > this._u8.length - this._pos) this._bad('length prefix exceeds remaining buffer');");
            w.line("return n;");
        });
        w.block("flag() {", "}", |w| {
            w.line("const b = this._u8[this._take(1, 'option flag')];");
            w.line("if (b > 1) this._bad('option flag byte out of range');");
            w.line("return b === 1;");
        });
        w.block("str() {", "}", |w| {
            w.line("const n = this.len();");
            w.line("const at = this._take(n, 'string bytes');");
            w.block("try {", "} catch (e) {", |w| {
                w.line("return _bufDec.decode(this._u8.subarray(at, at + n));");
            });
            w.scope(|w| {
                w.line("this._bad('string is not valid UTF-8');");
            });
            w.line("}");
        });
        w.block("bytes() {", "}", |w| {
            w.line("const n = this.len();");
            w.line("const at = this._take(n, 'byte buffer');");
            w.line("return this._u8.subarray(at, at + n).slice();");
        });
        w.block("end() {", "}", |w| {
            w.line("if (this._pos !== this._u8.length) this._bad('trailing bytes after value');");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the module-scope `_write_*`/`_read_*` codec pair for every record and
/// rich enum in the model, in model (declaration) order. Field order is fixed
/// at generation time, so the codecs are direct straight-line code with no
/// runtime dispatch.
fn emit_js_buffer_codecs(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::two_space();
    for m in &model.modules {
        for s in &m.structs {
            let (write_fn, read_fn) = buf_codec_names(&s.name, &m.path);
            w.line(format!(
                "// Serialize a `{}.{}` record into the value-buffer wire format.",
                m.path, s.name
            ));
            w.block(format!("function {write_fn}(w, v) {{"), "}", |w| {
                let mut tmp = 0u32;
                for f in &s.fields {
                    emit_buf_write_stmts(
                        w,
                        &f.ty,
                        "w",
                        &format!("v.{}", f.name),
                        &m.path,
                        &mut tmp,
                    );
                }
            });
            w.blank();
            w.line(format!(
                "// Decode a `{}.{}` record from the value-buffer wire format.",
                m.path, s.name
            ));
            w.block(format!("function {read_fn}(r) {{"), "}", |w| {
                w.line("const v = {};");
                for f in &s.fields {
                    w.line(format!(
                        "v.{} = {};",
                        f.name,
                        buf_read_expr(&f.ty, &m.path, "r")
                    ));
                }
                w.line("return v;");
            });
            w.blank();
        }
        for e in m.enums.iter().filter(|e| e.is_rich()) {
            let (write_fn, read_fn) = buf_codec_names(&e.name, &m.path);
            w.line(format!(
                "// Serialize a `{}.{}` rich enum into the value-buffer wire format:",
                m.path, e.name
            ));
            w.line("// an i32 tag, then the active variant's fields in order.");
            w.block(format!("function {write_fn}(w, v) {{"), "}", |w| {
                w.block("switch (v.tag) {", "}", |w| {
                    for v in &e.variants {
                        w.block(format!("case \"{}\": {{", v.name), "}", |w| {
                            w.line(format!("w.i32({});", v.value));
                            let mut tmp = 0u32;
                            for f in &v.fields {
                                emit_buf_write_stmts(
                                    w,
                                    &f.ty,
                                    "w",
                                    &format!("v.{}", f.name),
                                    &m.path,
                                    &mut tmp,
                                );
                            }
                            w.line("break;");
                        });
                    }
                    w.line("default:");
                    w.scope(|w| {
                        w.line(format!(
                            "throw new {ERROR_BRAND}(-3, \"unknown {} variant tag: \" + v.tag);",
                            e.name
                        ));
                    });
                });
            });
            w.blank();
            w.line(format!(
                "// Decode a `{}.{}` rich enum from the value-buffer wire format.",
                m.path, e.name
            ));
            w.block(format!("function {read_fn}(r) {{"), "}", |w| {
                w.line("const _tag = r.i32();");
                w.block("switch (_tag) {", "}", |w| {
                    for v in &e.variants {
                        if v.fields.is_empty() {
                            w.line(format!("case {}:", v.value));
                            w.scope(|w| {
                                w.line(format!("return {{ tag: \"{}\" }};", v.name));
                            });
                        } else {
                            w.block(format!("case {}: {{", v.value), "}", |w| {
                                w.line(format!("const v = {{ tag: \"{}\" }};", v.name));
                                for f in &v.fields {
                                    w.line(format!(
                                        "v.{} = {};",
                                        f.name,
                                        buf_read_expr(&f.ty, &m.path, "r")
                                    ));
                                }
                                w.line("return v;");
                            });
                        }
                    }
                    w.line("default:");
                    w.scope(|w| {
                        w.line(format!(
                            "throw new {ERROR_BRAND}(-3, \"malformed value buffer: unknown {} tag \" + _tag);",
                            e.name
                        ));
                    });
                });
            });
            w.blank();
        }
    }
    out.push_str(&w.finish());
}

/// Stage one idiomatic input `value` of type `ty` into the Wasm ABI.
///
/// Pushes any pre-call statements to `out` (at `indent`), the produced call
/// arguments to `args`, and any post-call cleanup statements to `cleanup`.
/// `tmp` is a collision-free local-name base; `module` resolves record and
/// rich-enum codec references. Buffered values (records, rich enums,
/// optionals, lists, maps) are encoded into a value buffer and staged like
/// bytes: allocate, copy, pass `(ptr, len)`, dealloc after the call. Assumes
/// `wasm` is in scope.
#[allow(clippy::too_many_arguments)]
fn emit_stage_input(
    out: &mut String,
    indent: &str,
    ty: &TypeRef,
    value: &str,
    tmp: &str,
    module: &str,
    args: &mut Vec<String>,
    cleanup: &mut Vec<String>,
) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    if is_buffered(ty) {
        w.line(format!("const {tmp}_w = new _BufWriter();"));
        let mut n = 0u32;
        emit_buf_write_stmts(&mut w, ty, &format!("{tmp}_w"), value, module, &mut n);
        w.line(format!(
            "const [{tmp}_p, {tmp}_l] = _bytes(wasm, {tmp}_w.finish());"
        ));
        args.push(format!("{tmp}_p"));
        args.push(format!("{tmp}_l"));
        cleanup.push(format!("wasm.weaveffi_dealloc({tmp}_p, {tmp}_l);"));
        out.push_str(&w.finish());
        return;
    }
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
        TypeRef::Interface(_) => {
            args.push(format!("{value}._handle"));
        }
        // A typed handle is an opaque pointer the consumer received earlier;
        // it passes through unwrapped.
        TypeRef::TypedHandle(_) => {
            args.push(value.to_string());
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
        // Only `Interface?` reaches here (every other optional is buffered):
        // a nullable borrowed object pointer, null meaning none.
        TypeRef::Optional(_) => {
            args.push(format!("({value} ? {value}._handle : 0)"));
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as an input"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    out.push_str(&w.finish());
}

/// Emit the body that invokes `symbol` with the already-staged `in_args`,
/// runs `cleanup`, routes the error slot through the `checker` helper, and
/// decodes/returns the idiomatic value for `ret`. A buffered or bytes return
/// allocates the trailing `out_len` slot before the call. Assumes `wasm` is
/// in scope at `indent`.
#[allow(clippy::too_many_arguments)]
fn emit_return_decode(
    out: &mut String,
    indent: &str,
    ret: Option<&TypeRef>,
    symbol: &str,
    in_args: &[String],
    cleanup: &[String],
    checker: &str,
    module: &str,
) {
    let needs_len =
        ret.is_some_and(|t| is_buffered(t) || matches!(t, TypeRef::Bytes | TypeRef::BorrowedBytes));

    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let mut call_args = in_args.to_vec();
    if needs_len {
        w.line("const _lp = wasm.weaveffi_alloc(4);");
        call_args.push("_lp".to_string());
    }
    w.line("const _err = _allocErr(wasm);");
    call_args.push("_err".to_string());

    let call = format!("wasm.{symbol}({})", call_args.join(", "));
    if ret.is_some() {
        w.line(format!("const _r = {call};"));
    } else {
        w.line(format!("{call};"));
    }

    for stmt in cleanup {
        w.line(stmt);
    }
    w.line(format!("{checker}(wasm, _err);"));
    w.line("_freeErr(wasm, _err);");
    out.push_str(&w.finish());

    emit_decode_value(out, indent, ret, "_r", module);
}

/// Emit the `return ...;` (if any) that converts the raw result `r` (plus the
/// `_lp` out-slot already in scope for a bytes or buffered return) into the
/// idiomatic value. A buffered return is copied out of linear memory,
/// released with `weaveffi_free_bytes`, and decoded through the buffer
/// reader, which rejects malformed encodings.
fn emit_decode_value(out: &mut String, indent: &str, ret: Option<&TypeRef>, r: &str, module: &str) {
    let Some(ret) = ret else {
        return;
    };
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    if is_buffered(ret) {
        w.line("const _len = new DataView(wasm.memory.buffer).getUint32(_lp, true);");
        w.line("wasm.weaveffi_dealloc(_lp, 4);");
        w.line(format!(
            "const _rd = new _BufReader(_takeBytes(wasm, {r}, _len));"
        ));
        w.line(format!(
            "const _out = {};",
            buf_read_expr(ret, module, "_rd")
        ));
        w.line("_rd.end();");
        w.line("return _out;");
        out.push_str(&w.finish());
        return;
    }
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
        | TypeRef::TypedHandle(_)
        | TypeRef::Enum(_) => {
            w.line(format!("return {r};"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("return _takeCStr(wasm, {r});"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("const _len = new DataView(wasm.memory.buffer).getUint32(_lp, true);");
            w.line("wasm.weaveffi_dealloc(_lp, 4);");
            w.line(format!("return _takeBytes(wasm, {r}, _len);"));
        }
        TypeRef::Interface(name) => {
            let cls = local_type_name(name);
            w.line(format!("return {cls}._wrap({r});"));
        }
        // Only `Interface?` reaches here: a nullable owned object pointer.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(name) => {
                let cls = local_type_name(name);
                w.line(format!("return {r} === 0 ? null : {cls}._wrap({r});"));
            }
            _ => unreachable!("non-interface optionals are buffered"),
        },
        TypeRef::Iterator(_) => unreachable!("iterator returns handled separately"),
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
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
        // A typed handle is an opaque i32 pointer at the ABI, surfaced as a
        // plain number.
        TypeRef::TypedHandle(_) => "number".into(),
        // Records, rich enums, plain enums, and interfaces surface as bare
        // local TS names; a cross-module reference (resolved to e.g.
        // `kv.Store`) must name the local `Store`, not the qualified IR name
        // which is undeclared here.
        TypeRef::Enum(name)
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
                out.push_str(&format!("  {}: {};\n", field.name, ts_type_for(&field.ty)));
            }
            out.push_str("}\n\n");
        }

        for e in &m.enums {
            // A rich (algebraic) enum is a tagged plain-object union, not a
            // by-value discriminant constant.
            if e.is_rich() {
                emit_dts_rich_enum_type(&mut out, e);
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

/// Emit the TypeScript declaration for a rich (algebraic) enum: a
/// discriminated union of plain object shapes, one member per variant, keyed
/// by the string `tag`. Mirrors the runtime representation the buffer codecs
/// pack and unpack.
fn emit_dts_rich_enum_type(out: &mut String, e: &EnumDef) {
    let name = &e.name;
    let mut w = CodeWriter::two_space();
    w.doc(&e.doc, DocCommentStyle::Javadoc);
    w.line(format!("export type {name} ="));
    w.scope(|w| {
        let last = e.variants.len().saturating_sub(1);
        for (i, v) in e.variants.iter().enumerate() {
            let fields: String = v
                .fields
                .iter()
                .map(|f| format!("; {}: {}", f.name, ts_type_for(&f.ty)))
                .collect();
            let term = if i == last { ";" } else { "" };
            w.line(format!("| {{ tag: \"{}\"{fields} }}{term}", v.name));
        }
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
/// subclasses (each carrying a literal-typed `CODE` and any declared payload
/// fields) and the static aliases hung on the domain class.
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
                    for f in &c.fields {
                        w.doc(&f.doc, DocCommentStyle::Javadoc);
                        w.line(format!("readonly {}: {};", f.name, ts_type_for(&f.ty)));
                    }
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
/// is also aliased onto its domain class (`KvError.KeyNotFound`), and each
/// domain gets a factory that builds the matching subclass and decodes any
/// declared payload fields from the error's value buffer into properties on
/// the thrown error.
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
        let has_payload = eb.codes.iter().any(|c| !c.fields.is_empty());
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
        if has_payload {
            w.line("// Codes that declare payload fields decode them from the error's");
            w.line("// borrowed value buffer into properties on the thrown error.");
        }
        w.block(
            format!("function {factory}(wasm, code, message, payloadPtr, payloadLen) {{"),
            "}",
            |w| {
                w.line(format!("const _cls = {table}[code];"));
                w.line(format!(
                    "const _e = _cls ? (message ? new _cls(message) : new _cls()) : new {ERROR_BRAND}(code, message);"
                ));
                if has_payload {
                    w.block("switch (code) {", "}", |w| {
                        for c in eb.codes.iter().filter(|c| !c.fields.is_empty()) {
                            w.block(format!("case {}: {{", c.value), "}", |w| {
                                w.line(
                                    "const _b = payloadPtr === 0 || payloadLen === 0 ? new Uint8Array(0) : new Uint8Array(wasm.memory.buffer, payloadPtr, payloadLen).slice();",
                                );
                                w.line("const _rd = new _BufReader(_b);");
                                for f in &c.fields {
                                    w.line(format!(
                                        "_e.{} = {};",
                                        f.name,
                                        buf_read_expr(&f.ty, &eb.owner_path, "_rd")
                                    ));
                                }
                                w.line("_rd.end();");
                                w.line("break;");
                            });
                        }
                    });
                }
                w.line("return _e;");
            },
        );
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
/// the domain's factory, so domain codes surface as their typed subclasses
/// with any declared payload fields decoded and attached. The payload is
/// decoded before `weaveffi_error_clear` releases it.
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
                w.line("const msg = _readCStr(wasm, dv.getUint32(errPtr + 4, true)) || '';");
                w.line(format!(
                    "const _e = {factory}(wasm, code, msg, dv.getUint32(errPtr + 8, true), dv.getUint32(errPtr + 12, true));"
                ));
                w.line("wasm.weaveffi_error_clear(errPtr);");
                w.line("wasm.weaveffi_dealloc(errPtr, 16);");
                w.line("throw _e;");
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
/// Records and rich enums declare no C symbols at all.
fn collect_called_symbols(model: &BindingModel) -> Vec<String> {
    fn push_unique(syms: &mut Vec<String>, s: &str) {
        if !syms.iter().any(|x| x == s) {
            syms.push(s.to_string());
        }
    }
    let mut syms = Vec::new();
    for m in &model.modules {
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
    let listener_cbs: Vec<(&str, &CallbackBinding)> = if emscripten {
        Vec::new()
    } else {
        collect_listener_callbacks(model)
    };
    let has_listeners = !listener_cbs.is_empty();
    // Records and rich enums are value types packed and unpacked by the
    // module-scope codecs; any of them (or any buffered type, or an error
    // payload) pulls in the buffer writer/reader runtime.
    let has_codecs = model
        .modules
        .iter()
        .any(|m| !m.structs.is_empty() || m.enums.iter().any(|e| e.is_rich()));
    let has_error_payloads = model.modules.iter().any(|m| {
        m.error
            .as_ref()
            .is_some_and(|e| e.declared_here && e.codes.iter().any(|c| !c.fields.is_empty()))
    });
    let needs_buf = has_codecs || has_error_payloads || api_deep_any(api, &is_buffered);
    // The buffer reader (and the codecs) reject malformed input by throwing
    // the brand error, so the error surface is needed whenever buffers are.
    let needs_err = has_functions || needs_buf;
    // Error messages always cross as C strings, so anything needing the error
    // helpers also needs the string-read helpers regardless of declared types.
    let needs_strings = needs_err || api_deep_any(api, &is_string_type);
    // Buffered values are staged and released exactly like bytes, so the byte
    // helpers cover both.
    let needs_bytes = needs_buf
        || api_deep_any(api, &|t| {
            matches!(t, TypeRef::Bytes | TypeRef::BorrowedBytes)
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
    out.push_str("//   Objects   -> i32 pointer into linear memory (0 = null/absent)\n");
    out.push_str("//   Enums     -> i32 discriminant value\n");
    out.push_str("//   i64/u64   -> JavaScript BigInt\n");
    out.push_str("//   Strings   -> NUL-terminated UTF-8 (const char*); a single i32 pointer\n");
    out.push_str("//   Bytes     -> i32 data pointer + i32 length (out_len for returns)\n");
    out.push_str("//   Buffered  -> records, rich enums, optionals, lists, and maps cross\n");
    out.push_str("//                as one value buffer: i32 pointer + i32 length\n");
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
        out.push_str("// Stage a byte buffer (or an encoded value buffer); returns [ptr, len];\n");
        out.push_str("// release with weaveffi_dealloc(ptr, len).\n");
        out.push_str("function _bytes(wasm, data) {\n");
        out.push_str("  const u8 = data instanceof Uint8Array ? data : new Uint8Array(data);\n");
        out.push_str("  const ptr = wasm.weaveffi_alloc(u8.length);\n");
        out.push_str(
            "  if (u8.length) new Uint8Array(wasm.memory.buffer, ptr, u8.length).set(u8);\n",
        );
        out.push_str("  return [ptr, u8.length];\n");
        out.push_str("}\n\n");
        out.push_str("// Copy then free a producer-owned byte (or value) buffer.\n");
        out.push_str("function _takeBytes(wasm, ptr, len) {\n");
        out.push_str("  if (ptr === 0 || len === 0) return new Uint8Array(0);\n");
        out.push_str("  const copy = new Uint8Array(wasm.memory.buffer, ptr, len).slice();\n");
        out.push_str("  wasm.weaveffi_free_bytes(ptr, len);\n");
        out.push_str("  return copy;\n");
        out.push_str("}\n\n");
    }

    if needs_buf {
        emit_js_buffer_runtime(&mut out);
    }

    if needs_err {
        out.push_str("// Allocate a zeroed 16-byte error slot:\n");
        out.push_str("// { i32 code, char* message, uint8_t* payload_ptr, size_t payload_len }.\n");
        out.push_str("function _allocErr(wasm) {\n");
        out.push_str("  const ptr = wasm.weaveffi_alloc(16);\n");
        out.push_str("  new Uint8Array(wasm.memory.buffer, ptr, 16).fill(0);\n");
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
        out.push_str("    wasm.weaveffi_dealloc(errPtr, 16);\n");
        out.push_str(&format!("    throw new {ERROR_BRAND}(code, msg);\n"));
        out.push_str("  }\n");
        out.push_str("}\n\n");
        out.push_str("// Release an error slot on the success path.\n");
        out.push_str("function _freeErr(wasm, errPtr) {\n");
        out.push_str("  wasm.weaveffi_dealloc(errPtr, 16);\n");
        out.push_str("}\n\n");
        emit_js_error_checkers(&mut out, model);
        if has_async {
            out.push_str("// Throw if a borrowed (producer-owned) error carries a non-zero\n");
            out.push_str("// code. Used by async callbacks: the producer owns and frees the\n");
            out.push_str("// error struct, so the slot is read but never deallocated here.\n");
            out.push_str("// `mkErr` maps domain codes (and decodes payload fields) for\n");
            out.push_str(&format!(
                "// throwing callables; without it the generic {ERROR_BRAND} is thrown.\n"
            ));
            out.push_str("function _checkErrRef(wasm, errPtr, mkErr) {\n");
            out.push_str("  const dv = new DataView(wasm.memory.buffer);\n");
            out.push_str("  const code = dv.getInt32(errPtr, true);\n");
            out.push_str("  if (code === 0) return;\n");
            out.push_str("  const msg = _readCStr(wasm, dv.getUint32(errPtr + 4, true)) || '';\n");
            out.push_str(
                "  if (mkErr) throw mkErr(wasm, code, msg, dv.getUint32(errPtr + 8, true), dv.getUint32(errPtr + 12, true));\n",
            );
            out.push_str(&format!("  throw new {ERROR_BRAND}(code, msg);\n"));
            out.push_str("}\n\n");
        }
    }

    if has_codecs {
        emit_js_buffer_codecs(&mut out, model);
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
        out.push_str("  constructor(wasm, handle, slotSize, callNext, destroy, check, decode) {\n");
        out.push_str("    this._wasm = wasm;\n");
        out.push_str("    this._handle = handle;\n");
        out.push_str("    this._slotSize = slotSize;\n");
        out.push_str("    this._callNext = callNext;\n");
        out.push_str("    this._destroyFn = destroy;\n");
        out.push_str("    this._check = check;\n");
        out.push_str("    this._decode = decode;\n");
        out.push_str("    this._slot = wasm.weaveffi_alloc(slotSize);\n");
        out.push_str("  }\n");
        out.push_str("  // Destroy the handle and release the element slot exactly once.\n");
        out.push_str("  _close() {\n");
        out.push_str("    if (this._handle === 0) return;\n");
        out.push_str("    this._destroyFn(this._handle);\n");
        out.push_str("    this._handle = 0;\n");
        out.push_str("    this._wasm.weaveffi_dealloc(this._slot, this._slotSize);\n");
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
            // Rich (algebraic) enums are tagged plain-object unions handled by
            // the buffer codecs; only C-style enums surface as a by-value
            // discriminant object.
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
    out.push_str(" * // Record: plain objects in and out (serialized automatically).\n");
    out.push_str(" * const person = api.contacts.create({ name: 'Ada', age: 36 });\n");
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
            for (path, cb) in &listener_cbs {
                emit_js_listener_trampoline(&mut out, path, cb, "  ");
            }
            out.push('\n');
        }

        // Interface classes close over the loaded `wasm` instance (and the
        // async machinery above), so they live inside the loader rather than
        // at module scope like the value-type codecs.
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

/// Whether a module subtree exposes anything at runtime (functions, interface
/// classes, or listeners), so empty namespace objects are not emitted.
/// Records and rich enums contribute nothing here: they are plain value
/// shapes with no runtime members.
fn module_tree_has_content(
    m: &Module,
    path: &str,
    by_path: &HashMap<&str, &ModuleBinding>,
) -> bool {
    let here = by_path.get(path).is_some_and(|mb| {
        !mb.functions.is_empty() || !mb.interfaces.is_empty() || !mb.listeners.is_empty()
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
            emit_js_callable(&mut tmp, mb, f, JsDecl::Object, None, &inner, emscripten);
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
/// instance handle for interface methods; `mb` supplies the module's error
/// domain for the throws split and the module path for codec references.
fn emit_js_callable(
    out: &mut String,
    mb: &ModuleBinding,
    f: &FnBinding,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
    emscripten: bool,
) {
    match &f.shape {
        CallShape::Iterator(ib) => {
            emit_js_iterator_function_wrapper(out, mb, f, ib, decl, self_arg, indent);
        }
        _ if f.is_async && emscripten => emit_js_async_stub(out, f, decl, indent),
        _ if f.is_async => emit_js_async_function_wrapper(out, mb, f, decl, self_arg, indent),
        _ => emit_js_function_wrapper(out, mb, f, decl, self_arg, indent),
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

/// Every callback typedef referenced by at least one listener, paired with
/// its declaring module's path and deduplicated by `c_fn_type` in declaration
/// order. Each gets one long-lived trampoline in the wasm function table,
/// shared by all of its subscriptions (the per-subscription context id
/// disambiguates), so register/unregister churn never grows the table.
fn collect_listener_callbacks(model: &BindingModel) -> Vec<(&str, &CallbackBinding)> {
    let mut cbs: Vec<(&str, &CallbackBinding)> = Vec::new();
    for m in &model.modules {
        for l in &m.listeners {
            let Some(cb) = m.callback(&l.event_callback) else {
                // Validation guarantees the referenced callback exists
                // in-module.
                unreachable!("listener '{}' references unknown callback", l.name);
            };
            if !cbs.iter().any(|(_, c)| c.c_fn_type == cb.c_fn_type) {
                cbs.push((m.path.as_str(), cb));
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

/// Emit the statements decoding one callback argument from its raw wasm slot
/// values into the idiomatic JS value (bound to `target`) the subscriber
/// sees.
///
/// The producer owns every argument for the duration of the dispatch (the
/// `emit_*` helper frees lowered payloads after the last subscriber returns),
/// so this is the borrowing side of the marshalling table: strings, byte
/// buffers, and buffered values are copied or decoded out of linear memory
/// and never freed here, and interface pointers are wrapped without taking
/// ownership. Assumes `wasm` in scope.
fn emit_cb_param_decode(
    out: &mut String,
    indent: &str,
    ty: &TypeRef,
    slots: &[String],
    target: &str,
    module: &str,
) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let a = &slots[0];
    if is_buffered(ty) {
        let b = &slots[1];
        w.line(format!(
            "const {target}_b = ({a} === 0 || {b} === 0) ? new Uint8Array(0) : new Uint8Array(wasm.memory.buffer, {a}, {b}).slice();"
        ));
        w.line(format!("const {target}_r = new _BufReader({target}_b);"));
        w.line(format!(
            "const {target} = {};",
            buf_read_expr(ty, module, &format!("{target}_r"))
        ));
        w.line(format!("{target}_r.end();"));
        out.push_str(&w.finish());
        return;
    }
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
        | TypeRef::TypedHandle(_)
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
        TypeRef::Interface(name) => {
            let cls = local_type_name(name);
            w.line(format!("const {target} = {cls}._wrap({a});"));
        }
        // Only `Interface?` reaches here: a nullable borrowed object pointer.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(name) => {
                let cls = local_type_name(name);
                w.line(format!(
                    "const {target} = {a} === 0 ? null : {cls}._wrap({a});"
                ));
            }
            _ => unreachable!("non-interface optionals are buffered"),
        },
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    out.push_str(&w.finish());
}

/// Emit the long-lived trampoline for one callback typedef at `indent`
/// (loader scope). The trampoline's wasm signature mirrors the callback's ABI
/// slots (the trailing `void* context` slot carries the subscription's
/// context id); it looks up the subscription, decodes each argument per the
/// borrowing contract, and invokes the JS callback synchronously. `module` is
/// the callback's declaring module path, used to resolve codec references.
fn emit_js_listener_trampoline(out: &mut String, module: &str, cb: &CallbackBinding, indent: &str) {
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
                emit_cb_param_decode(&mut tmp, &inner, &p.ty, slots, &target, module);
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

/// Emit a synchronous function as a method `name(params) { ... }` at `indent`,
/// staging idiomatic inputs, calling the C symbol, and decoding the return.
/// `self_arg` (an expression such as `this._handle`) becomes the implicit
/// leading argument for interface methods; the checker selected by
/// [`js_checker_name`] enforces the throws split on the out-err slot.
fn emit_js_function_wrapper(
    out: &mut String,
    mb: &ModuleBinding,
    f: &FnBinding,
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
            &mb.path,
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
        &js_checker_name(f, mb.error.as_ref()),
        &mb.path,
    );
    w.raw(inner);
    w.line(decl.close());
    out.push_str(&w.finish());
}

/// The `(w, p) => ...` closure converting one element out of an iterator's
/// `next` slot at pointer `p`, applying the per-element release plan: a
/// string is copied out of wasm memory and freed with `free_string`, a bytes
/// or buffered element is copied out of its `ptr` + `len` pair and freed with
/// `free_bytes` (buffered elements are then decoded through the buffer
/// reader), an interface pointer is adopted by `_wrap`, and a by-value
/// element is read directly.
fn js_iter_decode_closure(elem: &TypeRef, module: &str) -> String {
    if is_buffered(elem) {
        let read = buf_read_expr(elem, module, "_rd");
        return format!(
            "(w, p) => {{ const dv = new DataView(w.memory.buffer); const _rd = new _BufReader(_takeBytes(w, dv.getUint32(p, true), dv.getUint32(p + 4, true))); const _v = {read}; _rd.end(); return _v; }}"
        );
    }
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            "(w, p) => _takeCStr(w, new DataView(w.memory.buffer).getUint32(p, true))".into()
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            "(w, p) => { const dv = new DataView(w.memory.buffer); return _takeBytes(w, dv.getUint32(p, true), dv.getUint32(p + 4, true)); }".into()
        }
        TypeRef::Interface(name) => {
            let cls = local_type_name(name);
            format!("(w, p) => {cls}._wrap(new DataView(w.memory.buffer).getUint32(p, true))")
        }
        scalar => {
            let read = read_scalar_at(scalar, "new DataView(w.memory.buffer)", "p");
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
    mb: &ModuleBinding,
    f: &FnBinding,
    ib: &IteratorBinding,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
) {
    let body = format!("{indent}  ");
    let js_params: Vec<String> = f.params.iter().map(js_param_name).collect();
    let checker = js_checker_name(f, mb.error.as_ref());
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
            &mb.path,
            &mut args,
            &mut cleanup,
        );
    }
    if f.cancellable {
        args.push("0".to_string());
    }
    args.push("_err".to_string());
    let slot_size = iter_slot_size(&ib.elem);
    // A `ptr` + `len` element (bytes or buffered) writes through two out
    // slots; the second lives 4 bytes past the first.
    let two_slot =
        is_buffered(&ib.elem) || matches!(ib.elem, TypeRef::Bytes | TypeRef::BorrowedBytes);
    let next_call = if two_slot {
        format!(
            "(it, slot, ep) => wasm.{}(it, slot, slot + 4, ep),",
            ib.next.symbol
        )
    } else {
        format!("(it, slot, ep) => wasm.{}(it, slot, ep),", ib.next.symbol)
    };
    let decode = js_iter_decode_closure(&ib.elem, &mb.path);
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
        w.line(format!(
            "return new _WeaveFFIIterator(wasm, _it, {slot_size},"
        ));
        w.line(format!("  {next_call}"));
        w.line(format!("  (it) => wasm.{}(it),", ib.destroy_symbol));
        w.line(format!("  {checker}, {decode});"));
    });
    w.line(decl.close());
    out.push_str(&w.finish());
}

/// The wasm callback param-type list for an async function with the given
/// return: always `(ctx i32, err i32, ...result)`. Pointers are i32 on
/// wasm32; only `i64`/`u64` widen to i64; a buffered result arrives as a
/// borrowed `ptr` + `len` pair (two i32 slots).
fn async_cb_wasm_params(returns: Option<&TypeRef>) -> Vec<&'static str> {
    let mut params = vec!["i32", "i32"];
    let Some(ty) = returns else {
        return params;
    };
    if is_buffered(ty) {
        params.push("i32");
        params.push("i32");
        return params;
    }
    match ty {
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
        | TypeRef::Interface(_)
        | TypeRef::TypedHandle(_)
        | TypeRef::Iterator(_)
        // Only `Interface?` reaches here: a nullable object pointer.
        | TypeRef::Optional(_) => {
            params.push("i32");
        }
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle => {
            params.push("i64");
        }
        TypeRef::F32 => {
            params.push("f32");
        }
        TypeRef::F64 => {
            params.push("f64");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            params.push("i32");
            params.push("i32");
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
/// borrowing contract: string, byte, and value buffers are producer-owned and
/// valid only for the callback's duration, so they are deep-copied or decoded
/// out of wasm memory and never freed here. Owned interface results are the
/// exception: the callback receives ownership and the pointer is adopted by
/// its wrapper class.
fn emit_async_unwrap(
    out: &mut String,
    indent: &str,
    ret: Option<&TypeRef>,
    mk_err: Option<&str>,
    module: &str,
) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let base = match mk_err {
        Some(factory) => format!("resolve, reject, mkErr: {factory}"),
        None => "resolve, reject".to_string(),
    };
    let plain = format!("_asyncContexts.set(ctxId, {{ {base} }});");
    let Some(ret) = ret else {
        w.line(plain);
        out.push_str(&w.finish());
        return;
    };
    let open = format!("_asyncContexts.set(ctxId, {{ {base}, unwrap: ");
    if is_buffered(ret) {
        // Borrowed: copy the encoding out of wasm memory inside the callback,
        // decode, never free (the producer reclaims it afterwards).
        w.block(format!("{open}(w, ptr, len) => {{"), "} });", |w| {
            w.line(
                "const _b = ptr === 0 || len === 0 ? new Uint8Array(0) : new Uint8Array(w.memory.buffer, ptr, len).slice();",
            );
            w.line("const _rd = new _BufReader(_b);");
            w.line(format!("const _v = {};", buf_read_expr(ret, module, "_rd")));
            w.line("_rd.end();");
            w.line("return _v;");
        });
        out.push_str(&w.finish());
        return;
    }
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
        | TypeRef::TypedHandle(_)
        | TypeRef::Enum(_) => {
            w.line(plain);
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            // Borrowed: copy out of wasm memory, never free.
            w.line(format!("{open}(w, p) => _readCStr(w, p) }});"));
        }
        TypeRef::Interface(name) => {
            let cls = local_type_name(name);
            w.line(format!("{open}(w, h) => {cls}._wrap(h) }});"));
        }
        // Only `Interface?` reaches here: a nullable owned object pointer.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(name) => {
                let cls = local_type_name(name);
                w.line(format!(
                    "{open}(w, h) => h === 0 ? null : {cls}._wrap(h) }});"
                ));
            }
            _ => unreachable!("non-interface optionals are buffered"),
        },
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            // Borrowed: slice() deep-copies out of wasm memory, never free.
            w.line(format!(
                "{open}(w, ptr, len) => ptr === 0 || len === 0 ? new Uint8Array(0) : new Uint8Array(w.memory.buffer, ptr, len).slice() }});"
            ));
        }
        TypeRef::Iterator(_) => {
            w.line(plain);
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
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
    mb: &ModuleBinding,
    f: &FnBinding,
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
        js_err_factory(f, mb.error.as_ref()).as_deref(),
        &mb.path,
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
            &mb.path,
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
/// internal `_wrap(handle)` adopts an owned handle (returns, iterator
/// elements) without invoking the constructor, and `free()` releases the
/// handle exactly once via the destroy symbol.
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
                            &module.path,
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

        // Internal: adopt an owned handle (returns, iterator elements)
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
            emit_js_callable(
                &mut tmp,
                module,
                c,
                JsDecl::Static,
                None,
                &inner,
                emscripten,
            );
            w.raw(tmp);
        }
        for m in &i.methods {
            let mut tmp = String::new();
            emit_js_callable(
                &mut tmp,
                module,
                m,
                JsDecl::Method,
                Some("this._handle"),
                &inner,
                emscripten,
            );
            w.raw(tmp);
        }
        for s in &i.statics {
            let mut tmp = String::new();
            emit_js_callable(
                &mut tmp,
                module,
                s,
                JsDecl::Static,
                None,
                &inner,
                emscripten,
            );
            w.raw(tmp);
        }
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
            version: "0.6.0".into(),
            modules: vec![],
            generators: None,
            package: None,
        }
    }

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.6.0".into(),
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

    fn field(name: &str, ty: TypeRef) -> StructField {
        StructField {
            name: name.into(),
            ty,
            doc: None,
            default: None,
        }
    }

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

    fn param(name: &str, ty: TypeRef) -> Param {
        Param {
            name: name.into(),
            ty,
            mutable: false,
            doc: None,
        }
    }

    fn str_param(name: &str) -> Param {
        param(name, TypeRef::StringUtf8)
    }

    fn module(name: &str) -> Module {
        Module {
            name: name.into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }
    }

    fn sample_api() -> Api {
        make_api(vec![Module {
            functions: vec![Function {
                name: "add".into(),
                params: vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
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
                fields: vec![field("x", TypeRef::F64), field("y", TypeRef::F64)],
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
            ..module("math")
        }])
    }

    /// An API with a callback + listener, delivered synchronously through a
    /// long-lived function-table trampoline in the standard loader (and
    /// stubbed in Emscripten mode).
    fn listener_api() -> Api {
        make_api(vec![Module {
            functions: vec![member("send", vec![str_param("text")], None, false, false)],
            callbacks: vec![weaveffi_ir::ir::CallbackDef {
                name: "OnMessage".into(),
                params: vec![str_param("message")],
                doc: None,
            }],
            listeners: vec![weaveffi_ir::ir::ListenerDef {
                name: "message_listener".into(),
                event_callback: "OnMessage".into(),
                doc: None,
            }],
            ..module("events")
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
        assert!(readme.contains("buffered values"), "{readme}");
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
    fn readme_documents_records_as_plain_objects() {
        let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Records"));
        assert!(readme.contains("plain JavaScript objects"));
        assert!(readme.contains("value-buffer format"));
        assert!(readme.contains("nothing to free"));
        assert!(!readme.contains("opaque handles"));
    }

    #[test]
    fn readme_documents_enums() {
        let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Enums"));
        assert!(readme.contains("`i32` values"));
        assert!(readme.contains("discriminant"));
        assert!(readme.contains("tagged by variant name"));
    }

    #[test]
    fn readme_documents_optionals() {
        let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Optionals"));
        assert!(readme.contains("`null`"));
        assert!(readme.contains("presence flag"));
        assert!(readme.contains("nullable object pointer"));
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
    fn readme_documents_lists_and_maps() {
        let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("### Lists and Maps"));
        assert!(readme.contains("serialized in a value buffer"));
        assert!(readme.contains("pointer + length"));
    }

    #[test]
    fn readme_documents_error_struct_layout_and_payloads() {
        let readme = readme_for(&empty_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("16 bytes on wasm32"), "{readme}");
        assert!(readme.contains("payload_ptr"), "{readme}");
        assert!(
            readme.contains("decodes them from the error's value"),
            "{readme}"
        );
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
        assert!(js.contains("Record: plain objects in and out (serialized automatically)."));
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
        assert!(js.contains("Objects   -> i32 pointer into linear memory (0 = null/absent)"));
        assert!(js.contains("Enums     -> i32 discriminant value"));
        assert!(js.contains("Bytes     -> i32 data pointer + i32 length (out_len for returns)"));
        assert!(js.contains("Buffered  -> records, rich enums, optionals, lists, and maps cross"));
        assert!(js.contains("as one value buffer: i32 pointer + i32 length"));
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
    fn api_reference_struct_fields() {
        let readme = readme_for(&sample_api(), "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("##### `Point`"));
        assert!(readme.contains("A plain JS object, serialized in a value buffer"));
        assert!(readme.contains("| `x` | `f64` |"));
        assert!(readme.contains("| `y` | `f64` |"));
        // Records declare no C symbols: no getters, no create, no destroy.
        assert!(!readme.contains("weaveffi_math_Point"), "{readme}");
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
        // A string is a single NUL-terminated C string pointer.
        assert_eq!(wasm_type(&TypeRef::StringUtf8), "i32");
        assert_eq!(wasm_type(&TypeRef::Bytes), "i32, i32");
        assert_eq!(wasm_type(&TypeRef::Handle), "i64");
        // Buffered value types cross as one ptr + len pair.
        assert_eq!(wasm_type(&TypeRef::Record("Foo".into())), "i32, i32");
        assert_eq!(wasm_type(&TypeRef::RichEnum("Shape".into())), "i32, i32");
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
            "i32, i32"
        );
        assert_eq!(
            wasm_type(&TypeRef::Optional(Box::new(TypeRef::Record("Foo".into())))),
            "i32, i32"
        );
        assert_eq!(
            wasm_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "i32, i32"
        );
        // The one non-buffered optional: a nullable interface pointer.
        assert_eq!(
            wasm_type(&TypeRef::Optional(Box::new(TypeRef::Interface("S".into())))),
            "i32"
        );
        assert_eq!(wasm_type(&TypeRef::TypedHandle("Contact".into())), "i32");
        assert_eq!(wasm_type(&TypeRef::Interface("Store".into())), "i32");
    }

    #[test]
    fn wasm_type_note_covers_all_variants() {
        assert_eq!(wasm_type_note(&TypeRef::I32), "native Wasm i32");
        assert_eq!(wasm_type_note(&TypeRef::U32), "unsigned mapped to i32");
        assert_eq!(wasm_type_note(&TypeRef::Bool), "0 = false, 1 = true");
        assert_eq!(
            wasm_type_note(&TypeRef::StringUtf8),
            "NUL-terminated C string pointer"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Record("X".into())),
            "value buffer: ptr + len in linear memory"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::RichEnum("X".into())),
            "value buffer: ptr + len in linear memory"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Enum("E".into())),
            "variant discriminant"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::Record("S".into())))),
            "value buffer: ptr + len in linear memory"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "value buffer: ptr + len in linear memory"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::Interface("S".into())))),
            "nullable object pointer, 0 = absent"
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

    /// A `contacts` module with a string-to-optional-record lookup, reused by
    /// the API-reference and marshalling tests.
    fn contacts_api() -> Api {
        make_api(vec![Module {
            functions: vec![member(
                "find",
                vec![str_param("name")],
                Some(TypeRef::Optional(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
                false,
                false,
            )],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    field("id", TypeRef::I32),
                    field("name", TypeRef::StringUtf8),
                ],
            }],
            ..module("contacts")
        }])
    }

    #[test]
    fn api_reference_complex_types() {
        let readme = readme_for(&contacts_api(), "weaveffi", "weaveffi.yml", false);
        assert!(
            readme.contains("| `name` | `string` | `i32` | NUL-terminated C string pointer |"),
            "{readme}"
        );
        assert!(
            readme.contains(
                "| _returns_ | `Contact?` | `i32, i32` | value buffer: ptr + len in linear memory |"
            ),
            "{readme}"
        );
        assert!(
            !readme.contains("weaveffi_contacts_Contact_get"),
            "{readme}"
        );
    }

    #[test]
    fn api_reference_void_return() {
        let api = make_api(vec![Module {
            functions: vec![member("print", vec![str_param("msg")], None, false, false)],
            ..module("io")
        }]);
        let readme = readme_for(&api, "weaveffi", "weaveffi.yml", false);
        assert!(readme.contains("-> void`"));
        assert!(!readme.contains("_returns_"));
    }

    #[test]
    fn api_reference_multiple_modules() {
        let api = make_api(vec![module("math"), module("io")]);
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
        // The record is a plain value shape: codecs at module scope, no
        // wrapper class, no getters, no module-object factory.
        assert!(js.contains("function _write_math_Point(w, v) {"), "{js}");
        assert!(js.contains("function _read_math_Point(r) {"), "{js}");
        assert!(js.contains("w.f64(v.x);"), "{js}");
        assert!(js.contains("v.x = r.f64();"), "{js}");
        assert!(!js.contains("class Point"), "{js}");
        assert!(!js.contains("get x()"), "{js}");
        assert!(!js.contains("Point: {"), "{js}");
        assert!(js.contains("export const Color = Object.freeze("));
        assert!(js.contains("Red: 0"));
        assert!(js.contains("Green: 1"));
        assert!(js.contains("Blue: 2"));
        assert!(js.contains("_raw: wasm"));
        assert!(js.contains("math: {"));
    }

    #[test]
    fn wasm_js_emits_buffer_runtime_when_records_present() {
        let js = js_stub_for(
            &sample_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        assert!(js.contains("class _BufWriter {"), "{js}");
        assert!(js.contains("class _BufReader {"), "{js}");
        // The reader is strict: little-endian DataView reads plus validation.
        assert!(js.contains("truncated"), "{js}");
        assert!(js.contains("bool byte out of range"), "{js}");
        assert!(js.contains("option flag byte out of range"), "{js}");
        assert!(
            js.contains("length prefix exceeds remaining buffer"),
            "{js}"
        );
        assert!(js.contains("trailing bytes after value"), "{js}");
        assert!(js.contains("string is not valid UTF-8"), "{js}");
        assert!(js.contains("{ fatal: true }"), "{js}");
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
        // Records are plain object interfaces: mutable fields, no free().
        assert!(dts.contains("export interface Point"));
        assert!(dts.contains("  x: number;"));
        assert!(dts.contains("  y: number;"));
        assert!(!dts.contains("readonly x"));
        assert!(!dts.contains("free(): void;"));
        assert!(dts.contains("export declare const Color"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn wasm_js_has_string_helpers() {
        let api = make_api(vec![Module {
            functions: vec![member(
                "greet",
                vec![str_param("name")],
                Some(TypeRef::StringUtf8),
                false,
                false,
            )],
            ..module("greeting")
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
        // The slot is the 16-byte error struct with payload fields.
        assert!(js.contains("wasm.weaveffi_alloc(16)"));
        assert!(js.contains("wasm.weaveffi_dealloc(errPtr, 16);"));
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
            functions: vec![member(
                "get_info",
                vec![param("contact", TypeRef::TypedHandle("Contact".into()))],
                None,
                false,
                false,
            )],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![field("name", TypeRef::StringUtf8)],
            }],
            ..module("contacts")
        }]);
        let dts = dts_for(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
        assert!(
            dts.contains("contact: number"),
            "TypedHandle is an opaque i32 pointer surfaced as number: {dts}"
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
            js.contains("wasm.weaveffi_contacts_get_info(contact, _err)"),
            "TypedHandle passes through unwrapped: {js}"
        );
        assert!(!js.contains("contact._handle"), "{js}");
    }

    #[test]
    fn wasm_deeply_nested_optional() {
        let api = make_api(vec![Module {
            functions: vec![member(
                "process",
                vec![param(
                    "data",
                    TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                        Box::new(TypeRef::Record("Contact".into())),
                    ))))),
                )],
                None,
                false,
                false,
            )],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![field("name", TypeRef::StringUtf8)],
            }],
            ..module("edge")
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
            functions: vec![member(
                "process",
                vec![param(
                    "scores",
                    TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::List(Box::new(TypeRef::I32))),
                    ),
                )],
                None,
                false,
                false,
            )],
            ..module("edge")
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
            functions: vec![member(
                "process",
                vec![param(
                    "contacts",
                    TypeRef::Map(
                        Box::new(TypeRef::Enum("Color".into())),
                        Box::new(TypeRef::Record("Contact".into())),
                    ),
                )],
                None,
                false,
                false,
            )],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![field("name", TypeRef::StringUtf8)],
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
                ],
            }],
            ..module("edge")
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

    /// A one-function API returning a record, exercising both the buffered
    /// parameter staging and the buffered return decode.
    fn record_roundtrip_api() -> Api {
        make_api(vec![Module {
            functions: vec![member(
                "save",
                vec![param("contact", TypeRef::Record("Contact".into()))],
                Some(TypeRef::Record("Contact".into())),
                false,
                false,
            )],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![field("name", TypeRef::StringUtf8)],
            }],
            ..module("contacts")
        }])
    }

    #[test]
    fn buffered_param_staged_like_bytes_and_deallocated() {
        let js = js_for_api(&record_roundtrip_api());
        // Encode into a _BufWriter, stage via _bytes (weaveffi_alloc + copy),
        // pass (ptr, len), dealloc after the call.
        assert!(js.contains("const a0_w = new _BufWriter();"), "{js}");
        assert!(
            js.contains("_write_contacts_Contact(a0_w, contact);"),
            "{js}"
        );
        assert!(
            js.contains("const [a0_p, a0_l] = _bytes(wasm, a0_w.finish());"),
            "{js}"
        );
        assert!(
            js.contains("wasm.weaveffi_contacts_save(a0_p, a0_l, _lp, _err);"),
            "{js}"
        );
        let call = js.find("wasm.weaveffi_contacts_save(").unwrap();
        let dealloc = js.find("wasm.weaveffi_dealloc(a0_p, a0_l);").unwrap();
        assert!(call < dealloc, "staged encoding freed after the call: {js}");
    }

    #[test]
    fn buffered_return_read_decoded_and_freed() {
        let js = js_for_api(&record_roundtrip_api());
        // The trailing out_len slot is allocated before the call and read
        // (then released) afterwards.
        assert!(js.contains("const _lp = wasm.weaveffi_alloc(4);"), "{js}");
        assert!(
            js.contains("const _len = new DataView(wasm.memory.buffer).getUint32(_lp, true);"),
            "{js}"
        );
        assert!(js.contains("wasm.weaveffi_dealloc(_lp, 4);"), "{js}");
        // The buffer is copied out and freed by _takeBytes, then decoded
        // strictly (end() rejects trailing bytes).
        assert!(
            js.contains("const _rd = new _BufReader(_takeBytes(wasm, _r, _len));"),
            "{js}"
        );
        assert!(
            js.contains("const _out = _read_contacts_Contact(_rd);"),
            "{js}"
        );
        assert!(js.contains("_rd.end();"), "{js}");
        assert!(js.contains("function _takeBytes(wasm, ptr, len)"), "{js}");
        assert!(js.contains("wasm.weaveffi_free_bytes(ptr, len);"), "{js}");
        // Errors are checked before the result is decoded, and no wrapper
        // class exists for the record.
        let check = js.find("_checkErr(wasm, _err)").expect("error check");
        let decode = js
            .find("const _out = _read_contacts_Contact(_rd);")
            .expect("record decode");
        assert!(check < decode, "errors checked before decoding: {js}");
        assert!(!js.contains("class Contact"), "{js}");
    }

    #[test]
    fn optional_record_return_uses_presence_flag() {
        let js = js_for_api(&contacts_api());
        // An optional record is buffered: a one-byte flag, then the value.
        assert!(
            js.contains("const _out = (_rd.flag() ? _read_contacts_Contact(_rd) : null);"),
            "{js}"
        );
    }

    #[test]
    fn optional_scalar_return_decodes_from_buffer() {
        let js = js_for_api(&returning_api(
            TypeRef::Optional(Box::new(TypeRef::I32)),
            false,
        ));
        assert!(
            js.contains("const _out = (_rd.flag() ? _rd.i32() : null);"),
            "{js}"
        );
        // The old boxed-scalar protocol is gone.
        assert!(!js.contains("wasm.weaveffi_free_bytes(_r, 4);"), "{js}");
    }

    #[test]
    fn list_return_decodes_from_buffer() {
        let js = js_for_api(&returning_api(
            TypeRef::List(Box::new(TypeRef::StringUtf8)),
            false,
        ));
        assert!(
            js.contains(
                "const _out = (() => { const _n = _rd.len(); const _arr = []; for (let _i = 0; _i < _n; _i++) _arr.push(_rd.str()); return _arr; })();"
            ),
            "{js}"
        );
        // No parallel-array protocol remains.
        assert!(!js.contains("_takeStrArray"), "{js}");
    }

    #[test]
    fn map_return_decodes_from_buffer() {
        let js = js_for_api(&returning_api(
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            false,
        ));
        assert!(
            js.contains(
                "const _out = (() => { const _n = _rd.len(); const _obj = {}; for (let _i = 0; _i < _n; _i++) { const _k = _rd.str(); _obj[_k] = _rd.i32(); } return _obj; })();"
            ),
            "{js}"
        );
        assert!(!js.contains("_ka"), "no parallel key array remains: {js}");
    }

    #[test]
    fn list_param_serializes_elements_in_order() {
        let api = make_api(vec![Module {
            functions: vec![member(
                "sum",
                vec![param("xs", TypeRef::List(Box::new(TypeRef::I32)))],
                Some(TypeRef::I64),
                false,
                false,
            )],
            ..module("m")
        }]);
        let js = js_for_api(&api);
        assert!(js.contains("const _a1 = xs || [];"), "{js}");
        assert!(js.contains("a0_w.len(_a1.length);"), "{js}");
        assert!(js.contains("for (const _e1 of _a1) {"), "{js}");
        assert!(js.contains("a0_w.i32(_e1);"), "{js}");
    }

    #[test]
    fn map_param_accepts_map_instances_and_plain_objects() {
        let api = make_api(vec![Module {
            functions: vec![member(
                "load",
                vec![param(
                    "scores",
                    TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                )],
                None,
                false,
                false,
            )],
            ..module("m")
        }]);
        let js = js_for_api(&api);
        assert!(
            js.contains(
                "const _m1 = _s1 instanceof Map ? [..._s1.entries()] : Object.entries(_s1);"
            ),
            "{js}"
        );
        assert!(js.contains("a0_w.len(_m1.length);"), "{js}");
        assert!(js.contains("for (const [_k1, _v1] of _m1) {"), "{js}");
        assert!(js.contains("a0_w.str(_k1);"), "{js}");
        assert!(js.contains("a0_w.i32(_v1);"), "{js}");
    }

    #[test]
    fn optional_param_writes_presence_flag() {
        let api = make_api(vec![Module {
            functions: vec![member(
                "set_timeout",
                vec![param("ms", TypeRef::Optional(Box::new(TypeRef::I32)))],
                None,
                false,
                false,
            )],
            ..module("m")
        }]);
        let js = js_for_api(&api);
        assert!(
            js.contains("if (ms === null || ms === undefined) {"),
            "{js}"
        );
        assert!(js.contains("a0_w.flag(false);"), "{js}");
        assert!(js.contains("a0_w.flag(true);"), "{js}");
        assert!(js.contains("a0_w.i32(ms);"), "{js}");
    }

    #[test]
    fn wasm_async_returns_promise() {
        let api = make_api(vec![Module {
            functions: vec![Function {
                name: "compute".into(),
                params: vec![param("x", TypeRef::I32)],
                returns: Some(TypeRef::I32),
                doc: None,
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            ..module("math")
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
            functions: vec![Function {
                name: "compute".into(),
                params: vec![param("x", TypeRef::I32)],
                returns: Some(TypeRef::I32),
                doc: None,
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            ..module("math")
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
            functions: vec![
                Function {
                    name: "compute".into(),
                    params: vec![param("x", TypeRef::I32)],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    throws: false,
                    r#async: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                member(
                    "add",
                    vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                    Some(TypeRef::I32),
                    false,
                    false,
                ),
            ],
            ..module("math")
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
            functions: vec![member("outer_fn", vec![], Some(TypeRef::I32), false, false)],
            modules: vec![Module {
                functions: vec![member("inner_fn", vec![], Some(TypeRef::I32), false, false)],
                ..module("child")
            }],
            ..module("parent")
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
            ..module("docs")
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
    /// resolved to `TypeRef::RichEnum`) so the value-buffer marshalling is
    /// exercised too.
    fn rich_enum_api() -> Api {
        make_api(vec![Module {
            functions: vec![
                member(
                    "describe",
                    vec![param("shape", TypeRef::RichEnum("Shape".into()))],
                    Some(TypeRef::StringUtf8),
                    false,
                    false,
                ),
                member(
                    "scale",
                    vec![
                        param("shape", TypeRef::RichEnum("Shape".into())),
                        param("factor", TypeRef::F64),
                    ],
                    Some(TypeRef::RichEnum("Shape".into())),
                    false,
                    false,
                ),
                member(
                    "sum_bytes",
                    vec![param("values", TypeRef::List(Box::new(TypeRef::U8)))],
                    Some(TypeRef::U64),
                    false,
                    false,
                ),
            ],
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
            ..module("shapes")
        }])
    }

    #[test]
    fn wasm_rich_enum_emits_buffer_codecs() {
        let js = js_stub_for(
            &rich_enum_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        // The writer switches on the string tag and packs the i32 discriminant
        // plus the active variant's fields in declaration order.
        assert!(js.contains("function _write_shapes_Shape(w, v) {"), "{js}");
        assert!(js.contains("switch (v.tag) {"), "{js}");
        assert!(js.contains("case \"Empty\": {"), "{js}");
        assert!(js.contains("case \"Circle\": {"), "{js}");
        assert!(js.contains("w.i32(1);"), "{js}");
        assert!(js.contains("w.f64(v.radius);"), "{js}");
        assert!(js.contains("w.f32(v.width);"), "{js}");
        assert!(js.contains("w.str(v.label);"), "{js}");
        assert!(js.contains("w.u8(v.count);"), "{js}");
        assert!(js.contains("unknown Shape variant tag"), "{js}");
        // The reader switches on the numeric tag and rebuilds the tagged
        // plain object.
        assert!(js.contains("function _read_shapes_Shape(r) {"), "{js}");
        assert!(js.contains("const _tag = r.i32();"), "{js}");
        assert!(js.contains("return { tag: \"Empty\" };"), "{js}");
        assert!(js.contains("const v = { tag: \"Circle\" };"), "{js}");
        assert!(js.contains("v.radius = r.f64();"), "{js}");
        assert!(js.contains("unknown Shape tag"), "{js}");
        // No handle-wrapper machinery remains.
        assert!(!js.contains("class Shape"), "{js}");
        assert!(!js.contains("Shape.Tag"), "{js}");
        assert!(!js.contains("Shape_destroy"), "{js}");
        assert!(!js.contains("Shape_Circle_new"), "{js}");
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
        // The rich enum must NOT be emitted as a by-value discriminant object.
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
    fn wasm_rich_enum_function_marshals_value_buffer() {
        let js = js_stub_for(
            &rich_enum_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi",
            "weaveffi.yml",
            "weaveffi_wasm.js",
            false,
        );
        // A rich enum crosses the ABI as a value buffer: encoded on the way
        // in, staged like bytes, decoded on the way out.
        assert!(js.contains("_write_shapes_Shape(a0_w, shape);"), "{js}");
        assert!(
            js.contains("wasm.weaveffi_shapes_describe(a0_p, a0_l, _err);"),
            "describe must pass the staged (ptr, len) pair: {js}"
        );
        assert!(
            js.contains("wasm.weaveffi_shapes_scale(a0_p, a0_l, factor, _lp, _err);"),
            "scale must pass the pair, the scalar, and the out_len slot: {js}"
        );
        assert!(
            js.contains("const _out = _read_shapes_Shape(_rd);"),
            "scale must decode the returned buffer: {js}"
        );
        // Errors are checked before the result is decoded.
        let check = js
            .find("_checkErr(wasm, _err)")
            .expect("scale should check the error slot");
        let decode = js
            .find("const _out = _read_shapes_Shape(_rd);")
            .expect("scale should decode the result");
        assert!(
            check < decode,
            "errors must be checked before decoding: {js}"
        );
    }

    #[test]
    fn wasm_rich_enum_dts_union() {
        let dts = dts_for(
            &rich_enum_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
        // A discriminated union of plain object shapes, keyed by `tag`.
        assert!(dts.contains("export type Shape ="), "{dts}");
        assert!(dts.contains("| { tag: \"Empty\" }"), "{dts}");
        assert!(
            dts.contains("| { tag: \"Circle\"; radius: number }"),
            "{dts}"
        );
        assert!(
            dts.contains("| { tag: \"Rectangle\"; width: number; height: number }"),
            "{dts}"
        );
        assert!(
            dts.contains("| { tag: \"Labeled\"; label: string; count: number };"),
            "{dts}"
        );
        // Not a class, not a by-value const map.
        assert!(!dts.contains("export declare class Shape"), "{dts}");
        assert!(!dts.contains("export declare const Shape"), "{dts}");
        assert!(
            dts.contains("scale(shape: Shape, factor: number): Shape"),
            "functions should reference the union type: {dts}"
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
            functions: vec![Function {
                name: "compute".into(),
                params: vec![param("x", TypeRef::I32)],
                returns: Some(TypeRef::I32),
                doc: None,
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            ..module("math")
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
        // Records declare no C symbols, so nothing Point-related is bound.
        assert!(
            !js.contains("m['_weaveffi_math_Point"),
            "records must not bind any symbols: {js}"
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
        assert!(
            js.contains("weaveffi_free_bytes: m['_acme_free_bytes'],"),
            "free_bytes must map to the prefixed export: {js}"
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

    // --- Interfaces, typed errors, throws split, naming ---

    /// A kvstore-shaped module: a `Store` interface (canonical `new` plus an
    /// `open` factory, sync/iterator/async methods, one static), a `KvError`
    /// domain, and one non-throwing free function.
    fn kv_api() -> Api {
        make_api(vec![Module {
            functions: vec![member("flush_all", vec![], None, false, false)],
            errors: Some(weaveffi_ir::ir::ErrorDomain {
                name: "KvError".into(),
                codes: vec![
                    weaveffi_ir::ir::ErrorCode {
                        name: "KeyNotFound".into(),
                        code: 1001,
                        message: "key not found".into(),
                        doc: None,
                        fields: vec![],
                    },
                    weaveffi_ir::ir::ErrorCode {
                        name: "StoreFull".into(),
                        code: 1003,
                        message: "store is full".into(),
                        doc: None,
                        fields: vec![],
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
                        vec![str_param("key"), param("ttl_seconds", TypeRef::I64)],
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
            ..module("kv")
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
        // Disposal: free() releases exactly once.
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
        // The factory takes the payload slots (unused here: no code declares
        // fields) and maps unknown codes to the generic brand error.
        assert!(
            js.contains("function _kvErrorFrom(wasm, code, message, payloadPtr, payloadLen) {"),
            "{js}"
        );
        assert!(js.contains("const _cls = _KV_ERROR_CODES[code];"), "{js}");
        assert!(js.contains("new WeaveFFIError(code, message);"), "{js}");
    }

    #[test]
    fn throws_split_selects_typed_or_generic_checker() {
        let js = kv_js();
        // Throwing members route the out-err slot through the domain checker,
        // which reads all four error-struct fields (code, message, payload).
        assert!(
            js.contains("function _checkKvError(wasm, errPtr) {"),
            "{js}"
        );
        assert!(js.contains("_checkKvError(wasm, _err);"), "{js}");
        assert!(
            js.contains(
                "const _e = _kvErrorFrom(wasm, code, msg, dv.getUint32(errPtr + 8, true), dv.getUint32(errPtr + 12, true));"
            ),
            "{js}"
        );
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
        // The borrowed-error checker hands the payload slots to the factory.
        assert!(
            js.contains(
                "if (mkErr) throw mkErr(wasm, code, msg, dv.getUint32(errPtr + 8, true), dv.getUint32(errPtr + 12, true));"
            ),
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
        assert!(
            readme.contains("| `KeyNotFound` | `1001` | key not found | (none) |"),
            "{readme}"
        );
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

    #[test]
    fn optional_interface_stays_nullable_pointer() {
        let api = make_api(vec![Module {
            interfaces: vec![weaveffi_ir::ir::InterfaceDef {
                name: "Session".into(),
                doc: None,
                constructors: vec![member("new", vec![], None, false, false)],
                methods: vec![member(
                    "find",
                    vec![param(
                        "other",
                        TypeRef::Optional(Box::new(TypeRef::Interface("Session".into()))),
                    )],
                    Some(TypeRef::Optional(Box::new(TypeRef::Interface(
                        "Session".into(),
                    )))),
                    false,
                    false,
                )],
                statics: vec![],
            }],
            ..module("net")
        }]);
        let js = js_for_api(&api);
        // An optional interface is the one non-buffered optional: a nullable
        // borrowed pointer in, a nullable owned pointer out.
        assert!(js.contains("(other ? other._handle : 0)"), "{js}");
        assert!(
            js.contains("return _r === 0 ? null : Session._wrap(_r);"),
            "{js}"
        );
        let dts = dts_for(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
        assert!(
            dts.contains("find(other: Session | null): Session | null;"),
            "{dts}"
        );
    }

    // --- Structured error payloads ---

    /// An error domain where one code declares payload fields, plus a
    /// throwing function so the checker path is generated.
    fn payload_api() -> Api {
        make_api(vec![Module {
            functions: vec![member("login", vec![str_param("name")], None, true, false)],
            errors: Some(weaveffi_ir::ir::ErrorDomain {
                name: "AuthError".into(),
                codes: vec![
                    weaveffi_ir::ir::ErrorCode {
                        name: "LockedOut".into(),
                        code: 1001,
                        message: "locked out".into(),
                        doc: None,
                        fields: vec![
                            field("retry_after_secs", TypeRef::I32),
                            field("user", TypeRef::StringUtf8),
                        ],
                    },
                    weaveffi_ir::ir::ErrorCode {
                        name: "Denied".into(),
                        code: 1002,
                        message: "denied".into(),
                        doc: None,
                        fields: vec![],
                    },
                ],
            }),
            ..module("auth")
        }])
    }

    #[test]
    fn error_payload_fields_decoded_and_attached() {
        let js = js_for_api(&payload_api());
        // The factory decodes the borrowed payload buffer per code and
        // attaches the fields as properties on the thrown error.
        assert!(
            js.contains("function _authErrorFrom(wasm, code, message, payloadPtr, payloadLen) {"),
            "{js}"
        );
        assert!(js.contains("case 1001: {"), "{js}");
        assert!(js.contains("_e.retry_after_secs = _rd.i32();"), "{js}");
        assert!(js.contains("_e.user = _rd.str();"), "{js}");
        assert!(js.contains("_rd.end();"), "{js}");
        // The payload-free code has no decode arm.
        assert!(!js.contains("case 1002: {"), "{js}");
        // The checker hands the payload slots (offsets 8 and 12 of the
        // 16-byte struct) to the factory before clearing the error.
        assert!(
            js.contains("function _checkAuthError(wasm, errPtr) {"),
            "{js}"
        );
        let checker = js
            .split("function _checkAuthError(wasm, errPtr) {")
            .nth(1)
            .and_then(|s| s.split("\n}").next())
            .expect("checker body");
        let decode = checker
            .find("const _e = _authErrorFrom(wasm, code, msg, dv.getUint32(errPtr + 8, true), dv.getUint32(errPtr + 12, true));")
            .expect("checker decodes payload");
        let clear = checker
            .find("wasm.weaveffi_error_clear(errPtr);")
            .expect("clear");
        assert!(
            decode < clear,
            "payload must be decoded before error_clear frees it: {checker}"
        );
    }

    #[test]
    fn error_payload_fields_declared_in_dts() {
        let dts = dts_for(
            &payload_api(),
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
            false,
        );
        assert!(
            dts.contains("export declare class LockedOut extends AuthError {"),
            "{dts}"
        );
        assert!(dts.contains("readonly retry_after_secs: number;"), "{dts}");
        assert!(dts.contains("readonly user: string;"), "{dts}");
    }

    #[test]
    fn error_payload_fields_listed_in_readme() {
        let readme = readme_for(&payload_api(), "weaveffi", "weaveffi.yml", false);
        assert!(
            readme.contains(
                "| `LockedOut` | `1001` | locked out | `retry_after_secs: i32`, `user: string` |"
            ),
            "{readme}"
        );
        assert!(
            readme.contains("| `Denied` | `1002` | denied | (none) |"),
            "{readme}"
        );
    }

    // --- Buffered iterators and callbacks ---

    #[test]
    fn iterator_buffered_elements_decode_then_free() {
        let api = make_api(vec![Module {
            functions: vec![member(
                "scan",
                vec![],
                Some(TypeRef::Iterator(Box::new(TypeRef::Record("Entry".into())))),
                false,
                false,
            )],
            structs: vec![StructDef {
                name: "Entry".into(),
                doc: None,
                fields: vec![field("id", TypeRef::I32)],
            }],
            ..module("m")
        }]);
        let js = js_for_api(&api);
        // A buffered element writes through two out slots (ptr at p, len at
        // p + 4), so the slot is 8 bytes and next threads both pointers.
        assert!(
            js.contains("return new _WeaveFFIIterator(wasm, _it, 8,"),
            "{js}"
        );
        assert!(js.contains("_next(it, slot, slot + 4, ep),"), "{js}");
        // Each element is copied out and freed (via _takeBytes ->
        // weaveffi_free_bytes), then strictly decoded.
        assert!(
            js.contains(
                "const _rd = new _BufReader(_takeBytes(w, dv.getUint32(p, true), dv.getUint32(p + 4, true)));"
            ),
            "{js}"
        );
        assert!(js.contains("const _v = _read_m_Entry(_rd);"), "{js}");
        assert!(js.contains("_rd.end(); return _v;"), "{js}");
    }

    #[test]
    fn callback_buffered_argument_decoded_borrowed() {
        let api = make_api(vec![Module {
            structs: vec![StructDef {
                name: "Msg".into(),
                doc: None,
                fields: vec![field("text", TypeRef::StringUtf8)],
            }],
            callbacks: vec![weaveffi_ir::ir::CallbackDef {
                name: "OnMessage".into(),
                params: vec![param("msg", TypeRef::Record("Msg".into()))],
                doc: None,
            }],
            listeners: vec![weaveffi_ir::ir::ListenerDef {
                name: "message_listener".into(),
                event_callback: "OnMessage".into(),
                doc: None,
            }],
            ..module("events")
        }]);
        let js = js_for_api(&api);
        // The buffered argument occupies two i32 slots plus the context slot.
        assert!(
            js.contains("_registerTrampoline(_table, ['i32', 'i32', 'i32'],"),
            "{js}"
        );
        // Borrowed: the encoding is copied out of wasm memory (never freed)
        // and decoded before the subscriber runs.
        assert!(
            js.contains(
                "const _p0_b = (a0 === 0 || a1 === 0) ? new Uint8Array(0) : new Uint8Array(wasm.memory.buffer, a0, a1).slice();"
            ),
            "{js}"
        );
        assert!(js.contains("const _p0 = _read_events_Msg(_p0_r);"), "{js}");
        assert!(js.contains("_p0_r.end();"), "{js}");
        assert!(js.contains("_l.callback(_p0);"), "{js}");
    }

    // --- Async completion contract: borrowed buffers are copied, not freed ---

    /// A one-module API with a single free function of the given return type.
    fn returning_api(ret: TypeRef, is_async: bool) -> Api {
        make_api(vec![Module {
            functions: vec![member("get_it", vec![], Some(ret), false, is_async)],
            ..module("m")
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
    fn async_buffered_result_decoded_inside_callback_not_freed() {
        let js = js_for_api(&returning_api(
            TypeRef::List(Box::new(TypeRef::StringUtf8)),
            true,
        ));
        // The borrowed value buffer arrives as (ptr, len): the callback copies
        // it out of wasm memory, decodes, and never frees it.
        assert!(js.contains("unwrap: (w, ptr, len) => {"), "{js}");
        assert!(
            js.contains(
                "const _b = ptr === 0 || len === 0 ? new Uint8Array(0) : new Uint8Array(w.memory.buffer, ptr, len).slice();"
            ),
            "{js}"
        );
        assert!(js.contains("_arr.push(_rd.str())"), "{js}");
        assert!(js.contains("_rd.end();"), "{js}");
        // The unwrap closure binds the exports object as `w`, so any free
        // call inside it would spell `w.weaveffi_free_bytes` (the shared
        // `_takeBytes` helper legitimately frees owned returns elsewhere).
        assert!(
            !js.contains("w.weaveffi_free_bytes("),
            "the producer frees the borrowed result buffer: {js}"
        );
        assert!(
            !js.contains("unwrap: (w, ptr, len) => _takeBytes"),
            "async unwrap must not adopt the borrowed buffer: {js}"
        );
        // The completion callback carries (ctx, err, ptr, len): four i32s.
        assert!(js.contains("_cbPtr_i32_i32_i32_i32"), "{js}");
        assert!(
            js.contains("wasm.weaveffi_m_get_it_async(0, _cbPtr_i32_i32_i32_i32, ctxId);"),
            "{js}"
        );
    }

    #[test]
    fn async_optional_scalar_result_decodes_from_buffer() {
        let js = js_for_api(&returning_api(
            TypeRef::Optional(Box::new(TypeRef::I32)),
            true,
        ));
        assert!(
            js.contains("const _v = (_rd.flag() ? _rd.i32() : null);"),
            "async optional scalars decode from the borrowed buffer: {js}"
        );
    }

    #[test]
    fn async_record_result_decoded_from_buffer() {
        let api = make_api(vec![Module {
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
                fields: vec![field("id", TypeRef::I32)],
            }],
            ..module("m")
        }]);
        let js = js_for_api(&api);
        // A record result is a borrowed value buffer, decoded in the callback.
        assert!(js.contains("const _v = _read_m_Contact(_rd);"), "{js}");
        assert!(!js.contains("new Contact(w, h)"), "{js}");
    }

    #[test]
    fn async_interface_result_is_adopted() {
        let api = make_api(vec![Module {
            functions: vec![member(
                "connect",
                vec![],
                Some(TypeRef::Interface("Session".into())),
                false,
                true,
            )],
            interfaces: vec![weaveffi_ir::ir::InterfaceDef {
                name: "Session".into(),
                doc: None,
                constructors: vec![member("new", vec![], None, false, false)],
                methods: vec![],
                statics: vec![],
            }],
            ..module("net")
        }]);
        let js = js_for_api(&api);
        // An owned-object result transfers ownership: the callback adopts the
        // pointer into a wrapper whose free() calls the destroy symbol.
        assert!(
            js.contains("unwrap: (w, h) => Session._wrap(h) });"),
            "{js}"
        );
    }
}
