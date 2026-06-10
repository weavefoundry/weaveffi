//! WebAssembly binding generator for WeaveFFI.
//!
//! Emits a JavaScript loader stub and TypeScript declarations targeting a
//! `wasm32-unknown-unknown` cdylib build of the same Rust source.
//! Implements [`LanguageBackend`]; the shared driver bridges it into the
//! generator pipeline.

use std::collections::HashMap;
use std::fmt::Write as _;

use camino::Utf8Path;
use heck::ToUpperCamelCase;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::{self, TargetCapabilities};
use weaveffi_core::codegen::common::{
    emit_doc as common_emit_doc, walk_modules, walk_modules_with_path, DocCommentStyle,
};
use weaveffi_core::model::{
    BindingModel, CallShape, EnumBinding, FieldBinding, FnBinding, IteratorBinding,
    ListenerBinding, ModuleBinding, StructBinding,
};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::utils::{
    local_type_name, render_json_prelude, render_prelude, render_trailer, CommentStyle,
};
use weaveffi_ir::ir::{Api, Module, TypeRef};

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
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl WasmConfig {
    pub fn module_name(&self) -> &str {
        self.module_name.as_deref().unwrap_or(DEFAULT_MODULE_NAME)
    }

    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

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
                render_wasm_readme(api, prefix, input_basename),
            ),
            OutputFile::new(
                wasm_dir.join("package.json"),
                render_wasm_package_json(&package, &js_filename, &dts_filename, input_basename),
            ),
            OutputFile::new(
                wasm_dir.join(&js_filename),
                render_wasm_js_stub(api, module_name, prefix, input_basename, &js_filename),
            ),
            OutputFile::new(
                wasm_dir.join(&dts_filename),
                render_wasm_dts(api, module_name, input_basename, &dts_filename),
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
    }
}

fn render_wasm_readme(api: &Api, prefix: &str, input_basename: &str) -> String {
    let mut out = render_prelude(CommentStyle::Xml, input_basename);
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
    out.push_str("- `weaveffi_alloc(size: i32) -> i32` — allocate `size` bytes in linear memory\n");
    out.push_str("- `weaveffi_error_clear(err_ptr: i32)` — clear and free error resources\n");

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
/// listeners — generation only proceeds under `allow_unsupported`), document
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

    out.push_str("Passed as `i32` discriminant.\n\n");
    out.push_str("| Variant | Value |\n");
    out.push_str("|---------|-------|\n");
    for v in &e.variants {
        out.push_str(&format!("| `{}` | `{}` |\n", v.name, v.value));
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
            || m.modules.iter().any(|sub| module_any(sub, pred))
    }
    api.modules.iter().any(|m| module_any(m, pred))
}

/// The byte stride of one element of `ty` packed in a C array in linear memory
/// (wasm32: pointers and 32-bit scalars are 4 bytes, 64-bit values 8, bool 1).
fn wasm_stride(ty: &TypeRef) -> u32 {
    match ty {
        TypeRef::Bool => 1,
        TypeRef::I64 | TypeRef::F64 | TypeRef::Handle => 8,
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
        TypeRef::U32 => format!("{dv}.getUint32({off}, true)"),
        TypeRef::I32 | TypeRef::Enum(_) => format!("{dv}.getInt32({off}, true)"),
        TypeRef::I64 => format!("{dv}.getBigInt64({off}, true)"),
        TypeRef::Handle => format!("{dv}.getBigUint64({off}, true)"),
        TypeRef::F64 => format!("{dv}.getFloat64({off}, true)"),
        _ => format!("{dv}.getInt32({off}, true)"),
    }
}

/// Byte width of a scalar `ty` when boxed by pointer (optional-scalar ABI).
fn scalar_width(ty: &TypeRef) -> u32 {
    match ty {
        TypeRef::Bool => 1,
        TypeRef::I64 | TypeRef::F64 | TypeRef::Handle => 8,
        _ => 4,
    }
}

/// Emit a `DataView` write of scalar `ty` at `off` from JS value `val`.
fn emit_write_scalar(out: &mut String, indent: &str, ty: &TypeRef, dv: &str, off: &str, val: &str) {
    let stmt = match ty {
        TypeRef::Bool => format!("{dv}.setUint8({off}, {val} ? 1 : 0);"),
        TypeRef::U32 => format!("{dv}.setUint32({off}, {val}, true);"),
        TypeRef::I32 | TypeRef::Enum(_) => format!("{dv}.setInt32({off}, {val}, true);"),
        TypeRef::I64 => format!("{dv}.setBigInt64({off}, BigInt({val}), true);"),
        TypeRef::Handle => format!("{dv}.setBigUint64({off}, BigInt({val}), true);"),
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
        TypeRef::I64 | TypeRef::Handle => format!("BigInt({val})"),
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
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(
                out,
                "{indent}const [{tmp}_p, {tmp}_s] = _cstr(wasm, {value});"
            );
            args.push(format!("{tmp}_p"));
            cleanup.push(format!("wasm.weaveffi_dealloc({tmp}_p, {tmp}_s);"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let _ = writeln!(
                out,
                "{indent}const [{tmp}_p, {tmp}_l] = _bytes(wasm, {value});"
            );
            args.push(format!("{tmp}_p"));
            args.push(format!("{tmp}_l"));
            cleanup.push(format!("wasm.weaveffi_dealloc({tmp}_p, {tmp}_l);"));
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
            args.push(format!("{value}._handle"));
        }
        TypeRef::Bool
        | TypeRef::I32
        | TypeRef::U32
        | TypeRef::I64
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
                let _ = writeln!(out, "{indent}let {tmp}_p = 0, {tmp}_s = 0;");
                let _ = writeln!(
                    out,
                    "{indent}if ({value} !== null && {value} !== undefined) {{ [{tmp}_p, {tmp}_s] = _cstr(wasm, {value}); }}"
                );
                args.push(format!("{tmp}_p"));
                cleanup.push(format!(
                    "if ({tmp}_p !== 0) wasm.weaveffi_dealloc({tmp}_p, {tmp}_s);"
                ));
            }
            scalar => {
                let w = scalar_width(scalar);
                let _ = writeln!(out, "{indent}let {tmp}_p = 0;");
                let _ = writeln!(
                    out,
                    "{indent}if ({value} !== null && {value} !== undefined) {{"
                );
                let _ = writeln!(out, "{indent}  {tmp}_p = wasm.weaveffi_alloc({w});");
                let _ = writeln!(
                    out,
                    "{indent}  const {tmp}_dv = new DataView(wasm.memory.buffer);"
                );
                emit_write_scalar(
                    out,
                    &format!("{indent}  "),
                    scalar,
                    &format!("{tmp}_dv"),
                    &format!("{tmp}_p"),
                    value,
                );
                let _ = writeln!(out, "{indent}}}");
                args.push(format!("{tmp}_p"));
                cleanup.push(format!(
                    "if ({tmp}_p !== 0) wasm.weaveffi_dealloc({tmp}_p, {w});"
                ));
            }
        },
        TypeRef::List(inner) => {
            emit_stage_list(out, indent, inner, value, tmp, args, cleanup);
        }
        TypeRef::Map(k, v) => {
            let kt = format!("{tmp}_k");
            let vt = format!("{tmp}_v");
            let _ = writeln!(out, "{indent}const {tmp}_m = {value} || {{}};");
            let _ = writeln!(
                out,
                "{indent}const {tmp}_ks = ({tmp}_m instanceof Map) ? [...{tmp}_m.keys()] : Object.keys({tmp}_m);"
            );
            let _ = writeln!(
                out,
                "{indent}const {tmp}_vs = ({tmp}_m instanceof Map) ? [...{tmp}_m.values()] : Object.values({tmp}_m);"
            );
            let mut kargs = Vec::new();
            let mut vargs = Vec::new();
            emit_stage_list(
                out,
                indent,
                k,
                &format!("{tmp}_ks"),
                &kt,
                &mut kargs,
                cleanup,
            );
            emit_stage_list(
                out,
                indent,
                v,
                &format!("{tmp}_vs"),
                &vt,
                &mut vargs,
                cleanup,
            );
            // Each list staged `(base, len)`; the map ABI is `(keys, values, len)`.
            args.push(kargs[0].clone());
            args.push(vargs[0].clone());
            args.push(kargs[1].clone());
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as an input"),
    }
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
    let _ = writeln!(out, "{indent}const {tmp}_arr = {value} || [];");
    let _ = writeln!(out, "{indent}const {tmp}_n = {tmp}_arr.length;");
    let _ = writeln!(
        out,
        "{indent}const {tmp}_base = wasm.weaveffi_alloc({tmp}_n ? {tmp}_n * {stride} : 1);"
    );
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(out, "{indent}const {tmp}_ep = [];");
            let _ = writeln!(out, "{indent}for (let i = 0; i < {tmp}_n; i++) {tmp}_ep.push(_cstr(wasm, {tmp}_arr[i]));");
            let _ = writeln!(out, "{indent}{{");
            let _ = writeln!(
                out,
                "{indent}  const dv = new DataView(wasm.memory.buffer);"
            );
            let _ = writeln!(out, "{indent}  for (let i = 0; i < {tmp}_n; i++) dv.setUint32({tmp}_base + i * 4, {tmp}_ep[i][0], true);");
            let _ = writeln!(out, "{indent}}}");
            cleanup.push(format!(
                "for (const [ep, es] of {tmp}_ep) wasm.weaveffi_dealloc(ep, es);"
            ));
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
            let _ = writeln!(out, "{indent}{{");
            let _ = writeln!(
                out,
                "{indent}  const dv = new DataView(wasm.memory.buffer);"
            );
            let _ = writeln!(out, "{indent}  for (let i = 0; i < {tmp}_n; i++) dv.setInt32({tmp}_base + i * 4, {tmp}_arr[i]._handle, true);");
            let _ = writeln!(out, "{indent}}}");
        }
        scalar => {
            let _ = writeln!(out, "{indent}{{");
            let _ = writeln!(
                out,
                "{indent}  const dv = new DataView(wasm.memory.buffer);"
            );
            let _ = writeln!(out, "{indent}  for (let i = 0; i < {tmp}_n; i++) {{");
            emit_write_scalar(
                out,
                &format!("{indent}    "),
                scalar,
                "dv",
                &format!("{tmp}_base + i * {stride}"),
                &format!("{tmp}_arr[i]"),
            );
            let _ = writeln!(out, "{indent}  }}");
            let _ = writeln!(out, "{indent}}}");
        }
    }
    cleanup.push(format!(
        "wasm.weaveffi_dealloc({tmp}_base, {tmp}_n ? {tmp}_n * {stride} : 1);"
    ));
    args.push(format!("{tmp}_base"));
    args.push(format!("{tmp}_n"));
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
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(
                out,
                "{indent}const {target} = _takeStrArray(wasm, {base}, {len});"
            );
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let cls = local_type_name(name);
            let _ = writeln!(out, "{indent}const {target} = [];");
            let _ = writeln!(out, "{indent}{{");
            let _ = writeln!(
                out,
                "{indent}  const dv = new DataView(wasm.memory.buffer);"
            );
            let _ = writeln!(
                out,
                "{indent}  for (let i = 0; i < {len}; i++) {target}.push(new {cls}(wasm, dv.getInt32({base} + i * 4, true)));"
            );
            let _ = writeln!(out, "{indent}}}");
        }
        scalar => {
            let _ = writeln!(out, "{indent}const {target} = [];");
            let _ = writeln!(out, "{indent}{{");
            let _ = writeln!(
                out,
                "{indent}  const dv = new DataView(wasm.memory.buffer);"
            );
            let elem = wasm_read_scalar_elem(scalar, "dv", base, "i");
            let _ = writeln!(
                out,
                "{indent}  for (let i = 0; i < {len}; i++) {target}.push({elem});"
            );
            let _ = writeln!(out, "{indent}}}");
        }
    }
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
    let _ = writeln!(out, "{indent}const {target} = {{}};");
    let _ = writeln!(
        out,
        "{indent}for (let i = 0; i < {len}; i++) {target}[{target}_k[i]] = {target}_v[i];"
    );
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

    let mut call_args = in_args.to_vec();
    if needs_len {
        let _ = writeln!(out, "{indent}const _lp = wasm.weaveffi_alloc(4);");
        call_args.push("_lp".to_string());
    } else if needs_map {
        let _ = writeln!(out, "{indent}const _kp = wasm.weaveffi_alloc(4);");
        let _ = writeln!(out, "{indent}const _vp = wasm.weaveffi_alloc(4);");
        let _ = writeln!(out, "{indent}const _lp = wasm.weaveffi_alloc(4);");
        call_args.push("_kp".to_string());
        call_args.push("_vp".to_string());
        call_args.push("_lp".to_string());
    }
    if with_err {
        let _ = writeln!(out, "{indent}const _err = _allocErr(wasm);");
        call_args.push("_err".to_string());
    }

    let call = format!("wasm.{symbol}({})", call_args.join(", "));
    let captures_r = !needs_map && ret.is_some();
    if captures_r {
        let _ = writeln!(out, "{indent}const _r = {call};");
    } else {
        let _ = writeln!(out, "{indent}{call};");
    }

    for stmt in cleanup {
        let _ = writeln!(out, "{indent}{stmt}");
    }
    if with_err {
        let _ = writeln!(out, "{indent}_checkErr(wasm, _err);");
        let _ = writeln!(out, "{indent}_freeErr(wasm, _err);");
    }

    emit_decode_value(out, indent, ret, "_r");
}

/// Emit the `return ...;` (if any) that converts the raw result `r` plus any
/// `_lp`/`_kp`/`_vp` out-slots already in scope into the idiomatic value.
fn emit_decode_value(out: &mut String, indent: &str, ret: Option<&TypeRef>, r: &str) {
    let Some(ret) = ret else {
        return;
    };
    match ret {
        TypeRef::Bool => {
            let _ = writeln!(out, "{indent}return {r} !== 0;");
        }
        TypeRef::I32
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::F64
        | TypeRef::Handle
        | TypeRef::Enum(_) => {
            let _ = writeln!(out, "{indent}return {r};");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(out, "{indent}return _takeCStr(wasm, {r});");
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let cls = local_type_name(name);
            let _ = writeln!(out, "{indent}return new {cls}(wasm, {r});");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let _ = writeln!(out, "{indent}const _dv = new DataView(wasm.memory.buffer);");
            let _ = writeln!(out, "{indent}const _len = _dv.getUint32(_lp, true);");
            let _ = writeln!(out, "{indent}wasm.weaveffi_dealloc(_lp, 4);");
            let _ = writeln!(out, "{indent}return _takeBytes(wasm, {r}, _len);");
        }
        TypeRef::List(inner) => {
            let _ = writeln!(out, "{indent}const _dv = new DataView(wasm.memory.buffer);");
            let _ = writeln!(out, "{indent}const _len = _dv.getUint32(_lp, true);");
            let _ = writeln!(out, "{indent}wasm.weaveffi_dealloc(_lp, 4);");
            emit_read_list_into(out, indent, inner, r, "_len", "_out");
            let _ = writeln!(out, "{indent}return _out;");
        }
        TypeRef::Map(k, v) => {
            let _ = writeln!(out, "{indent}const _dv = new DataView(wasm.memory.buffer);");
            let _ = writeln!(out, "{indent}const _ka = _dv.getUint32(_kp, true);");
            let _ = writeln!(out, "{indent}const _va = _dv.getUint32(_vp, true);");
            let _ = writeln!(out, "{indent}const _len = _dv.getUint32(_lp, true);");
            let _ = writeln!(out, "{indent}wasm.weaveffi_dealloc(_kp, 4);");
            let _ = writeln!(out, "{indent}wasm.weaveffi_dealloc(_vp, 4);");
            let _ = writeln!(out, "{indent}wasm.weaveffi_dealloc(_lp, 4);");
            emit_read_map_into(out, indent, k, v, "_ka", "_va", "_len", "_out");
            let _ = writeln!(out, "{indent}return _out;");
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                let cls = local_type_name(name);
                let _ = writeln!(
                    out,
                    "{indent}return {r} === 0 ? null : new {cls}(wasm, {r});"
                );
            }
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                let _ = writeln!(out, "{indent}return _takeCStr(wasm, {r});");
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) | TypeRef::Map(_, _) => {
                // Aggregate optionals: a null base decodes to empty by the readers.
                emit_decode_value(out, indent, Some(inner), r);
            }
            scalar => {
                let getter = wasm_read_scalar_elem(scalar, "_dv", r, "0")
                    .replace(&format!("{r} + 0 * {}", wasm_stride(scalar)), r);
                let _ = writeln!(out, "{indent}if ({r} === 0) return null;");
                let _ = writeln!(out, "{indent}const _dv = new DataView(wasm.memory.buffer);");
                let _ = writeln!(out, "{indent}return {getter};");
            }
        },
        TypeRef::Iterator(_) => unreachable!("iterator returns handled separately"),
    }
}

fn ts_type_for(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 => "number".into(),
        TypeRef::Bool => "boolean".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Buffer".into(),
        TypeRef::Handle => "bigint".into(),
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
                out.push_str(&format!(" * @param {} {}\n", p.name, first));
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

fn render_wasm_dts(api: &Api, module_name: &str, input_basename: &str, filename: &str) -> String {
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
        out.push_str("  _raw: WebAssembly.Exports;\n");
        for module in &api.modules {
            render_dts_module_interface(&mut out, module, &module.name, "  ");
        }
    }
    out.push_str("}\n\n");

    out.push_str(&format!(
        "export function {load_fn}(url: string): Promise<{interface_name}>;\n\n"
    ));
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
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
        let mut tags = vec!["@throws {Error} if the native call fails".to_string()];
        if let Some(msg) = &func.deprecated {
            tags.insert(0, format!("@deprecated {msg}"));
        }
        emit_fn_doc(out, &func.doc, &func.params, &inner, &tags);
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

fn render_wasm_js_stub(
    api: &Api,
    module_name: &str,
    prefix: &str,
    input_basename: &str,
    filename: &str,
) -> String {
    let pascal_name = module_name.to_upper_camel_case();
    let load_fn = format!("load{pascal_name}");
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let model = BindingModel::build(api, prefix);
    let by_path: HashMap<&str, &ModuleBinding> =
        model.modules.iter().map(|m| (m.path.as_str(), m)).collect();

    let has_functions = model.modules.iter().any(|m| !m.functions.is_empty());
    let has_async = model.functions().any(|(_, f)| f.is_async);
    // Error messages always cross as C strings, so any sample with functions
    // needs the string-read helpers regardless of its declared types.
    let needs_strings = has_functions || api_deep_any(api, &|t| is_string_type(t));
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
    out.push_str("// Boundary conventions for a wasm32-unknown-unknown build:\n");
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

    if has_functions {
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
    out.push_str(&format!(
        " *   {prefix}_{{module}}_{{function}}(params...) -> result\n"
    ));
    out.push_str(" *\n");
    out.push_str(" * @example\n");
    out.push_str(&format!(" * const api = await {load_fn}('lib.wasm');\n"));
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
            render_js_module_object(&mut out, module, &module.name, &by_path, "    ");
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
        !mb.functions.is_empty() || !mb.structs.is_empty() || !mb.listeners.is_empty()
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
) {
    if !module_tree_has_content(m, module_path, by_path) {
        return;
    }
    let _ = writeln!(out, "{indent}{}: {{", m.name);
    let inner = format!("{indent}  ");
    let mb = by_path[module_path];
    for f in &mb.functions {
        match &f.shape {
            CallShape::Iterator(ib) => emit_js_iterator_function_wrapper(out, f, ib, &inner),
            _ if f.is_async => emit_js_async_function_wrapper(out, f, &inner),
            _ => emit_js_function_wrapper(out, f, &inner),
        }
    }
    for l in &mb.listeners {
        emit_js_listener_stub(out, l, &inner);
    }
    for s in &mb.structs {
        emit_js_struct_factory(out, s, &inner);
    }
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        render_js_module_object(out, sub, &sub_path, by_path, &inner);
    }
    let _ = writeln!(out, "{indent}}},");
}

/// Listeners are unsupported on wasm (see `WasmGenerator::capabilities`);
/// generation only reaches here when `allow_unsupported` is set. Each
/// register/unregister entry point becomes an explicit stub that throws at
/// call time, so the gap is impossible to miss from JS even though the
/// `.d.ts` deliberately omits the pair (a compile-time error for TS users).
fn emit_js_listener_stub(out: &mut String, l: &ListenerBinding, indent: &str) {
    for op in ["register", "unregister"] {
        let _ = writeln!(out, "{indent}{op}_{}() {{", l.name);
        let _ = writeln!(
            out,
            "{indent}  throw new Error(\"weaveffi: listener '{}' is not supported by the wasm \
             target (single-threaded wasm has no producer thread to deliver events); use a \
             native target for listeners\");",
            l.name
        );
        let _ = writeln!(out, "{indent}}},");
    }
}

/// Expose a struct's `create(...)` and (when present) `builder()` on the module
/// object, bound to the loaded `wasm` instance.
fn emit_js_struct_factory(out: &mut String, s: &StructBinding, indent: &str) {
    let _ = writeln!(out, "{indent}{}: {{", s.name);
    let _ = writeln!(
        out,
        "{indent}  create: (...args) => {}.create(wasm, ...args),",
        s.name
    );
    if s.builder.is_some() {
        let _ = writeln!(out, "{indent}  builder: () => new {}Builder(wasm),", s.name);
    }
    let _ = writeln!(out, "{indent}}},");
}

/// Emit a synchronous function as a method `name(params) { ... }` at `indent`,
/// staging idiomatic inputs, calling the C symbol, and decoding the return.
fn emit_js_function_wrapper(out: &mut String, f: &FnBinding, indent: &str) {
    let body = format!("{indent}  ");
    let js_params: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();

    if let Some(msg) = &f.deprecated {
        let _ = writeln!(out, "{indent}/** @deprecated {msg} */");
    }
    let _ = writeln!(out, "{indent}{}({}) {{", f.name, js_params.join(", "));

    let mut args = Vec::new();
    let mut cleanup = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            out,
            &body,
            &p.ty,
            &p.name,
            &format!("a{i}"),
            &mut args,
            &mut cleanup,
        );
    }
    emit_return_decode(out, &body, f.ret.as_ref(), &f.c_base, &args, &cleanup, true);
    let _ = writeln!(out, "{indent}}},");
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

    if let Some(msg) = &f.deprecated {
        let _ = writeln!(out, "{indent}/** @deprecated {msg} */");
    }
    let _ = writeln!(out, "{indent}{}({}) {{", f.name, js_params.join(", "));

    let mut args = Vec::new();
    let mut cleanup = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            out,
            &body,
            &p.ty,
            &p.name,
            &format!("a{i}"),
            &mut args,
            &mut cleanup,
        );
    }
    let _ = writeln!(out, "{body}const _err = _allocErr(wasm);");
    if f.cancellable {
        args.push("0".to_string());
    }
    args.push("_err".to_string());
    let _ = writeln!(
        out,
        "{body}const _it = wasm.{}({});",
        f.c_base,
        args.join(", ")
    );
    for stmt in &cleanup {
        let _ = writeln!(out, "{body}{stmt}");
    }
    let _ = writeln!(out, "{body}_checkErr(wasm, _err);");
    let _ = writeln!(out, "{body}_freeErr(wasm, _err);");

    let stride = wasm_stride(&ib.elem);
    let _ = writeln!(out, "{body}const _out = [];");
    let _ = writeln!(out, "{body}const _ip = wasm.weaveffi_alloc({stride});");
    let _ = writeln!(out, "{body}const _ierr = _allocErr(wasm);");
    let _ = writeln!(
        out,
        "{body}while (wasm.{}(_it, _ip, _ierr) !== 0) {{",
        ib.next.symbol
    );
    let _ = writeln!(out, "{body}  _checkErr(wasm, _ierr);");
    let _ = writeln!(out, "{body}  const _dv = new DataView(wasm.memory.buffer);");
    match &ib.elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(
                out,
                "{body}  _out.push(_takeCStr(wasm, _dv.getUint32(_ip, true)));"
            );
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let cls = local_type_name(name);
            let _ = writeln!(
                out,
                "{body}  _out.push(new {cls}(wasm, _dv.getInt32(_ip, true)));"
            );
        }
        scalar => {
            let elem = wasm_read_scalar_elem(scalar, "_dv", "_ip", "0")
                .replace(&format!("_ip + 0 * {stride}"), "_ip");
            let _ = writeln!(out, "{body}  _out.push({elem});");
        }
    }
    let _ = writeln!(out, "{body}}}");
    let _ = writeln!(out, "{body}_checkErr(wasm, _ierr);");
    let _ = writeln!(out, "{body}_freeErr(wasm, _ierr);");
    let _ = writeln!(out, "{body}wasm.weaveffi_dealloc(_ip, {stride});");
    let _ = writeln!(out, "{body}wasm.{}(_it);", ib.destroy_symbol);
    let _ = writeln!(out, "{body}return _out;");
    let _ = writeln!(out, "{indent}}},");
}

/// The wasm callback param-type list for an async function with the given
/// return: always `(ctx i32, err i32, ...result)`. Pointers/handles are i32 on
/// wasm32; only `i64`/`u64` widen to i64.
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
            | TypeRef::BorrowedStr
            | TypeRef::Struct(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Iterator(_),
        ) => {
            params.push("i32");
        }
        Some(TypeRef::I64 | TypeRef::Handle) => {
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
    let Some(ret) = ret else {
        let _ = writeln!(
            out,
            "{indent}_asyncContexts.set(ctxId, {{ resolve, reject }});"
        );
        return;
    };
    let open = format!("{indent}_asyncContexts.set(ctxId, {{ resolve, reject, unwrap: ");
    match ret {
        TypeRef::Bool => {
            let _ = writeln!(out, "{open}(w, r) => r !== 0 }});");
        }
        TypeRef::I32
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::F64
        | TypeRef::Handle
        | TypeRef::Enum(_) => {
            let _ = writeln!(
                out,
                "{indent}_asyncContexts.set(ctxId, {{ resolve, reject }});"
            );
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let _ = writeln!(out, "{open}(w, p) => _takeCStr(w, p) }});");
        }
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            let cls = local_type_name(name);
            let _ = writeln!(out, "{open}(w, h) => new {cls}(w, h) }});");
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                let cls = local_type_name(name);
                let _ = writeln!(out, "{open}(w, h) => h === 0 ? null : new {cls}(w, h) }});");
            }
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                let _ = writeln!(out, "{open}(w, p) => _takeCStr(w, p) }});");
            }
            _ => {
                let _ = writeln!(
                    out,
                    "{indent}_asyncContexts.set(ctxId, {{ resolve, reject }});"
                );
            }
        },
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let _ = writeln!(out, "{open}(w, ptr, len) => _takeBytes(w, ptr, len) }});");
        }
        TypeRef::List(inner) => {
            let _ = writeln!(out, "{open}(w, base, len) => {{");
            let _ = writeln!(out, "{indent}  const wasm = w;");
            emit_read_list_into(out, &format!("{indent}  "), inner, "base", "len", "_out");
            let _ = writeln!(out, "{indent}  return _out;");
            let _ = writeln!(out, "{indent}}} }});");
        }
        TypeRef::Map(k, v) => {
            let _ = writeln!(out, "{open}(w, ka, va, len) => {{");
            let _ = writeln!(out, "{indent}  const wasm = w;");
            emit_read_map_into(out, &format!("{indent}  "), k, v, "ka", "va", "len", "_out");
            let _ = writeln!(out, "{indent}  return _out;");
            let _ = writeln!(out, "{indent}}} }});");
        }
        TypeRef::Iterator(_) => {
            let _ = writeln!(
                out,
                "{indent}_asyncContexts.set(ctxId, {{ resolve, reject }});"
            );
        }
    }
}

/// Emit an async function as a method returning a `Promise` at `indent`.
fn emit_js_async_function_wrapper(out: &mut String, f: &FnBinding, indent: &str) {
    let body = format!("{indent}  ");
    let body2 = format!("{indent}    ");
    let js_params: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();

    if let Some(msg) = &f.deprecated {
        let _ = writeln!(out, "{indent}/** @deprecated {msg} */");
    }
    let _ = writeln!(out, "{indent}{}({}) {{", f.name, js_params.join(", "));
    let _ = writeln!(out, "{body}return new Promise((resolve, reject) => {{");
    let _ = writeln!(out, "{body2}const ctxId = _nextCtxId++;");
    emit_async_unwrap(out, &body2, f.ret.as_ref());

    let mut args = Vec::new();
    let mut cleanup = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            out,
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
    let _ = writeln!(out, "{body2}wasm.{}_async({});", f.c_base, args.join(", "));
    for stmt in &cleanup {
        let _ = writeln!(out, "{body2}{stmt}");
    }
    let _ = writeln!(out, "{body}}});");
    let _ = writeln!(out, "{indent}}},");
}

/// Emit the module-level `class` for a struct: constructor, field getters, and
/// a static `create(...)` factory.
fn emit_struct_class(out: &mut String, s: &StructBinding) {
    let cls = &s.name;
    let _ = writeln!(out, "class {cls} {{");
    let _ = writeln!(out, "  constructor(wasm, handle) {{");
    let _ = writeln!(out, "    this._wasm = wasm;");
    let _ = writeln!(out, "    this._handle = handle;");
    let _ = writeln!(out, "  }}");
    for field in &s.fields {
        emit_struct_getter(out, field);
    }
    emit_struct_create(out, s);
    let _ = writeln!(out, "}}");
    out.push('\n');
    if s.builder.is_some() {
        emit_builder_class(out, s);
    }
}

/// Emit one `get field() { ... }` accessor that decodes the C getter's return.
fn emit_struct_getter(out: &mut String, field: &FieldBinding) {
    let _ = writeln!(out, "  get {}() {{", field.name);
    let _ = writeln!(out, "    const wasm = this._wasm;");
    emit_return_decode(
        out,
        "    ",
        Some(&field.ty),
        &field.getter_symbol,
        &["this._handle".to_string()],
        &[],
        false,
    );
    let _ = writeln!(out, "  }}");
}

/// Emit `static create(wasm, <fields...>)` that stages every field and returns a
/// wrapped instance.
fn emit_struct_create(out: &mut String, s: &StructBinding) {
    let params: Vec<String> = s.fields.iter().map(|f| f.name.clone()).collect();
    let _ = writeln!(out, "  static create(wasm, {}) {{", params.join(", "));
    let mut args = Vec::new();
    let mut cleanup = Vec::new();
    for (i, f) in s.fields.iter().enumerate() {
        emit_stage_input(
            out,
            "    ",
            &f.ty,
            &f.name,
            &format!("a{i}"),
            &mut args,
            &mut cleanup,
        );
    }
    let ret = TypeRef::Struct(s.name.clone());
    emit_return_decode(
        out,
        "    ",
        Some(&ret),
        &s.create.symbol,
        &args,
        &cleanup,
        true,
    );
    let _ = writeln!(out, "  }}");
}

/// Emit the fluent `class XBuilder` for a struct that opted into a builder.
fn emit_builder_class(out: &mut String, s: &StructBinding) {
    let Some(b) = &s.builder else {
        return;
    };
    let cls = &s.name;
    let _ = writeln!(out, "class {cls}Builder {{");
    let _ = writeln!(out, "  constructor(wasm) {{");
    let _ = writeln!(out, "    this._wasm = wasm;");
    let _ = writeln!(out, "    this._b = wasm.{}();", b.new_symbol);
    let _ = writeln!(out, "  }}");
    for (field, (_fname, setter)) in s.fields.iter().zip(&b.setters) {
        let _ = writeln!(out, "  {}(value) {{", field.name);
        let _ = writeln!(out, "    const wasm = this._wasm;");
        let mut args = vec!["this._b".to_string()];
        let mut cleanup = Vec::new();
        emit_stage_input(
            out,
            "    ",
            &field.ty,
            "value",
            "a0",
            &mut args,
            &mut cleanup,
        );
        let _ = writeln!(out, "    wasm.{}({});", setter, args.join(", "));
        for stmt in &cleanup {
            let _ = writeln!(out, "    {stmt}");
        }
        let _ = writeln!(out, "    return this;");
        let _ = writeln!(out, "  }}");
    }
    let _ = writeln!(out, "  build() {{");
    let _ = writeln!(out, "    const wasm = this._wasm;");
    let ret = TypeRef::Struct(cls.clone());
    emit_return_decode(
        out,
        "    ",
        Some(&ret),
        &b.build_symbol,
        &["this._b".to_string()],
        &[],
        true,
    );
    let _ = writeln!(out, "  }}");
    let _ = writeln!(out, "}}");
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField};

    fn empty_api() -> Api {
        Api {
            version: "0.3.0".into(),
            modules: vec![],
            generators: None,
            package: None,
        }
    }

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.3.0".into(),
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
        );
        assert!(!dts.contains("register_message_listener"), "{dts}");
        assert!(dts.contains("send(text: string)"), "{dts}");
    }

    #[test]
    fn readme_documents_unsupported_features() {
        let readme = render_wasm_readme(&listener_api(), "weaveffi", "weaveffi.yml");
        assert!(readme.contains("## Unsupported Features"), "{readme}");
        assert!(readme.contains("events.message_listener"), "{readme}");
        assert!(readme.contains("events.OnMessage"), "{readme}");
        assert!(readme.contains("throw on call"), "{readme}");
    }

    #[test]
    fn supported_only_api_has_no_unsupported_section() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml");
        assert!(!readme.contains("## Unsupported Features"));
    }

    #[test]
    fn readme_documents_structs() {
        let readme = render_wasm_readme(&empty_api(), "weaveffi", "weaveffi.yml");
        assert!(readme.contains("### Structs"));
        assert!(readme.contains("opaque handles"));
        assert!(readme.contains("`i64` pointers"));
    }

    #[test]
    fn readme_documents_enums() {
        let readme = render_wasm_readme(&empty_api(), "weaveffi", "weaveffi.yml");
        assert!(readme.contains("### Enums"));
        assert!(readme.contains("`i32` values"));
        assert!(readme.contains("discriminant"));
    }

    #[test]
    fn readme_documents_optionals() {
        let readme = render_wasm_readme(&empty_api(), "weaveffi", "weaveffi.yml");
        assert!(readme.contains("### Optionals"));
        assert!(readme.contains("`0` / `null`"));
        assert!(readme.contains("_is_present"));
    }

    #[test]
    fn readme_documents_lists() {
        let readme = render_wasm_readme(&empty_api(), "weaveffi", "weaveffi.yml");
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
        let readme = render_wasm_readme(&empty_api(), "weaveffi", "weaveffi.yml");
        assert!(!readme.contains("## API Reference"));
    }

    #[test]
    fn api_reference_lists_module() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml");
        assert!(readme.contains("## API Reference"));
        assert!(readme.contains("### Module: `math`"));
    }

    #[test]
    fn api_reference_function_abi_name() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml");
        assert!(readme.contains("##### `weaveffi_math_add`"));
    }

    #[test]
    fn api_reference_function_signature() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml");
        assert!(readme.contains("`weaveffi_math_add(a: i32, b: i32) -> i32`"));
    }

    #[test]
    fn api_reference_function_param_table() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml");
        assert!(readme.contains("| `a` | `i32` | `i32` | native WASM i32 |"));
        assert!(readme.contains("| `b` | `i32` | `i32` | native WASM i32 |"));
        assert!(readme.contains("| _returns_ | `i32` | `i32` | native WASM i32 |"));
    }

    #[test]
    fn api_reference_function_doc() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml");
        assert!(readme.contains("Add two numbers"));
    }

    #[test]
    fn api_reference_struct_accessors() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml");
        assert!(readme.contains("##### `Point`"));
        assert!(readme.contains("opaque handle (`i64`)"));
        assert!(readme.contains("| `weaveffi_math_Point_get_x` | `f64` |"));
        assert!(readme.contains("| `weaveffi_math_Point_get_y` | `f64` |"));
    }

    #[test]
    fn api_reference_enum_discriminants() {
        let readme = render_wasm_readme(&sample_api(), "weaveffi", "weaveffi.yml");
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
        let readme = render_wasm_readme(&api, "weaveffi", "weaveffi.yml");
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
        let readme = render_wasm_readme(&api, "weaveffi", "weaveffi.yml");
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
        let readme = render_wasm_readme(&api, "weaveffi", "weaveffi.yml");
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
        let dts = render_wasm_dts(
            &api,
            DEFAULT_MODULE_NAME,
            "weaveffi.yml",
            "weaveffi_wasm.d.ts",
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
}
